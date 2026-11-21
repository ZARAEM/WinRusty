// The two things the bar asks the Windows Runtime for: what is playing (the system media session every player reports
// to: title, artist, playing or paused) and the Bluetooth radio (state, on / off). These calls wait for an answer from
// another process, so they run on short-lived worker threads; the bar's own thread only reads the last result.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicI32, Ordering::Relaxed},
};
use windows::{
    Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState},
    Media::Control::{GlobalSystemMediaTransportControlsSessionManager as Sessions, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status},
    Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
};

static MEDIA: Mutex<Option<(String, String, bool)>> = Mutex::new(None); // (title, artist, playing)
static BT: AtomicI32 = AtomicI32::new(-1); // -1 = no Bluetooth radio (or not asked yet), 0 = off, 1 = on
static BUSY: AtomicBool = AtomicBool::new(false);

/// What is playing right now: (title, artist, playing). None when no app has a media session.
pub fn media() -> Option<(String, String, bool)> {
    MEDIA.lock().unwrap().clone()
}

/// Bluetooth: -1 no radio, 0 off, 1 on.
pub fn bt() -> i32 {
    BT.load(Relaxed)
}

fn read_media() -> windows::core::Result<Option<(String, String, bool)>> {
    let Ok(s) = Sessions::RequestAsync()?.join()?.GetCurrentSession() else { return Ok(None) };
    let p = s.TryGetMediaPropertiesAsync()?.join()?;
    let playing = s.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    Ok(Some((p.Title()?.to_string(), p.Artist()?.to_string(), playing)))
}

fn bt_radio() -> windows::core::Result<Option<Radio>> {
    for r in Radio::GetRadiosAsync()?.join()? {
        if r.Kind()? == RadioKind::Bluetooth {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

fn read_all() {
    *MEDIA.lock().unwrap() = read_media().ok().flatten().filter(|(t, a, _)| !t.is_empty() || !a.is_empty());
    BT.store(bt_radio().ok().flatten().and_then(|r| r.State().ok()).map_or(-1, |s| (s == RadioState::On) as i32), Relaxed);
}

/// Asks again in the background (no-op while the last question is still open). The answer is there a moment later:
/// the panels that show it repaint every second anyway.
pub fn refresh() {
    if BUSY.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        read_all();
        BUSY.store(false, Relaxed);
    });
}

/// Switches the Bluetooth radio on or off, like the tile in Windows' quick settings.
pub fn toggle_bt() {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let _ = (|| -> windows::core::Result<()> {
            let Some(r) = bt_radio()? else { return Ok(()) };
            if Radio::RequestAccessAsync()?.join()? == RadioAccessStatus::Allowed {
                r.SetStateAsync(if r.State()? == RadioState::On { RadioState::Off } else { RadioState::On })?.join()?;
            }
            Ok(())
        })();
        read_all();
    });
}
