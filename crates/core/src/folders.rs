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
        })
    }
}

pub fn list(db: &Db) -> Result<Vec<Folder>> {
    let conn = db.conn()?;
    let mut stmt = conn
        .prepare("SELECT id, path, enabled, added_at FROM folders ORDER BY path COLLATE NOCASE")?;
    let folders = stmt
        .query_map([], Folder::from_row)?
        .collect::<std::result::Result<_, _>>()?;
    Ok(folders)
}

pub fn get(db: &Db, id: i64) -> Result<Folder> {
    db.conn()?
        .query_row(
            "SELECT id, path, enabled, added_at FROM folders WHERE id = ?1",
            [id],
            Folder::from_row,
        )
        .optional()?
        .ok_or(Error::NotFound)
}

/// Add a folder. Rejects folders that are already covered by (or contain) an existing one,
/// so no file is ever indexed twice.
pub fn add(db: &Db, path: &Path) -> Result<Folder> {
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

    let conn = db.conn()?;
    conn.execute(
        "INSERT INTO folders (path, enabled, added_at) VALUES (?1, 1, ?2)",
        (path.to_string_lossy(), Utc::now().to_rfc3339()),
    )?;
    get(db, conn.last_insert_rowid())
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
