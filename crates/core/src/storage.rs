//! Where the library data (database + cache) lives, and moving it to another folder.
//!
//! The "home" folder (`%LOCALAPPDATA%\Memora`, or `PHOTOS_DATA_DIR` for testing) always
//! exists. It holds `location.json` when the data was moved elsewhere; the database can't
//! store its own location. A move copies the data while the app runs, takes a consistent
//! copy of the database at the end, switches `location.json` and restarts; the old copy is
//! deleted on the next start ([`finish_move`]), once the new one is known to open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::db::Db;
use crate::{AppPaths, Error, Result};

const LOCATION_FILE: &str = "location.json";

/// `location.json` in the home folder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Where the data is, when not in the home folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// The previous data folder, whose copy is deleted at the next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_old: Option<PathBuf>,
}

/// The home folder: `PHOTOS_DATA_DIR` if set, else `%LOCALAPPDATA%\Memora`.
pub fn home_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("PHOTOS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_local_dir().ok_or_else(|| Error::invalid("cannot locate the local app data folder"))?;
    let dir = base.join("Memora");
    // The app was previously called "Local Photos"; keep existing libraries.
    let legacy = base.join("LocalPhotos");
    if !dir.exists() && legacy.exists() {
        let _ = std::fs::rename(&legacy, &dir);
    }
    Ok(dir)
}

pub fn read_location(home: &Path) -> Location {
    std::fs::read(home.join(LOCATION_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Written to a temporary file and renamed, so a crash never leaves half a file.
pub fn write_location(home: &Path, loc: &Location) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let path = home.join(LOCATION_FILE);
    if *loc == Location::default() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => return Ok(()),
        }
    }
    let tmp = home.join("location.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(loc)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Where the data should be, by `location.json`. `Err(dir)` when it was moved to a folder
/// that can't be reached now (an unplugged drive).
pub fn data_dir(home: &Path) -> std::result::Result<PathBuf, PathBuf> {
    match read_location(home).data_dir {
        Some(dir) if dir.join("library.db").is_file() => Ok(dir),
        Some(dir) => Err(dir),
        None => Ok(home.to_path_buf()),
    }
}

/// After a move: delete the old copy (database, cache, our own files; nothing else) and
/// forget it. Only once the current data folder has a database. Best effort: errors are logged.
pub fn finish_move(paths: &AppPaths) {
    let mut loc = read_location(&paths.home_dir);
    let Some(old) = loc.remove_old.clone() else { return };
    if same_dir(&old, &paths.data_dir) || !paths.db_path.is_file() {
        return;
    }
    tracing::info!("removing the old copy of the library data in {}", old.display());
    for name in OWN_FILES.iter().chain(["library.db", "library.db-wal", "library.db-shm", "dlna-requests.log"].iter()) {
        let _ = std::fs::remove_file(old.join(name));
    }
    for dir in ["cache", VAULT_DIR] {
        if let Err(e) = std::fs::remove_dir_all(old.join(dir))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("cannot remove the old {dir} folder in {}: {e}", old.display());
        }
    }
    // The folder we made for the data goes too, if nothing else is in it (never the home folder).
    if !same_dir(&old, &paths.home_dir) {
        let _ = std::fs::remove_dir(&old);
    }
    loc.remove_old = None;
    if let Err(e) = write_location(&paths.home_dir, &loc) {
        tracing::warn!("cannot update {LOCATION_FILE}: {e}");
    }
}

/// Small files of ours in the data folder that move with it. (`dlna-requests.log` starts
/// fresh with each launch, so it is only removed.)
const OWN_FILES: &[&str] = &["wallpaper.jpg"];
/// The locked folder (see `vault`): encrypted, moves with the data.
pub const VAULT_DIR: &str = "vault";

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase().replace('/', "\\");
    norm(a) == norm(b)
}

fn inside(path: &Path, base: &Path) -> bool {
    crate::folders::same_or_inside(path, base)
}

/// Folders that a sync client (OneDrive, Dropbox, Google Drive, iCloud) keeps in the cloud.
/// SQLite there gets copied while it is being written and can be corrupted.
fn cloud_folder(path: &Path) -> Option<&'static str> {
    for (var, name) in [("OneDrive", "OneDrive"), ("OneDriveConsumer", "OneDrive"), ("OneDriveCommercial", "OneDrive")] {
        if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty())
            && inside(path, Path::new(&dir))
        {
            return Some(name);
        }
    }
    for part in path.components() {
        let part = part.as_os_str().to_string_lossy().to_lowercase();
        if part.starts_with("onedrive") {
            return Some("OneDrive");
        }
        match part.as_str() {
            "dropbox" => return Some("Dropbox"),
            "google drive" | "my drive" | "googledrive" => return Some("Google Drive"),
            "icloud drive" | "iclouddrive" => return Some("iCloud"),
            _ => {}
        }
    }
    None
}

/// The folder the data would go to when the user picks `picked`: the folder itself if it is
/// empty (or new), else a `Memora` folder inside it. Refuses network and cloud folders and
/// folders inside the current data (or the other way round), with a reason to show.
pub fn target_for(paths: &AppPaths, picked: &Path) -> Result<PathBuf> {
    if !picked.is_absolute() {
        return Err(Error::invalid("choose a folder on a drive of this PC"));
    }
    let s = picked.to_string_lossy();
    if s.starts_with(r"\\") || s.starts_with("//") {
        return Err(Error::invalid(
            "network folders can't hold the library data: the database needs a drive of this PC",
        ));
    }
    if let Some(service) = cloud_folder(picked) {
        return Err(Error::invalid(format!(
            "this folder is synced by {service}. Syncing can damage the library database while it is in use; \
             choose a folder that isn't synced"
        )));
    }
    if same_dir(picked, &paths.data_dir) {
        return Err(Error::invalid("the library data is already in this folder"));
    }
    if picked.join("library.db").exists() && !same_dir(picked, &paths.home_dir) {
        return Err(Error::invalid("that folder already has a Memora library"));
    }
    if inside(picked, &paths.data_dir) {
        return Err(Error::invalid("choose a folder outside the current library data folder"));
    }
    // Back home: always the home folder itself.
    if same_dir(picked, &paths.home_dir) {
        return if picked.join("library.db").exists() {
            Err(Error::invalid("that folder already has a Memora library"))
        } else {
            Ok(paths.home_dir.clone())
        };
    }
    let target = if is_empty_dir(picked) { picked.to_path_buf() } else { picked.join("Memora") };
    if inside(&paths.data_dir, &target) {
        return Err(Error::invalid("choose a folder outside the current library data folder"));
    }
    if target.join("library.db").exists() {
        return Err(Error::invalid(format!("{} already has a Memora library", target.display())));
    }
    if !is_empty_dir(&target) {
        return Err(Error::invalid(format!("{} is not empty; choose an empty folder", target.display())));
    }
    Ok(target)
}

/// True for an empty folder or one that doesn't exist yet.
fn is_empty_dir(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn tree_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Bytes the data folder's movable contents take (database + cache + locked folder + our files).
pub fn data_size(paths: &AppPaths) -> u64 {
    let db: u64 = ["library.db", "library.db-wal"]
        .iter()
        .filter_map(|n| std::fs::metadata(paths.data_dir.join(n)).ok())
        .map(|m| m.len())
        .sum();
    db + tree_size(&paths.cache_dir) + tree_size(&paths.data_dir.join(VAULT_DIR))
}

/// Copy the cache to `target`, skipping files already there with the same size (so a
/// second pass only copies what changed). Calls `progress(bytes)` as it goes.
/// Files that vanish while copying (cache entries being replaced) are skipped.
pub fn copy_cache(paths: &AppPaths, target: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
    copy_tree(&paths.cache_dir, &target.join("cache"), progress)
}

fn copy_tree(from: &Path, to: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in WalkDir::new(from).min_depth(1) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if e.io_error().is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) => continue,
            Err(e) => return Err(Error::Io(std::io::Error::other(e))),
        };
        let rel = entry.path().strip_prefix(from).expect("inside the folder");
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if std::fs::metadata(&dest).is_ok_and(|d| d.len() == meta.len()) {
            progress(meta.len());
            continue;
        }
        match std::fs::copy(entry.path(), &dest) {
            Ok(n) => progress(n),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// The last part of a move, with background work stopped: a consistent copy of the database
/// (`VACUUM INTO`), the rest of the cache, our small files; then check the copy opens and has
/// the same items, and switch `location.json` to it. The app must restart afterwards.
pub fn finish_copy(db: &Db, paths: &AppPaths, target: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
    copy_cache(paths, target, progress)?;
    // The locked folder only changes on request, so it is copied once, here.
    let vault = paths.data_dir.join(VAULT_DIR);
    if vault.is_dir() {
        copy_tree(&vault, &target.join(VAULT_DIR), progress)?;
        if tree_size(&vault) != tree_size(&target.join(VAULT_DIR)) {
            return Err(Error::invalid("the copy of the locked folder didn't check out"));
        }
    }
    let new_db = target.join("library.db");
    let _ = std::fs::remove_file(&new_db);
    let conn = db.conn()?;
    conn.execute("VACUUM INTO ?1", [new_db.to_string_lossy()])?;
    let items: i64 = conn.query_row("SELECT count(*) FROM media", [], |r| r.get(0))?;
    drop(conn);
    for name in OWN_FILES {
        let from = paths.data_dir.join(name);
        if from.is_file() {
            std::fs::copy(&from, target.join(name))?;
        }
    }

    let check = r2d2_sqlite::rusqlite::Connection::open(&new_db)?;
    let ok: String = check.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    let copied: i64 = check.query_row("SELECT count(*) FROM media", [], |r| r.get(0))?;
    drop(check);
    if ok != "ok" || copied != items {
        let _ = std::fs::remove_file(&new_db);
        return Err(Error::invalid(format!("the copied library didn't check out ({ok}, {copied} of {items} items)")));
    }

    let data_dir = (!same_dir(target, &paths.home_dir)).then(|| target.to_path_buf());
    write_location(&paths.home_dir, &Location { data_dir, remove_old: Some(paths.data_dir.clone()) })
}

/// Undo a move that failed before switching: remove what was copied to `target`.
pub fn abandon(target: &Path, paths: &AppPaths) {
    if same_dir(target, &paths.data_dir) {
        return;
    }
    let _ = std::fs::remove_dir_all(target.join("cache"));
    let _ = std::fs::remove_dir_all(target.join(VAULT_DIR));
    for name in OWN_FILES.iter().chain(["library.db"].iter()) {
        let _ = std::fs::remove_file(target.join(name));
    }
    if !same_dir(target, &paths.home_dir) {
        let _ = std::fs::remove_dir(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(dir: &Path) -> (AppPaths, Db) {
        let paths = AppPaths::in_dir(dir).unwrap();
        let db = Db::open(&paths.db_path).unwrap();
        db.conn().unwrap().execute("INSERT INTO folders (path, added_at) VALUES ('C:\\Photos', '')", []).unwrap();
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, stage, added_at)
                 VALUES (1, 'C:\\Photos\\a.jpg', 'a.jpg', 1, 1, 'photo', 'image/jpeg', '2024-01-01T00:00:00', 3, '')",
                [],
            )
            .unwrap();
        std::fs::create_dir_all(paths.cache_dir.join("thumb/0")).unwrap();
        std::fs::write(paths.cache_dir.join("thumb/0/1.jpg"), b"thumbnail").unwrap();
        std::fs::write(paths.data_dir.join("wallpaper.jpg"), b"wall").unwrap();
        (paths, db)
    }

    #[test]
    fn move_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let (paths, db) = library(&home);
        assert_eq!(data_dir(&home), Ok(home.clone()));

        // A non-empty folder gets a Memora folder inside it.
        let drive = tmp.path().join("D");
        std::fs::create_dir_all(drive.join("Other")).unwrap();
        let target = target_for(&paths, &drive).unwrap();
        assert_eq!(target, drive.join("Memora"));

        let mut copied = 0;
        copy_cache(&paths, &target, &mut |n| copied += n).unwrap();
        assert_eq!(copied, 9);
        // A thumbnail made after the first pass is picked up by the second.
        std::fs::write(paths.cache_dir.join("thumb/0/2.jpg"), b"new").unwrap();
        finish_copy(&db, &paths, &target, &mut |_| {}).unwrap();
        drop(db);
        assert_eq!(std::fs::read(target.join("cache/thumb/0/2.jpg")).unwrap(), b"new");
        assert!(target.join("wallpaper.jpg").is_file());
        assert_eq!(data_dir(&home), Ok(target.clone()));
        assert_eq!(read_location(&home).remove_old, Some(home.clone()));

        // Next start: the new copy opens with the item, then the old copy is removed.
        let moved = AppPaths::with_home(&home, &target).unwrap();
        let db = Db::open(&moved.db_path).unwrap();
        assert_eq!(crate::media::timeline(&db).unwrap().len(), 1);
        std::fs::write(home.join("location-keep.txt"), b"x").unwrap();
        finish_move(&moved);
        assert!(!home.join("library.db").exists() && !home.join("cache").exists() && !home.join("wallpaper.jpg").exists());
        assert!(home.join("location-keep.txt").exists(), "only our own files are removed");
        assert_eq!(read_location(&home), Location { data_dir: Some(target.clone()), remove_old: None });

        // And back home again: location.json goes away.
        let back = target_for(&moved, &home).unwrap();
        assert_eq!(back, home);
        finish_copy(&db, &moved, &back, &mut |_| {}).unwrap();
        drop(db);
        assert_eq!(data_dir(&home), Ok(home.clone()));
        let home_paths = AppPaths::with_home(&home, &home).unwrap();
        finish_move(&home_paths);
        assert!(!target.exists(), "the folder made for the data is removed when empty");
        assert!(!home.join("location.json").exists());
    }

    #[test]
    fn unreachable_data_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("E/Memora");
        write_location(tmp.path(), &Location { data_dir: Some(gone.clone()), remove_old: None }).unwrap();
        assert_eq!(data_dir(tmp.path()), Err(gone));
    }

    #[test]
    fn refuses_bad_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, _db) = library(&tmp.path().join("home"));
        let err = |p: &Path| target_for(&paths, p).unwrap_err().to_string();
        assert!(err(Path::new(r"\\nas\photos")).contains("network"));
        assert!(err(&tmp.path().join("Dropbox/Memora")).contains("Dropbox"));
        assert!(err(&tmp.path().join("OneDrive - Contoso")).contains("OneDrive"));
        assert!(err(&paths.data_dir).contains("already"));
        assert!(err(&paths.cache_dir).contains("outside"));
        assert_eq!(target_for(&paths, tmp.path()).unwrap(), tmp.path().join("Memora"), "next to the data, not around it");
        let full = tmp.path().join("full");
        std::fs::create_dir_all(full.join("Memora")).unwrap();
        std::fs::write(full.join("Memora/x.txt"), b"x").unwrap();
        assert!(err(&full).contains("not empty"));
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("library.db"), b"").unwrap();
        assert!(err(&other).contains("already has a Memora library"));
        assert_eq!(target_for(&paths, &tmp.path().join("new")).unwrap(), tmp.path().join("new"));
    }
}
