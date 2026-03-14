use std::time::Duration;

use crate::{
    ffi::wasapi::WasapiAudioSink,
    media::audio::{AudioStreamFormat, DecodedAudioFrame},
};

pub struct AudioSink {
    inner: WasapiAudioSink,
    format: AudioStreamFormat,
    started: bool,
}

impl AudioSink {
    pub fn create_shared_default() -> Result<Self, Box<dyn std::error::Error>> {
        let (inner, format) = WasapiAudioSink::create_shared_default()?;
        Ok(Self {
            inner,
            format,
            started: false,
        })
    }

    pub fn format(&self) -> AudioStreamFormat {
        self.format
    }

    pub fn resume(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.start()?;
        self.started = true;
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.stop()?;
        self.started = false;
        Ok(())
    }

    pub fn write_frame(
        &mut self,
        frame: &DecodedAudioFrame,
        frame_offset: u32,
    ) -> Result<u32, Box<dyn std::error::Error>> {
        let bytes_per_frame = frame.bytes_per_frame();
        let start = frame_offset as usize * bytes_per_frame;
        let remaining_frames = frame.frame_count().saturating_sub(frame_offset);
        if remaining_frames == 0 || start >= frame.data.len() {
            return Ok(0);
        }

        self.inner
            .write_interleaved(&frame.data[start..], remaining_frames, self.format)
    }

    pub fn playback_position(&self) -> Result<Duration, Box<dyn std::error::Error>> {
        self.inner.playback_position()
    }

    pub fn buffered_frames(&self) -> Result<u32, Box<dyn std::error::Error>> {
        self.inner.buffered_frames()
    }
    pub fn is_started(&self) -> bool {
        self.started
    }
}
