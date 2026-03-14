use std::{cell::Cell, error::Error, fmt, time::Duration};

use windows::{
    Win32::{
        Media::Audio::{
            eConsole, eRender, IAudioClient3, IAudioClock, IAudioRenderClient, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
        },
        Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE,
        Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
                COINIT_APARTMENTTHREADED,
            },
        },
    },
};

use crate::media::audio::AudioStreamFormat;

#[derive(Debug)]
pub struct WasapiError(String);

impl fmt::Display for WasapiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for WasapiError {}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, Box<dyn Error>> {
        // SAFETY:
        // - M3 creates the WASAPI sink on the UI thread and drops it on the same thread
        // - apartment-threaded COM is sufficient for IMMDevice/IAudioClient usage here
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct WasapiAudioSink {
    _com: ComApartment,
    audio_client: IAudioClient3,
    render_client: IAudioRenderClient,
    audio_clock: IAudioClock,
    buffer_frames: u32,
    clock_origin: Cell<Option<u64>>,
}

impl WasapiAudioSink {
    pub fn create_shared_default() -> Result<(Self, AudioStreamFormat), Box<dyn Error>> {
        let com = ComApartment::initialize()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let device: IMMDevice =
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };
        let audio_client: IAudioClient3 = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let mix_format = MixFormat::query(&audio_client)?;
        let actual_format = mix_format.audio_stream_format()?;
        if actual_format.bytes_per_sample != 4 {
            return Err(Box::new(WasapiError(format!(
                "default shared mix format is {} Hz, {} channels, {} bytes/sample; M3 currently supports only float shared-mode sinks",
                actual_format.sample_rate,
                actual_format.channels,
                actual_format.bytes_per_sample,
            ))));
        }

        let mut default_period_in_frames = 0u32;
        let mut fundamental_period_in_frames = 0u32;
        let mut min_period_in_frames = 0u32;
        let mut max_period_in_frames = 0u32;

        unsafe {
            audio_client.GetSharedModeEnginePeriod(
                mix_format.as_ptr(),
                &mut default_period_in_frames,
                &mut fundamental_period_in_frames,
                &mut min_period_in_frames,
                &mut max_period_in_frames,
            )?;
            let _ = (fundamental_period_in_frames, min_period_in_frames, max_period_in_frames);
            audio_client.InitializeSharedAudioStream(
                0,
                default_period_in_frames,
                mix_format.as_ptr(),
                None,
            )?;
        }

        let render_client: IAudioRenderClient = unsafe { audio_client.GetService()? };
        let audio_clock: IAudioClock = unsafe { audio_client.GetService()? };
        let buffer_frames = unsafe { audio_client.GetBufferSize()? };

        Ok((
            Self {
                _com: com,
                audio_client,
                render_client,
                audio_clock,
                buffer_frames,
                clock_origin: Cell::new(None),
            },
            actual_format,
        ))
    }

    pub fn start(&self) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.audio_client.Start()?;
        }
        if self.clock_origin.get().is_none() {
            self.clock_origin.set(Some(self.raw_clock_position()?));
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.audio_client.Stop()?;
        }
        Ok(())
    }

    pub fn reset(&self) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.audio_client.Stop()?;
            self.audio_client.Reset()?;
        }
        self.clock_origin.set(None);
        Ok(())
    }

    pub fn write_interleaved(
        &mut self,
        data: &[u8],
        frame_count: u32,
        format: AudioStreamFormat,
    ) -> Result<u32, Box<dyn Error>> {
        if data.is_empty() || frame_count == 0 {
            return Ok(0);
        }

        let padding = unsafe { self.audio_client.GetCurrentPadding()? };
        let available_frames = self.buffer_frames.saturating_sub(padding);
        let frames_to_write = available_frames.min(frame_count);
        if frames_to_write == 0 {
            return Ok(0);
        }

        let bytes_per_frame = format.bytes_per_frame() as usize;
        let bytes_to_copy = frames_to_write as usize * bytes_per_frame;
        if bytes_to_copy > data.len() {
            return Err(Box::new(WasapiError(
                "audio frame payload was smaller than the declared frame count".into(),
            )));
        }

        // SAFETY:
        // - WASAPI returns a writable render buffer for exactly `frames_to_write` frames
        // - source and destination slices are non-overlapping and sized in bytes
        unsafe {
            let destination = self.render_client.GetBuffer(frames_to_write)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), destination.cast::<u8>(), bytes_to_copy);
            self.render_client.ReleaseBuffer(frames_to_write, 0)?;
        }

        Ok(frames_to_write)
    }

    pub fn playback_position(&self) -> Result<Duration, Box<dyn Error>> {
        unsafe {
            let frequency = self.audio_clock.GetFrequency()?;
            if frequency == 0 {
                return Ok(Duration::ZERO);
            }
            let current = self.raw_clock_position()?;
            let origin = self.clock_origin.get().unwrap_or(current);
            let delta = current.saturating_sub(origin);
            return Ok(Duration::from_secs_f64(delta as f64 / frequency as f64));
        }
    }

    pub fn buffered_frames(&self) -> Result<u32, Box<dyn Error>> {
        Ok(unsafe { self.audio_client.GetCurrentPadding()? })
    }

    fn raw_clock_position(&self) -> Result<u64, Box<dyn Error>> {
        let mut position = 0u64;
        unsafe {
            self.audio_clock.GetPosition(&mut position, None)?;
        }
        Ok(position)
    }
}

struct MixFormat(*mut WAVEFORMATEX);

impl MixFormat {
    fn query(audio_client: &IAudioClient3) -> Result<Self, Box<dyn Error>> {
        let format = unsafe { audio_client.GetMixFormat()? };
        if format.is_null() {
            return Err(Box::new(WasapiError("IAudioClient3::GetMixFormat returned null".into())));
        }
        Ok(Self(format))
    }

    fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0
    }

    fn audio_stream_format(&self) -> Result<AudioStreamFormat, Box<dyn Error>> {
        let format = unsafe { *self.0 };
        let format_tag = format.wFormatTag;
        let sample_rate = format.nSamplesPerSec;
        let channels = format.nChannels;
        let bits_per_sample = format.wBitsPerSample;
        let bytes_per_sample = bits_per_sample / 8;
        if bytes_per_sample == 0 {
            return Err(Box::new(WasapiError("mix format reported zero bytes per sample".into())));
        }
        if format_tag != WAVE_FORMAT_IEEE_FLOAT as u16 && format_tag != WAVE_FORMAT_EXTENSIBLE as u16 {
            return Err(Box::new(WasapiError(format!(
                "default shared mix format tag {} is not a float format",
                format_tag
            ))));
        }

        let mut channel_mask = 0u64;
        if format_tag == WAVE_FORMAT_EXTENSIBLE as u16 {
            // SAFETY:
            // - GetMixFormat returns a valid WAVEFORMATEX/WAVEFORMATEXTENSIBLE allocation
            // - the extensible variant is only read when the format tag indicates it
            let extensible = unsafe { &*self.0.cast::<WAVEFORMATEXTENSIBLE>() };
            channel_mask = extensible.dwChannelMask as u64;
        }

        Ok(AudioStreamFormat {
            sample_rate,
            channels,
            bytes_per_sample,
            channel_mask,
        })
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}
