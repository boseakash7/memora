use std::path::{Path, PathBuf};

use crate::storage;

/// Where the app keeps its own data. Photos themselves are never stored here.
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// `%LOCALAPPDATA%\Memora` (or `PHOTOS_DATA_DIR`): always here, holds `location.json`
    /// when the data was moved, and is where models are looked for.
    pub home_dir: PathBuf,
    /// The library data: database and cache. The home folder unless moved (see `storage`).
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    /// The data folder as `location.json` says. `Ok(Err(dir))` when the data was moved to a
    /// folder that can't be reached now (an unplugged drive).
    pub fn from_env() -> crate::Result<std::result::Result<Self, PathBuf>> {
        let home = storage::home_dir()?;
        std::fs::create_dir_all(&home)?;
        match storage::data_dir(&home) {
            Ok(dir) => Ok(Ok(Self::with_home(&home, &dir)?)),
            Err(dir) => Ok(Err(dir)),
        }
    }

    /// Data kept in its home folder (tests).
    pub fn in_dir(data_dir: &Path) -> crate::Result<Self> {
        Self::with_home(data_dir, data_dir)
    }

    pub fn with_home(home_dir: &Path, data_dir: &Path) -> crate::Result<Self> {
        let paths = AppPaths {
            home_dir: home_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            db_path: data_dir.join("library.db"),
            cache_dir: data_dir.join("cache"),
        };
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.cache_dir)?;
        Ok(paths)
    }
}
