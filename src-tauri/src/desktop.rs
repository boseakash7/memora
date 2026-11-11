//! Windows desktop integration that isn't about Memora's own window.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::UI::WindowsAndMessaging::{
    SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SystemParametersInfoW,
};

/// Makes a picture file the desktop background (Windows keeps its own "fit" setting).
pub fn set_wallpaper(path: &Path) -> Result<(), String> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
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
