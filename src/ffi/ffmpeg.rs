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
    media::source::MediaSource,
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

pub(crate) fn decode_first_video_frame(
    source: &MediaSource,
    device: &D3D11Device,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
) -> Result<PendingVideoFrame, String> {
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

        let mut decoder: *const AVCodec = null();
        let stream_index = ffmpeg_check(
            av_find_best_stream(
                input.0,
                AVMediaType_AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                &mut decoder,
                0,
            ),
            "av_find_best_stream",
        )? as usize;
        if decoder.is_null() {
            return Err("no decoder found for selected video stream".into());
        }

        let stream = selected_stream(input.0, stream_index)?;
        let codec_context = avcodec_alloc_context3(decoder);
        if codec_context.is_null() {
            return Err("avcodec_alloc_context3 returned null".into());
        }
        let codec = CodecContext(codec_context);

        let codec_parameters = fastplay_ffmpeg_stream_codecpar(stream);
        if codec_parameters.is_null() {
            return Err("selected AVStream codec parameters were null".into());
        }

        ffmpeg_check(
            avcodec_parameters_to_context(codec.0, codec_parameters),
            "avcodec_parameters_to_context",
        )?;
        (*codec.0).pkt_timebase = fastplay_ffmpeg_stream_time_base(stream);
        (*codec.0).get_format = Some(select_d3d11_pixel_format);
        configure_hw_device(codec.0, device, decoder)?;
        ffmpeg_check(avcodec_open2(codec.0, decoder, null_mut()), "avcodec_open2")?;

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
            if read_status < 0 {
                break;
            }

            if (*packet.0).stream_index != stream_index as i32 {
                av_packet_unref(packet.0);
                continue;
            }

            ffmpeg_check(
                avcodec_send_packet(codec.0, packet.0),
                "avcodec_send_packet",
            )?;
            av_packet_unref(packet.0);

            if let Some(frame) =
                try_receive_first_frame(codec.0, frame.0, device, open_gen, seek_gen, op_id)?
            {
                return Ok(frame);
            }
        }

        ffmpeg_check(
            avcodec_send_packet(codec.0, null()),
            "avcodec_send_packet(flush)",
        )?;
        if let Some(frame) =
            try_receive_first_frame(codec.0, frame.0, device, open_gen, seek_gen, op_id)?
        {
            return Ok(frame);
        }

        Err("no decodable video frame was produced".into())
    }
}

unsafe fn try_receive_first_frame(
    codec_context: *mut AVCodecContext,
    frame: *mut AVFrame,
    device: &D3D11Device,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    op_id: OperationId,
) -> Result<Option<PendingVideoFrame>, String> {
    loop {
        let status = avcodec_receive_frame(codec_context, frame);
        if status < 0 {
            return Ok(None);
        }

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

        return Ok(Some(result));
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
