// Per-app volume, the way EarTrumpet does it: the audio sessions of the default output device grouped by app, each with
// its own level, mute and live peak meter, plus the list of output devices to switch the default one.
use super::*;
use windows::Win32::{
    Foundation::PROPERTYKEY,
    Media::Audio::{eCapture, AudioSessionStateExpired, DEVICE_STATE_ACTIVE, Endpoints::IAudioMeterInformation, IAudioSessionControl2, IAudioSessionManager2, IMMDevice, ISimpleAudioVolume},
    System::Com::{CoTaskMemFree, STGM_READ},
};

/// One app that plays (or recently played) sound: all its audio sessions move together.
pub struct App {
    pub key: String, // lower-case exe path; "" = the system sounds
    pub name: String,
    pub icon: HICON, // owned by EXE_ICONS; null = draw the speaker glyph
    pub vol: i32,
    pub muted: bool,
    pub peak: f32, // 0..1, quick up, slow down
    sess: Vec<(ISimpleAudioVolume, Option<IAudioMeterInformation>)>,
}

/// One active output device.
pub struct Dev {
    pub id: Vec<u16>, // endpoint id, NUL-terminated
    pub name: String,
    pub default: bool,
}

thread_local! {
    pub static APPS: RefCell<Vec<App>> = const { RefCell::new(Vec::new()) };
    pub static DEVS: RefCell<Vec<Dev>> = const { RefCell::new(Vec::new()) };
    pub static MASTER_PEAK: Cell<f32> = const { Cell::new(0.0) };
    pub static INS: RefCell<Vec<Dev>> = const { RefCell::new(Vec::new()) }; // active input devices (microphones)
    pub static MIC_VOL: Cell<(i32, bool)> = const { Cell::new((-1, false)) }; // default microphone: (percent or -1 = none, muted)
    static MIC: RefCell<Option<IAudioEndpointVolume>> = const { RefCell::new(None) };
    static MASTER: RefCell<Option<IAudioMeterInformation>> = const { RefCell::new(None) };
    static NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new()); // exe path -> display name
}

/// Name of the default output device ("" when there is none).
pub fn device_name() -> String {
    DEVS.with_borrow(|d| d.iter().find(|d| d.default).map(|d| d.name.clone()).unwrap_or_default())
}

unsafe fn dev_id(d: &IMMDevice) -> Vec<u16> {
    let Ok(p) = d.GetId() else { return vec![0] };
    let id = p.as_wide().iter().copied().chain([0]).collect();
    CoTaskMemFree(Some(p.0 as *const _));
    id
}

unsafe fn dev_name(d: &IMMDevice) -> String {
    // PKEY_Device_FriendlyName: "Speakers (Realtek Audio)"
    const NAME: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
    d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&NAME)).map(|v| v.to_string()).unwrap_or_default()
}

unsafe fn pid_exe(pid: u32) -> String {
    let p = OpenProcess(0x1000, 0, pid); // PROCESS_QUERY_LIMITED_INFORMATION
    if p.is_null() {
        return String::new();
    }
    let (mut b, mut n) = ([0u16; 520], 520u32);
    let ok = QueryFullProcessImageNameW(p, 0, b.as_mut_ptr(), &mut n);
    CloseHandle(p);
    if ok == 0 { String::new() } else { String::from_utf16_lossy(&b[..n as usize]).to_lowercase() }
}

/// "FileDescription" from the exe's version resource ("Mozilla Firefox"), None when it has none.
unsafe fn exe_desc(path: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let p = wide(path);
    let n = GetFileVersionInfoSizeW(p.as_ptr(), null_mut());
    if n == 0 {
        return None;
    }
    let mut buf = vec![0u8; n as usize];
    if GetFileVersionInfoW(p.as_ptr(), 0, n, buf.as_mut_ptr() as *mut _) == 0 {
        return None;
    }
    let (mut tr, mut len): (*mut u16, u32) = (null_mut(), 0);
    if VerQueryValueW(buf.as_ptr() as *const _, wide(r"\VarFileInfo\Translation").as_ptr(), &mut tr as *mut _ as *mut _, &mut len) == 0 || len < 4 || tr.is_null() {
        return None;
    }
    let (lang, cp) = (tr.read_unaligned(), tr.add(1).read_unaligned());
    let (mut s, mut sl): (*mut u16, u32) = (null_mut(), 0);
    let q = wide(&format!(r"\StringFileInfo\{lang:04x}{cp:04x}\FileDescription"));
    if VerQueryValueW(buf.as_ptr() as *const _, q.as_ptr(), &mut s as *mut _ as *mut _, &mut sl) == 0 || sl == 0 || s.is_null() {
        return None;
    }
    let w: Vec<u16> = (0..sl as usize).map(|i| s.add(i).read_unaligned()).take_while(|&c| c != 0).collect();
    let d = String::from_utf16_lossy(&w).trim().to_string();
    (!d.is_empty()).then_some(d)
}

/// What the mixer calls the app at `exe`: its file description, else the file name with a capital letter.
unsafe fn app_name(exe: &str) -> String {
    if let Some(n) = NAMES.with_borrow(|m| m.get(exe).cloned()) {
        return n;
    }
    let n = exe_desc(exe).unwrap_or_else(|| stem_name(exe));
    NAMES.with_borrow_mut(|m| m.insert(exe.to_string(), n.clone()));
    n
}

/// r"c:\apps\vlc.exe" -> "Vlc".
pub fn stem_name(exe: &str) -> String {
    let file = exe.rsplit('\\').next().unwrap_or(exe);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    stem.chars().take(1).flat_map(char::to_uppercase).chain(stem.chars().skip(1)).collect()
}

/// Reads the devices and the default device's sessions again. Peaks carry over, so the meters do not blink.
pub unsafe fn read() {
    let r = (|| -> windows::core::Result<(Vec<App>, Vec<Dev>, Option<IAudioMeterInformation>)> {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let def = dev_id(&dev);
        let mut devs = Vec::new();
        if let Ok(col) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
            for i in 0..col.GetCount().unwrap_or(0) {
                if let Ok(d) = col.Item(i) {
                    let id = dev_id(&d);
                    devs.push(Dev { name: dev_name(&d), default: id == def, id });
                }
            }
        }
        let mut apps: Vec<App> = Vec::new();
        let list = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)?.GetSessionEnumerator()?;
        for i in 0..list.GetCount()? {
            let Ok(c) = list.GetSession(i) else { continue };
            if c.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                continue; // its app is gone
            }
            let (Ok(c2), Ok(sv)) = (c.cast::<IAudioSessionControl2>(), c.cast::<ISimpleAudioVolume>()) else { continue };
            let system = c2.IsSystemSoundsSession().0 == 0; // S_OK; S_FALSE for an app's session
            let pid = c2.GetProcessId().unwrap_or(0);
            let key = if system { String::new() } else { pid_exe(pid) };
            if !system && key.is_empty() {
                continue; // a process we may not even look at (protected): nothing to name or draw
            }
            let (vol, muted) = ((sv.GetMasterVolume().unwrap_or(1.0) * 100.0).round() as i32, sv.GetMute().is_ok_and(|m| m.as_bool()));
            let meter = c.cast::<IAudioMeterInformation>().ok();
            match apps.iter_mut().find(|a| a.key == key) {
                Some(a) => {
                    a.muted |= muted;
                    a.sess.push((sv, meter));
                }
                None => {
                    let (name, icon) = if system {
                        (t!("Системные звуки", "System sounds").to_string(), null_mut())
                    } else {
                        (app_name(&key), EXE_ICONS.with_borrow_mut(|m| *m.entry(key.clone()).or_insert_with(|| item_icon(&key, icon_px()))))
                    };
                    apps.push(App { key, name, icon, vol, muted, peak: 0.0, sess: vec![(sv, meter)] });
                }
            }
        }
        // the system sounds first, then by name: rows must not jump around while a slider is held
        apps.sort_by(|a, b| (!a.key.is_empty(), a.name.to_lowercase()).cmp(&(!b.key.is_empty(), b.name.to_lowercase())));
        Ok((apps, devs, dev.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None).ok()))
    })();
    read_mic();
    let (mut apps, devs, master) = r.unwrap_or_default();
    APPS.with_borrow(|old| apps.iter_mut().for_each(|a| a.peak = old.iter().find(|o| o.key == a.key).map_or(0.0, |o| o.peak)));
    APPS.set(apps);
    DEVS.set(devs);
    MASTER.set(master);
}

/// The input side: the active microphones and the default one's level. On its own, so a PC without speakers still gets it.
unsafe fn read_mic() {
    let r = (|| -> windows::core::Result<(Vec<Dev>, IAudioEndpointVolume)> {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eCapture, eConsole)?;
        let def = dev_id(&dev);
        let mut ins = Vec::new();
        if let Ok(col) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
            for i in 0..col.GetCount().unwrap_or(0) {
                if let Ok(d) = col.Item(i) {
                    let id = dev_id(&d);
                    ins.push(Dev { name: dev_name(&d), default: id == def, id });
                }
            }
        }
        Ok((ins, dev.Activate(CLSCTX_ALL, None)?))
    })();
    match r {
        Ok((ins, ep)) => {
            MIC_VOL.set(((ep.GetMasterVolumeLevelScalar().unwrap_or(0.0) * 100.0).round() as i32, ep.GetMute().is_ok_and(|m| m.as_bool())));
            INS.set(ins);
            MIC.set(Some(ep));
        }
        Err(_) => {
            MIC_VOL.set((-1, false));
            INS.set(Vec::new());
            MIC.set(None);
        }
    }
}

/// Name of the default microphone ("" when there is none).
pub fn mic_name() -> String {
    INS.with_borrow(|d| d.iter().find(|d| d.default).map(|d| d.name.clone()).unwrap_or_default())
}

/// Sets the default microphone's level (0..100) and unmutes it.
pub unsafe fn set_mic(v: i32) {
    let v = v.clamp(0, 100);
    MIC.with_borrow(|m| {
        if let Some(e) = m {
            let _ = e.SetMasterVolumeLevelScalar(v as f32 / 100.0, null());
            let _ = e.SetMute(false, null());
            MIC_VOL.set((v, false));
        }
    });
}

pub unsafe fn mute_mic() {
    MIC.with_borrow(|m| {
        if let Some(e) = m {
            let (v, muted) = MIC_VOL.get();
            let _ = e.SetMute(!muted, null());
            MIC_VOL.set((v, !muted));
        }
    });
}

/// Forgets the sessions (the mixer closed): no COM objects are held while nobody looks.
pub fn drop_all() {
    MIC.set(None);
    APPS.set(Vec::new());
    MASTER.set(None);
    MASTER_PEAK.set(0.0);
}

fn follow(old: f32, new: f32) -> f32 {
    if new >= old { new } else { old * 0.82 + new * 0.18 }
}

/// One meter frame: the loudest session of each app, and the device's own peak.
pub unsafe fn tick() {
    APPS.with_borrow_mut(|apps| {
        for a in apps.iter_mut() {
            let p = a.sess.iter().filter_map(|(_, m)| m.as_ref()?.GetPeakValue().ok()).fold(0.0f32, f32::max);
            a.peak = follow(a.peak, if a.muted { 0.0 } else { p });
        }
    });
    let p = MASTER.with_borrow(|m| m.as_ref().and_then(|m| m.GetPeakValue().ok())).unwrap_or(0.0);
    MASTER_PEAK.set(follow(MASTER_PEAK.get(), if VOL.get().1 { 0.0 } else { p }));
}

/// Sets the level of app `i` (0..100) on all its sessions and unmutes it.
pub unsafe fn set_app(i: usize, v: i32) {
    let v = v.clamp(0, 100);
    APPS.with_borrow_mut(|apps| {
        if let Some(a) = apps.get_mut(i) {
            for (s, _) in &a.sess {
                let _ = s.SetMasterVolume(v as f32 / 100.0, null());
                let _ = s.SetMute(false, null());
            }
            (a.vol, a.muted) = (v, false);
        }
    });
}

pub unsafe fn add_app(i: usize, delta: i32) {
    if let Some(v) = APPS.with_borrow(|a| a.get(i).map(|a| a.vol)) {
        set_app(i, v + delta);
    }
}

pub unsafe fn mute_app(i: usize) {
    APPS.with_borrow_mut(|apps| {
        if let Some(a) = apps.get_mut(i) {
            a.muted = !a.muted;
            for (s, _) in &a.sess {
                let _ = s.SetMute(a.muted, null());
            }
        }
    });
}

/// Makes device `i` the default output for every role (console, multimedia, communications), like picking it in
/// EarTrumpet or in the Windows sound flyout. Windows has no documented call for this: it goes through IPolicyConfig
/// (the interface the Sound control panel itself uses), called by vtable slot because no header declares it.
pub unsafe fn set_default(i: usize) {
    if let Some(id) = DEVS.with_borrow(|d| d.get(i).map(|d| d.id.clone())) {
        set_default_id(&id);
    }
}

/// The same for input device `i`: the default microphone.
pub unsafe fn set_default_in(i: usize) {
    if let Some(id) = INS.with_borrow(|d| d.get(i).map(|d| d.id.clone())) {
        set_default_id(&id);
    }
}

unsafe fn set_default_id(id: &[u16]) {
    use std::ffi::c_void;
    const CLSID_POLICY_CONFIG: windows::core::GUID = windows::core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
    const IID_POLICY_CONFIG: windows::core::GUID = windows::core::GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
    let Ok(unk) = CoCreateInstance::<_, windows::core::IUnknown>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) else { return };
    let mut p: *mut c_void = null_mut();
    if unk.query(&IID_POLICY_CONFIG, &mut p).is_err() || p.is_null() {
        return;
    }
    // IUnknown (3 slots), then GetMixFormat, GetDeviceFormat, ResetDeviceFormat, SetDeviceFormat, GetProcessingPeriod,
    // SetProcessingPeriod, GetShareMode, SetShareMode, GetPropertyValue, SetPropertyValue, SetDefaultEndpoint (slot 13)
    let vt = *(p as *const *const *const c_void);
    let set: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32 = std::mem::transmute(*vt.add(13));
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vt.add(2));
    for role in 0..3 {
        set(p, id.as_ptr(), role);
    }
    release(p);
    EP.set(None); // the bar's master volume now belongs to the new device
    read_volume();
    read();
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_and_meter() {
        assert_eq!(super::stem_name(r"c:\apps\vlc.exe"), "Vlc");
        assert_eq!(super::stem_name("spotify"), "Spotify");
        assert_eq!(super::follow(0.2, 0.9), 0.9); // jumps up with the sound
        let d = super::follow(1.0, 0.0);
        assert!(d < 1.0 && d > 0.5); // and sinks back gradually
    }
}
