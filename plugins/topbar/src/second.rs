// A bar on every other monitor, like "Show my taskbar on all displays": a strip with the buttons of the windows that
// are on that monitor, and the clock. The full bar (Start, pins, tray, panels) stays on the primary monitor, as the
// system tray does in Windows. Each window has its own button here (no grouping, no previews, no dragging): a
// deliberately small second surface, not a second copy of the bar.
use super::*;

struct Bar2 {
    wnd: HWND,
    hmon: HMONITOR,
    mon: RECT, // the whole monitor
    appbar: bool, // registered with Explorer as an appbar (else we set the monitor's work area ourselves)
    last: (i32, i32, i32, i32), // the strip rectangle last given to Explorer: setting it again would make Explorer notify every appbar, ours included
}

thread_local! {
    static BARS: RefCell<Vec<Bar2>> = const { RefCell::new(Vec::new()) };
    static HOT2: Cell<(isize, i32)> = const { Cell::new((0, -1)) }; // (strip window, button under the pointer)
}

pub unsafe fn init(hi: HINSTANCE) {
    let c = wide("topbar_second");
    RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hi, hCursor: LoadCursorW(null_mut(), IDC_ARROW), lpszClassName: c.as_ptr(), ..zeroed() });
}

unsafe extern "system" fn each_monitor(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> i32 {
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(m, &mut mi) != 0 && mi.dwFlags & 1 == 0 {
        (*(lp as *mut Vec<(HMONITOR, RECT)>)).push((m, mi.rcMonitor)); // not the primary one
    }
    1
}

/// The strip's rectangle on monitor `m`, and the monitor's work area with the strip taken out.
fn strip(m: &RECT, h: i32, top: bool) -> (RECT, RECT) {
    if top {
        (RECT { bottom: m.top + h, ..*m }, RECT { top: m.top + h, ..*m })
    } else {
        (RECT { top: m.bottom - h, ..*m }, RECT { bottom: m.bottom - h, ..*m })
    }
}

/// Reserves the strip (or, with `on` false, gives it back): through Explorer's appbar list when Explorer runs the
/// desktop, else by setting that monitor's work area directly. Only when it differs: the change is broadcast.
unsafe fn reserve(b: &mut Bar2, on: bool) {
    let (rc, work) = strip(&b.mon, bar_h(), at_top());
    let mut ab: APPBARDATA = zeroed();
    ab.cbSize = size_of::<APPBARDATA>() as u32;
    ab.hWnd = b.wnd;
    if !find_other("Shell_TrayWnd").is_null() {
        if on {
            if !b.appbar {
                ab.uCallbackMessage = WM_APP + 2;
                SHAppBarMessage(ABM_NEW, &mut ab);
                b.appbar = true;
            }
            if b.last != (rc.left, rc.top, rc.right, rc.bottom) {
                b.last = (rc.left, rc.top, rc.right, rc.bottom);
                ab.uEdge = if at_top() { ABE_TOP } else { ABE_BOTTOM };
                ab.rc = rc;
                SHAppBarMessage(ABM_SETPOS, &mut ab);
            }
        } else if b.appbar {
            SHAppBarMessage(ABM_REMOVE, &mut ab);
            b.appbar = false;
            b.last = (0, 0, 0, 0);
        }
        return;
    }
    let mut want = if on { work } else { b.mon };
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(b.hmon, &mut mi);
    let cur = mi.rcWork;
    if (cur.left, cur.top, cur.right, cur.bottom) != (want.left, want.top, want.right, want.bottom) {
        SystemParametersInfoW(SPI_SETWORKAREA, 0, &mut want as *mut _ as *mut _, SPIF_SENDCHANGE); // applies to the monitor the rectangle is on
    }
}

/// Makes the strips match the monitors and the settings: one per non-primary monitor, none with "main display only",
/// in the dock style or with auto-hide (those bars reserve no space and slide; the strips do neither).
pub unsafe fn sync() {
    let mut mons: Vec<(HMONITOR, RECT)> = Vec::new();
    if !cfg("onemon") && !cfg("autohide") && style() != Style::Dock {
        EnumDisplayMonitors(null_mut(), null(), Some(each_monitor), &mut mons as *mut _ as LPARAM);
    }
    let mut bars = BARS.take();
    bars.retain_mut(|b| {
        let keep = mons.iter().any(|(m, _)| *m == b.hmon);
        if !keep {
            reserve(b, false);
            DestroyWindow(b.wnd);
        }
        keep
    });
    let (c, hi) = (wide("topbar_second"), GetModuleHandleW(null()));
    for (hmon, mon) in mons {
        if !bars.iter().any(|b| b.hmon == hmon) {
            let wnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, c.as_ptr(), null(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), hi, null());
            bars.push(Bar2 { wnd, hmon, mon, appbar: false, last: (0, 0, 0, 0) });
        }
        let b = bars.iter_mut().find(|b| b.hmon == hmon).unwrap();
        b.mon = mon;
        reserve(b, true);
        let (rc, _) = strip(&mon, bar_h(), at_top());
        SetWindowPos(b.wnd, HWND_TOPMOST, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        InvalidateRect(b.wnd, null(), 0);
    }
    BARS.set(bars);
}

/// With the main bar's refresh (twice a second): repaint, and step aside for a fullscreen window on that monitor.
pub unsafe fn refresh() {
    let fg = GetForegroundWindow();
    let (mut fr, mut pid): (RECT, u32) = (zeroed(), 0);
    GetWindowRect(fg, &mut fr);
    GetWindowThreadProcessId(fg, &mut pid);
    let app = !fg.is_null() && fg != GetShellWindow() && pid != std::process::id();
    BARS.with_borrow(|bars| {
        for b in bars {
            let covered = app && fr.left <= b.mon.left && fr.top <= b.mon.top && fr.right >= b.mon.right && fr.bottom >= b.mon.bottom;
            ShowWindow(b.wnd, if covered { SW_HIDE } else { SW_SHOWNOACTIVATE });
            InvalidateRect(b.wnd, null(), 0);
        }
    });
}

/// The bar is closing: the strips go and their monitors get the reserved space back.
pub unsafe fn close() {
    for mut b in BARS.take() {
        reserve(&mut b, false);
        DestroyWindow(b.wnd);
    }
}

/// The app windows on the monitor of strip `w`, in the main bar's button order. A minimized window counts for the
/// monitor it will come back to.
unsafe fn wins_on(w: HWND) -> Vec<HWND> {
    let Some(hmon) = BARS.with_borrow(|b| b.iter().find(|b| b.wnd == w).map(|b| b.hmon)) else { return vec![] };
    let all: Vec<HWND> = BTNS.with_borrow(|b| b.iter().flat_map(|b| b.wins.iter().copied()).collect());
    all.into_iter()
        .filter(|&x| {
            let mut wp: WINDOWPLACEMENT = zeroed();
            wp.length = size_of::<WINDOWPLACEMENT>() as u32;
            let m = if IsIconic(x) != 0 && GetWindowPlacement(x, &mut wp) != 0 { MonitorFromRect(&wp.rcNormalPosition, MONITOR_DEFAULTTONEAREST) } else { MonitorFromWindow(x, MONITOR_DEFAULTTONEAREST) };
            m == hmon
        })
        .collect()
}

/// Button index at client x, -1 for none.
fn hit2(x: i32, n: usize) -> i32 {
    let (x0, pitch) = (sc(8), sc(44));
    let i = (x - x0).div_euclid(pitch);
    if x < x0 || (x - x0) % pitch >= sc(40) || i >= n as i32 { -1 } else { i }
}

unsafe fn paint(w: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let wdc = BeginPaint(w, &mut ps);
    let mut cr: RECT = zeroed();
    GetClientRect(w, &mut cr);
    let (dc, bmp) = (CreateCompatibleDC(wdc), CreateCompatibleBitmap(wdc, cr.right, cr.bottom));
    let old = SelectObject(dc, bmp);
    fill(dc, cr, pal().bg, 0);
    let edge = if at_top() { cr.bottom - 1 } else { 0 };
    fill(dc, RECT { left: 0, top: edge, right: cr.right, bottom: edge + 1 }, pal().line, 0);
    SetBkMode(dc, TRANSPARENT as i32);
    let (top, bot) = (sc(4), cr.bottom - sc(4));
    let mid = (top + bot) / 2;
    let fg = GetForegroundWindow();
    let hot = if HOT2.get().0 == w as isize { HOT2.get().1 } else { -1 };
    for (i, &x) in wins_on(w).iter().enumerate() {
        let l = sc(8) + i as i32 * sc(44);
        if l + sc(40) > cr.right - sc(100) {
            break; // shortcut: no overflow menu here; the main bar has every window
        }
        let r = RECT { left: l, top, right: l + sc(40), bottom: bot };
        if x == fg || hot == i as i32 {
            fill(dc, r, if x == fg { mix(pal().active, pal().accent, 0.12) } else { pal().hover }, sc(8));
        }
        let ic = win_icon(x);
        if !ic.is_null() {
            DrawIconEx(dc, l + (sc(40) - icon_px()) / 2, mid - icon_px() / 2 - sc(2), ic, icon_px(), icon_px(), 0, null_mut(), DI_NORMAL);
        }
        let pw = if x == fg { sc(18) } else { sc(6) };
        let col = if x == fg { pal().accent } else if IsIconic(x) != 0 { mix(pal().dim, pal().bg, 0.45) } else { pal().dim };
        fill(dc, RECT { left: l + (sc(40) - pw) / 2, top: bot - sc(4), right: l + (sc(40) + pw) / 2, bottom: bot - sc(1) }, col, sc(3));
    }
    // the clock, as on the main bar
    let face = wide(ui_face());
    let font = |px: i32, wt: i32| CreateFontW(-sc(px), 0, 0, 0, wt, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
    let (f_bold, f_small) = (font(13, 600), font(12, 400));
    let (tm, dt) = clock_texts();
    let cb = RECT { left: cr.right - sc(96), top, right: cr.right - sc(6), bottom: bot };
    let of = SelectObject(dc, f_bold);
    text(dc, &tm, RECT { bottom: mid + sc(1), ..cb }, pal().text, DT_CENTER);
    SelectObject(dc, f_small);
    text(dc, &dt, RECT { top: mid - sc(1), ..cb }, mix(pal().dim, pal().text, 0.35), DT_CENTER);
    SelectObject(dc, of);
    DeleteObject(f_bold);
    DeleteObject(f_small);
    BitBlt(wdc, 0, 0, cr.right, cr.bottom, dc, 0, 0, SRCCOPY);
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
    EndPaint(w, &ps);
}

unsafe extern "system" fn proc(w: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let x = (lp & 0xffff) as i16 as i32;
    match msg {
        WM_PAINT => paint(w),
        WM_ERASEBKGND => return 1,
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: w, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
            let h = (w as isize, hit2(x, wins_on(w).len()));
            if HOT2.replace(h) != h {
                InvalidateRect(w, null(), 0);
            }
        }
        WM_MOUSELEAVE => {
            HOT2.set((0, -1));
            InvalidateRect(w, null(), 0);
        }
        WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP => {
            let wins = wins_on(w);
            let Some(&win) = wins.get(hit2(x, wins.len()).max(0) as usize).filter(|_| hit2(x, wins.len()) >= 0) else { return 0 };
            match msg {
                WM_MBUTTONUP => close_window(win),
                WM_RBUTTONUP => sys_menu(w, win),
                // like the taskbar: the window in front minimizes, any other comes forward
                _ if win == GetForegroundWindow() && IsIconic(win) == 0 => drop(ShowWindow(win, SW_MINIMIZE) != 0),
                _ => {
                    if IsIconic(win) != 0 {
                        ShowWindow(win, SW_RESTORE);
                    }
                    force_fg(win);
                }
            }
            InvalidateRect(w, null(), 0);
        }
        _ => return DefWindowProcW(w, msg, wp, lp),
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_geometry() {
        use super::{RECT, strip};
        let m = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let (s, w) = strip(&m, 40, false);
        assert_eq!((s.top, s.bottom, w.bottom, w.top, s.left, s.right), (1040, 1080, 1040, 0, 1920, 3840));
        let (s, w) = strip(&m, 40, true);
        assert_eq!((s.top, s.bottom, w.top, w.bottom), (0, 40, 40, 1080));
    }
}
