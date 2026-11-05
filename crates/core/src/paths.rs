use std::path::{Path, PathBuf};

/// Where the app keeps its own data. Photos themselves are never stored here.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    /// `%LOCALAPPDATA%\Memora`, or `PHOTOS_DATA_DIR` if set (handy for testing).
    pub fn from_env() -> crate::Result<Self> {
        let data_dir = match std::env::var_os("PHOTOS_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let base = dirs::data_local_dir().ok_or_else(|| {
                    crate::Error::invalid("cannot locate the local app data folder")
                })?;
                let dir = base.join("Memora");
                // The app was previously called "Local Photos"; keep existing libraries.
                let legacy = base.join("LocalPhotos");
                if !dir.exists() && legacy.exists() {
                    let _ = std::fs::rename(&legacy, &dir);
                }
                dir
            }
        };
        Self::in_dir(&data_dir)
    }

    pub fn in_dir(data_dir: &Path) -> crate::Result<Self> {
        let paths = AppPaths {
            data_dir: data_dir.to_path_buf(),
            db_path: data_dir.join("library.db"),
            cache_dir: data_dir.join("cache"),
        };
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.cache_dir)?;
        Ok(paths)
    }
}
