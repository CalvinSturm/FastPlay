use std::{
    error::Error,
    fmt, mem,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, ThreadId},
};

use windows::Win32::{
    Media::Audio::{
        eConsole, eRender, EDataFlow, ERole, IAudioClient3, IAudioRenderClient, IMMDevice,
        IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
        AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    },
    Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE,
    Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT,
    System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_APARTMENTTHREADED,
    },
    UI::Shell::PropertiesSystem::PROPERTYKEY,
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
    render_client: IAudioRenderClient,
    audio_client: IAudioClient3,
    buffer_frames: u32,
    // Rust drops fields in declaration order. Keep the apartment guard last so
    // both COM interfaces release before the matching CoUninitialize.
    _com: ComApartment,
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
                render_client,
                audio_client,
                buffer_frames,
                _com: com,
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

// ── Default-endpoint change notification ─────────────────────────────────────

/// COM callback that records when Windows switches the default render endpoint.
///
/// Exists because the reactive path cannot see this case. An `IAudioClient` is
/// bound to one specific `IMMDevice` for its lifetime, so when the *default*
/// changes while the old device stays valid — the user plugs in headphones,
/// picks another output in the volume flyout, connects a Bluetooth sink — every
/// WASAPI call keeps succeeding and audio keeps coming out of the old endpoint.
/// There is no error for `submit_due_audio` to notice. (Device *removal* is
/// different: that fails with `AUDCLNT_E_DEVICE_INVALIDATED` and is already
/// handled on the next write.)
///
/// The callback does the least work that is correct: it hashes the provided
/// endpoint ID and sets a flag only when that identity changes. It runs on an
/// MMDevice API worker thread, where COM forbids blocking and forbids re-entering
/// the enumerator, and where none of the coordinator's state may be touched.
/// `PlaybackSession` polls the flag on the UI thread during `tick`, exactly as it
/// polls the window for resize requests.
#[windows::core::implement(IMMNotificationClient)]
struct DefaultRenderEndpointNotifier {
    changed: Arc<AtomicBool>,
    last_endpoint_hash: Arc<AtomicU64>,
}

/// Stable, allocation-free identity for an MMDevice endpoint ID.
///
/// Windows can deliver the same `(eRender, eConsole)` change more than once,
/// far enough apart for the UI thread to consume the edge-triggered flag in
/// between. Hashing the callback-owned UTF-16 ID lets the callback suppress
/// those duplicates without blocking, allocating, or re-entering MMDevice.
fn endpoint_id_hash(device_id: &windows_core::PCWSTR) -> u64 {
    if device_id.is_null() {
        return 1;
    }

    let mut hash = 0xcbf29ce484222325_u64;
    // SAFETY: MMDevice owns this NUL-terminated endpoint ID and guarantees it
    // remains valid for the duration of the callback.
    for code_unit in unsafe { device_id.as_wide() } {
        hash ^= u64::from(*code_unit);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn current_default_endpoint_hash(enumerator: &IMMDeviceEnumerator) -> Option<u64> {
    // SAFETY: the enumerator is live in this apartment. GetId returns a
    // CoTaskMem allocation which is released before returning.
    unsafe {
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = device.GetId().ok()?;
        let hash = endpoint_id_hash(&windows_core::PCWSTR(id.0));
        CoTaskMemFree(Some(id.0.cast()));
        Some(hash)
    }
}

impl IMMNotificationClient_Impl for DefaultRenderEndpointNotifier_Impl {
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        default_device_id: &windows_core::PCWSTR,
    ) -> windows_core::Result<()> {
        // Only the render endpoint this sink asks for. Windows raises one
        // notification per role (eConsole / eMultimedia / eCommunications) for
        // the same physical switch; filtering to the role the sink is opened
        // with (`eConsole`, see `create_shared_default`) removes the other two.
        // Some drivers still repeat the console notification after `tick` has
        // consumed the first flag, so also suppress an unchanged endpoint ID.
        if flow == eRender && role == eConsole {
            let endpoint_hash = endpoint_id_hash(default_device_id);
            let previous = self
                .last_endpoint_hash
                .swap(endpoint_hash, Ordering::AcqRel);
            if previous != endpoint_hash {
                self.changed.store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    fn OnDeviceStateChanged(
        &self,
        _device_id: &windows_core::PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows_core::Result<()> {
        // Removal/disable of the device in use surfaces as a failed WASAPI
        // write, which `submit_due_audio` already recovers from. Reacting here
        // too would double-recover on one event.
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &windows_core::PCWSTR) -> windows_core::Result<()> {
        // A new device only matters if Windows makes it the default, which
        // arrives separately as OnDefaultDeviceChanged.
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &windows_core::PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &windows_core::PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows_core::Result<()> {
        Ok(())
    }
}

/// A live registration for default-render-endpoint changes.
///
/// The MMDevice registration does not retain the callback for us. Destruction
/// therefore unregisters first on the thread that installed the watch, then
/// releases the callback and enumerator, and only then uninitializes COM. The
/// COM-owned fields are `Option`s so `Drop` controls that order explicitly.
pub struct DefaultRenderEndpointWatch {
    changed: Arc<AtomicBool>,
    owner_thread: ThreadId,
    client: Option<IMMNotificationClient>,
    enumerator: Option<IMMDeviceEnumerator>,
    com: Option<ComApartment>,
}

impl DefaultRenderEndpointWatch {
    /// Register for default-render-endpoint changes. Non-fatal by design: the
    /// caller degrades to the reactive-only path if this fails.
    pub fn install() -> Result<Self, Box<dyn Error>> {
        let com = ComApartment::initialize()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let changed = Arc::new(AtomicBool::new(false));
        let last_endpoint_hash = Arc::new(AtomicU64::new(
            current_default_endpoint_hash(&enumerator).unwrap_or(0),
        ));
        let client: IMMNotificationClient = DefaultRenderEndpointNotifier {
            changed: changed.clone(),
            last_endpoint_hash,
        }
        .into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client)? };
        Ok(Self {
            changed,
            owner_thread: thread::current().id(),
            client: Some(client),
            enumerator: Some(enumerator),
            com: Some(com),
        })
    }

    /// Consume the "default render endpoint changed" flag, returning whether one
    /// was pending. Edge-triggered: true at most once per switch.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

impl Drop for DefaultRenderEndpointWatch {
    fn drop(&mut self) {
        if thread::current().id() != self.owner_thread {
            crate::flog!(
                "[audio_endpoint] watch dropped off its owning thread; retaining COM registration"
            );
            self.retain_registration();
            return;
        }

        let (Some(enumerator), Some(client)) = (self.enumerator.as_ref(), self.client.as_ref())
        else {
            return;
        };

        // SAFETY:
        // - this is the same enumerator/client pair passed to Register...
        // - Drop runs on the UI thread that installed the apartment and watch
        // - this is not an IMMNotificationClient callback, so unregistering does
        //   not re-enter the enumerator from inside a notification
        if let Err(error) = unsafe { enumerator.UnregisterEndpointNotificationCallback(client) } {
            crate::flog!(
                "[audio_endpoint] failed to unregister endpoint callback; retaining COM registration: {error}"
            );
            self.retain_registration();
            return;
        }

        // The registration is gone, so the callback may release its final
        // reference. Release both interfaces before balancing CoInitializeEx.
        drop(self.client.take());
        drop(self.enumerator.take());
        drop(self.com.take());
    }
}

impl DefaultRenderEndpointWatch {
    /// Preserve a potentially live MMDevice registration. Leaking is the only
    /// safe fallback: RegisterEndpointNotificationCallback does not AddRef the
    /// callback, so releasing any of these objects after failed unregistration
    /// could leave Windows with a dangling callback pointer.
    fn retain_registration(&mut self) {
        if let Some(client) = self.client.take() {
            mem::forget(client);
        }
        if let Some(enumerator) = self.enumerator.take() {
            mem::forget(enumerator);
        }
        if let Some(com) = self.com.take() {
            mem::forget(com);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::{eCapture, eCommunications, eMultimedia};

    /// Build the notifier behind its COM interface, plus the flag it writes, so
    /// the callback can be invoked exactly as the audio engine would.
    fn notifier() -> (IMMNotificationClient, Arc<AtomicBool>) {
        let changed = Arc::new(AtomicBool::new(false));
        let client: IMMNotificationClient = DefaultRenderEndpointNotifier {
            changed: changed.clone(),
            last_endpoint_hash: Arc::new(AtomicU64::new(0)),
        }
        .into();
        (client, changed)
    }

    fn notify(client: &IMMNotificationClient, flow: EDataFlow, role: ERole) {
        unsafe {
            client
                .OnDefaultDeviceChanged(flow, role, windows_core::PCWSTR::null())
                .expect("callback must not fail");
        }
    }

    #[test]
    fn records_a_default_render_console_switch() {
        let (client, changed) = notifier();
        assert!(!changed.load(Ordering::Acquire), "starts clear");
        notify(&client, eRender, eConsole);
        assert!(
            changed.load(Ordering::Acquire),
            "the switch must be recorded"
        );
    }

    #[test]
    fn ignores_roles_and_flows_the_sink_does_not_use() {
        // The sink opens (eRender, eConsole). Windows raises a notification per
        // role for one physical switch, and capture devices are irrelevant here;
        // reacting to those would restart playback for events that do not affect
        // where this stream is rendered.
        for (flow, role) in [
            (eRender, eMultimedia),
            (eRender, eCommunications),
            (eCapture, eConsole),
            (eCapture, eMultimedia),
            (eCapture, eCommunications),
        ] {
            let (client, changed) = notifier();
            notify(&client, flow, role);
            assert!(
                !changed.load(Ordering::Acquire),
                "flow {flow:?} role {role:?} must not trigger a switch"
            );
        }
    }

    #[test]
    fn repeated_notifications_coalesce_into_one_switch() {
        // One user action can raise several notifications. The coordinator must
        // restart once, not once per notification.
        let changed = Arc::new(AtomicBool::new(false));
        let client: IMMNotificationClient = DefaultRenderEndpointNotifier {
            changed: changed.clone(),
            last_endpoint_hash: Arc::new(AtomicU64::new(0)),
        }
        .into();
        for _ in 0..5 {
            notify(&client, eRender, eConsole);
        }
        assert!(changed.swap(false, Ordering::AcqRel), "one switch pending");
        assert!(
            !changed.swap(false, Ordering::AcqRel),
            "and only one: the flag is edge-triggered"
        );
        notify(&client, eRender, eConsole);
        assert!(
            !changed.load(Ordering::Acquire),
            "the same endpoint remains suppressed after the flag was consumed"
        );
    }

    #[test]
    fn a_distinct_endpoint_records_a_new_switch() {
        let changed = Arc::new(AtomicBool::new(false));
        let last_endpoint_hash = Arc::new(AtomicU64::new(0));
        let client: IMMNotificationClient = DefaultRenderEndpointNotifier {
            changed: changed.clone(),
            last_endpoint_hash,
        }
        .into();
        let first = "endpoint-a\0".encode_utf16().collect::<Vec<_>>();
        let second = "endpoint-b\0".encode_utf16().collect::<Vec<_>>();

        unsafe {
            client
                .OnDefaultDeviceChanged(eRender, eConsole, windows_core::PCWSTR(first.as_ptr()))
                .unwrap();
        }
        assert!(changed.swap(false, Ordering::AcqRel));

        unsafe {
            client
                .OnDefaultDeviceChanged(eRender, eConsole, windows_core::PCWSTR(second.as_ptr()))
                .unwrap();
        }
        assert!(changed.load(Ordering::Acquire));
    }

    #[test]
    fn the_other_callbacks_do_not_trigger_a_switch() {
        // Device add/remove/state/property changes are either irrelevant or
        // already covered by the reactive write-failure path. Reacting here too
        // would double-recover on a single event.
        let (client, changed) = notifier();
        unsafe {
            let id = windows_core::PCWSTR::null();
            client.OnDeviceAdded(id).unwrap();
            client.OnDeviceRemoved(id).unwrap();
            client.OnDeviceStateChanged(id, DEVICE_STATE(1)).unwrap();
            client
                .OnPropertyValueChanged(id, PROPERTYKEY::default())
                .unwrap();
        }
        assert!(!changed.load(Ordering::Acquire));
    }
}
