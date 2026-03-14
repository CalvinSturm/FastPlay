#pragma once

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/channel_layout.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>

AVStream *fastplay_ffmpeg_stream_at(AVFormatContext *ctx, unsigned int index);
AVCodecParameters *fastplay_ffmpeg_stream_codecpar(AVStream *stream);
AVRational fastplay_ffmpeg_stream_time_base(AVStream *stream);
int fastplay_ffmpeg_error_eagain(void);
int fastplay_ffmpeg_error_eof(void);
int fastplay_ffmpeg_error_stream_not_found(void);
int fastplay_ffmpeg_seek_to_micros(AVFormatContext *ctx, int64_t timestamp_micros);
void fastplay_ffmpeg_flush_codec(AVCodecContext *ctx);
uint64_t fastplay_ffmpeg_channel_layout_mask_or_default(const AVChannelLayout *layout);
uint64_t fastplay_ffmpeg_stereo_layout_mask(void);
SwrContext *fastplay_ffmpeg_create_float_resampler(
    const AVChannelLayout *input_layout,
    enum AVSampleFormat input_sample_fmt,
    int input_sample_rate,
    uint64_t output_channel_mask,
    int output_channels,
    int output_sample_rate
);
