//! The UI style shared by the WinRust programs: %ProgramData%\WinRust\style.txt holds its name (the topbar gear menu writes it).
//! Programs re-read it at most once a second and repaint at once on the broadcast `set_style` sends (`msg()`).
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering::Relaxed},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, PostMessageW, RegisterWindowMessageW};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Normal,
    Terminal, // monospace, green on black, square corners
    Dock,     // the bar is a floating pill in the middle
    Tiling,   // a thin status strip at the top, square corners, windows side by side
}

pub const ALL: [Style; 4] = [Style::Normal, Style::Terminal, Style::Dock, Style::Tiling];

impl Style {
    pub fn name(self) -> &'static str {
        match self {
            Style::Normal => "normal",
            Style::Terminal => "terminal",
            Style::Dock => "dock",
            Style::Tiling => "tiling",
        }
    }
    /// Unknown or empty text (no file yet, a hand edit) is the normal style.
    pub fn parse(s: &str) -> Style {
        ALL.into_iter().find(|v| v.name().eq_ignore_ascii_case(s.trim())).unwrap_or(Style::Normal)
    }
    /// Square corners everywhere.
    pub fn square(self) -> bool {
        matches!(self, Style::Terminal | Style::Tiling)
    }
    pub fn mono(self) -> bool {
        matches!(self, Style::Terminal | Style::Tiling)
    }
}

static CUR: AtomicU8 = AtomicU8::new(u8::MAX); // index into ALL; MAX = not read yet
static READ_AT: AtomicU64 = AtomicU64::new(0); // when the file was last read, ms since the epoch

fn file() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("WinRust").join("style.txt"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn store(s: Style) {
    CUR.store(ALL.iter().position(|&v| v == s).unwrap_or(0) as u8, Relaxed);
    READ_AT.store(now_ms(), Relaxed);
}

/// Reads the file again now (on the "style changed" broadcast).
pub fn reload() {
    store(Style::parse(&file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default()));
}

/// The current style. The file is read again at most once a second, so a program that missed the broadcast still follows.
pub fn style() -> Style {
    if CUR.load(Relaxed) == u8::MAX || now_ms().saturating_sub(READ_AT.load(Relaxed)) > 1000 {
        reload();
    }
    ALL[CUR.load(Relaxed) as usize % ALL.len()]
}

/// Switches this process, writes the choice and tells the other WinRust windows to repaint in it.
pub fn set_style(s: Style) {
    store(s);
    if let Some(f) = file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(f, s.name());
    }
    unsafe { PostMessageW(HWND_BROADCAST, msg(), 0, 0) };
}

/// The broadcast message a window gets when the style changed: call `reload()` and repaint.
pub fn msg() -> u32 {
    static M: AtomicU32 = AtomicU32::new(0);
    match M.load(Relaxed) {
        0 => {
            let n: Vec<u16> = "WinRustStyleChanged\0".encode_utf16().collect();
            let m = unsafe { RegisterWindowMessageW(n.as_ptr()) };
            M.store(m, Relaxed);
            m
        }
        m => m,
    }
}

/// Colours of one style, as COLORREF (0x00BBGGRR) like GDI wants them.
pub struct Pal {
    pub bg: u32,
    pub line: u32,
    pub hover: u32,
    pub active: u32,
    pub accent: u32,
    pub text: u32,
    pub dim: u32,
}

pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// In the order of `ALL`.
pub const PALS: [Pal; 4] = [
    // normal: Windows 11 blue
    Pal { bg: rgb(24, 30, 44), line: rgb(44, 52, 72), hover: rgb(40, 48, 68), active: rgb(52, 61, 84), accent: rgb(76, 194, 255), text: rgb(255, 255, 255), dim: rgb(150, 158, 175) },
    // terminal: green on black
    Pal { bg: rgb(10, 15, 10), line: rgb(40, 72, 40), hover: rgb(20, 40, 20), active: rgb(28, 56, 28), accent: rgb(91, 215, 91), text: rgb(207, 232, 207), dim: rgb(100, 145, 100) },
    // dock: graphite with violet
    Pal { bg: rgb(27, 29, 34), line: rgb(50, 53, 62), hover: rgb(42, 45, 53), active: rgb(52, 55, 66), accent: rgb(167, 139, 250), text: rgb(255, 255, 255), dim: rgb(154, 160, 170) },
    // tiling: dark with amber
    Pal { bg: rgb(17, 19, 23), line: rgb(40, 44, 52), hover: rgb(30, 34, 41), active: rgb(38, 43, 52), accent: rgb(229, 192, 123), text: rgb(215, 218, 224), dim: rgb(110, 117, 130) },
];

pub fn pal() -> &'static Pal {
    let s = style();
    if s == Style::Normal {
        if let Some(p) = system_pal() {
            return p;
        }
    }
    &PALS[ALL.iter().position(|&v| v == s).unwrap_or(0)]
}

// ---------- "Windows colours": the normal style in the system's light / dark mode and accent colour ----------

static SYS_PAL: std::sync::atomic::AtomicPtr<Pal> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()); // null = not following
static SYS_KEY: AtomicU32 = AtomicU32::new(u32::MAX); // what SYS_PAL was built from (accent | light << 24), MAX = nothing yet
static SYS_AT: AtomicU64 = AtomicU64::new(0); // when the registry was last looked at

fn system_file() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("WinRust").join("system-colors.txt"))
}

/// The normal style follows the Windows light / dark mode and accent colour (the bar's gear menu switches it).
pub fn follow_system() -> bool {
    system_file().is_some_and(|f| f.is_file())
}

pub fn set_follow_system(on: bool) {
    if let Some(f) = system_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = if on { std::fs::write(&f, "1") } else { std::fs::remove_file(&f) };
    }
    SYS_AT.store(0, Relaxed);
    unsafe { PostMessageW(HWND_BROADCAST, msg(), 0, 0) }; // every WinRust program repaints
}

fn reg_dword(key: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (mut v, mut n) = (0u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, w(key).as_ptr(), w(value).as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut v as *mut _ as *mut _, &mut n) };
    (r == 0).then_some(v)
}

fn blend(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| (((a >> s) & 255) as f32 + (((b >> s) & 255) as f32 - ((a >> s) & 255) as f32) * t) as u32;
    ch(0) | ch(8) << 8 | ch(16) << 16
}

/// The palette for a system accent colour (COLORREF) in light or dark mode. On dark the accent is lightened and on
/// light darkened a little, so text and marks in it stay readable on the panel colour.
pub fn system_palette(accent: u32, light: bool) -> Pal {
    if light {
        Pal { bg: rgb(243, 243, 243), line: rgb(216, 216, 216), hover: rgb(228, 228, 228), active: rgb(212, 212, 212), accent: blend(accent, rgb(0, 0, 0), 0.15), text: rgb(26, 26, 26), dim: rgb(96, 96, 96) }
    } else {
        Pal { bg: rgb(28, 28, 28), line: rgb(52, 52, 52), hover: rgb(45, 45, 45), active: rgb(58, 58, 58), accent: blend(accent, rgb(255, 255, 255), 0.35), text: rgb(255, 255, 255), dim: rgb(160, 160, 160) }
    }
}

/// The palette to use instead of the normal one, or None when the setting is off. The registry is looked at once
/// every two seconds at most (pal() is called for every rectangle painted); a palette lives for the rest of the run
/// (a new one is made only when the user changes the Windows colours).
fn system_pal() -> Option<&'static Pal> {
    let now = now_ms();
    if now.saturating_sub(SYS_AT.load(Relaxed)) > 2000 {
        SYS_AT.store(now, Relaxed);
        if !follow_system() {
            SYS_PAL.store(std::ptr::null_mut(), Relaxed);
            SYS_KEY.store(u32::MAX, Relaxed);
        } else {
            // the bar and the other shell parts follow the "Windows mode" (taskbar / Start), as the real taskbar does
            let light = reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "SystemUsesLightTheme").unwrap_or(0) != 0;
            let accent = reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor").map_or(PALS[0].accent, |c| c & 0x00FF_FFFF); // stored as AABBGGRR: the low 24 bits are a COLORREF
            let key = accent | (light as u32) << 24;
            if SYS_KEY.swap(key, Relaxed) != key {
                SYS_PAL.store(Box::leak(Box::new(system_palette(accent, light))), Relaxed);
            }
        }
    }
    let p = SYS_PAL.load(Relaxed);
    (!p.is_null()).then(|| unsafe { &*p })
}

/// The UI font of the current style.
pub fn ui_face() -> &'static str {
    if style().mono() { "Consolas" } else { "Segoe UI" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_palettes_keep_contrast() {
        let lum = |c: u32| (c & 255) + ((c >> 8) & 255) + ((c >> 16) & 255);
        for accent in [rgb(0, 120, 212), rgb(255, 185, 0), rgb(0, 0, 0), rgb(255, 255, 255)] {
            let (d, l) = (system_palette(accent, false), system_palette(accent, true));
            assert!(lum(d.text) > lum(d.bg) + 300 && lum(l.bg) > lum(l.text) + 300); // text against the panel
            assert!(lum(d.accent) >= 255 * 3 * 35 / 100 - 3, "a black accent is still visible on dark");
            assert!(lum(l.accent) <= 255 * 3 * 85 / 100 + 3, "a white accent is still visible on light");
        }
    }

    #[test]
    fn names_round_trip_and_junk_is_normal() {
        for s in ALL {
            assert_eq!(Style::parse(s.name()), s);
        }
        assert_eq!(Style::parse(" Tiling\r\n"), Style::Tiling);
        assert_eq!(Style::parse(""), Style::Normal);
        assert_eq!(Style::parse("neon"), Style::Normal);
    }
}
