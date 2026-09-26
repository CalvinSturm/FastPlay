//! Stream-copy export of an in/out clip range to a new file.
//!
//! Packets are remuxed, never decoded or re-encoded, so an export is fast and
//! lossless and touches no GPU state. The cost of copying is that the clip must
//! start on a video keyframe: the cut snaps back to the last keyframe at or
//! before the in-point, so the exported clip may begin slightly early. The end
//! is cut on the out-point.
//!
//! Runs entirely on the caller's (export worker) thread with its own demuxer;
//! it never shares FFmpeg state with the playback decode workers.

use std::{
    ffi::{c_void, CString},
    path::Path,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};

use super::*;

const MICROS_TIME_BASE: AVRational = AVRational {
    num: 1,
    den: 1_000_000,
};

/// What an export produced, for logging.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClipExportSummary {
    /// Where the written clip actually starts in source time. Earlier than the
    /// requested in-point when that point is not on a keyframe.
    pub actual_start: Duration,
    pub packets_written: u64,
}

/// Remux `[start, end)` of `source` into `destination`, copying every video,
/// audio and subtitle stream the destination container can hold. `start` and
/// `end` are source presentation timestamps; `end == None` exports through to
/// the end of the file. `cancel` aborts the export (checked per packet and
/// from FFmpeg's blocking I/O).
///
/// On failure any partially written `destination` is removed.
pub(crate) fn export_clip(
    source: &Path,
    destination: &Path,
    start: Duration,
    end: Option<Duration>,
    cancel: Box<dyn Fn() -> bool>,
) -> Result<ClipExportSummary, String> {
    if end.is_some_and(|end| end <= start) {
        return Err("clip range is empty".into());
    }
    let result = unsafe { remux_range(source, destination, start, end, cancel) };
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

/// Owns a muxer `AVFormatContext` and, once opened, its output `AVIOContext`.
struct OutputContext(*mut AVFormatContext);

impl Drop for OutputContext {
    fn drop(&mut self) {
        unsafe {
            if self.0.is_null() {
                return;
            }
            let oformat = (*self.0).oformat;
            if !oformat.is_null()
                && ((*oformat).flags & AVFMT_NOFILE as i32) == 0
                && !(*self.0).pb.is_null()
            {
                avio_closep(&mut (*self.0).pb);
            }
            avformat_free_context(self.0);
        }
    }
}

/// How one copied input stream maps onto the output.
struct StreamMap {
    output_index: i32,
    time_base: AVRational,
    /// The video stream whose keyframes decide where the clip may start.
    is_anchor: bool,
    /// Whether this stream must reach the out-point before the export may
    /// stop. Sparse subtitle streams do not gate the end.
    gates_end: bool,
    /// The stream's decode timestamps have reached the out-point.
    done: bool,
}

/// A packet read while the start keyframe is still being located.
struct BufferedPacket {
    input_index: usize,
    packet: Packet,
    ts_micros: i64,
    is_anchor: bool,
}

enum Step {
    Continue,
    Finished,
}

unsafe fn remux_range(
    source: &Path,
    destination: &Path,
    start: Duration,
    end: Option<Duration>,
    cancel: Box<dyn Fn() -> bool>,
) -> Result<ClipExportSummary, String> {
    let source_cstr = path_cstring(source, "source")?;
    let destination_cstr = path_cstring(destination, "destination")?;
    let start_micros = duration_micros(start);
    let end_micros = end.map(duration_micros).unwrap_or(i64::MAX);

    // Same open sequence as playback: arm the interrupt before the blocking
    // open, and declare `interrupt` before `input` so it outlives it.
    let format_context = fastplay_ffmpeg_alloc_context();
    if format_context.is_null() {
        return Err("avformat_alloc_context returned null".into());
    }
    let interrupt = Box::new(InterruptState::new(cancel));
    fastplay_ffmpeg_set_interrupt(
        format_context,
        Some(ffmpeg_interrupt_callback),
        (&*interrupt as *const InterruptState) as *mut c_void,
    );
    let mut format_context = format_context;
    interrupt.set_deadline(Some(Instant::now() + OPEN_TIMEOUT));
    check_blocking(
        avformat_open_input(
            &mut format_context,
            source_cstr.as_ptr(),
            null(),
            null_mut(),
        ),
        "avformat_open_input",
        &interrupt,
    )?;
    let input = InputContext(format_context);
    interrupt.set_deadline(Some(Instant::now() + OPEN_TIMEOUT));
    check_blocking(
        avformat_find_stream_info(input.0, null_mut()),
        "avformat_find_stream_info",
        &interrupt,
    )?;
    interrupt.set_deadline(None);

    let mut output_context: *mut AVFormatContext = null_mut();
    ffmpeg_check(
        avformat_alloc_output_context2(
            &mut output_context,
            null(),
            null(),
            destination_cstr.as_ptr(),
        ),
        "avformat_alloc_output_context2",
    )?;
    if output_context.is_null() {
        return Err("avformat_alloc_output_context2 returned null".into());
    }
    let output = OutputContext(output_context);

    let mut streams = map_streams(input.0, output.0)?;
    if !streams.iter().flatten().any(|stream| stream.gates_end) {
        return Err("no audio or video stream can be copied into the clip container".into());
    }
    let has_anchor = streams.iter().flatten().any(|stream| stream.is_anchor);

    let oformat = (*output.0).oformat;
    if ((*oformat).flags & AVFMT_NOFILE as i32) == 0 {
        ffmpeg_check(
            avio_open(
                &mut (*output.0).pb,
                destination_cstr.as_ptr(),
                AVIO_FLAG_WRITE as i32,
            ),
            "avio_open",
        )?;
    }
    ffmpeg_check(
        avformat_write_header(output.0, null_mut()),
        "avformat_write_header",
    )?;

    // Land on the keyframe at or before the in-point. A failed seek is not
    // fatal: reading from the top still finds the right keyframe, just slower.
    if start_micros > 0 {
        interrupt.set_deadline(Some(Instant::now() + SEEK_TIMEOUT));
        let _ = fastplay_ffmpeg_seek_to_micros(input.0, start_micros);
        interrupt.set_deadline(None);
    }

    let packet = Packet(av_packet_alloc());
    if packet.0.is_null() {
        return Err("av_packet_alloc returned null".into());
    }

    // Audio-only media can cut exactly on the in-point. With video the cut is
    // the last anchor keyframe at or before the in-point, which is only known
    // once the anchor stream moves past the in-point; until then packets are
    // held in `pending` (at most about one GOP).
    let mut cut_micros: Option<i64> = (!has_anchor).then_some(start_micros);
    let mut pending: Vec<BufferedPacket> = Vec::new();
    let mut pending_has_keyframe = false;
    let mut packets_written = 0u64;

    loop {
        if let Some(reason) = interrupt.aborted_reason() {
            return Err(format!("clip export {reason}"));
        }
        interrupt.set_deadline(Some(Instant::now() + READ_TIMEOUT));
        let read_status = av_read_frame(input.0, packet.0);
        if read_status == fastplay_ffmpeg_error_eof() {
            interrupt.set_deadline(None);
            break;
        }
        check_blocking(read_status, "av_read_frame", &interrupt)?;
        interrupt.set_deadline(None);

        let step = route_packet(
            output.0,
            packet.0,
            &mut streams,
            &mut cut_micros,
            &mut pending,
            &mut pending_has_keyframe,
            start_micros,
            end_micros,
            &mut packets_written,
        );
        av_packet_unref(packet.0);
        if let Step::Finished = step? {
            break;
        }
    }

    // End of file before the anchor passed the in-point (a clip in the final
    // GOP): the last keyframe seen is the cut.
    if cut_micros.is_none() && pending_has_keyframe {
        let _ = flush_pending(
            output.0,
            &mut streams,
            &mut cut_micros,
            &mut pending,
            end_micros,
            &mut packets_written,
        )?;
    }

    if packets_written == 0 {
        return Err("no media found in the selected range".into());
    }
    ffmpeg_check(av_write_trailer(output.0), "av_write_trailer")?;

    Ok(ClipExportSummary {
        actual_start: Duration::from_micros(cut_micros.unwrap_or(start_micros).max(0) as u64),
        packets_written,
    })
}

/// Create an output stream for every copyable input stream. Returns one entry
/// per input stream, `None` for streams that are skipped.
unsafe fn map_streams(
    input: *mut AVFormatContext,
    output: *mut AVFormatContext,
) -> Result<Vec<Option<StreamMap>>, String> {
    let anchor_index =
        av_find_best_stream(input, AVMediaType_AVMEDIA_TYPE_VIDEO, -1, -1, null_mut(), 0);
    let oformat = (*output).oformat;
    let stream_count = (*input).nb_streams;
    let mut streams = Vec::with_capacity(stream_count as usize);

    for index in 0..stream_count {
        let stream = fastplay_ffmpeg_stream_at(input, index);
        if stream.is_null() {
            streams.push(None);
            continue;
        }
        let parameters = fastplay_ffmpeg_stream_codecpar(stream);
        let kind = (*parameters).codec_type;
        let copyable = kind == AVMediaType_AVMEDIA_TYPE_VIDEO
            || kind == AVMediaType_AVMEDIA_TYPE_AUDIO
            || kind == AVMediaType_AVMEDIA_TYPE_SUBTITLE;
        // Cover art is a single still "video" packet at time zero; it is not
        // part of any clip range.
        let attached_picture = ((*stream).disposition & AV_DISPOSITION_ATTACHED_PIC as i32) != 0;
        // 0 means the container definitely cannot hold this codec (e.g. a
        // mov_text subtitle going into Matroska); negative means unknown, in
        // which case the header write is the arbiter.
        let supported = avformat_query_codec(oformat, (*parameters).codec_id, 0) != 0;
        if !copyable || attached_picture || !supported {
            streams.push(None);
            continue;
        }

        let out_stream = avformat_new_stream(output, null());
        if out_stream.is_null() {
            return Err("avformat_new_stream returned null".into());
        }
        ffmpeg_check(
            avcodec_parameters_copy((*out_stream).codecpar, parameters),
            "avcodec_parameters_copy",
        )?;
        // Keep the source codec tag when the output container accepts it
        // (e.g. HEVC `hvc1` in MP4, which Apple players require), otherwise
        // let the muxer choose. Mirrors the ffmpeg CLI's stream-copy rule.
        let tags = (*oformat).codec_tag;
        let codec_id = (*parameters).codec_id;
        let codec_tag = (*parameters).codec_tag;
        let mut container_tag = 0u32;
        let keep_tag = tags.is_null()
            || av_codec_get_id(tags, codec_tag) == codec_id
            || av_codec_get_tag2(tags, codec_id, &mut container_tag) == 0;
        if !keep_tag {
            (*(*out_stream).codecpar).codec_tag = 0;
        }
        let time_base = fastplay_ffmpeg_stream_time_base(stream);
        (*out_stream).time_base = time_base;

        streams.push(Some(StreamMap {
            output_index: (*out_stream).index,
            time_base,
            is_anchor: index as i32 == anchor_index,
            gates_end: kind != AVMediaType_AVMEDIA_TYPE_SUBTITLE,
            done: false,
        }));
    }

    Ok(streams)
}

#[allow(clippy::too_many_arguments)]
unsafe fn route_packet(
    output: *mut AVFormatContext,
    packet: *mut AVPacket,
    streams: &mut [Option<StreamMap>],
    cut_micros: &mut Option<i64>,
    pending: &mut Vec<BufferedPacket>,
    pending_has_keyframe: &mut bool,
    start_micros: i64,
    end_micros: i64,
    packets_written: &mut u64,
) -> Result<Step, String> {
    let input_index = (*packet).stream_index as usize;
    let Some(stream) = streams.get(input_index).and_then(Option::as_ref) else {
        return Ok(Step::Continue);
    };
    let Some((ts_micros, dts_micros)) = packet_micros(packet, stream.time_base) else {
        return Ok(Step::Continue);
    };
    let is_anchor = stream.is_anchor;

    if cut_micros.is_none() {
        if !is_anchor {
            pending.push(BufferedPacket {
                input_index,
                packet: clone_packet(packet)?,
                ts_micros,
                is_anchor,
            });
            return Ok(Step::Continue);
        }
        let is_keyframe = ((*packet).flags & AV_PKT_FLAG_KEY as i32) != 0;
        if is_keyframe && (ts_micros <= start_micros || !*pending_has_keyframe) {
            // A later keyframe at or before the in-point (or the first one
            // after a seek that overshot) supersedes everything held so far.
            pending.retain(|held| !held.is_anchor && held.ts_micros >= ts_micros);
            *pending_has_keyframe = true;
            if ts_micros > start_micros {
                *cut_micros = Some(ts_micros);
            }
        } else if !*pending_has_keyframe {
            // Anchor packets before the first keyframe cannot be decoded.
            return Ok(Step::Continue);
        } else if dts_micros > start_micros {
            // Decode order has moved past the in-point, so no later keyframe
            // can start the clip any closer to it: the held keyframe is final.
            if let Step::Finished = flush_pending(
                output,
                streams,
                cut_micros,
                pending,
                end_micros,
                packets_written,
            )? {
                return Ok(Step::Finished);
            }
        }
        if cut_micros.is_none() {
            pending.push(BufferedPacket {
                input_index,
                packet: clone_packet(packet)?,
                ts_micros,
                is_anchor,
            });
            return Ok(Step::Continue);
        }
        if !pending.is_empty() {
            if let Step::Finished = flush_pending(
                output,
                streams,
                cut_micros,
                pending,
                end_micros,
                packets_written,
            )? {
                return Ok(Step::Finished);
            }
        }
    }

    emit_packet(
        output,
        packet,
        input_index,
        streams,
        cut_micros.unwrap_or(start_micros),
        end_micros,
        packets_written,
    )
}

/// Fix the cut at the held anchor keyframe and write everything held since.
unsafe fn flush_pending(
    output: *mut AVFormatContext,
    streams: &mut [Option<StreamMap>],
    cut_micros: &mut Option<i64>,
    pending: &mut Vec<BufferedPacket>,
    end_micros: i64,
    packets_written: &mut u64,
) -> Result<Step, String> {
    let cut = match *cut_micros {
        Some(cut) => cut,
        None => {
            let Some(keyframe) = pending.iter().find(|held| held.is_anchor) else {
                return Ok(Step::Continue);
            };
            keyframe.ts_micros
        }
    };
    *cut_micros = Some(cut);
    let mut step = Step::Continue;
    for held in pending.drain(..) {
        if let Step::Finished = emit_packet(
            output,
            held.packet.0,
            held.input_index,
            streams,
            cut,
            end_micros,
            packets_written,
        )? {
            step = Step::Finished;
        }
    }
    Ok(step)
}

/// Write one packet shifted so the cut lands at time zero, applying the
/// out-point and dropping non-anchor packets from before the cut.
unsafe fn emit_packet(
    output: *mut AVFormatContext,
    packet: *mut AVPacket,
    input_index: usize,
    streams: &mut [Option<StreamMap>],
    cut_micros: i64,
    end_micros: i64,
    packets_written: &mut u64,
) -> Result<Step, String> {
    let Some(stream) = streams.get_mut(input_index).and_then(Option::as_mut) else {
        return Ok(Step::Continue);
    };
    if stream.done {
        return Ok(Step::Continue);
    }
    let Some((ts_micros, dts_micros)) = packet_micros(packet, stream.time_base) else {
        return Ok(Step::Continue);
    };

    if dts_micros >= end_micros {
        stream.done = true;
        let finished = streams
            .iter()
            .flatten()
            .filter(|stream| stream.gates_end)
            .all(|stream| stream.done);
        return Ok(if finished {
            Step::Finished
        } else {
            Step::Continue
        });
    }
    // Everything after the anchor keyframe is needed to decode the video, but
    // audio and subtitles from before the cut are just lead-in.
    if !stream.is_anchor && ts_micros < cut_micros {
        return Ok(Step::Continue);
    }

    let offset = av_rescale_q(cut_micros, MICROS_TIME_BASE, stream.time_base);
    if (*packet).pts != AV_NOPTS_SENTINEL {
        (*packet).pts -= offset;
    }
    if (*packet).dts != AV_NOPTS_SENTINEL {
        (*packet).dts -= offset;
    }
    let out_stream = fastplay_ffmpeg_stream_at(output, stream.output_index as u32);
    if out_stream.is_null() {
        return Err("clip export output stream missing".into());
    }
    av_packet_rescale_ts(packet, stream.time_base, (*out_stream).time_base);
    (*packet).stream_index = stream.output_index;
    (*packet).pos = -1;
    ffmpeg_check(
        av_interleaved_write_frame(output, packet),
        "av_interleaved_write_frame",
    )?;
    *packets_written += 1;
    Ok(Step::Continue)
}

/// The packet's presentation time (falling back to decode time) and decode
/// time (falling back to presentation time) in microseconds, or `None` when it
/// carries neither and cannot be placed in the range.
unsafe fn packet_micros(packet: *mut AVPacket, time_base: AVRational) -> Option<(i64, i64)> {
    let pts = (*packet).pts;
    let dts = (*packet).dts;
    let ts = if pts != AV_NOPTS_SENTINEL { pts } else { dts };
    if ts == AV_NOPTS_SENTINEL {
        return None;
    }
    let ts_micros = av_rescale_q(ts, time_base, MICROS_TIME_BASE);
    let dts_micros = if dts != AV_NOPTS_SENTINEL {
        av_rescale_q(dts, time_base, MICROS_TIME_BASE)
    } else {
        ts_micros
    };
    Some((ts_micros, dts_micros))
}

unsafe fn clone_packet(packet: *mut AVPacket) -> Result<Packet, String> {
    let clone = av_packet_clone(packet);
    if clone.is_null() {
        return Err("av_packet_clone returned null".into());
    }
    Ok(Packet(clone))
}

fn duration_micros(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

fn path_cstring(path: &Path, role: &str) -> Result<CString, String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("clip {role} path must be valid UTF-8 for FFmpeg"))?;
    CString::new(path).map_err(|_| format!("clip {role} path contained NUL"))
}
