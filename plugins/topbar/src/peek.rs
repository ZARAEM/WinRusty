// Window previews: resting the pointer on a running app's button shows live thumbnails of its windows above the bar, like
// the Windows taskbar. A click switches to that window; the cross in its corner (or a middle click) closes it.
// The pictures are DWM thumbnails: the compositor draws the windows' own current content, nothing is captured here.
use super::*;
use windows_sys::Win32::Graphics::Dwm::{DWM_THUMBNAIL_PROPERTIES, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties};

const CELL_W: i32 = 208;
const HEAD: i32 = 30; // icon + title + close button
const THUMB_H: i32 = 118;
const PAD: i32 = 8;

struct Item {
    win: HWND,
    thumb: isize, // DWM thumbnail handle, 0 if DWM refused
    title: String,
    icon: HICON, // the window's own, not ours to destroy
}

thread_local! {
    static WND: Cell<HWND> = const { Cell::new(null_mut()) };
    static ITEMS: RefCell<Vec<Item>> = const { RefCell::new(Vec::new()) };
    static FOR: Cell<i32> = const { Cell::new(-1) }; // the bar button the previews belong to, -1 = closed
    static OVER: Cell<(i32, bool)> = const { Cell::new((-1, false)) }; // preview under the pointer, and whether on its cross
    static AWAY: Cell<u32> = const { Cell::new(0) }; // polls in a row with the pointer on neither the button nor the previews
    static CX: Cell<i32> = const { Cell::new(0) }; // screen x the popup is centered on
    static POS: Cell<(i32, i32)> = const { Cell::new((0, 0)) }; // where it rests
    static T0: Cell<u64> = const { Cell::new(0) }; // when it started sliding in
    static OVER_T: Cell<u64> = const { Cell::new(0) }; // since when the pointer has been on the current preview
    static LIVE: Cell<HWND> = const { Cell::new(null_mut()) }; // the window shown on its own right now (the rest are glass)
}

/// Aero Peek: shows `win` alone on the desktop, every other window as a glass outline; None ends it. This is what the
/// Windows taskbar does while the pointer rests on a thumbnail. Undocumented: dwmapi.dll exports it by ordinal 113 only
/// (the taskbar replacements that have it call the same ordinal); if the export is missing nothing happens.
unsafe fn live(win: Option<HWND>) {
    if LIVE.get() == win.unwrap_or(null_mut()) {
        return;
    }
    LIVE.set(win.unwrap_or(null_mut()));
    let Some(f) = GetProcAddress(GetModuleHandleW(wide("dwmapi.dll").as_ptr()), 113usize as *const u8) else { return };
    // (activate, window to show, window that asks: it stays visible, kind: 3 = one window, reserved)
    let f: unsafe extern "system" fn(u32, HWND, HWND, u32, *const core::ffi::c_void) -> i32 = std::mem::transmute(f);
    match win {
        Some(w) => f(1, w, WND.get(), 3, null()),
        None => f(0, null_mut(), null_mut(), 0, null()),
    };
}

pub unsafe fn init(hi: HINSTANCE) {
    let c = wide("topbar_peek");
    RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hi, hCursor: LoadCursorW(null_mut(), IDC_ARROW), lpszClassName: c.as_ptr(), ..zeroed() });
    WND.set(CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, c.as_ptr(), null(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), hi, null()));
}

pub fn is_open() -> bool {
    FOR.get() >= 0
}

/// Largest size with the proportions of `sw` x `sh` that fits `bw` x `bh`; a small window is not blown up.
fn fit(sw: i32, sh: i32, bw: i32, bh: i32) -> (i32, i32) {
    if sw <= 0 || sh <= 0 {
        return (bw, bh);
    }
    let k = (bw as f32 / sw as f32).min(bh as f32 / sh as f32).min(1.0);
    (((sw as f32 * k).round() as i32).max(1), ((sh as f32 * k).round() as i32).max(1))
}

/// Client rectangle of preview `i` (title row and picture).
fn cell(i: usize) -> RECT {
    RECT { left: sc(PAD) + i as i32 * sc(CELL_W), top: sc(PAD), right: sc(PAD) + (i as i32 + 1) * sc(CELL_W), bottom: sc(PAD + HEAD + THUMB_H + PAD) }
}

fn close_rect(c: &RECT) -> RECT {
    RECT { left: c.right - sc(32), top: c.top + sc(3), right: c.right - sc(6), bottom: c.top + sc(27) }
}

unsafe fn clear() {
    for x in ITEMS.take() {
        if x.thumb != 0 {
            DwmUnregisterThumbnail(x.thumb);
        }
    }
}

pub unsafe fn hide() {
    if !is_open() {
        return;
    }
    let w = WND.get();
    live(None);
    KillTimer(w, 1);
    KillTimer(w, 2);
    ShowWindow(w, SW_HIDE);
    clear();
    FOR.set(-1);
}

/// Shows the previews of `wins` (the windows behind bar button `btn`) centered on screen x `cx`. Moving from one button
/// to the next swaps the pictures in place; only the first appearance slides in.
pub unsafe fn show(btn: i32, cx: i32, wins: &[HWND]) {
    let w = WND.get();
    if w.is_null() || wins.is_empty() || fly::any_open() {
        return;
    }
    if FOR.get() == btn && ITEMS.with_borrow(|it| it.iter().map(|i| i.win).eq(wins.iter().copied())) {
        return;
    }
    let fresh = !is_open();
    clear();
    // shortcut: as many as fit across the screen, the rest are reachable from the button's right-click menu
    let max = ((GetSystemMetrics(SM_CXSCREEN) - sc(2 * PAD + 16)) / sc(CELL_W)).max(1) as usize;
    let items = wins
        .iter()
        .take(max)
        .map(|&win| {
            let mut thumb = 0isize;
            DwmRegisterThumbnail(w, win, &mut thumb);
            let mut b = [0u16; 256];
            let n = GetWindowTextW(win, b.as_mut_ptr(), 256).max(0) as usize;
            Item { win, thumb, title: String::from_utf16_lossy(&b[..n]), icon: icon_of(win) }
        })
        .collect();
    ITEMS.set(items);
    FOR.set(btn);
    OVER.set((-1, false));
    AWAY.set(0);
    CX.set(cx);
    place(fresh);
}

/// Sizes the popup for the current previews, puts it above the bar and lays the thumbnails out.
unsafe fn place(slide: bool) {
    let w = WND.get();
    let n = ITEMS.with_borrow(|i| i.len()) as i32;
    let (ww, wh) = (n * sc(CELL_W) + 2 * sc(PAD), sc(PAD + HEAD + THUMB_H + PAD + PAD));
    let x = (CX.get() - ww / 2).clamp(sc(8), (GetSystemMetrics(SM_CXSCREEN) - ww - sc(8)).max(sc(8)));
    let y = pop_y(wh);
    POS.set((x, y));
    SetWindowRgn(w, CreateRoundRectRgn(0, 0, ww + 1, wh + 1, rr(12), rr(12)), 1);
    ITEMS.with_borrow(|it| {
        for (i, item) in it.iter().enumerate().filter(|(_, x)| x.thumb != 0) {
            let mut s = SIZE { cx: 0, cy: 0 };
            DwmQueryThumbnailSourceSize(item.thumb, &mut s);
            let c = cell(i);
            let area = RECT { left: c.left + sc(8), top: c.top + sc(HEAD), right: c.right - sc(8), bottom: c.bottom - sc(PAD) };
            let (tw, th) = fit(s.cx, s.cy, area.right - area.left, area.bottom - area.top);
            let (l, t) = ((area.left + area.right - tw) / 2, (area.top + area.bottom - th) / 2);
            let p = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: 0x1 | 0x4 | 0x8 | 0x10, // destination rectangle, opacity, visible, whole window (not only the client area)
                rcDestination: RECT { left: l, top: t, right: l + tw, bottom: t + th },
                rcSource: zeroed(),
                opacity: 255,
                fVisible: 1,
                fSourceClientAreaOnly: 0,
            };
            DwmUpdateThumbnailProperties(item.thumb, &p);
        }
    });
    if slide {
        T0.set(now_ms());
        SetWindowPos(w, HWND_TOPMOST, x, y + slide_off(0.0), ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetTimer(w, 2, 15, None);
    } else {
        SetWindowPos(w, HWND_TOPMOST, x, y, ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    SetTimer(w, 1, 100, None);
    InvalidateRect(w, null(), 0);
}

/// How far from its place the popup is at sliding progress `p` (0..1): it rises out of the bar.
fn slide_off(p: f32) -> i32 {
    let d = ((1.0 - p) * sc(12) as f32) as i32;
    if at_top() { -d } else { d }
}

/// Every 100 ms while open: windows that closed leave the row, and the popup goes away once the pointer has been off
/// both the button and the previews for three polls in a row (so crossing the gap between them does not close it).
unsafe fn poll(w: HWND) {
    if fly::any_open() || FULLSCREEN.load(Relaxed) || DRAGB.get().is_some_and(|d| d.moved) {
        return hide();
    }
    let before = ITEMS.with_borrow(|i| i.len());
    ITEMS.with_borrow_mut(|it| {
        it.retain(|x| {
            let alive = IsWindow(x.win) != 0;
            if !alive && x.thumb != 0 {
                DwmUnregisterThumbnail(x.thumb);
            }
            alive
        })
    });
    let n = ITEMS.with_borrow(|i| i.len());
    if n == 0 {
        return hide();
    }
    if n != before {
        OVER.set((-1, false));
        place(false);
    }
    let (mut pt, mut r): (POINT, RECT) = (zeroed(), zeroed());
    GetCursorPos(&mut pt);
    GetWindowRect(w, &mut r);
    // the strip between the popup and the bar counts as the popup
    let (top, bottom) = if at_top() { (r.top - sc(12), r.bottom) } else { (r.top, r.bottom + sc(12)) };
    let inside = pt.x >= r.left && pt.x < r.right && pt.y >= top && pt.y < bottom;
    if !inside && OVER.replace((-1, false)).0 >= 0 {
        InvalidateRect(w, null(), 0);
    }
    // resting on one preview for half a second peeks at that window; moving off any preview ends the peek
    let (over, on_close) = OVER.get();
    let rest = ITEMS.with_borrow(|it| it.get(over.max(0) as usize).filter(|_| over >= 0 && !on_close && inside).map(|x| x.win));
    match rest {
        Some(win) if IsIconic(win) == 0 && now_ms().saturating_sub(OVER_T.get()) >= 500 => live(Some(win)),
        Some(_) => {}
        None => live(None),
    }
    if inside || HOT.get() == FOR.get() {
        AWAY.set(0);
    } else if AWAY.replace(AWAY.get() + 1) >= 2 {
        hide();
    }
}

unsafe fn paint(w: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let wdc = BeginPaint(w, &mut ps);
    let mut cr: RECT = zeroed();
    GetClientRect(w, &mut cr);
    let (dc, bmp) = (CreateCompatibleDC(wdc), CreateCompatibleBitmap(wdc, cr.right, cr.bottom));
    let old = SelectObject(dc, bmp);
    fill(dc, cr, pal().line, sc(12));
    fill(dc, RECT { left: 1, top: 1, right: cr.right - 1, bottom: cr.bottom - 1 }, pal().bg, sc(12));
    SetBkMode(dc, TRANSPARENT as i32);
    let (face, gface) = (wide(ui_face()), wide("Segoe MDL2 Assets"));
    let f_txt = CreateFontW(-sc(12), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
    let f_glyph = CreateFontW(-sc(10), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, gface.as_ptr());
    let of = SelectObject(dc, f_txt);
    let (over, on_close) = OVER.get();
    let fg = app_window();
    ITEMS.with_borrow(|it| {
        for (i, x) in it.iter().enumerate() {
            let c = cell(i);
            let hot = over == i as i32;
            if hot || x.win == fg {
                // the pointed-at preview, and (fainter) the window that is in front right now
                let col = if hot { pal().hover } else { mix(pal().bg, pal().accent, 0.14) };
                fill(dc, RECT { left: c.left + sc(2), top: c.top - sc(2), right: c.right - sc(2), bottom: c.bottom }, col, sc(10));
            }
            if !x.icon.is_null() {
                DrawIconEx(dc, c.left + sc(10), c.top + sc(7), x.icon, sc(16), sc(16), 0, null_mut(), DI_NORMAL);
            }
            SelectObject(dc, f_txt);
            text(dc, &x.title, RECT { left: c.left + sc(32), top: c.top, right: c.right - sc(if hot { 36 } else { 10 }), bottom: c.top + sc(HEAD) }, pal().text, DT_LEFT);
            if hot {
                let b = close_rect(&c);
                if on_close {
                    fill(dc, b, BAD, sc(8));
                }
                SelectObject(dc, f_glyph);
                text(dc, "\u{E8BB}", b, if on_close { rgb(255, 255, 255) } else { pal().text }, DT_CENTER);
            }
        }
    });
    BitBlt(wdc, 0, 0, cr.right, cr.bottom, dc, 0, 0, SRCCOPY);
    SelectObject(dc, of);
    DeleteObject(f_txt);
    DeleteObject(f_glyph);
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
    EndPaint(w, &ps);
}

/// The preview at client point (x, y) and whether the point is on its close button.
fn at(x: i32, y: i32) -> (i32, bool) {
    let n = ITEMS.with_borrow(|i| i.len());
    for i in 0..n {
        let c = cell(i);
        if x >= c.left && x < c.right && y >= c.top - sc(2) && y < c.bottom {
            let b = close_rect(&c);
            return (i as i32, x >= b.left && x < b.right && y >= b.top && y < b.bottom);
        }
    }
    (-1, false)
}

unsafe extern "system" fn proc(w: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = ((lp & 0xffff) as i16 as i32, (lp >> 16 & 0xffff) as i16 as i32);
    match msg {
        WM_PAINT => paint(w),
        WM_ERASEBKGND => return 1,
        WM_TIMER if wp == 1 => poll(w),
        WM_TIMER if wp == 2 => {
            let t = (now_ms().saturating_sub(T0.get()) as f32 / 150.0).min(1.0);
            let (px, py) = POS.get();
            SetWindowPos(w, null_mut(), px, py + slide_off(1.0 - (1.0 - t).powi(3)), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            if t >= 1.0 {
                KillTimer(w, 2);
            }
        }
        WM_MOUSEMOVE => {
            let o = at(x, y);
            if OVER.replace(o) != o {
                OVER_T.set(now_ms());
                InvalidateRect(w, null(), 0);
            }
        }
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let (i, on_close) = at(x, y);
            let Some(win) = ITEMS.with_borrow(|it| it.get(i.max(0) as usize).filter(|_| i >= 0).map(|x| x.win)) else { return 0 };
            if on_close || msg == WM_MBUTTONUP {
                close_window(win); // it leaves the row when the window is really gone (it may ask to save first)
            } else {
                hide();
                if IsIconic(win) != 0 {
                    ShowWindow(win, SW_RESTORE);
                }
                force_fg(win);
                InvalidateRect(BAR.load(Relaxed) as HWND, null(), 0);
            }
        }
        _ => return DefWindowProcW(w, msg, wp, lp),
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn thumbnails_keep_their_shape() {
        use super::fit;
        assert_eq!(fit(1920, 1080, 192, 118), (192, 108)); // wide window: full width
        assert_eq!(fit(600, 1200, 192, 118), (59, 118)); // tall window: full height
        assert_eq!(fit(100, 50, 192, 118), (100, 50)); // a small window is not enlarged
        assert_eq!(fit(0, 0, 192, 118), (192, 118)); // DWM gave no size: the whole area
    }
}
