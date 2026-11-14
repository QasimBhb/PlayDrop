//! The computer's own output volume on Windows, offered to peers as the reserved player
//! `@system` (protocol.md 5.9). What is common with Linux (the entry, the messages, the
//! worker loop, scalar to percent) is in `system_volume_model.rs`.
//!
//! Backend: Core Audio. The output is the default render endpoint for the multimedia role
//! (`IMMDeviceEnumerator::GetDefaultAudioEndpoint(eRender, eMultimedia)`, the device the
//! volume keys change), read and set through `IAudioEndpointVolume`: the master level as a
//! scalar (what the Windows volume slider shows, times 100) and the mute flag. Setting a
//! level above zero unmutes, as on Linux.
//!
//! Nothing is polled while an output is open. `IAudioEndpointVolumeCallback` tells the
//! worker the level or mute changed, and `IMMNotificationClient` tells it the default
//! device changed, so it opens the new one. With no output device at all the entry is not
//! reported, and the worker looks again on a device notice or every `RETRY_EVERY`.
//!
//! COM lives on the worker's own thread (multithreaded apartment, initialised and
//! uninitialised there), so no runtime task ever waits on Core Audio. The two notice
//! objects are made with `#[implement]`; all they do is send a message to the worker,
//! which is all Core Audio allows from inside a notice anyway.

use std::marker::PhantomData;
use std::sync::mpsc as std_mpsc;

use playdrop_core::host::MediaState;
use tokio::sync::mpsc;
use windows::core::IUnknown;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback};
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IMMDeviceEnumerator, IMMNotificationClient, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use super::system_volume_model::{
    self as model, percent_from_scalar, scalar_from_percent, Msg, Output, Reading, SystemVolume,
};

/// Starts following the default output. Never fails: with no output device there is
/// simply no entry, and the worker keeps looking for one.
pub(super) fn start(out: mpsc::Sender<MediaState>) -> SystemVolume {
    SystemVolume::start(out, worker)
}

fn worker(
    rx: &std_mpsc::Receiver<Msg>,
    tx: &std_mpsc::Sender<Msg>,
    out: &mpsc::Sender<MediaState>,
) {
    let Some(com) = Com::init() else {
        return;
    };
    // Made on the first open that gets that far and kept for the worker's life, so a
    // change of default device is heard also while no output is open.
    let mut devices: Option<Devices> = None;
    model::run(rx, out, || {
        let devices = match &mut devices {
            Some(devices) => devices,
            empty => empty.insert(Devices::new(tx)?),
        };
        Endpoint::open(&devices.enumerator, tx)
    });
    // Every COM object goes before COM is uninitialised.
    drop(devices);
    drop(com);
}

/// COM initialised on this thread, uninitialised on drop. Not `Send`: it belongs to the
/// thread that made it.
struct Com(PhantomData<*const ()>);

impl Com {
    #[allow(unsafe_code)]
    fn init() -> Option<Self> {
        // SAFETY: no pointer is passed (the reserved argument is None). A success, S_OK or
        // S_FALSE, is balanced by the CoUninitialize in Drop on this same thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Some(Self(PhantomData))
        } else {
            tracing::warn!(error = %windows::core::Error::from(hr), "COM could not be started; the computer's volume is not offered");
            None
        }
    }
}

impl Drop for Com {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this thread's CoInitializeEx succeeded (Self exists only then), and every
        // COM object of the thread was dropped before this guard.
        unsafe { CoUninitialize() };
    }
}

/// The device enumerator with our default-device listener registered on it.
struct Devices {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl Devices {
    #[allow(unsafe_code)]
    fn new(tx: &std_mpsc::Sender<Msg>) -> anyhow::Result<Self> {
        // SAFETY: COM is initialised on this thread (worker and test both hold a `Com`);
        // MMDeviceEnumerator is the documented class ID and no outer object is passed.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None::<&IUnknown>, CLSCTX_ALL)? };
        let client: IMMNotificationClient = notices::DeviceNotices { tx: tx.clone() }.into();
        // SAFETY: `client` is a valid IMMNotificationClient; the enumerator keeps its own
        // reference until the Unregister in Drop.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client)? };
        Ok(Self { enumerator, client })
    }
}

impl Drop for Devices {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the same client registered in `new`, on a live enumerator.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
    }
}

/// The default output, opened, with our change listener registered on it.
struct Endpoint {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

impl Endpoint {
    #[allow(unsafe_code)]
    fn open(enumerator: &IMMDeviceEnumerator, tx: &std_mpsc::Sender<Msg>) -> anyhow::Result<Self> {
        // SAFETY: plain COM calls on live interfaces with value arguments; Activate gets
        // no activation parameters.
        let volume: IAudioEndpointVolume = unsafe {
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
            device.Activate(CLSCTX_ALL, None)?
        };
        let callback: IAudioEndpointVolumeCallback =
            notices::VolumeNotices { tx: tx.clone() }.into();
        // SAFETY: `callback` is a valid IAudioEndpointVolumeCallback; the endpoint keeps its
        // own reference until the Unregister in Drop.
        unsafe { volume.RegisterControlChangeNotify(&callback)? };
        Ok(Self { volume, callback })
    }

    #[allow(unsafe_code)]
    fn muted(&self) -> anyhow::Result<bool> {
        // SAFETY: a plain COM call on a live interface.
        Ok(unsafe { self.volume.GetMute()? }.as_bool())
    }

    /// The raw master level, for a test to put back exactly what it found.
    #[cfg(test)]
    #[allow(unsafe_code)]
    fn scalar(&self) -> anyhow::Result<f32> {
        // SAFETY: a plain COM call on a live interface.
        Ok(unsafe { self.volume.GetMasterVolumeLevelScalar()? })
    }

    #[cfg(test)]
    #[allow(unsafe_code)]
    fn set_scalar(&self, scalar: f32) -> anyhow::Result<()> {
        // SAFETY: a plain COM call on a live interface; a null event context is allowed.
        unsafe {
            self.volume
                .SetMasterVolumeLevelScalar(scalar, std::ptr::null())?;
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the same callback registered in `open`, on a live endpoint.
        let _ = unsafe { self.volume.UnregisterControlChangeNotify(&self.callback) };
    }
}

impl Output for Endpoint {
    #[allow(unsafe_code)]
    fn read(&mut self) -> anyhow::Result<Reading> {
        // SAFETY: a plain COM call on a live interface.
        let scalar = unsafe { self.volume.GetMasterVolumeLevelScalar()? };
        Ok(Reading {
            level: percent_from_scalar(scalar),
            muted: self.muted()?,
        })
    }

    #[allow(unsafe_code)]
    fn set_level(&mut self, level: u8) -> anyhow::Result<()> {
        // SAFETY: plain COM calls on a live interface; a null event context is allowed and
        // means "no context".
        unsafe {
            self.volume
                .SetMasterVolumeLevelScalar(scalar_from_percent(level), std::ptr::null())?;
        }
        if level > 0 && self.muted()? {
            self.set_muted(false)?;
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn set_muted(&mut self, muted: bool) -> anyhow::Result<()> {
        // SAFETY: as in `set_level`.
        unsafe { self.volume.SetMute(muted, std::ptr::null())? };
        Ok(())
    }
}

/// The two notice objects. `#[implement]` writes their COM plumbing (reference counting,
/// `QueryInterface`, the method tables); the allows below cover only that generated code,
/// which uses `unsafe`, `#[inline(always)]` and reference to pointer casts. The code
/// written here is safe Rust.
mod notices {
    #![allow(unsafe_code, clippy::inline_always, clippy::ref_as_ptr)]

    use std::sync::mpsc as std_mpsc;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::Endpoints::{
        IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
    };
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, EDataFlow, ERole, IMMNotificationClient, IMMNotificationClient_Impl,
        AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
    };
    use windows_core::implement;

    use super::Msg;

    /// Turns each level or mute notice of an endpoint into [`Msg::Changed`]. The worker reads
    /// the level and mute itself, as it does after its own changes.
    #[implement(IAudioEndpointVolumeCallback)]
    pub(super) struct VolumeNotices {
        pub(super) tx: std_mpsc::Sender<Msg>,
    }

    impl IAudioEndpointVolumeCallback_Impl for VolumeNotices_Impl {
        fn OnNotify(
            &self,
            _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA,
        ) -> windows::core::Result<()> {
            // A worker that has ended no longer cares; the notice still succeeded.
            let _ = self.tx.send(Msg::Changed);
            Ok(())
        }
    }

    /// Turns a change of default output, or of any device's state, into
    /// [`Msg::DeviceChanged`].
    #[implement(IMMNotificationClient)]
    pub(super) struct DeviceNotices {
        pub(super) tx: std_mpsc::Sender<Msg>,
    }

    impl IMMNotificationClient_Impl for DeviceNotices_Impl {
        fn OnDeviceStateChanged(
            &self,
            _device: &PCWSTR,
            _state: DEVICE_STATE,
        ) -> windows::core::Result<()> {
            // A device was plugged in, unplugged, enabled or disabled: the default output may
            // have come back or gone away. Opening again is cheap and reports only a difference.
            let _ = self.tx.send(Msg::DeviceChanged);
            Ok(())
        }

        fn OnDeviceAdded(&self, _device: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDeviceRemoved(&self, _device: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            role: ERole,
            _device: &PCWSTR,
        ) -> windows::core::Result<()> {
            // Windows sends one notice per role; only the one the worker opens matters.
            if flow == eRender && role == eMultimedia {
                let _ = self.tx.send(Msg::DeviceChanged);
            }
            Ok(())
        }

        fn OnPropertyValueChanged(
            &self,
            _device: &PCWSTR,
            _key: &PROPERTYKEY,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::system_volume_model::SYSTEM_PLAYER;
    use playdrop_core::session::types::MediaAction;
    use std::time::{Duration, Instant};

    /// Every wait in these tests ends after this long.
    const WAIT: Duration = Duration::from_secs(5);

    /// The next state within `limit`; `None` on a timeout or a closed channel.
    fn recv_within(rx: &mut mpsc::Receiver<MediaState>, limit: Duration) -> Option<MediaState> {
        let deadline = Instant::now() + limit;
        loop {
            match rx.try_recv() {
                Ok(s) => return Some(s),
                Err(mpsc::error::TryRecvError::Disconnected) => return None,
                Err(mpsc::error::TryRecvError::Empty) if Instant::now() >= deadline => return None,
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    /// States until the last one reported (`shown`) is `want`, within [`WAIT`]. Returns at
    /// once when it already is: the worker sends only differences.
    fn wait_for(rx: &mut mpsc::Receiver<MediaState>, shown: &mut Option<Reading>, want: Reading) {
        let deadline = Instant::now() + WAIT;
        while *shown != Some(want) {
            let Some(s) = recv_within(rx, deadline.saturating_duration_since(Instant::now()))
            else {
                panic!("no state with {want:?} within {WAIT:?}, last {shown:?}");
            };
            if let (Some(level), Some(muted)) = (s.volume, s.muted) {
                *shown = Some(Reading { level, muted });
            }
        }
    }

    /// Waits for the worker to end and drop its sender, within twice [`WAIT`].
    fn wait_closed(rx: &mut mpsc::Receiver<MediaState>) {
        let deadline = Instant::now() + WAIT * 2;
        loop {
            match rx.try_recv() {
                Err(mpsc::error::TryRecvError::Disconnected) => return,
                _ if Instant::now() >= deadline => panic!("the worker did not end"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// The default output, if this machine has one, with a channel its notices go to.
    fn open_default() -> Option<(Devices, Endpoint)> {
        let (tx, _rx) = std_mpsc::channel();
        match Devices::new(&tx)
            .and_then(|devices| Endpoint::open(&devices.enumerator, &tx).map(|e| (devices, e)))
        {
            Ok(found) => Some(found),
            Err(e) => {
                println!("no default output device on this machine ({e})");
                None
            }
        }
    }

    /// Runs on every Windows test machine and never changes the sound. With a default
    /// output it reads the level and mute, and the worker must report exactly that; without
    /// one (GitHub's runners often have no sound device) the worker must report nothing.
    /// Either way it must end when stopped.
    #[test]
    fn computer_volume_is_reported_as_it_is() {
        let com = Com::init().expect("COM starts on a test thread");
        let found = open_default().map(|(devices, mut endpoint)| {
            let reading = endpoint.read().expect("read");
            println!("default output found: {reading:?}");
            drop(endpoint);
            drop(devices);
            reading
        });
        let (out, mut states) = mpsc::channel(8);
        let volume = start(out);
        let first = recv_within(&mut states, WAIT);
        match found {
            Some(reading) => {
                let first = first.expect("a state for the default output");
                assert_eq!(first.player, SYSTEM_PLAYER);
                assert_eq!(first.can, vec![MediaAction::Volume, MediaAction::Mute]);
                assert_eq!(
                    (first.volume, first.muted),
                    (Some(reading.level), Some(reading.muted))
                );
            }
            None => assert!(first.is_none(), "nothing is reported without an output"),
        }
        drop(volume);
        wait_closed(&mut states);
        drop(com);
    }

    /// Puts the exact raw level and the mute found at the start back, also when an
    /// assertion failed.
    struct Restore {
        endpoint: Endpoint,
        scalar: f32,
        muted: bool,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = self.endpoint.set_scalar(self.scalar);
            let _ = self.endpoint.set_muted(self.muted);
        }
    }

    /// Changes the computer's volume for a moment and puts it back: moves the level by
    /// about one point, toggles mute, and checks the worker hears both changes made outside
    /// it. Runs only with `PLAYDROP_AUDIO_TEST=1` (set in the Windows CI workflow) and only
    /// when a default output exists. A device with coarse steps may not land on the exact
    /// level asked for, so the test works with the level the device reports back.
    #[test]
    fn computer_volume_changes_when_allowed() {
        if std::env::var("PLAYDROP_AUDIO_TEST").as_deref() != Ok("1") {
            println!("PLAYDROP_AUDIO_TEST is not 1: the volume is left alone");
            return;
        }
        let com = Com::init().expect("COM starts on a test thread");
        let Some((devices, mut endpoint)) = open_default() else {
            drop(com);
            return;
        };
        let before = endpoint.read().expect("read");
        let scalar = endpoint.scalar().expect("raw level");
        println!("default output found: {before:?}");
        let target = if before.level >= 50 {
            before.level - 1
        } else {
            before.level + 1
        };
        endpoint.set_level(target).expect("set level");
        let moved = endpoint.read().expect("read");
        assert!(
            moved.level.abs_diff(target) <= 5,
            "asked for {target}, the device reports {}",
            moved.level
        );
        assert!(!moved.muted, "a level above zero unmutes");
        endpoint.set_muted(true).expect("mute");
        let muted = endpoint.read().expect("read");
        assert!(muted.muted);
        assert_eq!(muted.level, moved.level, "the level stays while muted");
        endpoint.set_muted(false).expect("unmute");
        endpoint.set_scalar(scalar).expect("restore level");
        endpoint.set_muted(before.muted).expect("restore mute");
        assert_eq!(endpoint.read().expect("read"), before);
        let mut guard = Restore {
            endpoint,
            scalar,
            muted: before.muted,
        };

        // The worker reports the output, then changes made outside it (the notices).
        let (out, mut states) = mpsc::channel(8);
        let volume = start(out);
        let mut shown = None;
        wait_for(&mut states, &mut shown, before);
        guard.endpoint.set_level(target).expect("set level");
        let moved = guard.endpoint.read().expect("read");
        wait_for(&mut states, &mut shown, moved);
        guard.endpoint.set_muted(true).expect("mute");
        wait_for(
            &mut states,
            &mut shown,
            Reading {
                level: moved.level,
                muted: true,
            },
        );
        guard.endpoint.set_scalar(scalar).expect("restore level");
        guard
            .endpoint
            .set_muted(before.muted)
            .expect("restore mute");
        wait_for(&mut states, &mut shown, before);
        drop(volume);
        wait_closed(&mut states);
        drop(guard);
        drop(devices);
        drop(com);
    }
}
