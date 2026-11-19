//! Library core: storage, settings, watched folders, indexing, thumbnails,
//! organizing (favorites, albums, trash, search), places (reverse geocoding)
//! duplicate detection, and the stored side of people and content search.

pub mod ai;
pub mod albums;
pub mod backup;
pub mod browse;
pub mod db;
pub mod duplicates;
pub mod edit;
pub mod error;
pub mod ffmpeg;
pub mod folders;
pub mod hash;
pub mod heif;
pub mod library;
pub mod media;
pub mod memories;
pub mod metadata;
pub mod organize;
pub mod paths;
pub mod people;
pub mod places;
pub mod raw;
pub mod scan;
pub mod semantic;
pub mod settings;
pub mod snapshot;
pub mod storage;
pub mod sync;
pub mod thumbs;
pub mod transcode;
pub mod vault;

pub use db::Db;
pub use error::{Error, Result};
pub use library::{Library, Status};
pub use paths::AppPaths;
