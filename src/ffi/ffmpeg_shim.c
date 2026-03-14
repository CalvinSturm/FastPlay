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

int fastplay_ffmpeg_error_stream_not_found(void) {
    return AVERROR_STREAM_NOT_FOUND;
}

int64_t fastplay_ffmpeg_duration_micros(AVFormatContext *ctx) {
    if (!ctx || ctx->duration == AV_NOPTS_VALUE) {
        return AV_NOPTS_VALUE;
    }

    return ctx->duration;
}

int64_t fastplay_ffmpeg_start_time_micros(AVFormatContext *ctx) {
    if (!ctx || ctx->start_time == AV_NOPTS_VALUE) {
        return AV_NOPTS_VALUE;
    }

    return ctx->start_time;
}

int fastplay_ffmpeg_seek_to_micros(AVFormatContext *ctx, int64_t timestamp_micros) {
    if (!ctx) {
        return AVERROR(EINVAL);
    }

    return av_seek_frame(ctx, -1, timestamp_micros, AVSEEK_FLAG_BACKWARD);
}

void fastplay_ffmpeg_flush_codec(AVCodecContext *ctx) {
    if (ctx) {
        avcodec_flush_buffers(ctx);
    }
}

uint64_t fastplay_ffmpeg_channel_layout_mask_or_default(const AVChannelLayout *layout) {
    if (!layout) {
        return 0;
    }

    if (layout->order == AV_CHANNEL_ORDER_NATIVE && layout->u.mask != 0) {
        return layout->u.mask;
    }

    AVChannelLayout fallback = {0};
    av_channel_layout_default(&fallback, layout->nb_channels);
    return fallback.u.mask;
}

uint64_t fastplay_ffmpeg_stereo_layout_mask(void) {
    AVChannelLayout layout = AV_CHANNEL_LAYOUT_STEREO;
    return layout.u.mask;
}

SwrContext *fastplay_ffmpeg_create_float_resampler(
    const AVChannelLayout *input_layout,
    enum AVSampleFormat input_sample_fmt,
    int input_sample_rate,
    uint64_t output_channel_mask,
    int output_channels,
    int output_sample_rate
) {
    if (!input_layout) {
        return NULL;
    }

    AVChannelLayout output_layout = {0};
    if (output_channel_mask != 0) {
        if (av_channel_layout_from_mask(&output_layout, output_channel_mask) < 0) {
            return NULL;
        }
    } else {
        av_channel_layout_default(&output_layout, output_channels);
    }

    SwrContext *ctx = NULL;
    if (swr_alloc_set_opts2(
            &ctx,
            &output_layout,
            AV_SAMPLE_FMT_FLT,
            output_sample_rate,
            input_layout,
            input_sample_fmt,
            input_sample_rate,
            0,
            NULL) < 0) {
        if (ctx) {
            swr_free(&ctx);
        }
        return NULL;
    }

    if (swr_init(ctx) < 0) {
        swr_free(&ctx);
        return NULL;
    }

    return ctx;
}
