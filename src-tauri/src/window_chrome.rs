//! Native bits of the custom title bar:
//! - the Windows window menu (Alt+Space / right-click on the bar), and
//! - Snap Layouts: the layout picker Windows 11 shows when the pointer rests on a maximize button.
//!
//! Windows only shows Snap Layouts when a window answers `WM_NCHITTEST` with `HTMAXBUTTON`. The
//! web page can't do that (WebView2 covers the whole window and gets the mouse first), so a small,
//! invisible native window sits over the page's maximize button and plays that button: it reports
//! `HTMAXBUTTON`, maximizes/restores on click, and tells the page when to draw hover and press.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use tauri::{AppHandle, Emitter};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, EnableMenuItem, GWLP_USERDATA, GetClientRect, GetCursorPos,
    GetParent, GetSystemMenu, GetWindowLongPtrW, GetWindowRect, HTMAXBUTTON, HWND_TOP, IDC_ARROW, IsZoomed,
    LoadCursorW, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, PostMessageW, RegisterClassExW, SC_CLOSE, SC_MAXIMIZE,
    SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    TrackPopupMenu, WINDOW_EX_STYLE, WM_DPICHANGED, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONUP, WM_SIZE, WM_SYSCOMMAND,
    WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS,
};
use windows::core::w;

/// Shows the standard window menu (Restore, Move, Size, Minimize, Maximize, Close) at a point in
/// the window's client area (physical pixels), and runs the chosen command.
///
/// The window has no Windows title bar, so Windows never opens this menu by itself.
pub fn show_system_menu(hwnd: isize, x: i32, y: i32) {
    let hwnd = HWND(hwnd as _);
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut pt);
        show_system_menu_at(hwnd, pt);
    }
}

/// Same, at a point on the screen.
unsafe fn show_system_menu_at(hwnd: HWND, pt: POINT) {
    unsafe {
        let menu = GetSystemMenu(hwnd, false);
        if menu.is_invalid() {
            return;
        }
        // Windows updates these only for windows with a native caption.
        let maximized = IsZoomed(hwnd).as_bool();
        for (id, on) in [
            (SC_RESTORE, maximized),
            (SC_MOVE, !maximized),
            (SC_SIZE, !maximized),
            (SC_MINIMIZE, true),
            (SC_MAXIMIZE, !maximized),
            (SC_CLOSE, true),
        ] {
            let _ = EnableMenuItem(menu, id, MF_BYCOMMAND | if on { MF_ENABLED } else { MF_GRAYED });
        }

        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        if cmd.0 != 0 {
            let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd.0 as usize), LPARAM(0));
        }
    }
}

// ---------- Snap Layouts: a native stand-in for the maximize button ----------

/// The stand-in window, once created.
static MAX_BUTTON: AtomicIsize = AtomicIsize::new(0);
/// Where the page's maximize button is, in CSS pixels: distance from the right edge, top, width, height.
static MAX_BUTTON_CSS: Mutex<[f64; 4]> = Mutex::new([0.0; 4]);

/// Event the page listens to for drawing the maximize button: "hover", "press" or "leave".
pub const MAX_BUTTON_EVENT: &str = "max-button";

struct MaxButton {
    app: AppHandle,
    hover: bool,
    pressed: bool,
}

/// Creates the stand-in (hidden until the page reports where its button is).
pub fn install_max_button(app: &AppHandle, parent: isize) {
    let parent = HWND(parent as _);
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(max_button_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("MemoraMaxButton"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        let data = Box::into_raw(Box::new(MaxButton { app: app.clone(), hover: false, pressed: false }));
        // It never paints, so the page's own button stays visible through it.
        let child = match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("MemoraMaxButton"),
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(instance),
            Some(data as _),
        ) {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!("no Snap Layouts on the maximize button: {e}");
                drop(Box::from_raw(data));
                return;
            }
        };
        MAX_BUTTON.store(child.0 as isize, Ordering::Relaxed);
        let _ = SetWindowSubclass(parent, Some(parent_proc), 0x4d4d_4258, 0);
    }
}

/// The page reports where its maximize button is (CSS px; width 0 = no button, e.g. full screen).
pub fn set_max_button_rect(right: f64, top: f64, width: f64, height: f64) {
    *MAX_BUTTON_CSS.lock().unwrap() = [right, top, width, height];
    place_max_button();
}

fn place_max_button() {
    let child = MAX_BUTTON.load(Ordering::Relaxed);
    if child == 0 {
        return;
    }
    let child = HWND(child as _);
    let [right, top, width, height] = *MAX_BUTTON_CSS.lock().unwrap();
    unsafe {
        let Ok(parent) = GetParent(child) else { return };
        if width <= 0.0 || height <= 0.0 {
            let _ = ShowWindow(child, SW_HIDE);
            return;
        }
        let scale = GetDpiForWindow(parent) as f64 / 96.0;
        let mut client = RECT::default();
        let _ = GetClientRect(parent, &mut client);
        let px = |v: f64| (v * scale).round() as i32;
        let _ = SetWindowPos(
            child,
            Some(HWND_TOP),
            client.right - px(right + width),
            px(top),
            px(width),
            px(height),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        // After maximize/restore the button may have moved away from the pointer, and Windows
        // then sends no "mouse left": clear the hover here.
        let data = GetWindowLongPtrW(child, GWLP_USERDATA) as *mut MaxButton;
        if !data.is_null() && (*data).hover {
            let mut pt = POINT::default();
            let mut rect = RECT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = GetWindowRect(child, &mut rect);
            let inside = pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom;
            if !inside {
                (*data).hover = false;
                (*data).pressed = false;
                let _ = (*data).app.emit(MAX_BUTTON_EVENT, "leave");
            }
        }
    }
}

/// Keeps the stand-in on the button when the window is resized or moves to another monitor.
unsafe extern "system" fn parent_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    if msg == WM_SIZE || msg == WM_DPICHANGED {
        place_max_button();
    }
    result
}

unsafe extern "system" fn max_button_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MaxButton;
        if data.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let parent = GetParent(hwnd).unwrap_or_default();
        let app = (*data).app.clone();
        let emit = |state: &str| {
            let _ = app.emit(MAX_BUTTON_EVENT, state);
        };
        match msg {
            WM_NCHITTEST => LRESULT(HTMAXBUTTON as isize),
            WM_NCMOUSEMOVE => {
                if !(*data).hover {
                    (*data).hover = true;
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    emit(if (*data).pressed { "press" } else { "hover" });
                }
                LRESULT(0)
            }
            WM_NCMOUSELEAVE => {
                (*data).hover = false;
                (*data).pressed = false;
                emit("leave");
                LRESULT(0)
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
                (*data).pressed = true;
                emit("press");
                LRESULT(0)
            }
            WM_NCLBUTTONUP => {
                if (*data).pressed {
                    (*data).pressed = false;
                    let cmd = if IsZoomed(parent).as_bool() { SC_RESTORE } else { SC_MAXIMIZE };
                    let _ = PostMessageW(Some(parent), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
                }
                emit("hover");
                LRESULT(0)
            }
            WM_NCRBUTTONUP => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                show_system_menu_at(parent, pt);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                MAX_BUTTON.store(0, Ordering::Relaxed);
                drop(Box::from_raw(data));
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
