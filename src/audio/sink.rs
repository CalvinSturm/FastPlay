use std::time::Duration;

use crate::{
    ffi::wasapi::WasapiAudioSink,
    media::audio::{AudioStreamFormat, DecodedAudioFrame},
};

pub struct AudioSink {
    inner: WasapiAudioSink,
    format: AudioStreamFormat,
    started: bool,
    volume: f32,
    volume_scratch: Vec<u8>,
}

impl AudioSink {
    pub fn create_shared_default() -> Result<Self, Box<dyn std::error::Error>> {
        let (inner, format) = WasapiAudioSink::create_shared_default()?;
        Ok(Self {
            inner,
            format,
            started: false,
            volume: 1.0,
            volume_scratch: Vec::new(),
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

    pub fn reset(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.reset()?;
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

        if (self.volume - 1.0).abs() < f32::EPSILON {
            return self
                .inner
                .write_interleaved(&frame.data[start..], remaining_frames, self.format);
        }

        let data = &frame.data[start..];
        self.volume_scratch.resize(data.len(), 0);
        self.volume_scratch.copy_from_slice(data);
        for sample in self.volume_scratch.chunks_exact_mut(4) {
            let value = f32::from_ne_bytes([sample[0], sample[1], sample[2], sample[3]]);
            let scaled_value = (value * self.volume).clamp(-1.0, 1.0);
            sample.copy_from_slice(&scaled_value.to_ne_bytes());
        }

        self.inner
            .write_interleaved(&self.volume_scratch, remaining_frames, self.format)
    }

    pub fn playback_position(&self) -> Result<Duration, Box<dyn std::error::Error>> {
        self.inner.playback_position()
    }

    pub fn buffered_frames(&self) -> Result<u32, Box<dyn std::error::Error>> {
        self.inner.buffered_frames()
    }

    pub fn adjust_volume_steps(&mut self, steps: i16) {
        let delta = 0.05 * steps as f32;
        self.volume = (self.volume + delta).clamp(0.0, 1.5);
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn volume_percent(&self) -> u32 {
        (self.volume * 100.0).round().max(0.0) as u32
    }

    pub fn is_started(&self) -> bool {
        self.started
    }
}
