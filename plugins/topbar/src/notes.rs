// Notifications: what apps announce through their tray icon (Shell_NotifyIcon with NIF_INFO, the classic "balloon":
// Windows itself turns these into toasts) pops up above the bar's clock side for a few seconds and is kept in a list
// behind the clock (right click on it). A click on the popup or on a list entry tells the app, which usually opens
// the thing it announced. Toasts that modern apps send straight to the Windows notification platform do not pass
// through the tray and are not seen here.
use super::*;

pub struct Note {
    pub title: String,
    pub text: String,
    pub app: String,  // the icon's tooltip, or its exe name
    icon: HICON,      // our own copy
    src: (HWND, u32, u32, u32), // the tray icon it came from: where a click is reported
}

const KEEP: usize = 30;
const W: i32 = 348;
const H: i32 = 88;
const SHOW_MS: u64 = 5500;

thread_local! {
    static LIST: RefCell<Vec<Note>> = const { RefCell::new(Vec::new()) }; // oldest first
    static UNREAD: Cell<usize> = const { Cell::new(0) };
    static WND: Cell<HWND> = const { Cell::new(null_mut()) };
    static T0: Cell<u64> = const { Cell::new(0) }; // when the popup appeared
    static POS: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
}

pub unsafe fn init(hi: HINSTANCE) {
    let c = wide("topbar_note");
    RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hi, hCursor: LoadCursorW(null_mut(), IDC_ARROW), lpszClassName: c.as_ptr(), ..zeroed() });
    WND.set(CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED, c.as_ptr(), null(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), hi, null()));
}

/// Notifications that arrived since the list was last opened.
pub fn unread() -> usize {
    UNREAD.get()
}

/// Opacity 0..1 of the popup `age` ms after it appeared: fades in, stays, fades out. None = time to hide.
fn popup_alpha(age: u64) -> Option<f32> {
    match age {
        0..200 => Some(age as f32 / 200.0),
        200..SHOW_MS => Some(1.0),
        _ if age < SHOW_MS + 300 => Some(1.0 - (age - SHOW_MS) as f32 / 300.0),
        _ => None,
    }
}

/// A new notification from tray icon `src`. `icon` is copied. Shown as a popup unless "do not disturb" is on or a
/// fullscreen app has the screen; always added to the list.
pub unsafe fn push(title: String, text: String, app: String, icon: HICON, src: (HWND, u32, u32, u32)) {
    let note = Note { title, text, app, icon: CopyIcon(icon), src };
    LIST.with_borrow_mut(|l| {
        if l.len() >= KEEP {
            DestroyIcon(l.remove(0).icon);
        }
        l.push(note);
    });
    UNREAD.set(UNREAD.get() + 1);
    InvalidateRect(BAR.load(Relaxed) as HWND, null(), 0); // the dot by the clock
    if cfg("quiet") || FULLSCREEN.load(Relaxed) {
        return;
    }
    let w = WND.get();
    let (ww, wh) = (sc(W), sc(H));
    let (x, y) = (GetSystemMetrics(SM_CXSCREEN) - ww - sc(8), pop_y(wh));
    POS.set((x, y));
    T0.set(now_ms());
    SetWindowRgn(w, CreateRoundRectRgn(0, 0, ww + 1, wh + 1, rr(12), rr(12)), 1);
    SetLayeredWindowAttributes(w, 0, 0, LWA_ALPHA);
    SetWindowPos(w, HWND_TOPMOST, x, y, ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    InvalidateRect(w, null(), 0);
    SetTimer(w, 1, 15, None);
}

unsafe fn hide(w: HWND) {
    KillTimer(w, 1);
    ShowWindow(w, SW_HIDE);
}

/// Tells the app its notification was clicked (NIN_BALLOONUSERCLICK) and drops it from the list.
unsafe fn activate(i: usize) {
    let Some(n) = LIST.with_borrow_mut(|l| (i < l.len()).then(|| l.remove(i))) else { return };
    if IsWindow(n.src.0) != 0 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(n.src.0, &mut pid);
        AllowSetForegroundWindow(pid); // so the app can bring its window to the front
        tray_post(n.src, &[0x405]);
    }
    DestroyIcon(n.icon);
}

/// The list behind the clock: the newest notifications first, "do not disturb", and "clear all".
pub unsafe fn menu(cx: i32) {
    UNREAD.set(0);
    const SHOWN: usize = 8;
    let n = LIST.with_borrow(|l| l.len());
    let first = n.saturating_sub(SHOWN);
    let mut items: Vec<fly::MItem> = LIST.with_borrow(|l| {
        l.iter().rev().take(SHOWN).map(|x| fly::MItem { icon: x.icon, ..fly::MItem::new(if x.title.is_empty() { format!("{} · {}", x.app, x.text) } else { format!("{} · {}", x.title, x.text) }) }).collect()
    });
    let shown = items.len();
    if shown == 0 {
        items.push(fly::MItem::new(t!("Уведомлений нет", "No notifications")));
    }
    items.push(fly::MItem::new(t!("Не беспокоить", "Do not disturb")).on(cfg("quiet")).sep());
    items.push(fly::MItem::new(t!("Очистить все", "Clear all")).danger());
    let head = (null_mut(), "\u{EA8F}", t!("Уведомления", "Notifications").to_string(), if n == 0 { String::new() } else { tf!("Всего: {n}", "{n} in the list") });
    fly::menu(cx, Some(head), items, move |i| {
        let fixed = shown.max(1); // rows before "do not disturb"
        if i < shown {
            activate(first + shown - 1 - i); // row 0 is the newest = the last of the list
        } else if i == fixed {
            cfg_toggle("quiet");
        } else if i == fixed + 1 {
            for x in LIST.take() {
                DestroyIcon(x.icon);
            }
        }
        InvalidateRect(BAR.load(Relaxed) as HWND, null(), 0);
    });
}

unsafe fn paint(w: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(w, &mut ps);
    let mut cr: RECT = zeroed();
    GetClientRect(w, &mut cr);
    fill(dc, cr, pal().line, sc(12));
    fill(dc, RECT { left: 1, top: 1, right: cr.right - 1, bottom: cr.bottom - 1 }, pal().bg, sc(12));
    SetBkMode(dc, TRANSPARENT as i32);
    let face = wide(ui_face());
    let font = |px: i32, wt: i32| CreateFontW(-sc(px), 0, 0, 0, wt, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
    let (f_title, f_txt, f_small) = (font(13, 600), font(13, 400), font(11, 400));
    let of = SelectObject(dc, f_title);
    LIST.with_borrow(|l| {
        let Some(n) = l.last() else { return };
        let tile = RECT { left: sc(12), top: sc(12), right: sc(48), bottom: sc(48) };
        fill(dc, tile, mix(pal().bg, pal().accent, 0.16), sc(10));
        if !n.icon.is_null() {
            DrawIconEx(dc, tile.left + sc(6), tile.top + sc(6), n.icon, sc(24), sc(24), 0, null_mut(), DI_NORMAL);
        }
        let (tl, tr) = (sc(60), cr.right - sc(12));
        text(dc, if n.title.is_empty() { &n.app } else { &n.title }, RECT { left: tl, top: sc(8), right: tr, bottom: sc(30) }, pal().text, DT_LEFT);
        SelectObject(dc, f_txt);
        SetTextColor(dc, mix(pal().dim, pal().text, 0.7));
        let mut body = RECT { left: tl, top: sc(30), right: tr, bottom: sc(66) };
        DrawTextW(dc, wide(&n.text).as_ptr(), -1, &mut body, DT_LEFT | DT_NOPREFIX | DT_WORDBREAK | DT_END_ELLIPSIS | DT_EDITCONTROL);
        if !n.title.is_empty() {
            SelectObject(dc, f_small);
            text(dc, &n.app, RECT { left: tl, top: sc(66), right: tr, bottom: sc(84) }, pal().dim, DT_LEFT);
        }
    });
    SelectObject(dc, of);
    for f in [f_title, f_txt, f_small] {
        DeleteObject(f);
    }
    EndPaint(w, &ps);
}

unsafe extern "system" fn proc(w: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => paint(w),
        WM_ERASEBKGND => return 1,
        WM_TIMER => {
            let age = now_ms().saturating_sub(T0.get());
            match popup_alpha(age) {
                Some(a) => {
                    SetLayeredWindowAttributes(w, 0, (a * 255.0) as u8, LWA_ALPHA);
                    // rises a few pixels out of the bar while it fades in
                    let (x, y) = POS.get();
                    let d = ((1.0 - (age as f32 / 200.0).min(1.0)) * sc(10) as f32) as i32;
                    SetWindowPos(w, null_mut(), x, y + if at_top() { -d } else { d }, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                }
                None => {
                    hide(w);
                    // NIN_BALLOONTIMEOUT: nobody clicked
                    if let Some(src) = LIST.with_borrow(|l| l.last().map(|n| n.src)).filter(|s| IsWindow(s.0) != 0) {
                        tray_post(src, &[0x404]);
                    }
                }
            }
        }
        WM_LBUTTONUP => {
            hide(w);
            let n = LIST.with_borrow(|l| l.len());
            if n > 0 {
                activate(n - 1);
                UNREAD.set(UNREAD.get().saturating_sub(1));
                InvalidateRect(BAR.load(Relaxed) as HWND, null(), 0);
            }
        }
        WM_RBUTTONUP | WM_MBUTTONUP => hide(w), // dismissed: it stays in the list
        _ => return DefWindowProcW(w, msg, wp, lp),
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn popup_fades() {
        use super::{SHOW_MS, popup_alpha};
        assert_eq!(popup_alpha(0), Some(0.0));
        assert_eq!(popup_alpha(100), Some(0.5));
        assert_eq!(popup_alpha(1000), Some(1.0));
        assert_eq!(popup_alpha(SHOW_MS + 150), Some(0.5));
        assert_eq!(popup_alpha(SHOW_MS + 300), None);
    }
}
