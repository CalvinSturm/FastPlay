#![allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    improper_ctypes,
    unnecessary_transmutes
)]

use std::{
    ffi::{c_void, CStr, CString},
    ptr::{null, null_mut},
    time::Duration,
};

use crate::{
    ffi::d3d11::{D3D11Device, VideoSurface},
    media::{
        audio::AudioStreamFormat,
        source::MediaSource,
        video::{VideoDecodeMode, VideoDecodePreference},
    },
    playback::generations::{OpenGeneration, OperationId, SeekGeneration},
};

include!(concat!(env!("OUT_DIR"), "/ffmpeg_bindings.rs"));

const SWS_BILINEAR_FLAGS: i32 = 2;
const AV_NOPTS_SENTINEL: i64 = i64::MIN;
const AV_TIME_BASE_MICROS: i128 = 1_000_000;

#[derive(Debug)]
pub(crate) enum PendingVideoFrame {
    D3D11 {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        pts: Duration,
        width: u32,
        height: u32,
        surface: VideoSurface,
    },
}

impl PendingVideoFrame {
    pub fn open_gen(&self) -> OpenGeneration {
        match self {
            Self::D3D11 { open_gen, .. } => *open_gen,
        }
    }

    pub fn seek_gen(&self) -> SeekGeneration {
        match self {
            Self::D3D11 { seek_gen, .. } => *seek_gen,
        }
    }

    pub fn op_id(&self) -> OperationId {
        match self {
            Self::D3D11 { op_id, .. } => *op_id,
        }
    }

    pub fn pts(&self) -> Duration {
        match self {
            Self::D3D11 { pts, .. } => *pts,
        }
    }
}

#[derive(Debug)]
pub(crate) struct PendingAudioFrame {
    pub open_gen: OpenGeneration,
    pub seek_gen: SeekGeneration,
    pub op_id: OperationId,
    pub pts: Duration,
    pub format: AudioStreamFormat,
    pub frame_count: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamSummary {
    pub had_audio_stream: bool,
    pub produced_video_frames: u64,
    pub produced_audio_frames: u64,
    pub decode_mode: VideoDecodeMode,
    pub hw_fallback_count: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamStatus {
    Completed(StreamSummary),
    Cancelled,
}

pub(crate) fn stream_media<V, A, C>(
    source: &MediaSource,
    device: &D3D11Device,
    audio_output_format: AudioStreamFormat,
    start_position: Option<Duration>,
    decode_preference: VideoDecodePreference,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    mut on_decode_mode: impl FnMut(VideoDecodeMode, u64, u8) -> Result<(), String>,
    mut on_duration: impl FnMut(Duration) -> Result<(), String>,
    should_cancel: C,
    mut on_video: V,
    mut on_audio: A,
) -> Result<StreamStatus, String>
where
    V: FnMut(PendingVideoFrame) -> Result<(), String>,
    A: FnMut(PendingAudioFrame) -> Result<(), String>,
    C: Fn() -> bool,
{
    let source_path = source
        .path()
        .to_str()
        .ok_or_else(|| "media path must be valid UTF-8 for FFmpeg open".to_string())?;
    let source_cstr =
        CString::new(source_path).map_err(|_| "media path contained NUL".to_string())?;

    unsafe {
        let mut format_context: *mut AVFormatContext = null_mut();
        ffmpeg_check(
            avformat_open_input(
                &mut format_context,
                source_cstr.as_ptr(),
                null(),
                null_mut(),
            ),
            "avformat_open_input",
        )?;
        let input = InputContext(format_context);

        ffmpeg_check(
            avformat_find_stream_info(input.0, null_mut()),
            "avformat_find_stream_info",
        )?;

        // Check cancellation before allocating a hardware decoder on the GPU.
        // Without this, rapid seeks pile up concurrent decoder sessions from
        // threads that haven't reached the main decode loop yet, exhausting
        // the GPU's session limit (typically 8-16) and causing device loss.
        if should_cancel() {
            return Ok(StreamStatus::Cancelled);
        }

        let mut video = open_video_decoder(input.0, device, decode_preference)?;
        // Re-check after decoder creation so a cancel that arrived during
        // open_video_decoder drops the session immediately.
        if should_cancel() {
            return Ok(StreamStatus::Cancelled);
        }
        let mut audio = open_audio_decoder(input.0, audio_output_format)?;
        let mut audio_batch = audio
            .as_ref()
            .map(|audio| AudioBatcher::new(audio.output_format));
        on_decode_mode(video.mode, video.hw_fallback_count, video.rotation_quarter_turns)?;
        let mut summary = StreamSummary {
            had_audio_stream: audio.is_some(),
            produced_video_frames: 0,
            produced_audio_frames: 0,
            decode_mode: video.mode,
            hw_fallback_count: video.hw_fallback_count,
        };
        let total_duration = frame_pts(fastplay_ffmpeg_duration_micros(input.0), AVRational {
            num: 1,
            den: 1_000_000,
        });
        if !total_duration.is_zero() {
            on_duration(total_duration)?;
        }

        if let Some(target) = start_position {
            eprintln!("[worker] seeking to {:.3}s", target.as_secs_f64());
            seek_and_flush(input.0, &video, audio.as_ref(), target)?;
        }

        let packet = av_packet_alloc();
        if packet.is_null() {
            return Err("av_packet_alloc returned null".into());
        }
        let packet = Packet(packet);

        let frame = av_frame_alloc();
        if frame.is_null() {
            return Err("av_frame_alloc returned null".into());
        }
        let frame = Frame(frame);

        let mut hw_mid_fallback_done = false;

        loop {
            if should_cancel() {
                return Ok(StreamStatus::Cancelled);
            }

            let read_status = av_read_frame(input.0, packet.0);
            if read_status == fastplay_ffmpeg_error_eof() {
                break;
            }
            ffmpeg_check(read_status, "av_read_frame")?;

            if (*packet.0).stream_index == video.stream_index as i32 {
                // Bail out early if the D3D11 device was removed (GPU TDR)
                // during hardware decode — avcodec_send_packet would call into
                // the dead device via FFmpeg's D3D11VA backend and crash in
                // avutil-60.dll.
                if video.mode == VideoDecodeMode::HardwareD3D11 && device.is_device_removed() {
                    av_packet_unref(packet.0);
                    return Err("D3D11 device removed during hardware decode".into());
                }
                let send_result = avcodec_send_packet(video.codec.0, packet.0);
                if send_result < 0
                    && video.mode == VideoDecodeMode::HardwareD3D11
                    && !hw_mid_fallback_done
                {
                    // HW decode failed on first real packet — try software fallback.
                    av_packet_unref(packet.0);
                    match open_software_video_decoder(input.0) {
                        Ok(mut sw_decoder) => {
                            eprintln!(
                                "hw decode failed mid-stream ({}), falling back to software",
                                send_result
                            );
                            sw_decoder.hw_fallback_count = video.hw_fallback_count + 1;
                            video = sw_decoder;
                            hw_mid_fallback_done = true;
                            summary.decode_mode = video.mode;
                            summary.hw_fallback_count = video.hw_fallback_count;
                            on_decode_mode(video.mode, video.hw_fallback_count, video.rotation_quarter_turns)?;
                            let restart = start_position.unwrap_or(Duration::ZERO);
                            seek_and_flush(input.0, &video, audio.as_ref(), restart)?;
                            continue;
                        }
                        Err(sw_error) => {
                            return Err(ffmpeg_check(send_result, "avcodec_send_packet(video)")
                                .unwrap_err()
                                + &format!("; software fallback also failed: {sw_error}"));
                        }
                    }
                }
                ffmpeg_check(send_result, "avcodec_send_packet(video)")?;
                av_packet_unref(packet.0);
                receive_video_frames(
                    &mut video,
                    frame.0,
                    device,
                    open_gen,
                    seek_gen,
                    op_id,
                    &mut summary.produced_video_frames,
                    &mut on_video,
                    &|| should_cancel(),
                )?;
                continue;
            }

            if let Some(audio) = audio.as_mut() {
                if (*packet.0).stream_index == audio.stream_index as i32 {
                    ffmpeg_check(
                        avcodec_send_packet(audio.codec.0, packet.0),
                        "avcodec_send_packet(audio)",
                    )?;
                    av_packet_unref(packet.0);
                    receive_audio_frames(
                        audio,
                    frame.0,
                    open_gen,
                    seek_gen,
                    op_id,
                    audio_batch.as_mut(),
                    &mut summary.produced_audio_frames,
                    &mut on_audio,
                    &|| should_cancel(),
                )?;
                continue;
            }
            }

            av_packet_unref(packet.0);
        }

        if should_cancel() {
            return Ok(StreamStatus::Cancelled);
        }

        ffmpeg_check(
            avcodec_send_packet(video.codec.0, null()),
            "avcodec_send_packet(video flush)",
        )?;
        receive_video_frames(
            &mut video,
            frame.0,
            device,
            open_gen,
            seek_gen,
            op_id,
            &mut summary.produced_video_frames,
            &mut on_video,
            &|| should_cancel(),
        )?;

        if let Some(audio) = audio.as_mut() {
            ffmpeg_check(
                avcodec_send_packet(audio.codec.0, null()),
                "avcodec_send_packet(audio flush)",
            )?;
            receive_audio_frames(
                audio,
                frame.0,
                open_gen,
                seek_gen,
                op_id,
                audio_batch.as_mut(),
                &mut summary.produced_audio_frames,
                &mut on_audio,
                &|| should_cancel(),
            )?;
            if let Some(batch) = audio_batch.as_mut() {
                batch.flush(open_gen, seek_gen, op_id, &mut summary.produced_audio_frames, &mut on_audio)?;
            }
        }

        if summary.produced_video_frames == 0 {
            return Err("no decodable video frame was produced".into());
        }

        Ok(StreamStatus::Completed(summary))
    }
}

struct VideoDecoder {
    stream_index: usize,
    codec: CodecContext,
    pts_time_base: AVRational,
    output: VideoDecoderOutput,
    mode: VideoDecodeMode,
    hw_fallback_count: u64,
    /// Clockwise quarter-turns derived from the stream's display matrix side
    /// data (0 = no rotation, 1 = 90° CW, 2 = 180°, 3 = 270° CW).
    rotation_quarter_turns: u8,
}

enum VideoDecoderOutput {
    Hardware,
    Software(SoftwareVideoConverter),
}

struct AudioDecoder {
    stream_index: usize,
    codec: CodecContext,
    pts_time_base: AVRational,
    resampler: Resampler,
    output_format: AudioStreamFormat,
}

unsafe fn seek_and_flush(
    format_context: *mut AVFormatContext,
    video: &VideoDecoder,
    audio: Option<&AudioDecoder>,
    target: Duration,
) -> Result<(), String> {
    let target_micros = target.as_micros().min(i64::MAX as u128) as i64;
    let start_time_micros = fastplay_ffmpeg_start_time_micros(format_context);
    let absolute_target_micros = if start_time_micros == AV_NOPTS_SENTINEL {
        target_micros
    } else {
        start_time_micros.saturating_add(target_micros)
    };
    ffmpeg_check(
        fastplay_ffmpeg_seek_to_micros(format_context, absolute_target_micros),
        "av_seek_frame",
    )?;
    fastplay_ffmpeg_flush_codec(video.codec.0);
    if let Some(audio) = audio {
        fastplay_ffmpeg_flush_codec(audio.codec.0);
    }
    Ok(())
}

unsafe fn open_video_decoder(
    format_context: *mut AVFormatContext,
    device: &D3D11Device,
    decode_preference: VideoDecodePreference,
) -> Result<VideoDecoder, String> {
    match decode_preference {
        VideoDecodePreference::ForceSoftware => open_software_video_decoder(format_context),
        VideoDecodePreference::Auto => match open_hardware_video_decoder(format_context, device) {
            Ok(decoder) => Ok(decoder),
            Err(hw_error) => match open_software_video_decoder(format_context) {
                Ok(mut decoder) => {
                    decoder.hw_fallback_count = 1;
                    eprintln!("video decode fallback: {hw_error}");
                    Ok(decoder)
                }
                Err(sw_error) => Err(format!(
                    "hardware decode unavailable ({hw_error}); software fallback failed ({sw_error})"
                )),
            },
        },
    }
}

/// Read the clockwise rotation in quarter-turns from a stream's display matrix
/// side data. Returns 0 if no rotation metadata is present.
unsafe fn stream_rotation_quarter_turns(codec_parameters: *const AVCodecParameters) -> u8 {
    if codec_parameters.is_null() {
        return 0;
    }
    let side_data = (*codec_parameters).coded_side_data;
    let count = (*codec_parameters).nb_coded_side_data;
    if side_data.is_null() || count <= 0 {
        return 0;
    }
    for i in 0..count as usize {
        let entry = &*side_data.add(i);
        if entry.type_ != AVPacketSideDataType_AV_PKT_DATA_DISPLAYMATRIX {
            continue;
        }
        if entry.size < 36 || entry.data.is_null() {
            break;
        }
        // The display matrix is a 3x3 array of i32 in fixed-point (Q16.16).
        let m = entry.data as *const i32;
        let a = *m.add(0) as f64 / 65536.0; // cos(θ) * scale
        let b = *m.add(1) as f64 / 65536.0; // sin(θ) * scale
        let scale = (a * a + b * b).sqrt();
        if scale < 1e-6 {
            break;
        }
        // av_display_rotation_get uses CCW convention: -atan2(b, a).
        // D3D11 VideoProcessorSetStreamRotation uses CW convention,
        // so we negate to get CW degrees: atan2(b, a).
        let cw_degrees = b.atan2(a).to_degrees();
        // Round to nearest 90° and express as clockwise quarter-turns.
        let quarter = ((cw_degrees / 90.0).round() as i32).rem_euclid(4) as u8;
        eprintln!("display_matrix rotation: {cw_degrees:.1}° CW → {quarter} quarter-turns");
        return quarter;
    }
    0
}

unsafe fn open_hardware_video_decoder(
    format_context: *mut AVFormatContext,
    device: &D3D11Device,
) -> Result<VideoDecoder, String> {
    let mut decoder: *const AVCodec = null();
    let stream_index = ffmpeg_check(
        av_find_best_stream(
            format_context,
            AVMediaType_AVMEDIA_TYPE_VIDEO,
            -1,
            -1,
            &mut decoder,
            0,
        ),
        "av_find_best_stream(video)",
    )? as usize;
    if decoder.is_null() {
        return Err("no decoder found for selected video stream".into());
    }

    let stream = selected_stream(format_context, stream_index)?;
    let codec_context = avcodec_alloc_context3(decoder);
    if codec_context.is_null() {
        return Err("avcodec_alloc_context3(video) returned null".into());
    }
    let codec = CodecContext(codec_context);

    let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
    if codec_parameters.is_null() {
        return Err("selected video stream codec parameters were null".into());
    }

    let rotation_quarter_turns = stream_rotation_quarter_turns(codec_parameters);

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(video)",
    )?;
    let pts_time_base = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).pkt_timebase = pts_time_base;
    (*codec.0).get_format = Some(select_d3d11_pixel_format);
    configure_hw_device(codec.0, device, decoder)?;
    ffmpeg_check(avcodec_open2(codec.0, decoder, null_mut()), "avcodec_open2(video)")?;

    Ok(VideoDecoder {
        stream_index,
        codec,
        pts_time_base,
        output: VideoDecoderOutput::Hardware,
        mode: VideoDecodeMode::HardwareD3D11,
        hw_fallback_count: 0,
        rotation_quarter_turns,
    })
}

unsafe fn open_software_video_decoder(
    format_context: *mut AVFormatContext,
) -> Result<VideoDecoder, String> {
    let mut decoder: *const AVCodec = null();
    let stream_index = ffmpeg_check(
        av_find_best_stream(
            format_context,
            AVMediaType_AVMEDIA_TYPE_VIDEO,
            -1,
            -1,
            &mut decoder,
            0,
        ),
        "av_find_best_stream(video)",
    )? as usize;
    if decoder.is_null() {
        return Err("no decoder found for selected video stream".into());
    }

    let stream = selected_stream(format_context, stream_index)?;
    let codec_context = avcodec_alloc_context3(decoder);
    if codec_context.is_null() {
        return Err("avcodec_alloc_context3(video) returned null".into());
    }
    let codec = CodecContext(codec_context);

    let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
    if codec_parameters.is_null() {
        return Err("selected video stream codec parameters were null".into());
    }

    let rotation_quarter_turns = stream_rotation_quarter_turns(codec_parameters);

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(video)",
    )?;
    let pts_time_base = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).pkt_timebase = pts_time_base;
    ffmpeg_check(avcodec_open2(codec.0, decoder, null_mut()), "avcodec_open2(video)")?;

    Ok(VideoDecoder {
        stream_index,
        codec,
        pts_time_base,
        output: VideoDecoderOutput::Software(SoftwareVideoConverter::default()),
        mode: VideoDecodeMode::Software,
        hw_fallback_count: 0,
        rotation_quarter_turns,
    })
}

unsafe fn open_audio_decoder(
    format_context: *mut AVFormatContext,
    output_format: AudioStreamFormat,
) -> Result<Option<AudioDecoder>, String> {
    let mut decoder: *const AVCodec = null();
    let stream_index = av_find_best_stream(
        format_context,
        AVMediaType_AVMEDIA_TYPE_AUDIO,
        -1,
        -1,
        &mut decoder,
        0,
    );
    if stream_index == fastplay_ffmpeg_error_stream_not_found() {
        return Ok(None);
    }
    ffmpeg_check(stream_index, "av_find_best_stream(audio)")?;
    if decoder.is_null() {
        return Err("no decoder found for selected audio stream".into());
    }

    let stream_index = stream_index as usize;
    let stream = selected_stream(format_context, stream_index)?;
    let codec_context = avcodec_alloc_context3(decoder);
    if codec_context.is_null() {
        return Err("avcodec_alloc_context3(audio) returned null".into());
    }
    let codec = CodecContext(codec_context);

    let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
    if codec_parameters.is_null() {
        return Err("selected audio stream codec parameters were null".into());
    }

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(audio)",
    )?;
    let pts_time_base = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).pkt_timebase = pts_time_base;
    ffmpeg_check(avcodec_open2(codec.0, decoder, null_mut()), "avcodec_open2(audio)")?;

    let input_channel_layout = &(*codec.0).ch_layout;
    if fastplay_ffmpeg_channel_layout_mask_or_default(input_channel_layout) == 0 {
        return Err("audio decoder did not provide a usable channel layout".into());
    }

    let resampler = Resampler::new(
        output_format,
        input_channel_layout,
        (*codec.0).sample_fmt,
        (*codec.0).sample_rate,
    )?;

    Ok(Some(AudioDecoder {
        stream_index,
        codec,
        pts_time_base,
        resampler,
        output_format,
    }))
}

unsafe fn receive_video_frames<F>(
    video: &mut VideoDecoder,
    frame: *mut AVFrame,
    device: &D3D11Device,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    produced_frames: &mut u64,
    on_frame: &mut F,
    should_cancel: &dyn Fn() -> bool,
) -> Result<(), String>
where
    F: FnMut(PendingVideoFrame) -> Result<(), String>,
{
    loop {
        if should_cancel() {
            return Ok(());
        }
        // When using hardware decode, FFmpeg's avcodec_receive_frame calls
        // into D3D11 internally (av_hwframe_transfer_data).  If the GPU has
        // TDR'd, those calls crash inside avutil/d3d11.dll.  Check device
        // health before touching the codec so the worker exits cleanly.
        if matches!(video.output, VideoDecoderOutput::Hardware) && device.is_device_removed() {
            return Err("D3D11 device removed during hardware decode".into());
        }
        let status = avcodec_receive_frame(video.codec.0, frame);
        if status == fastplay_ffmpeg_error_eagain() || status == fastplay_ffmpeg_error_eof() {
            return Ok(());
        }
        ffmpeg_check(status, "avcodec_receive_frame(video)")?;

        // Check cancellation *after* receiving the frame but *before*
        // the expensive CreateTexture2D + CopySubresourceRegion.  This
        // prevents stale workers from allocating GPU textures for frames
        // that will be immediately discarded, reducing VRAM pressure
        // during rapid seeking.
        if should_cancel() {
            av_frame_unref(frame);
            return Ok(());
        }

        let result = match &mut video.output {
            VideoDecoderOutput::Hardware => {
                let pixel_format = (*frame).format as AVPixelFormat;
                if pixel_format != AVPixelFormat_AV_PIX_FMT_D3D11 {
                    av_frame_unref(frame);
                    return Err(format!(
                        "decoder produced unexpected pixel format {} instead of AV_PIX_FMT_D3D11",
                        (*frame).format
                    ));
                }

                let surface = device
                    .surface_from_raw_texture(
                        (*frame).data[0].cast::<c_void>(),
                        (*frame).data[1] as usize as u32,
                        (*frame).width as u32,
                        (*frame).height as u32,
                    )
                    .map_err(|error| error.to_string())?;

                PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts: decoded_frame_pts(frame, video.pts_time_base),
                    width: (*frame).width as u32,
                    height: (*frame).height as u32,
                    surface,
                }
            }
            VideoDecoderOutput::Software(converter) => {
                let surface = converter.convert(frame, device)?;
                PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts: decoded_frame_pts(frame, video.pts_time_base),
                    width: (*frame).width as u32,
                    height: (*frame).height as u32,
                    surface,
                }
            }
        };
        av_frame_unref(frame);
        *produced_frames = (*produced_frames).saturating_add(1);
        on_frame(result)?;
    }
}

#[derive(Default)]
struct SoftwareVideoConverter {
    context: *mut SwsContext,
    source_width: i32,
    source_height: i32,
    source_format: AVPixelFormat,
    /// Reusable contiguous NV12 buffer: Y plane followed immediately by UV plane.
    /// Avoids per-frame heap allocation once the first frame has been decoded.
    frame_buf: Vec<u8>,
}

impl SoftwareVideoConverter {
    unsafe fn convert(
        &mut self,
        frame: *mut AVFrame,
        device: &D3D11Device,
    ) -> Result<VideoSurface, String> {
        let width = (*frame).width;
        let height = (*frame).height;
        if width <= 0 || height <= 0 {
            return Err("software decode produced invalid frame dimensions".into());
        }
        if width % 2 != 0 || height % 2 != 0 {
            return Err("software fallback currently supports only even-sized frames".into());
        }

        let source_format = (*frame).format as AVPixelFormat;
        if self.context.is_null()
            || self.source_width != width
            || self.source_height != height
            || self.source_format != source_format
        {
            self.recreate(width, height, source_format)?;
        }

        let stride = width as usize;
        let y_len = stride * height as usize;
        let uv_len = stride * (height as usize / 2);
        let total = y_len + uv_len;
        self.frame_buf.resize(total, 0);

        // Point sws_scale directly into the contiguous buffer: Y at offset 0,
        // UV immediately after the Y plane.
        let mut dst_data = [
            self.frame_buf.as_mut_ptr(),
            self.frame_buf.as_mut_ptr().add(y_len),
            null_mut(),
            null_mut(),
        ];
        let mut dst_linesize = [stride as i32, stride as i32, 0, 0];

        let scaled = sws_scale(
            self.context,
            (*frame).data.as_ptr().cast(),
            (*frame).linesize.as_ptr(),
            0,
            height,
            dst_data.as_mut_ptr(),
            dst_linesize.as_mut_ptr(),
        );
        ffmpeg_check(scaled, "sws_scale(video)")?;

        device
            .upload_nv12_surface_contiguous(width as u32, height as u32, &self.frame_buf, stride)
            .map_err(|e| e.to_string())
    }

    unsafe fn recreate(
        &mut self,
        width: i32,
        height: i32,
        source_format: AVPixelFormat,
    ) -> Result<(), String> {
        if !self.context.is_null() {
            sws_freeContext(self.context);
            self.context = null_mut();
        }

        self.context = sws_getContext(
            width,
            height,
            source_format,
            width,
            height,
            AVPixelFormat_AV_PIX_FMT_NV12,
            SWS_BILINEAR_FLAGS,
            null_mut(),
            null_mut(),
            null(),
        );
        if self.context.is_null() {
            return Err(format!(
                "failed to create software video converter from pixel format {} to NV12",
                source_format
            ));
        }

        self.source_width = width;
        self.source_height = height;
        self.source_format = source_format;
        Ok(())
    }
}

impl Drop for SoftwareVideoConverter {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                sws_freeContext(self.context);
            }
        }
    }
}

unsafe fn receive_audio_frames<F>(
    audio: &mut AudioDecoder,
    frame: *mut AVFrame,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    mut batcher: Option<&mut AudioBatcher>,
    produced_frames: &mut u64,
    on_frame: &mut F,
    should_cancel: &dyn Fn() -> bool,
) -> Result<(), String>
where
    F: FnMut(PendingAudioFrame) -> Result<(), String>,
{
    loop {
        if should_cancel() {
            return Ok(());
        }
        let status = avcodec_receive_frame(audio.codec.0, frame);
        if status == fastplay_ffmpeg_error_eagain() || status == fastplay_ffmpeg_error_eof() {
            return Ok(());
        }
        ffmpeg_check(status, "avcodec_receive_frame(audio)")?;

        let pts = decoded_frame_pts(frame, audio.pts_time_base);
        let data = audio.resampler.convert(frame)?;
        let frame_count = (data.len() / audio.output_format.bytes_per_frame() as usize) as u32;
        av_frame_unref(frame);
        if let Some(batcher) = batcher.as_deref_mut() {
            batcher.push(
                pts,
                frame_count,
                data,
                open_gen,
                seek_gen,
                op_id,
                produced_frames,
                on_frame,
            )?;
        } else {
            *produced_frames = (*produced_frames).saturating_add(1);
            on_frame(PendingAudioFrame {
                open_gen,
                seek_gen,
                op_id,
                pts,
                format: audio.output_format,
                frame_count,
                data: data.to_vec(),
            })?;
        }
    }
}

struct Resampler {
    context: *mut SwrContext,
    output_format: AudioStreamFormat,
    output_buffer: Vec<u8>,
}

impl Resampler {
    unsafe fn new(
        output_format: AudioStreamFormat,
        input_channel_layout: &AVChannelLayout,
        input_sample_format: AVSampleFormat,
        input_sample_rate: i32,
    ) -> Result<Self, String> {
        let context = fastplay_ffmpeg_create_float_resampler(
            input_channel_layout,
            input_sample_format,
            input_sample_rate,
            output_format.channel_mask,
            output_format.channels as i32,
            output_format.sample_rate as i32,
        );
        if context.is_null() {
            return Err(format!(
                "failed to create float resampler for {} Hz / {} channels output",
                output_format.sample_rate,
                output_format.channels
            ));
        }
        Ok(Self {
            context,
            output_format,
            output_buffer: Vec::new(),
        })
    }

    unsafe fn convert(&mut self, frame: *mut AVFrame) -> Result<&[u8], String> {
        let out_samples = swr_get_out_samples(self.context, (*frame).nb_samples);
        ffmpeg_check(out_samples, "swr_get_out_samples")?;

        let bytes_per_frame = self.output_format.bytes_per_frame() as usize;
        self.output_buffer.resize(out_samples as usize * bytes_per_frame, 0);
        let output_planes = [self.output_buffer.as_mut_ptr()];
        let converted = swr_convert(
            self.context,
            output_planes.as_ptr().cast(),
            out_samples,
            (*frame).extended_data.cast(),
            (*frame).nb_samples,
        );
        ffmpeg_check(converted, "swr_convert")?;
        let len = converted as usize * bytes_per_frame;
        Ok(&self.output_buffer[..len])
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe {
            swr_free(&mut self.context);
        }
    }
}

struct AudioBatcher {
    format: AudioStreamFormat,
    pts: Option<Duration>,
    frame_count: u32,
    data: Vec<u8>,
    target_frames: u32,
}

impl AudioBatcher {
    fn new(format: AudioStreamFormat) -> Self {
        let target_frames = (format.sample_rate / 10).max(1024);
        Self {
            format,
            pts: None,
            frame_count: 0,
            data: Vec::new(),
            target_frames,
        }
    }

    fn push<F>(
        &mut self,
        pts: Duration,
        frame_count: u32,
        data: &[u8],
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        produced_frames: &mut u64,
        on_frame: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(PendingAudioFrame) -> Result<(), String>,
    {
        if self.pts.is_none() {
            self.pts = Some(pts);
        }
        self.frame_count = self.frame_count.saturating_add(frame_count);
        self.data.extend_from_slice(data);
        if self.frame_count >= self.target_frames {
            self.flush(open_gen, seek_gen, op_id, produced_frames, on_frame)?;
        }
        Ok(())
    }

    fn flush<F>(
        &mut self,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        produced_frames: &mut u64,
        on_frame: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(PendingAudioFrame) -> Result<(), String>,
    {
        let Some(pts) = self.pts.take() else {
            return Ok(());
        };
        *produced_frames = (*produced_frames).saturating_add(1);
        let data = std::mem::take(&mut self.data);
        let frame_count = std::mem::take(&mut self.frame_count);
        on_frame(PendingAudioFrame {
            open_gen,
            seek_gen,
            op_id,
            pts,
            format: self.format,
            frame_count,
            data,
        })
    }
}

unsafe fn configure_hw_device(
    codec_context: *mut AVCodecContext,
    device: &D3D11Device,
    decoder: *const AVCodec,
) -> Result<(), String> {
    ensure_decoder_supports_d3d11(decoder)?;

    let mut hw_device = av_hwdevice_ctx_alloc(AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA);
    if hw_device.is_null() {
        return Err("av_hwdevice_ctx_alloc returned null".into());
    }

    let hw_ctx = (*hw_device).data as *mut AVHWDeviceContext;
    let d3d11_ctx = (*hw_ctx).hwctx as *mut AVD3D11VADeviceContext;
    if d3d11_ctx.is_null() {
        av_buffer_unref(&mut hw_device);
        return Err("D3D11 hwctx was null".into());
    }

    (*d3d11_ctx).device = device.raw_device_ptr().cast();
    ffmpeg_check(av_hwdevice_ctx_init(hw_device), "av_hwdevice_ctx_init")?;
    (*codec_context).hw_device_ctx = av_buffer_ref(hw_device);
    av_buffer_unref(&mut hw_device);
    if (*codec_context).hw_device_ctx.is_null() {
        return Err("av_buffer_ref for hw_device_ctx returned null".into());
    }

    Ok(())
}

unsafe fn selected_stream(
    format_context: *mut AVFormatContext,
    stream_index: usize,
) -> Result<*mut AVStream, String> {
    let stream = fastplay_ffmpeg_stream_at(format_context, stream_index as u32);
    if stream.is_null() {
        return Err("selected AVStream pointer was null or out of bounds".into());
    }

    Ok(stream)
}

unsafe fn ensure_decoder_supports_d3d11(decoder: *const AVCodec) -> Result<(), String> {
    let mut index = 0;
    loop {
        let config = avcodec_get_hw_config(decoder, index);
        if config.is_null() {
            break;
        }

        if (*config).pix_fmt == AVPixelFormat_AV_PIX_FMT_D3D11
            && (*config).device_type == AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA
            && ((*config).methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) != 0
        {
            return Ok(());
        }

        index += 1;
    }

    Err("decoder does not advertise AV_PIX_FMT_D3D11 via HW_DEVICE_CTX".into())
}

unsafe extern "C" fn select_d3d11_pixel_format(
    _codec_context: *mut AVCodecContext,
    pixel_formats: *const AVPixelFormat,
) -> AVPixelFormat {
    let mut current = pixel_formats;
    while !current.is_null() && *current != AVPixelFormat_AV_PIX_FMT_NONE {
        if *current == AVPixelFormat_AV_PIX_FMT_D3D11 {
            return *current;
        }
        current = current.add(1);
    }

    AVPixelFormat_AV_PIX_FMT_NONE
}

fn frame_pts(value: i64, time_base: AVRational) -> Duration {
    if value == AV_NOPTS_SENTINEL || time_base.den == 0 || time_base.num == 0 {
        return Duration::ZERO;
    }

    let micros = (value as i128)
        .saturating_mul(time_base.num as i128)
        .saturating_mul(AV_TIME_BASE_MICROS)
        / (time_base.den as i128);
    if micros <= 0 {
        Duration::ZERO
    } else {
        Duration::from_micros(micros.min(u64::MAX as i128) as u64)
    }
}

fn decoded_frame_pts(frame: *mut AVFrame, time_base: AVRational) -> Duration {
    unsafe {
        let best_effort = (*frame).best_effort_timestamp;
        if best_effort != AV_NOPTS_SENTINEL {
            return frame_pts(best_effort, time_base);
        }

        frame_pts((*frame).pts, time_base)
    }
}

fn ffmpeg_check(status: i32, operation: &str) -> Result<i32, String> {
    if status >= 0 {
        return Ok(status);
    }

    let mut buffer = [0i8; 256];
    unsafe {
        let _ = av_strerror(status, buffer.as_mut_ptr(), buffer.len());
        let message = CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned();
        Err(format!("{operation} failed: {message} ({status})"))
    }
}

struct InputContext(*mut AVFormatContext);

impl Drop for InputContext {
    fn drop(&mut self) {
        unsafe {
            avformat_close_input(&mut self.0);
        }
    }
}

struct CodecContext(*mut AVCodecContext);

impl Drop for CodecContext {
    fn drop(&mut self) {
        unsafe {
            avcodec_free_context(&mut self.0);
        }
    }
}

struct Packet(*mut AVPacket);

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe {
            av_packet_free(&mut self.0);
        }
    }
}

struct Frame(*mut AVFrame);

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.0);
        }
    }
}
