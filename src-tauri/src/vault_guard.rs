//! Keeps the locked folder locked when nobody is at the PC: it locks when Windows locks
//! (Win+L, sleep, switching user) and, if chosen in Settings, when Memora is minimized.
//! Also empties the webview's disk cache after items moved into the locked folder, so no
//! picture of them shown earlier stays there.

use std::sync::OnceLock;

use photos_core::Library;
use webview2_com::ClearBrowsingDataCompletedHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE, ICoreWebView2_13, ICoreWebView2Profile2,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_SIZE;
use windows::core::Interface;

const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 0x7;
const SIZE_MINIMIZED: usize = 1;

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// Watch the main window for Windows locking and for minimizing.
pub fn install(hwnd: isize, library: Library) {
    let _ = LIBRARY.set(library);
    let hwnd = HWND(hwnd as _);
    unsafe {
        if let Err(e) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            tracing::warn!("the locked folder won't lock with Windows: {e}");
        }
        let _ = SetWindowSubclass(hwnd, Some(guard_proc), 0x4d56_4c54, 0);
    }
}

fn lock(why: &str) {
    let Some(lib) = LIBRARY.get() else { return };
    if lib.vault().state().unlocked {
        lib.vault().lock();
        tracing::info!("locked folder locked ({why})");
    }
}

unsafe extern "system" fn guard_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_WTSSESSION_CHANGE && wparam.0 == WTS_SESSION_LOCK {
        lock("Windows locked");
    } else if msg == WM_SIZE && wparam.0 == SIZE_MINIMIZED {
        let on = LIBRARY
            .get()
            .and_then(|lib| photos_core::settings::load(lib.db()).ok())
            .is_some_and(|s| s.vault_lock_on_minimize);
        if on {
            lock("Memora minimized");
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Empty the webview's disk cache (pictures shown before they moved into the locked folder).
/// Cookies and local storage stay.
#[tauri::command]
pub fn clear_web_cache(window: tauri::WebviewWindow) -> Result<(), String> {
    window
        .with_webview(|webview| {
            let result = (|| -> windows::core::Result<()> {
                unsafe {
                    let core = webview.controller().CoreWebView2()?;
                    let profile: ICoreWebView2Profile2 = core.cast::<ICoreWebView2_13>()?.Profile()?.cast()?;
                    let done = ClearBrowsingDataCompletedHandler::create(Box::new(|_| Ok(())));
                    profile.ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE, &done)
                }
            })();
            if let Err(e) = result {
                tracing::warn!("cannot empty the webview cache: {e}");
            }
        })
        .map_err(|e| e.to_string())
}
