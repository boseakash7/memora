use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] r2d2_sqlite::rusqlite::Error),
    #[error("database pool error: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("folder {0} is already in the library")]
    FolderExists(PathBuf),
    /// The locked folder must be unlocked first.
    #[error("the locked folder is locked")]
    Locked,
    #[error("wrong password")]
    WrongPassword,
    /// A file could not be read or decoded.
    #[error("{0}")]
    Media(String),
}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    pub fn media(msg: impl Into<String>) -> Self {
        Error::Media(msg.into())
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Media(format!("cannot decode image: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
