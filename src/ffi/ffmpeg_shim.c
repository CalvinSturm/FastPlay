#include "ffmpeg_shim.h"

AVStream *fastplay_ffmpeg_stream_at(AVFormatContext *ctx, unsigned int index) {
    if (!ctx || index >= ctx->nb_streams) {
        return NULL;
    }

    return ctx->streams[index];
}

AVCodecParameters *fastplay_ffmpeg_stream_codecpar(AVStream *stream) {
    if (!stream) {
        return NULL;
    }

    return stream->codecpar;
}

AVRational fastplay_ffmpeg_stream_time_base(AVStream *stream) {
    if (!stream) {
        AVRational zero = {0, 1};
        return zero;
    }

    return stream->time_base;
}

int fastplay_ffmpeg_error_eagain(void) {
    return AVERROR(EAGAIN);
}

int fastplay_ffmpeg_error_eof(void) {
    return AVERROR_EOF;
}
