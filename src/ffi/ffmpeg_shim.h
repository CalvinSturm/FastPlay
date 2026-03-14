#pragma once

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>

AVStream *fastplay_ffmpeg_stream_at(AVFormatContext *ctx, unsigned int index);
AVCodecParameters *fastplay_ffmpeg_stream_codecpar(AVStream *stream);
AVRational fastplay_ffmpeg_stream_time_base(AVStream *stream);
int fastplay_ffmpeg_error_eagain(void);
int fastplay_ffmpeg_error_eof(void);
