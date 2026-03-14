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
    media::{audio::AudioStreamFormat, source::MediaSource},
    playback::generations::{OpenGeneration, OperationId, SeekGeneration},
};

include!(concat!(env!("OUT_DIR"), "/ffmpeg_bindings.rs"));

#[derive(Debug)]
pub(crate) struct PendingVideoFrame {
    pub open_gen: OpenGeneration,
    pub seek_gen: SeekGeneration,
    pub op_id: OperationId,
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub surface: VideoSurface,
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

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StreamSummary {
    pub had_audio_stream: bool,
    pub produced_video_frames: u64,
    pub produced_audio_frames: u64,
}

pub(crate) fn stream_media<V, A>(
    source: &MediaSource,
    device: &D3D11Device,
    audio_output_format: AudioStreamFormat,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    mut on_video: V,
    mut on_audio: A,
) -> Result<StreamSummary, String>
where
    V: FnMut(PendingVideoFrame) -> Result<(), String>,
    A: FnMut(PendingAudioFrame) -> Result<(), String>,
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

        let video = open_video_decoder(input.0, device)?;
        let audio = open_audio_decoder(input.0, audio_output_format)?;
        let mut audio_batch = audio
            .as_ref()
            .map(|audio| AudioBatcher::new(audio.output_format));
        let mut summary = StreamSummary {
            had_audio_stream: audio.is_some(),
            produced_video_frames: 0,
            produced_audio_frames: 0,
        };

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

        loop {
            let read_status = av_read_frame(input.0, packet.0);
            if read_status == fastplay_ffmpeg_error_eof() {
                break;
            }
            ffmpeg_check(read_status, "av_read_frame")?;

            if (*packet.0).stream_index == video.stream_index as i32 {
                ffmpeg_check(
                    avcodec_send_packet(video.codec.0, packet.0),
                    "avcodec_send_packet(video)",
                )?;
                av_packet_unref(packet.0);
                receive_video_frames(
                    video.codec.0,
                    frame.0,
                    device,
                    open_gen,
                    seek_gen,
                    op_id,
                    &mut summary.produced_video_frames,
                    &mut on_video,
                )?;
                continue;
            }

            if let Some(audio) = audio.as_ref() {
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
                )?;
                continue;
            }
            }

            av_packet_unref(packet.0);
        }

        ffmpeg_check(
            avcodec_send_packet(video.codec.0, null()),
            "avcodec_send_packet(video flush)",
        )?;
        receive_video_frames(
            video.codec.0,
            frame.0,
            device,
            open_gen,
            seek_gen,
            op_id,
            &mut summary.produced_video_frames,
            &mut on_video,
        )?;

        if let Some(audio) = audio.as_ref() {
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
            )?;
            if let Some(batch) = audio_batch.as_mut() {
                batch.flush(open_gen, seek_gen, op_id, &mut summary.produced_audio_frames, &mut on_audio)?;
            }
        }

        if summary.produced_video_frames == 0 {
            return Err("no decodable video frame was produced".into());
        }

        Ok(summary)
    }
}

struct VideoDecoder {
    stream_index: usize,
    codec: CodecContext,
}

struct AudioDecoder {
    stream_index: usize,
    codec: CodecContext,
    resampler: Resampler,
    output_format: AudioStreamFormat,
}

unsafe fn open_video_decoder(
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

    ffmpeg_check(
        avcodec_parameters_to_context(codec.0, codec_parameters),
        "avcodec_parameters_to_context(video)",
    )?;
    (*codec.0).pkt_timebase = fastplay_ffmpeg_stream_time_base(stream);
    (*codec.0).get_format = Some(select_d3d11_pixel_format);
    configure_hw_device(codec.0, device, decoder)?;
    ffmpeg_check(avcodec_open2(codec.0, decoder, null_mut()), "avcodec_open2(video)")?;

    Ok(VideoDecoder { stream_index, codec })
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
    (*codec.0).pkt_timebase = fastplay_ffmpeg_stream_time_base(stream);
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
        resampler,
        output_format,
    }))
}

unsafe fn receive_video_frames<F>(
    codec_context: *mut AVCodecContext,
    frame: *mut AVFrame,
    device: &D3D11Device,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    produced_frames: &mut u64,
    on_frame: &mut F,
) -> Result<(), String>
where
    F: FnMut(PendingVideoFrame) -> Result<(), String>,
{
    loop {
        let status = avcodec_receive_frame(codec_context, frame);
        if status == fastplay_ffmpeg_error_eagain() || status == fastplay_ffmpeg_error_eof() {
            return Ok(());
        }
        ffmpeg_check(status, "avcodec_receive_frame(video)")?;

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

        let result = PendingVideoFrame {
            open_gen,
            seek_gen,
            op_id,
            pts: frame_pts((*frame).best_effort_timestamp, (*frame).time_base),
            width: (*frame).width as u32,
            height: (*frame).height as u32,
            surface,
        };
        av_frame_unref(frame);
        *produced_frames = (*produced_frames).saturating_add(1);
        on_frame(result)?;
    }
}

unsafe fn receive_audio_frames<F>(
    audio: &AudioDecoder,
    frame: *mut AVFrame,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
    mut batcher: Option<&mut AudioBatcher>,
    produced_frames: &mut u64,
    on_frame: &mut F,
) -> Result<(), String>
where
    F: FnMut(PendingAudioFrame) -> Result<(), String>,
{
    loop {
        let status = avcodec_receive_frame(audio.codec.0, frame);
        if status == fastplay_ffmpeg_error_eagain() || status == fastplay_ffmpeg_error_eof() {
            return Ok(());
        }
        ffmpeg_check(status, "avcodec_receive_frame(audio)")?;

        let pts = frame_pts((*frame).best_effort_timestamp, (*frame).time_base);
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
                data,
            })?;
        }
    }
}

struct Resampler {
    context: *mut SwrContext,
    output_format: AudioStreamFormat,
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
        })
    }

    unsafe fn convert(&self, frame: *mut AVFrame) -> Result<Vec<u8>, String> {
        let out_samples = swr_get_out_samples(self.context, (*frame).nb_samples);
        ffmpeg_check(out_samples, "swr_get_out_samples")?;

        let bytes_per_frame = self.output_format.bytes_per_frame() as usize;
        let mut output = vec![0u8; out_samples as usize * bytes_per_frame];
        let output_planes = [output.as_mut_ptr()];
        let converted = swr_convert(
            self.context,
            output_planes.as_ptr().cast(),
            out_samples,
            (*frame).extended_data.cast(),
            (*frame).nb_samples,
        );
        ffmpeg_check(converted, "swr_convert")?;
        output.truncate(converted as usize * bytes_per_frame);
        Ok(output)
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
        data: Vec<u8>,
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
        self.data.extend_from_slice(&data);
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
    if value <= 0 || time_base.den == 0 || time_base.num == 0 {
        return Duration::ZERO;
    }

    let seconds = (value as f64) * (time_base.num as f64) / (time_base.den as f64);
    Duration::from_secs_f64(seconds.max(0.0))
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
