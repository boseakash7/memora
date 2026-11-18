use std::path::{Path, PathBuf};

use chrono::Utc;
use r2d2_sqlite::rusqlite::{OptionalExtension, Row};
use serde::Serialize;

use crate::db::Db;
use crate::{Error, Result};

/// A folder the user added to the library. Files inside are indexed in place.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: i64,
    pub path: PathBuf,
    pub enabled: bool,
    pub added_at: String,
    /// False when the folder is currently unreachable (e.g. unplugged drive).
    pub available: bool,
    /// Library info (favorites, albums, people...) is kept in a `.memora` folder too.
    pub sync: bool,
    /// The folder holding that `.memora`: this folder, or a parent that had one already.
    pub sync_root: Option<PathBuf>,
    /// While syncing: also keep the finished index there, so other PCs skip indexing.
    pub keep_index: bool,
}

impl Folder {
    fn from_row(row: &Row) -> r2d2_sqlite::rusqlite::Result<Self> {
        let path = PathBuf::from(row.get::<_, String>(1)?);
        Ok(Folder {
            id: row.get(0)?,
            available: path.is_dir(),
            path,
            enabled: row.get(2)?,
            added_at: row.get(3)?,
            sync: row.get(4)?,
            sync_root: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
            keep_index: row.get(6)?,
        })
    }
}

pub fn list(db: &Db) -> Result<Vec<Folder>> {
    let conn = db.conn()?;
    let mut stmt = conn
        .prepare("SELECT id, path, enabled, added_at, sync, sync_root, keep_index FROM folders ORDER BY path COLLATE NOCASE")?;
    let folders = stmt
        .query_map([], Folder::from_row)?
        .collect::<std::result::Result<_, _>>()?;
    Ok(folders)
}

pub fn get(db: &Db, id: i64) -> Result<Folder> {
    db.conn()?
        .query_row(
            "SELECT id, path, enabled, added_at, sync, sync_root, keep_index FROM folders WHERE id = ?1",
            [id],
            Folder::from_row,
        )
        .optional()?
        .ok_or(Error::NotFound)
}

/// Add a folder. Rejects folders that are already covered by (or contain) an existing one,
/// so no file is ever indexed twice.
pub fn add(db: &Db, path: &Path) -> Result<Folder> {
    add_with(db, path, false)
}

/// Add a folder. With `keep_info`, it keeps its library info in a `.memora` folder from the
/// start (its own, or a parent's that has one), see `sync`. Set in the same write as the
/// folder itself, so no scan ever sees the folder without it.
pub fn add_with(db: &Db, path: &Path, keep_info: bool) -> Result<Folder> {
    let path = dunce::canonicalize(path)
        .map_err(|_| Error::invalid(format!("folder not found: {}", path.display())))?;
    if !path.is_dir() {
        return Err(Error::invalid(format!("not a folder: {}", path.display())));
    }

    for existing in list(db)? {
        if same_or_inside(&path, &existing.path) || same_or_inside(&existing.path, &path) {
            return Err(Error::FolderExists(existing.path));
        }
    }

    let root = keep_info.then(|| crate::sync::prepare_root(&path));
    let conn = db.conn()?;
    conn.execute(
        "INSERT INTO folders (path, enabled, added_at, sync, sync_root) VALUES (?1, 1, ?2, ?3, ?4)",
        (path.to_string_lossy(), Utc::now().to_rfc3339(), keep_info, root.map(|r| r.to_string_lossy().into_owned())),
    )?;
    get(db, conn.last_insert_rowid())
}

/// The folder was moved or its drive letter changed: point it (and every item in it) at
/// `new_path`, so nothing is indexed again and favorites, albums and people stay.
/// Refuses folders that overlap another watched folder, and folders where none of the
/// indexed files are (to catch picking the wrong folder).
pub fn relocate(db: &Db, id: i64, new_path: &Path) -> Result<Folder> {
    let old = get(db, id)?;
    let new_path = dunce::canonicalize(new_path)
        .map_err(|_| Error::invalid(format!("folder not found: {}", new_path.display())))?;
    if !new_path.is_dir() {
        return Err(Error::invalid(format!("not a folder: {}", new_path.display())));
    }
    for existing in list(db)?.into_iter().filter(|f| f.id != id) {
        if same_or_inside(&new_path, &existing.path) || same_or_inside(&existing.path, &new_path) {
            return Err(Error::FolderExists(existing.path));
        }
    }
    let old_prefix = format!("{}\\", old.path.to_string_lossy().trim_end_matches(['\\', '/']));
    let new_prefix = format!("{}\\", new_path.to_string_lossy().trim_end_matches(['\\', '/']));

    let mut conn = db.conn()?;
    // Some of the indexed files must be in the new place (a sample, newest first).
    let sample: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT path FROM media WHERE folder_id = ?1 AND missing = 0 ORDER BY added_at DESC LIMIT 200",
        )?;
        stmt.query_map([id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    let moved = |p: &String| -> Option<PathBuf> {
        if !p.to_lowercase().starts_with(&old_prefix.to_lowercase()) {
            return None;
        }
        let rest: String = p.chars().skip(old_prefix.chars().count()).collect();
        Some(PathBuf::from(format!("{new_prefix}{rest}")))
    };
    if !sample.is_empty() && !sample.iter().filter_map(moved).any(|p| p.is_file()) {
        return Err(Error::invalid(format!(
            "none of the photos from {} are in {}; choose the folder they were moved to",
            old.path.display(),
            new_path.display()
        )));
    }

    let tx = conn.transaction()?;
    tx.execute("UPDATE folders SET path = ?2 WHERE id = ?1", (id, new_path.to_string_lossy()))?;
    tx.execute(
        "UPDATE media SET path = ?2 || substr(path, length(?3) + 1)
         WHERE folder_id = ?1 AND substr(path, 1, length(?3)) = ?3 COLLATE NOCASE",
        (id, &new_prefix, &old_prefix),
    )?;
    tx.commit()?;
    drop(conn);
    get(db, id)
}

pub fn remove(db: &Db, id: i64) -> Result<()> {
    let changed = db.conn()?.execute("DELETE FROM folders WHERE id = ?1", [id])?;
    if changed == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

/// Windows paths are case-insensitive, so compare lowercased components.
pub(crate) fn same_or_inside(path: &Path, base: &Path) -> bool {
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    lower(path).starts_with(lower(base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[test]
    fn add_list_remove() {
        let (dir, db) = test_db();
        let photos = dir.path().join("Photos");
        std::fs::create_dir_all(photos.join("2024")).unwrap();

        let f = add(&db, &photos).unwrap();
        assert!(f.available && f.enabled);
        assert_eq!(list(&db).unwrap().len(), 1);

        // Same folder, a subfolder, or a parent are all rejected.
        assert!(matches!(add(&db, &photos), Err(Error::FolderExists(_))));
        assert!(matches!(add(&db, &photos.join("2024")), Err(Error::FolderExists(_))));
        assert!(matches!(add(&db, dir.path()), Err(Error::FolderExists(_))));

        remove(&db, f.id).unwrap();
        assert!(list(&db).unwrap().is_empty());
        assert!(matches!(remove(&db, f.id), Err(Error::NotFound)));
    }

    #[test]
    fn relocate_rewrites_paths() {
        let (dir, db) = test_db();
        let old = dir.path().join("E/Photos");
        std::fs::create_dir_all(old.join("2024")).unwrap();
        let f = add(&db, &old).unwrap();
        let other = add(&db, &{
            let o = dir.path().join("Other");
            std::fs::create_dir_all(&o).unwrap();
            o
        })
        .unwrap();
        let insert = |path: &Path| {
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, added_at)
                     VALUES (?1, ?2, 'x', 1, 1, 'photo', 'image/jpeg', '', '')",
                    (f.id, path.to_string_lossy()),
                )
                .unwrap()
        };
        insert(&f.path.join("2024").join("a.jpg"));
        insert(&f.path.join("b.jpg"));

        // The drive letter changed: E: is gone, the same folder is now on F:.
        let new = dir.path().join("F/Photos");
        std::fs::create_dir_all(new.join("2024")).unwrap();
        // Not the right folder yet: none of the files are there.
        assert!(relocate(&db, f.id, &new).unwrap_err().to_string().contains("none of the photos"));
        std::fs::write(new.join("2024/a.jpg"), b"x").unwrap();
        assert!(matches!(relocate(&db, f.id, &other.path), Err(Error::FolderExists(_))));

        let moved = relocate(&db, f.id, &new).unwrap();
        assert_eq!(moved.path, dunce::canonicalize(&new).unwrap());
        let paths: Vec<String> = {
            let conn = db.conn().unwrap();
            let mut stmt = conn.prepare("SELECT path FROM media ORDER BY id").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().map(|p| p.unwrap()).collect()
        };
        assert_eq!(paths, [moved.path.join("2024").join("a.jpg"), moved.path.join("b.jpg")].map(|p| p.to_string_lossy().into_owned()));
        assert!(matches!(relocate(&db, 999, &new), Err(Error::NotFound)));
    }

    #[test]
    fn rejects_missing_and_files() {
        let (dir, db) = test_db();
        assert!(add(&db, &dir.path().join("nope")).is_err());
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(add(&db, &file).is_err());
    }

    #[test]
    fn sibling_with_common_prefix_is_allowed() {
        let (dir, db) = test_db();
        std::fs::create_dir_all(dir.path().join("Photos")).unwrap();
        std::fs::create_dir_all(dir.path().join("Photos2")).unwrap();
        add(&db, &dir.path().join("Photos")).unwrap();
        add(&db, &dir.path().join("Photos2")).unwrap();
    }
}
