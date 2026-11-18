//! Finds photos and videos on disk and keeps the `media` table in step with them.
//! Only cheap file-system facts are recorded here; metadata and thumbnails come later
//! from the background pipeline (see `library`).

use std::collections::HashMap;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::Utc;
use r2d2_sqlite::rusqlite::{Connection, OptionalExtension, params};
use walkdir::{DirEntry, WalkDir};

use crate::Result;
use crate::db::Db;
use crate::folders::{Folder, same_or_inside};
use crate::media::{Format, Stage, file_type};
use crate::metadata::{DATE_FORMAT, local_time_from_millis};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub found: u64,
    pub added: u64,
    pub changed: u64,
    pub missing: u64,
}

impl ScanStats {
    pub fn any_change(&self) -> bool {
        self.added + self.changed + self.missing > 0
    }
}

/// A file-system change reported by the watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
    Renamed(PathBuf, PathBuf),
}

struct Existing {
    id: i64,
    size: i64,
    mtime: i64,
    missing: bool,
}

enum Op {
    Insert { path: PathBuf, size: i64, mtime: i64, mime: &'static str, format: Format },
    /// Content changed: run the whole pipeline again.
    Reset { id: i64, size: i64, mtime: i64 },
    /// Was missing, is back unchanged.
    Restore { id: i64 },
    Missing { id: i64 },
}

/// Walk a whole folder and reconcile it with the database.
/// `progress` is called with the number of media files seen so far.
pub fn scan_folder(db: &Db, folder: &Folder, progress: &mut dyn FnMut(u64)) -> Result<ScanStats> {
    let mut existing = load_existing(db, folder.id)?;
    let mut stats = ScanStats::default();
    let mut ops = Vec::new();
    let mut unreadable = Vec::new();

    walk(&folder.path, &mut unreadable, &mut |path, size, mtime, mime, format| {
        stats.found += 1;
        if stats.found % 250 == 0 {
            progress(stats.found);
        }
        let old = existing.remove(&key(&path));
        if let Some(op) = diff(old.as_ref(), path, size, mtime, mime, format) {
            ops.push(op);
        }
    });
    progress(stats.found);

    // Whatever wasn't seen is gone, unless it sits in a folder we couldn't read.
    for (path, old) in existing {
        let in_unreadable = unreadable.iter().any(|dir| same_or_inside(Path::new(&path), dir));
        if !old.missing && !in_unreadable {
            ops.push(Op::Missing { id: old.id });
        }
    }

    apply(db, folder.id, &ops, &mut stats)?;
    Ok(stats)
}

/// Apply watcher events. Returns true if anything visible changed.
pub fn apply_changes(db: &Db, folders: &[Folder], changes: &[Change]) -> Result<bool> {
    let mut changed = false;
    for change in changes {
        changed |= match change {
            Change::Created(p) => sync_path(db, folders, p, true)?,
            Change::Modified(p) => sync_path(db, folders, p, false)?,
            Change::Removed(p) => sync_path(db, folders, p, true)?,
            Change::Renamed(from, to) => rename(db, folders, from, to)?,
        };
    }
    Ok(changed)
}

/// Bring one path up to date. Directories are walked only when `walk_dirs` is set
/// (creates and moves), not on every "contents changed" notification.
fn sync_path(db: &Db, folders: &[Folder], path: &Path, walk_dirs: bool) -> Result<bool> {
    let Some(folder) = folders.iter().find(|f| same_or_inside(path, &f.path)) else {
        return Ok(false);
    };
    if path.is_dir() {
        if !walk_dirs {
            return Ok(false);
        }
        let mut ops = Vec::new();
        let conn = db.conn()?;
        walk(path, &mut Vec::new(), &mut |p, size, mtime, mime, format| {
            let old = find_existing(&conn, &p).ok().flatten();
            if let Some(op) = diff(old.as_ref(), p, size, mtime, mime, format) {
                ops.push(op);
            }
        });
        drop(conn);
        let mut stats = ScanStats::default();
        apply(db, folder.id, &ops, &mut stats)?;
        return Ok(stats.any_change());
    }
    if path.is_file() {
        let Some((mime, format)) = file_type(path) else { return Ok(false) };
        let Some((size, mtime)) = path.metadata().ok().map(|m| stat(&m)) else { return Ok(false) };
        let old = find_existing(&*db.conn()?, path)?;
        let mut stats = ScanStats::default();
        if let Some(op) = diff(old.as_ref(), path.to_path_buf(), size, mtime, mime, format) {
            apply(db, folder.id, &[op], &mut stats)?;
        }
        return Ok(stats.any_change());
    }
    mark_missing(db, path)
}

fn rename(db: &Db, folders: &[Folder], from: &Path, to: &Path) -> Result<bool> {
    let target = folders.iter().find(|f| same_or_inside(to, &f.path));
    let Some(target) = target else {
        // Moved out of the library.
        return mark_missing(db, from);
    };
    let to_str = to.to_string_lossy();
    let conn = db.conn()?;

    let moved = if to.is_dir() {
        let from_prefix = dir_prefix(from);
        conn.execute(
            "UPDATE media SET path = ?1 || substr(path, length(?2) + 1), folder_id = ?3
             WHERE substr(path, 1, length(?4)) = ?4 COLLATE NOCASE",
            params![to_str, from.to_string_lossy(), target.id, from_prefix],
        )
    } else if file_type(to).is_some() {
        let name = file_name(to);
        conn.execute(
            "UPDATE media SET path = ?1, name = ?2, folder_id = ?3, missing = 0 WHERE path = ?4",
            params![to_str, name, target.id, from.to_string_lossy()],
        )
    } else {
        Ok(0)
    };
    drop(conn);

    match moved {
        Ok(n) => {
            // Pick up anything at the new location we didn't know about (or a changed file).
            let synced = sync_path(db, folders, to, true)?;
            if file_type(to).is_none() && !to.is_dir() {
                // Renamed to a non-media name: the old entry is gone.
                return Ok(mark_missing(db, from)? || synced);
            }
            Ok(n > 0 || synced)
        }
        // Something already sits at the new path: treat as delete + create.
        Err(_) => Ok(mark_missing(db, from)? | sync_path(db, folders, to, true)?),
    }
}

/// Mark a file, or everything under a folder, as missing.
fn mark_missing(db: &Db, path: &Path) -> Result<bool> {
    let n = db.conn()?.execute(
        "UPDATE media SET missing = 1
         WHERE missing = 0 AND (path = ?1 OR substr(path, 1, length(?2)) = ?2 COLLATE NOCASE)",
        params![path.to_string_lossy(), dir_prefix(path)],
    )?;
    Ok(n > 0)
}

// ---------- helpers ----------

fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn dir_prefix(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if !s.ends_with(MAIN_SEPARATOR_STR) {
        s.push_str(MAIN_SEPARATOR_STR);
    }
    s
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stat(meta: &std::fs::Metadata) -> (i64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (meta.len() as i64, mtime)
}

fn diff(
    old: Option<&Existing>,
    path: PathBuf,
    size: i64,
    mtime: i64,
    mime: &'static str,
    format: Format,
) -> Option<Op> {
    match old {
        None => Some(Op::Insert { path, size, mtime, mime, format }),
        Some(o) if o.size != size || o.mtime != mtime => Some(Op::Reset { id: o.id, size, mtime }),
        Some(o) if o.missing => Some(Op::Restore { id: o.id }),
        Some(_) => None,
    }
}

/// Folders that never hold the user's photos.
fn skip_dir(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    if name.starts_with('.') || name.starts_with('$') || name.eq_ignore_ascii_case("System Volume Information") {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if let Ok(m) = entry.metadata()
            && m.file_attributes() & (HIDDEN | SYSTEM) != 0
        {
            return true;
        }
    }
    false
}

fn walk(
    root: &Path,
    unreadable: &mut Vec<PathBuf>,
    f: &mut dyn FnMut(PathBuf, i64, i64, &'static str, Format),
) {
    for entry in WalkDir::new(root).into_iter().filter_entry(|e| !skip_dir(e)) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", e.path().map(Path::display).map(|d| d.to_string()).unwrap_or_default());
                if let Some(p) = e.path() {
                    unreadable.push(p.to_path_buf());
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some((mime, format)) = file_type(entry.path()) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let (size, mtime) = stat(&meta);
        f(entry.into_path(), size, mtime, mime, format);
    }
}

fn load_existing(db: &Db, folder_id: i64) -> Result<HashMap<String, Existing>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare("SELECT id, path, size, mtime, missing FROM media WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder_id], |r| {
        Ok((
            r.get::<_, String>(1)?.to_lowercase(),
            Existing { id: r.get(0)?, size: r.get(2)?, mtime: r.get(3)?, missing: r.get(4)? },
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn find_existing(conn: &Connection, path: &Path) -> Result<Option<Existing>> {
    Ok(conn
        .query_row(
            "SELECT id, size, mtime, missing FROM media WHERE path = ?1",
            [path.to_string_lossy()],
            |r| Ok(Existing { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, missing: r.get(3)? }),
        )
        .optional()?)
}

fn apply(db: &Db, folder_id: i64, ops: &[Op], stats: &mut ScanStats) -> Result<()> {
    if ops.is_empty() {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    let mut conn = db.conn()?;
    for chunk in ops.chunks(1000) {
        let tx = conn.transaction()?;
        for op in chunk {
            match op {
                Op::Insert { path, size, mtime, mime, format } => {
                    let kind = if *format == Format::Video { "video" } else { "photo" };
                    let taken_at = local_time_from_millis(*mtime).format(DATE_FORMAT).to_string();
                    tx.prepare_cached(
                        "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, stage, added_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    )?
                    .execute(params![
                        folder_id,
                        path.to_string_lossy(),
                        file_name(path),
                        size,
                        mtime,
                        kind,
                        mime,
                        taken_at,
                        Stage::NEW,
                        now
                    ])?;
                    stats.added += 1;
                }
                Op::Reset { id, size, mtime } => {
                    tx.prepare_cached(
                        "UPDATE media SET size = ?2, mtime = ?3, missing = 0, stage = ?4, error = NULL, imported = 0 WHERE id = ?1",
                    )?
                    .execute(params![id, size, mtime, Stage::NEW])?;
                    stats.changed += 1;
                }
                Op::Restore { id } => {
                    tx.prepare_cached("UPDATE media SET missing = 0 WHERE id = ?1")?.execute([id])?;
                    stats.changed += 1;
                }
                Op::Missing { id } => {
                    tx.prepare_cached("UPDATE media SET missing = 1 WHERE id = ?1")?.execute([id])?;
                    stats.missing += 1;
                }
            }
        }
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::folders;

    fn png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbImage::new(4, 3).save(path).unwrap();
    }

    fn visible(db: &Db) -> Vec<String> {
        let conn = db.conn().unwrap();
        let mut stmt = conn.prepare("SELECT name FROM media WHERE missing = 0 ORDER BY name").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn scan_adds_updates_and_marks_missing() {
        let (dir, db) = test_db();
        let root = dir.path().join("Photos");
        png(&root.join("a.png"));
        png(&root.join("sub/b.png"));
        png(&root.join(".hidden/c.png"));
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        let folder = folders::add(&db, &root).unwrap();

        let s = scan_folder(&db, &folder, &mut |_| {}).unwrap();
        assert_eq!((s.found, s.added), (2, 2));
        assert_eq!(visible(&db), ["a.png", "b.png"]);

        // Unchanged rescan does nothing.
        assert!(!scan_folder(&db, &folder, &mut |_| {}).unwrap().any_change());

        std::fs::remove_file(root.join("a.png")).unwrap();
        let s = scan_folder(&db, &folder, &mut |_| {}).unwrap();
        assert_eq!(s.missing, 1);
        assert_eq!(visible(&db), ["b.png"]);

        // Coming back restores the same row.
        png(&root.join("a.png"));
        let s = scan_folder(&db, &folder, &mut |_| {}).unwrap();
        assert_eq!((s.added, s.changed), (0, 1));
        assert_eq!(visible(&db), ["a.png", "b.png"]);
    }

    #[test]
    fn watcher_changes() {
        let (dir, db) = test_db();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(&root).unwrap();
        let folder = folders::add(&db, &root).unwrap();
        let all = vec![folder.clone()];
        let root = folder.path.clone();

        png(&root.join("x.png"));
        assert!(apply_changes(&db, &all, &[Change::Created(root.join("x.png"))]).unwrap());
        let id: i64 = db.conn().unwrap().query_row("SELECT id FROM media", [], |r| r.get(0)).unwrap();

        // File rename keeps the row (and so its thumbnail, favorites, albums).
        std::fs::rename(root.join("x.png"), root.join("y.png")).unwrap();
        apply_changes(&db, &all, &[Change::Renamed(root.join("x.png"), root.join("y.png"))]).unwrap();
        assert_eq!(visible(&db), ["y.png"]);

        // Folder rename moves everything under it.
        png(&root.join("trip/z.png"));
        apply_changes(&db, &all, &[Change::Created(root.join("trip"))]).unwrap();
        std::fs::rename(root.join("trip"), root.join("Trip 2024")).unwrap();
        apply_changes(&db, &all, &[Change::Renamed(root.join("trip"), root.join("Trip 2024"))]).unwrap();
        let moved: String = db
            .conn()
            .unwrap()
            .query_row("SELECT path FROM media WHERE name = 'z.png'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(PathBuf::from(moved), root.join("Trip 2024").join("z.png"));

        // Deleting the folder hides its contents; the renamed file kept its id.
        std::fs::remove_dir_all(root.join("Trip 2024")).unwrap();
        assert!(apply_changes(&db, &all, &[Change::Removed(root.join("Trip 2024"))]).unwrap());
        assert_eq!(visible(&db), ["y.png"]);
        let still: i64 = db.conn().unwrap().query_row("SELECT id FROM media WHERE name='y.png'", [], |r| r.get(0)).unwrap();
        assert_eq!(still, id);

        // Paths outside the library are ignored.
        assert!(!apply_changes(&db, &all, &[Change::Created(dir.path().join("elsewhere.png"))]).unwrap());
    }
}
