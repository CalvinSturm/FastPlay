#![allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    improper_ctypes,
    unnecessary_transmutes
)]

use std::{
    cell::Cell,
    ffi::{c_void, CStr, CString},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};

use crate::{
    ffi::{
        d3d11::{D3D11Device, SurfaceColor, VideoSurface},
        dxgi::query_hdr_presentation_capabilities,
    },
    media::{
        audio::AudioStreamFormat,
        source::MediaSource,
        video::{VideoDecodeMode, VideoDecodePreference},
    },
    playback::generations::{OpenGeneration, OperationId, SeekGeneration},
    render::hdr::{
        classify_color_tags, select_video_presentation_path, ContentColorInfo, ContentColorMode,
        ContentLightMetadata, HdrError, MasteringDisplayMetadata, VideoPresentationPath,
    },
};

include!(concat!(env!("OUT_DIR"), "/ffmpeg_bindings.rs"));

const SWS_BILINEAR_FLAGS: i32 = 2;
const AV_NOPTS_SENTINEL: i64 = i64::MIN;
const AV_TIME_BASE_MICROS: i128 = 1_000_000;

// Wall-clock ceilings for blocking FFmpeg I/O. Without an interrupt callback
// avformat_open_input / av_read_frame / av_seek_frame can block forever on a
// disconnected network share, a vanished removable drive, or a corrupt file.
// The interrupt callback (see `InterruptState`) aborts the operation once the
// matching deadline passes, surfacing a clear error instead of a frozen worker.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const SEEK_TIMEOUT: Duration = Duration::from_secs(15);

/// Shared state behind an `AVFormatContext`'s interrupt callback. FFmpeg polls
/// the callback during blocking I/O; returning non-zero aborts the operation.
/// The callback aborts when the owning worker has been asked to cancel (so a
/// shutdown can unblock a worker wedged in FFmpeg) or when the deadline for the
/// current blocking operation has elapsed.
///
/// Created and mutated only on the decode worker thread that owns the session;
/// the embedded `cancel` closure typically observes a `Send`/`Sync` shutdown
/// flag, which is the only field touched from another thread.
pub(crate) struct InterruptState {
    cancel: Box<dyn Fn() -> bool>,
    deadline: Cell<Option<Instant>>,
}

impl InterruptState {
    fn new(cancel: Box<dyn Fn() -> bool>) -> Self {
        Self {
            cancel,
            deadline: Cell::new(None),
        }
    }

    /// Arm (or clear) the deadline guarding the next blocking operation.
    fn set_deadline(&self, deadline: Option<Instant>) {
        self.deadline.set(deadline);
    }

    /// Why a blocking operation should abort, if it should. Checked both by the
    /// FFmpeg interrupt callback and after a blocking call returns an error so
    /// the reason can be reported.
    fn aborted_reason(&self) -> Option<&'static str> {
        if (self.cancel)() {
            return Some("cancelled");
        }
        if self.deadline.get().is_some_and(|d| Instant::now() >= d) {
            return Some("timed out");
        }
        None
    }
}

/// FFmpeg interrupt callback. `opaque` points at an [`InterruptState`] that
/// outlives the `AVFormatContext` it is attached to.
unsafe extern "C" fn ffmpeg_interrupt_callback(opaque: *mut c_void) -> i32 {
    if opaque.is_null() {
        return 0;
    }
    let state = &*(opaque as *const InterruptState);
    i32::from(state.aborted_reason().is_some())
}

/// `ffmpeg_check` for a blocking call guarded by an interrupt callback: if the
/// call failed because the callback aborted it (cancel or timeout), report that
/// reason instead of FFmpeg's generic `AVERROR_EXIT` message.
unsafe fn check_blocking(
    status: i32,
    operation: &str,
    interrupt: &InterruptState,
) -> Result<i32, String> {
    if status >= 0 {
        return Ok(status);
    }
    if let Some(reason) = interrupt.aborted_reason() {
        return Err(format!("{operation} {reason}"));
    }
    ffmpeg_check(status, operation)
}

#[derive(Debug)]
pub(crate) enum PendingVideoFrame {
    D3D11 {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        pts: Duration,
        width: u32,
        height: u32,
        sar_num: u32,
        sar_den: u32,
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

/// One open media file with its video (and optional audio) decoders, ready to
/// decode. The persistent decode worker opens a session once and seeks within
/// it (`seek` + `run_to_eof`) instead of reopening the file per operation.
pub(crate) struct DecodeSession {
    input: InputContext,
    video: VideoDecoder,
    audio: Option<AudioDecoder>,
    audio_batch: Option<AudioBatcher>,
    packet: Packet,
    frame: Frame,
    summary: StreamSummary,
    /// Position the decoder is currently seeked to (None = start of stream).
    /// Used to restart decoding after a mid-stream hardware→software fallback.
    position: Option<Duration>,
    /// Backs the format context's interrupt callback. Declared last so it is
    /// dropped *after* `input` (whose teardown could otherwise reference a
    /// freed callback opaque pointer). The `AVFormatContext` holds a raw
    /// pointer to this heap allocation, so it must not move while in use —
    /// `Box` keeps the address stable across `DecodeSession` moves.
    interrupt: Box<InterruptState>,
}

impl DecodeSession {
    /// Whether this session decoded any audio stream (drives the audio
    /// stream-ended event after a run completes).
    pub(crate) fn had_audio_stream(&self) -> bool {
        self.summary.had_audio_stream
    }

    /// Open the file, find streams, and allocate the video/audio decoders.
    /// Returns `Ok(None)` if cancellation was signalled during the (expensive)
    /// open so no decode work begins. Reports the selected decode mode and the
    /// media duration through the callbacks, then applies `start_position`.
    pub(crate) unsafe fn open(
        source: &MediaSource,
        device: &D3D11Device,
        audio_output_format: AudioStreamFormat,
        start_position: Option<Duration>,
        decode_preference: VideoDecodePreference,
        decode_audio: bool,
        io_cancel: Box<dyn Fn() -> bool>,
        should_cancel: &impl Fn() -> bool,
        on_decode_mode: &mut impl FnMut(VideoDecodeMode, u64, u8) -> Result<(), String>,
        on_duration: &mut impl FnMut(Duration) -> Result<(), String>,
    ) -> Result<Option<Self>, String> {
        let source_path = source
            .path()
            .to_str()
            .ok_or_else(|| "media path must be valid UTF-8 for FFmpeg open".to_string())?;
        let source_cstr =
            CString::new(source_path).map_err(|_| "media path contained NUL".to_string())?;

        // Pre-allocate the format context so the interrupt callback is armed
        // *before* the (potentially blocking) open. `interrupt` is declared
        // before `input` so that on any early return it is dropped after the
        // input context, never leaving a live callback pointing at freed state.
        let format_context = fastplay_ffmpeg_alloc_context();
        if format_context.is_null() {
            return Err("avformat_alloc_context returned null".into());
        }
        let interrupt = Box::new(InterruptState::new(io_cancel));
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

        // Check cancellation before allocating a hardware decoder on the GPU.
        // Without this, rapid seeks pile up concurrent decoder sessions from
        // threads that haven't reached the main decode loop yet, exhausting
        // the GPU's session limit (typically 8-16) and causing device loss.
        if should_cancel() {
            return Ok(None);
        }

        let video = open_video_decoder(input.0, device, decode_preference)?;
        // Re-check after decoder creation so a cancel that arrived during
        // open_video_decoder drops the session immediately.
        if should_cancel() {
            return Ok(None);
        }

        // Presentation-path fork (HDR skeleton). Decided from stream-level
        // tags only, before any frame is decoded; everything below this
        // block — the decode loop, first-frame handling, swapchain, and
        // processor configuration — is the pre-existing verified SDR flow,
        // untouched. Every HDR outcome is a typed error surfaced through
        // the existing OpenFailed flow until the HDR pipeline is verified
        // end to end.
        //
        // SDR short-circuits before any capability probing: an SDR open
        // performs zero new COM work here, so a capability-query failure on
        // exotic systems (headless/RDP output, drivers without the newer
        // interfaces) can never regress SDR open availability.
        if video.content_color.mode != ContentColorMode::Sdr {
            let hdr_capabilities = query_hdr_presentation_capabilities(device, None)
                .map_err(|error| error.to_string())?;
            match select_video_presentation_path(&video.content_color, &hdr_capabilities) {
                // Unreachable for non-SDR modes (the decision function
                // returns ExistingSdr only for Sdr content); kept as an
                // explicit no-op for match exhaustiveness.
                VideoPresentationPath::ExistingSdr => {}
                VideoPresentationPath::Hdr10Passthrough => {
                    // Integration point for the passthrough commit: create
                    // the HDR renderer via SwapChainPresenter::new_for_path,
                    // then refine the classification from the first decoded
                    // frame (refine_color_from_first_frame) once the HDR
                    // swapchain exists. Until the color spaces are verified
                    // this is a typed error, never a panic.
                    return Err(HdrError::HdrColorSpaceUnverified.to_string());
                }
                VideoPresentationPath::HdrToSdrToneMapRequired => {
                    return Err(HdrError::ToneMappingNotImplemented.to_string());
                }
                VideoPresentationPath::UnsupportedHdr => {
                    return Err(HdrError::UnsupportedHdrPresentation.to_string());
                }
            }
        }
        // Audio can be handled by an independent [`AudioDecodeSession`] on its
        // own thread so it is never gated behind slow (e.g. software-decoded
        // 4K60) video work here. When `decode_audio` is false this session is
        // video-only and emits no audio frames or audio-ended events.
        let audio = if decode_audio {
            open_audio_decoder(input.0, audio_output_format)?
        } else {
            None
        };
        let audio_batch = audio
            .as_ref()
            .map(|audio| AudioBatcher::new(audio.output_format));
        on_decode_mode(
            video.mode,
            video.hw_fallback_count,
            video.rotation_quarter_turns,
        )?;
        let summary = StreamSummary {
            had_audio_stream: audio.is_some(),
            produced_video_frames: 0,
            produced_audio_frames: 0,
            decode_mode: video.mode,
            hw_fallback_count: video.hw_fallback_count,
        };
        let total_duration = frame_pts(
            fastplay_ffmpeg_duration_micros(input.0),
            AVRational {
                num: 1,
                den: 1_000_000,
            },
        );
        if !total_duration.is_zero() {
            on_duration(total_duration)?;
        }

        if let Some(target) = start_position {
            flog!("[worker] seeking to {:.3}s", target.as_secs_f64());
            interrupt.set_deadline(Some(Instant::now() + SEEK_TIMEOUT));
            seek_and_flush(input.0, &video, audio.as_ref(), target)?;
            interrupt.set_deadline(None);
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

        Ok(Some(Self {
            input,
            video,
            audio,
            audio_batch,
            packet,
            frame,
            summary,
            position: start_position,
            interrupt,
        }))
    }

    /// Seek within the already-open file to `target` and flush the decoders,
    /// without reopening anything. The next `run_to_eof` resumes from here.
    pub(crate) unsafe fn seek(&mut self, target: Duration) -> Result<(), String> {
        self.interrupt
            .set_deadline(Some(Instant::now() + SEEK_TIMEOUT));
        let result = seek_and_flush(self.input.0, &self.video, self.audio.as_ref(), target);
        self.interrupt.set_deadline(None);
        result?;
        // Drop any partial pre-seek audio batch so its stale first-sample pts
        // cannot stamp post-seek audio (see AudioBatcher::reset).
        if let Some(batch) = self.audio_batch.as_mut() {
            batch.reset();
        }
        self.position = Some(target);
        Ok(())
    }

    /// Decode from the current position to end of stream, delivering frames
    /// through the callbacks. Returns `Cancelled` if cancellation was signalled
    /// mid-stream, otherwise `Completed` with the run summary.
    pub(crate) unsafe fn run_to_eof(
        &mut self,
        device: &D3D11Device,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        should_cancel: &impl Fn() -> bool,
        on_decode_mode: &mut impl FnMut(VideoDecodeMode, u64, u8) -> Result<(), String>,
        on_video: &mut impl FnMut(PendingVideoFrame) -> Result<(), String>,
        on_audio: &mut impl FnMut(PendingAudioFrame) -> Result<(), String>,
    ) -> Result<StreamStatus, String> {
        let mut hw_mid_fallback_done = false;

        loop {
            if should_cancel() {
                return Ok(StreamStatus::Cancelled);
            }

            self.interrupt
                .set_deadline(Some(Instant::now() + READ_TIMEOUT));
            let read_status = av_read_frame(self.input.0, self.packet.0);
            if read_status == fastplay_ffmpeg_error_eof() {
                self.interrupt.set_deadline(None);
                break;
            }
            check_blocking(read_status, "av_read_frame", &self.interrupt)?;
            self.interrupt.set_deadline(None);

            if (*self.packet.0).stream_index == self.video.stream_index as i32 {
                // D3D11VA's avcodec_send_packet may call into the video
                // context — hold the lock to prevent racing with the UI
                // thread's VideoProcessorBlt.
                let send_result = if self.video.mode == VideoDecodeMode::HardwareD3D11 {
                    if device.is_device_removed() {
                        av_packet_unref(self.packet.0);
                        return Err("D3D11 device removed during hardware decode".into());
                    }
                    let _lock = device.lock_context();
                    avcodec_send_packet(self.video.codec.0, self.packet.0)
                } else {
                    avcodec_send_packet(self.video.codec.0, self.packet.0)
                };
                if send_result < 0
                    && self.video.mode == VideoDecodeMode::HardwareD3D11
                    && !hw_mid_fallback_done
                {
                    // HW decode failed on first real packet — try software fallback.
                    av_packet_unref(self.packet.0);
                    match open_software_video_decoder(self.input.0) {
                        Ok(mut sw_decoder) => {
                            flog!(
                                "hw decode failed mid-stream ({}), falling back to software",
                                send_result
                            );
                            sw_decoder.hw_fallback_count = self.video.hw_fallback_count + 1;
                            self.video = sw_decoder;
                            hw_mid_fallback_done = true;
                            self.summary.decode_mode = self.video.mode;
                            self.summary.hw_fallback_count = self.video.hw_fallback_count;
                            on_decode_mode(
                                self.video.mode,
                                self.video.hw_fallback_count,
                                self.video.rotation_quarter_turns,
                            )?;
                            let restart = self.position.unwrap_or(Duration::ZERO);
                            seek_and_flush(
                                self.input.0,
                                &self.video,
                                self.audio.as_ref(),
                                restart,
                            )?;
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
                av_packet_unref(self.packet.0);
                receive_video_frames(
                    &mut self.video,
                    self.frame.0,
                    device,
                    open_gen,
                    seek_gen,
                    op_id,
                    &mut self.summary.produced_video_frames,
                    on_video,
                    &|| should_cancel(),
                )?;
                continue;
            }

            if let Some(audio) = self.audio.as_mut() {
                if (*self.packet.0).stream_index == audio.stream_index as i32 {
                    ffmpeg_check(
                        avcodec_send_packet(audio.codec.0, self.packet.0),
                        "avcodec_send_packet(audio)",
                    )?;
                    av_packet_unref(self.packet.0);
                    receive_audio_frames(
                        audio,
                        self.frame.0,
                        open_gen,
                        seek_gen,
                        op_id,
                        self.audio_batch.as_mut(),
                        &mut self.summary.produced_audio_frames,
                        on_audio,
                        &|| should_cancel(),
                    )?;
                    continue;
                }
            }

            av_packet_unref(self.packet.0);
        }

        if should_cancel() {
            return Ok(StreamStatus::Cancelled);
        }

        {
            let _lock = if self.video.mode == VideoDecodeMode::HardwareD3D11 {
                Some(device.lock_context())
            } else {
                None
            };
            ffmpeg_check(
                avcodec_send_packet(self.video.codec.0, null()),
                "avcodec_send_packet(video flush)",
            )?;
        }
        receive_video_frames(
            &mut self.video,
            self.frame.0,
            device,
            open_gen,
            seek_gen,
            op_id,
            &mut self.summary.produced_video_frames,
            on_video,
            &|| should_cancel(),
        )?;

        if let Some(audio) = self.audio.as_mut() {
            ffmpeg_check(
                avcodec_send_packet(audio.codec.0, null()),
                "avcodec_send_packet(audio flush)",
            )?;
            receive_audio_frames(
                audio,
                self.frame.0,
                open_gen,
                seek_gen,
                op_id,
                self.audio_batch.as_mut(),
                &mut self.summary.produced_audio_frames,
                on_audio,
                &|| should_cancel(),
            )?;
            if let Some(batch) = self.audio_batch.as_mut() {
                batch.flush(
                    open_gen,
                    seek_gen,
                    op_id,
                    &mut self.summary.produced_audio_frames,
                    on_audio,
                )?;
            }
        }

        if self.summary.produced_video_frames == 0 {
            return Err("no decodable video frame was produced".into());
        }

        Ok(StreamStatus::Completed(self.summary))
    }
}

/// An independent, audio-only decode session with its own `AVFormatContext`.
///
/// Runs on a dedicated worker thread in parallel with the video [`DecodeSession`]
/// so audio decoding is never gated behind sub-realtime (e.g. software-decoded
/// 4K60) video work on a shared worker. The two sessions share nothing except
/// the seek target the coordinator hands each of them: late video frames are
/// dropped at the presenter while this session keeps the audio sink fed.
///
/// Audio decode is cheap and never uses the GPU, so there is no hardware
/// fallback and no D3D11 device involved here.
pub(crate) struct AudioDecodeSession {
    input: InputContext,
    audio: AudioDecoder,
    audio_batch: AudioBatcher,
    packet: Packet,
    frame: Frame,
    produced_audio_frames: u64,
    interrupt: Box<InterruptState>,
}

impl AudioDecodeSession {
    /// Open `source` audio-only. Returns `Ok(None)` when the file has no audio
    /// stream (the coordinator then simply runs without an audio worker) or when
    /// cancellation was signalled during the open.
    pub(crate) unsafe fn open(
        source: &MediaSource,
        audio_output_format: AudioStreamFormat,
        start_position: Option<Duration>,
        io_cancel: Box<dyn Fn() -> bool>,
        should_cancel: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let source_path = source
            .path()
            .to_str()
            .ok_or_else(|| "media path must be valid UTF-8 for FFmpeg open".to_string())?;
        let source_cstr =
            CString::new(source_path).map_err(|_| "media path contained NUL".to_string())?;

        let format_context = fastplay_ffmpeg_alloc_context();
        if format_context.is_null() {
            return Err("avformat_alloc_context returned null".into());
        }
        let interrupt = Box::new(InterruptState::new(io_cancel));
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
            "avformat_open_input(audio)",
            &interrupt,
        )?;
        let input = InputContext(format_context);

        interrupt.set_deadline(Some(Instant::now() + OPEN_TIMEOUT));
        check_blocking(
            avformat_find_stream_info(input.0, null_mut()),
            "avformat_find_stream_info(audio)",
            &interrupt,
        )?;
        interrupt.set_deadline(None);

        if should_cancel() {
            return Ok(None);
        }

        let Some(audio) = open_audio_decoder(input.0, audio_output_format)? else {
            return Ok(None);
        };
        let audio_batch = AudioBatcher::new(audio.output_format);

        if let Some(target) = start_position {
            interrupt.set_deadline(Some(Instant::now() + SEEK_TIMEOUT));
            seek_format_to_target(input.0, target)?;
            fastplay_ffmpeg_flush_codec(audio.codec.0);
            interrupt.set_deadline(None);
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

        Ok(Some(Self {
            input,
            audio,
            audio_batch,
            packet,
            frame,
            produced_audio_frames: 0,
            interrupt,
        }))
    }

    /// Seek within the already-open file to `target` and flush the audio
    /// decoder, without reopening anything. The next `run_to_eof` resumes from
    /// here. Mirrors [`DecodeSession::seek`] for the audio-only worker so rapid
    /// scrubbing reuses the open demuxer instead of reopening the file.
    pub(crate) unsafe fn seek(&mut self, target: Duration) -> Result<(), String> {
        self.interrupt
            .set_deadline(Some(Instant::now() + SEEK_TIMEOUT));
        let result = seek_format_to_target(self.input.0, target);
        self.interrupt.set_deadline(None);
        result?;
        fastplay_ffmpeg_flush_codec(self.audio.codec.0);
        // Drop any partial pre-seek audio batch so its stale first-sample pts
        // cannot stamp post-seek audio (see AudioBatcher::reset).
        self.audio_batch.reset();
        Ok(())
    }

    /// Decode audio from the current position to end of stream, delivering
    /// batched frames through `on_audio`. Returns `Cancelled` if cancellation
    /// was signalled mid-stream.
    pub(crate) unsafe fn run_to_eof(
        &mut self,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        should_cancel: &impl Fn() -> bool,
        on_audio: &mut impl FnMut(PendingAudioFrame) -> Result<(), String>,
    ) -> Result<StreamStatus, String> {
        loop {
            if should_cancel() {
                return Ok(StreamStatus::Cancelled);
            }
            self.interrupt
                .set_deadline(Some(Instant::now() + READ_TIMEOUT));
            let read_status = av_read_frame(self.input.0, self.packet.0);
            if read_status == fastplay_ffmpeg_error_eof() {
                self.interrupt.set_deadline(None);
                break;
            }
            check_blocking(read_status, "av_read_frame(audio)", &self.interrupt)?;
            self.interrupt.set_deadline(None);

            if (*self.packet.0).stream_index == self.audio.stream_index as i32 {
                ffmpeg_check(
                    avcodec_send_packet(self.audio.codec.0, self.packet.0),
                    "avcodec_send_packet(audio)",
                )?;
                av_packet_unref(self.packet.0);
                receive_audio_frames(
                    &mut self.audio,
                    self.frame.0,
                    open_gen,
                    seek_gen,
                    op_id,
                    Some(&mut self.audio_batch),
                    &mut self.produced_audio_frames,
                    on_audio,
                    &|| should_cancel(),
                )?;
            } else {
                av_packet_unref(self.packet.0);
            }
        }

        if should_cancel() {
            return Ok(StreamStatus::Cancelled);
        }
        ffmpeg_check(
            avcodec_send_packet(self.audio.codec.0, null()),
            "avcodec_send_packet(audio flush)",
        )?;
        receive_audio_frames(
            &mut self.audio,
            self.frame.0,
            open_gen,
            seek_gen,
            op_id,
            Some(&mut self.audio_batch),
            &mut self.produced_audio_frames,
            on_audio,
            &|| should_cancel(),
        )?;
        self.audio_batch.flush(
            open_gen,
            seek_gen,
            op_id,
            &mut self.produced_audio_frames,
            on_audio,
        )?;

        Ok(StreamStatus::Completed(StreamSummary {
            had_audio_stream: true,
            produced_video_frames: 0,
            produced_audio_frames: self.produced_audio_frames,
            // Unused for the audio path; the audio worker never inspects it.
            decode_mode: VideoDecodeMode::Software,
            hw_fallback_count: 0,
        }))
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
    /// Stream-level color classification from `AVCodecParameters`, captured
    /// at decoder open. Drives the presentation-path decision before any
    /// frame is decoded.
    content_color: ContentColorInfo,
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

/// Seek the format context to `target` (relative to the stream start time).
/// Codec flushing is the caller's responsibility.
unsafe fn seek_format_to_target(
    format_context: *mut AVFormatContext,
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
    )
    .map(|_| ())
}

unsafe fn seek_and_flush(
    format_context: *mut AVFormatContext,
    video: &VideoDecoder,
    audio: Option<&AudioDecoder>,
    target: Duration,
) -> Result<(), String> {
    seek_format_to_target(format_context, target)?;
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
                    flog!("video decode fallback: {hw_error}");
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
        flog!("display_matrix rotation: {cw_degrees:.1}° CW → {quarter} quarter-turns");
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
    let codec = CodecContext(codec_context, Some(device.clone()));

    let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
    if codec_parameters.is_null() {
        return Err("selected video stream codec parameters were null".into());
    }

    let rotation_quarter_turns = stream_rotation_quarter_turns(codec_parameters);
    let content_color = classify_stream_color(codec_parameters)?;

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(video)",
    )?;
    let pts_time_base = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).pkt_timebase = pts_time_base;
    (*codec.0).get_format = Some(select_d3d11_pixel_format);
    configure_hw_device(codec.0, device, decoder)?;
    ffmpeg_check(
        avcodec_open2(codec.0, decoder, null_mut()),
        "avcodec_open2(video)",
    )?;

    Ok(VideoDecoder {
        stream_index,
        codec,
        pts_time_base,
        output: VideoDecoderOutput::Hardware,
        mode: VideoDecodeMode::HardwareD3D11,
        hw_fallback_count: 0,
        rotation_quarter_turns,
        content_color,
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
    let codec = CodecContext(codec_context, None);

    let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
    if codec_parameters.is_null() {
        return Err("selected video stream codec parameters were null".into());
    }

    let rotation_quarter_turns = stream_rotation_quarter_turns(codec_parameters);
    let content_color = classify_stream_color(codec_parameters)?;

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(video)",
    )?;
    let pts_time_base = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).pkt_timebase = pts_time_base;
    ffmpeg_check(
        avcodec_open2(codec.0, decoder, null_mut()),
        "avcodec_open2(video)",
    )?;

    Ok(VideoDecoder {
        stream_index,
        codec,
        pts_time_base,
        output: VideoDecoderOutput::Software(SoftwareVideoConverter::default()),
        mode: VideoDecodeMode::Software,
        hw_fallback_count: 0,
        rotation_quarter_turns,
        content_color,
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
    let codec = CodecContext(codec_context, None);

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
    ffmpeg_check(
        avcodec_open2(codec.0, decoder, null_mut()),
        "avcodec_open2(audio)",
    )?;

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

/// Stream-level color classification from `AVCodecParameters` only.
///
/// Called during open, after `avformat_find_stream_info` completes and
/// before any frame is decoded — this is what drives presentation-path
/// selection (and therefore swapchain format choice). It never parses HDR
/// side data; that belongs to first-frame refinement on the HDR path.
///
/// SAFETY contract: `codecpar` must be the live codec parameters of a
/// stream owned by an open `AVFormatContext`. It is read-only here and not
/// retained beyond the call.
unsafe fn classify_stream_color(
    codecpar: *const AVCodecParameters,
) -> Result<ContentColorInfo, String> {
    if codecpar.is_null() {
        return Err("video stream codec parameters were null during color classification".into());
    }
    let color_primaries = (*codecpar).color_primaries;
    let color_transfer = (*codecpar).color_trc;
    Ok(ContentColorInfo {
        mode: classify_color_tags(color_primaries, color_transfer),
        color_primaries,
        color_transfer,
        color_space: (*codecpar).color_space,
        color_range: (*codecpar).color_range,
        mastering_display: None,
        content_light: None,
    })
}

/// First-frame refinement of the stream-level classification.
///
/// Precedence rule: valid, *specified* frame-level tags override
/// stream-level tags; unspecified frame fields leave the stream-level
/// values in place. Frame side data (mastering display, content light) is
/// attached only here, never at stream level.
///
/// Integration point: runs ONLY on the HDR path, after the HDR swapchain
/// exists, on a first frame that already flowed through the unchanged
/// decode path (see the `Hdr10Passthrough` arm in `DecodeSession::open`).
/// The verified SDR path never calls this and its first-frame handling is
/// untouched.
///
/// HDR-VERIFY: the concrete field-by-field refinement (upgrading `mode`
/// from frame-level trc/primaries, range/matrix overrides) is unresolved;
/// today the stream classification passes through with side-data
/// attachment only.
///
/// SAFETY contract: `frame` must be a live decoded `AVFrame` owned by the
/// caller. Read-only access; nothing is retained beyond the call.
unsafe fn refine_color_from_first_frame(
    stream_info: ContentColorInfo,
    frame: *const AVFrame,
) -> Result<ContentColorInfo, String> {
    if frame.is_null() {
        return Err("first decoded frame was null during HDR color refinement".into());
    }
    let (mastering_display, content_light) = extract_hdr_metadata_from_frame(frame)?;
    let mut refined = stream_info;
    refined.mastering_display = mastering_display;
    refined.content_light = content_light;
    Ok(refined)
}

/// Locate HDR10 static metadata side data on a decoded frame.
///
/// Missing side data yields `None` and never fails playback. Side data
/// that IS present cannot be parsed yet: the payload structs
/// (`AVMasteringDisplayMetadata`, `AVContentLightMetadata`) are not on our
/// bindgen allowlist, and their layouts must not be guessed — presence is
/// a typed error so the verification commit cannot be skipped silently.
///
/// HDR-VERIFY: bind the payload structs and parse `data`/`size` into
/// [`MasteringDisplayMetadata`] / [`ContentLightMetadata`].
///
/// SAFETY contract: `frame` must be a live decoded `AVFrame`; the
/// `side_data` entries are owned by the frame, and only the bound
/// `AVFrameSideData` header (the `type_` tag) is read — never the payload.
unsafe fn extract_hdr_metadata_from_frame(
    frame: *const AVFrame,
) -> Result<
    (
        Option<MasteringDisplayMetadata>,
        Option<ContentLightMetadata>,
    ),
    String,
> {
    if frame.is_null() {
        return Err("frame was null during HDR metadata extraction".into());
    }
    let count = (*frame).nb_side_data.max(0) as isize;
    for index in 0..count {
        let entry = *(*frame).side_data.offset(index);
        if entry.is_null() {
            continue;
        }
        let side_data_type = (*entry).type_;
        if side_data_type == AVFrameSideDataType_AV_FRAME_DATA_MASTERING_DISPLAY_METADATA
            || side_data_type == AVFrameSideDataType_AV_FRAME_DATA_CONTENT_LIGHT_LEVEL
        {
            return Err(HdrError::HdrMetadataConversionUnverified.to_string());
        }
    }
    Ok((None, None))
}

/// Reduce a decoded frame's colorimetry tags to the matrix/range pair the
/// D3D11 video processor understands.
///
/// Untagged streams fall back on the industry convention: SD material
/// (≤576 lines) is BT.601, anything larger is BT.709. RGB sources map to
/// BT.601 limited because that is what sws_scale produces when it converts
/// RGB to NV12 in the software-fallback path.
unsafe fn frame_surface_color(frame: *const AVFrame) -> SurfaceColor {
    let colorspace = (*frame).colorspace;
    let bt709 = match colorspace {
        AVColorSpace_AVCOL_SPC_BT709 => true,
        AVColorSpace_AVCOL_SPC_RGB
        | AVColorSpace_AVCOL_SPC_BT470BG
        | AVColorSpace_AVCOL_SPC_SMPTE170M
        | AVColorSpace_AVCOL_SPC_SMPTE240M => false,
        _ => (*frame).height > 576,
    };
    let full_range = colorspace != AVColorSpace_AVCOL_SPC_RGB
        && (*frame).color_range == AVColorRange_AVCOL_RANGE_JPEG;
    SurfaceColor { bt709, full_range }
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
    let is_hw = matches!(video.output, VideoDecoderOutput::Hardware);
    loop {
        if should_cancel() {
            return Ok(());
        }
        // When using hardware decode, FFmpeg's avcodec_receive_frame calls
        // D3D11VA methods (DecoderBeginFrame, SubmitDecoderBuffers,
        // DecoderEndFrame) on the ID3D11VideoContext, which is NOT
        // covered by SetMultithreadProtected.  Hold the context lock
        // around the entire receive + CopySubresourceRegion sequence
        // so the UI thread's VideoProcessorBlt cannot race.
        let _hw_lock = if is_hw {
            if device.is_device_removed() {
                return Err("D3D11 device removed during hardware decode".into());
            }
            Some(device.lock_context())
        } else {
            None
        };
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

                let sar = (*frame).sample_aspect_ratio;
                let sar_num = if sar.num > 0 && sar.den > 0 {
                    sar.num as u32
                } else {
                    1
                };
                let sar_den = if sar.num > 0 && sar.den > 0 {
                    sar.den as u32
                } else {
                    1
                };

                let surface = device
                    .surface_from_raw_texture(
                        (*frame).data[0].cast::<c_void>(),
                        (*frame).data[1] as usize as u32,
                        (*frame).width as u32,
                        (*frame).height as u32,
                        sar_num,
                        sar_den,
                        frame_surface_color(frame),
                    )
                    .map_err(|error| error.to_string())?;

                PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts: decoded_frame_pts(frame, video.pts_time_base),
                    width: (*frame).width as u32,
                    height: (*frame).height as u32,
                    sar_num,
                    sar_den,
                    surface,
                }
            }
            VideoDecoderOutput::Software(converter) => {
                let surface = converter.convert(frame, device)?;
                let sar = (*frame).sample_aspect_ratio;
                let sar_num = if sar.num > 0 && sar.den > 0 {
                    sar.num as u32
                } else {
                    1
                };
                let sar_den = if sar.num > 0 && sar.den > 0 {
                    sar.den as u32
                } else {
                    1
                };
                PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts: decoded_frame_pts(frame, video.pts_time_base),
                    width: (*frame).width as u32,
                    height: (*frame).height as u32,
                    sar_num,
                    sar_den,
                    surface,
                }
            }
        };
        drop(_hw_lock);
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

        let sar = (*frame).sample_aspect_ratio;
        let sar_num = if sar.num > 0 && sar.den > 0 {
            sar.num as u32
        } else {
            1
        };
        let sar_den = if sar.num > 0 && sar.den > 0 {
            sar.den as u32
        } else {
            1
        };

        device
            .upload_nv12_surface_contiguous(
                width as u32,
                height as u32,
                &self.frame_buf,
                stride,
                sar_num,
                sar_den,
                frame_surface_color(frame),
            )
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
                output_format.sample_rate, output_format.channels
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
        self.output_buffer
            .resize(out_samples as usize * bytes_per_frame, 0);
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
    target_bytes: usize,
}

impl AudioBatcher {
    fn new(format: AudioStreamFormat) -> Self {
        let target_frames = (format.sample_rate / 10).max(1024);
        let target_bytes = target_frames as usize * format.bytes_per_frame() as usize;
        Self {
            format,
            pts: None,
            frame_count: 0,
            data: Vec::with_capacity(target_bytes),
            target_frames,
            target_bytes,
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

    /// Drop any partially-accumulated batch without emitting it. Called on seek:
    /// the buffered samples belong to the pre-seek position, and — critically —
    /// the retained `pts` (the first buffered sample's timestamp) would
    /// otherwise stamp the *next* batch of post-seek audio, anchoring the audio
    /// master clock to a stale position and desyncing A/V (worst on backward
    /// seeks, where the stale pts is ahead of the seek target and so escapes the
    /// coordinator's `seek_discard_before_pts` guard).
    fn reset(&mut self) {
        self.pts = None;
        self.frame_count = 0;
        self.data.clear();
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
        // Hand off the accumulated buffer and start the next batch pre-sized,
        // so the next run of `extend_from_slice` calls doesn't re-grow from
        // zero capacity on every batch.
        let data = std::mem::replace(&mut self.data, Vec::with_capacity(self.target_bytes));
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

/// Owns an `AVCodecContext`.  Field `.0` is the raw pointer (kept as the first
/// field so the many `.codec.0` call sites are unchanged).  Field `.1` holds the
/// `D3D11Device` for hardware decoders, and is `None` for software/audio codecs.
struct CodecContext(*mut AVCodecContext, Option<D3D11Device>);

impl Drop for CodecContext {
    fn drop(&mut self) {
        // For the D3D11VA hardware decoder, avcodec_free_context releases the
        // decoder and its D3D11 surface pool through the shared
        // ID3D11VideoContext — which SetMultithreadProtected does NOT cover.
        // Serialize that teardown under context_lock so a cancelled HW worker
        // cannot corrupt the immediate context while the live worker / UI
        // thread is running VideoProcessorBlt.  Software and audio codecs carry
        // no device, so they free without taking the lock.
        let _lock = self.1.as_ref().map(|device| device.lock_context());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::audio::AudioStreamFormat;
    use std::num::NonZeroU64;
    use std::time::Duration;

    fn op() -> OperationId {
        OperationId(NonZeroU64::new(1).unwrap())
    }

    // Collect the pts of every batch the batcher emits.
    fn drive(reset_between: bool) -> Vec<Duration> {
        let fmt = AudioStreamFormat::stereo_f32_48khz();
        let bpf = fmt.bytes_per_frame() as usize;
        let mut batcher = AudioBatcher::new(fmt);
        let mut produced = 0u64;
        let mut emitted: Vec<Duration> = Vec::new();
        // 10 frames is far below target_frames (sample_rate/10), so push does not
        // auto-flush; the batch stays partial until the explicit flush below.
        let data = vec![0u8; 10 * bpf];

        // Pre-seek audio decoded ahead of playback (e.g. ~14.3s).
        batcher
            .push(
                Duration::from_millis(14_300),
                10,
                &data,
                OpenGeneration(0),
                SeekGeneration(0),
                op(),
                &mut produced,
                &mut |f| {
                    emitted.push(f.pts);
                    Ok(())
                },
            )
            .unwrap();

        // A backward seek to ~7s happens here.
        if reset_between {
            batcher.reset();
        }

        // Post-seek audio decoded from the new (7s) position.
        batcher
            .push(
                Duration::from_millis(7_000),
                10,
                &data,
                OpenGeneration(0),
                SeekGeneration(1),
                op(),
                &mut produced,
                &mut |f| {
                    emitted.push(f.pts);
                    Ok(())
                },
            )
            .unwrap();
        batcher
            .flush(
                OpenGeneration(0),
                SeekGeneration(1),
                op(),
                &mut produced,
                &mut |f| {
                    emitted.push(f.pts);
                    Ok(())
                },
            )
            .unwrap();

        emitted
    }

    #[test]
    fn reset_on_seek_stamps_post_seek_audio_with_post_seek_pts() {
        // With the seek reset, the emitted batch carries the post-seek pts (7s),
        // so the audio master clock anchors at the seek target — A/V stay synced.
        assert_eq!(drive(true), vec![Duration::from_millis(7_000)]);
    }

    #[test]
    fn without_reset_stale_pre_seek_pts_leaks() {
        // Documents the desync bug the fix prevents: without the reset, the
        // partial pre-seek batch retains its first-sample pts (14.3s) and stamps
        // the post-seek audio, anchoring the clock ~7s ahead of video.
        assert_eq!(drive(false), vec![Duration::from_millis(14_300)]);
    }
}
