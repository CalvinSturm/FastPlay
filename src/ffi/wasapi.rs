use std::{error::Error, fmt};

use windows::Win32::{
    Media::Audio::{
        eConsole, eRender, IAudioClient3, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    },
    Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE,
    Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT,
    System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_APARTMENTTHREADED,
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

/// Shared-mode render buffer size, in `REFERENCE_TIME` units (100 ns).
/// 200 ms of headroom so transient UI-thread stalls don't underrun the
/// audio engine (which would otherwise reset the A/V clock and stutter video).
const SHARED_BUFFER_DURATION_HNS: i64 = 200 * 10_000;

pub struct WasapiAudioSink {
    _com: ComApartment,
    audio_client: IAudioClient3,
    render_client: IAudioRenderClient,
    buffer_frames: u32,
}

impl WasapiAudioSink {
    pub fn create_shared_default() -> Result<(Self, AudioStreamFormat), Box<dyn Error>> {
        let com = ComApartment::initialize()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let device: IMMDevice = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };
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

        // Initialize shared mode with a deep buffer instead of the
        // IAudioClient3 low-latency engine period (~10 ms). A media player
        // wants headroom, not minimum latency: the ~10 ms path underran
        // thousands of times per clip whenever the UI thread was busy for
        // more than one device period (Present, VideoProcessorBlt, a stalled
        // drain loop), and each underrun reset the A/V clock anchor and
        // stalled video presentation — the visible "stutter". A 200 ms buffer
        // tolerates those hitches without draining dry.
        unsafe {
            audio_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                SHARED_BUFFER_DURATION_HNS,
                0,
                mix_format.as_ptr(),
                None,
            )?;
        }

        let render_client: IAudioRenderClient = unsafe { audio_client.GetService()? };
        let buffer_frames = unsafe { audio_client.GetBufferSize()? };
        crate::flog!(
            "[wasapi] init buffer_frames={} ({:.1}ms @ {}Hz)",
            buffer_frames,
            buffer_frames as f64 * 1000.0 / actual_format.sample_rate as f64,
            actual_format.sample_rate
        );

        Ok((
            Self {
                _com: com,
                audio_client,
                render_client,
                buffer_frames,
            },
            actual_format,
        ))
    }

    pub fn start(&self) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.audio_client.Start()?;
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

            // Once GetBuffer succeeds the render buffer is checked out and must
            // be returned with exactly one ReleaseBuffer, or the audio client is
            // left locked. The guard releases 0 frames if anything below fails
            // or panics; the success path disarms it and releases the real
            // count. (Today the copy cannot fail, but the guard keeps the
            // acquire/release balanced if a fallible conversion is ever added
            // between GetBuffer and ReleaseBuffer.)
            let mut release = ReleaseBufferGuard {
                render_client: &self.render_client,
                armed: true,
            };

            std::ptr::copy_nonoverlapping(data.as_ptr(), destination.cast::<u8>(), bytes_to_copy);

            // Disarm before the real release so a ReleaseBuffer failure cannot
            // trigger a second (incorrect) release from the guard.
            release.armed = false;
            self.render_client.ReleaseBuffer(frames_to_write, 0)?;
        }

        Ok(frames_to_write)
    }

    pub fn buffered_frames(&self) -> Result<u32, Box<dyn Error>> {
        Ok(unsafe { self.audio_client.GetCurrentPadding()? })
    }
}

/// Releases a checked-out WASAPI render buffer if dropped while still armed.
/// Guarantees a `GetBuffer` is never left without a matching `ReleaseBuffer`
/// when the copy/conversion between them fails or unwinds.
struct ReleaseBufferGuard<'a> {
    render_client: &'a IAudioRenderClient,
    armed: bool,
}

impl Drop for ReleaseBufferGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Release 0 frames: the buffer is returned to WASAPI emitting no
            // audio. Best-effort and never panics in Drop.
            // SAFETY: paired with a successful GetBuffer on the same client.
            unsafe {
                let _ = self.render_client.ReleaseBuffer(0, 0);
            }
        }
    }
}

struct MixFormat(*mut WAVEFORMATEX);

impl MixFormat {
    fn query(audio_client: &IAudioClient3) -> Result<Self, Box<dyn Error>> {
        let format = unsafe { audio_client.GetMixFormat()? };
        if format.is_null() {
            return Err(Box::new(WasapiError(
                "IAudioClient3::GetMixFormat returned null".into(),
            )));
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
            return Err(Box::new(WasapiError(
                "mix format reported zero bytes per sample".into(),
            )));
        }
        if format_tag != WAVE_FORMAT_IEEE_FLOAT as u16
            && format_tag != WAVE_FORMAT_EXTENSIBLE as u16
        {
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
