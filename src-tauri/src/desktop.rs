//! Windows desktop integration that isn't about Memora's own window.

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};
use windows::Win32::UI::WindowsAndMessaging::{
    IDRETRY, MB_ICONWARNING, MB_RETRYCANCEL, MessageBoxW, SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE,
    SPIF_UPDATEINIFILE, SystemParametersInfoW,
};
use windows::core::PCWSTR;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Makes a picture file the desktop background (Windows keeps its own "fit" setting).
pub fn set_wallpaper(path: &Path) -> Result<(), String> {
    let mut wide = wide(path.as_os_str());
    unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr().cast()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    }
    .map_err(|e| format!("Windows didn't accept the picture: {e}"))
}

/// `DRIVE_REMOTE` from `GetDriveTypeW`.
const DRIVE_REMOTE: u32 = 4;

/// Free bytes on the drive of `path` (or of its nearest folder that exists). Refuses mapped
/// network drives: SQLite's file locking isn't reliable there.
pub fn free_space(path: &Path) -> Result<u64, String> {
    let root: PathBuf = path.components().take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir)).collect();
    if unsafe { GetDriveTypeW(PCWSTR(wide(root.as_os_str()).as_ptr())) } == DRIVE_REMOTE {
        return Err("network drives can't hold the library data: the database needs a drive of this PC".into());
    }
    let existing = path.ancestors().find(|p| p.is_dir()).ok_or_else(|| format!("{} can't be reached", root.display()))?;
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide(existing.as_os_str()).as_ptr()), Some(&mut free), None, None) }
        .map_err(|e| format!("cannot read the free space on {}: {e}", root.display()))?;
    Ok(free)
}

/// Asks to connect the drive with the library data; true for "Retry", false for "Cancel".
pub fn ask_retry(title: &str, text: &str) -> bool {
    let (title, text) = (wide(title.as_ref()), wide(text.as_ref()));
    let answer =
        unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_RETRYCANCEL | MB_ICONWARNING) };
    answer == IDRETRY
}
