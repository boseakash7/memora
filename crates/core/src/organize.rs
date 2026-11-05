//! Favorites and the trash.
//!
//! "Trash" here is a soft delete: the file stays where it is and the item is hidden.
//! Only emptying the trash touches files, and then they go to the Windows Recycle Bin,
//! never a permanent delete.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use r2d2_sqlite::rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::{AppPaths, Result, thumbs};

/// Days an item stays in the trash before it is removed for good.
pub const TRASH_DAYS: i64 = 30;

const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

fn ids_json(ids: &[i64]) -> String {
    serde_json::to_string(ids).expect("ids serialize")
}

/// Returns how many items changed.
pub fn set_favorite(db: &Db, ids: &[i64], favorite: bool) -> Result<usize> {
    Ok(db.conn()?.execute(
        "UPDATE media SET favorite = ?2
         WHERE favorite != ?2 AND id IN (SELECT value FROM json_each(?1))",
        params![ids_json(ids), favorite],
    )?)
}

/// Hide items in the trash. Returns how many items changed.
pub fn trash(db: &Db, ids: &[i64]) -> Result<usize> {
    Ok(db.conn()?.execute(
        "UPDATE media SET trashed_at = ?2
         WHERE trashed_at IS NULL AND id IN (SELECT value FROM json_each(?1))",
        params![ids_json(ids), Utc::now().format(TIME_FORMAT).to_string()],
    )?)
}

/// Take items out of the trash. Returns how many items changed.
pub fn restore(db: &Db, ids: &[i64]) -> Result<usize> {
    Ok(db.conn()?.execute(
        "UPDATE media SET trashed_at = NULL
         WHERE trashed_at IS NOT NULL AND id IN (SELECT value FROM json_each(?1))",
        [ids_json(ids)],
    )?)
}

/// What an empty-trash run did.
#[derive(Debug, Default, Serialize)]
pub struct Deleted {
    /// Items removed from the library (their files are in the Recycle Bin).
    pub deleted: usize,
    /// Items that could not be removed, with the reason. They stay in the trash.
    pub failed: Vec<String>,
}

struct Row {
    id: i64,
    path: PathBuf,
    folder_id: i64,
}

/// Move trashed items' files to the Recycle Bin and forget the items.
/// `ids` limits it to some items; None means everything in the trash.
/// Items that are not in the trash are never touched.
pub fn empty_trash(db: &Db, paths: &AppPaths, ids: Option<&[i64]>) -> Result<Deleted> {
    let rows = match ids {
        Some(ids) => select_rows(
            db,
            "trashed_at IS NOT NULL AND id IN (SELECT value FROM json_each(?1))",
            Some(&ids_json(ids)),
        )?,
        None => select_rows(db, "trashed_at IS NOT NULL", None)?,
    };
    delete_rows(db, paths, rows, &move_to_recycle_bin)
}

/// Remove items that have been in the trash for more than [`TRASH_DAYS`] days.
pub fn purge_expired(db: &Db, paths: &AppPaths) -> Result<Deleted> {
    let cutoff = (Utc::now() - Duration::days(TRASH_DAYS)).format(TIME_FORMAT).to_string();
    let rows = select_rows(db, "trashed_at IS NOT NULL AND trashed_at < ?1", Some(&cutoff))?;
    delete_rows(db, paths, rows, &move_to_recycle_bin)
}

/// Rows matching `filter`, which uses `?1` if `arg` is given.
fn select_rows(db: &Db, filter: &str, arg: Option<&str>) -> Result<Vec<Row>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!("SELECT id, path, folder_id FROM media WHERE {filter}"))?;
    let map = |r: &r2d2_sqlite::rusqlite::Row| {
        Ok(Row { id: r.get(0)?, path: PathBuf::from(r.get::<_, String>(1)?), folder_id: r.get(2)? })
    };
    let rows = match arg {
        Some(arg) => stmt.query_map([arg], map)?.collect::<std::result::Result<_, _>>()?,
        None => stmt.query_map([], map)?.collect::<std::result::Result<_, _>>()?,
    };
    Ok(rows)
}

fn move_to_recycle_bin(path: &Path) -> std::result::Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

fn delete_rows(
    db: &Db,
    paths: &AppPaths,
    rows: Vec<Row>,
    remove_file: &dyn Fn(&Path) -> std::result::Result<(), String>,
) -> Result<Deleted> {
    let mut report = Deleted::default();
    let mut forgotten = Vec::new();
    // A missing file only counts as already deleted if its folder is reachable;
    // with an unplugged drive we can't tell, and must not drop the item.
    let mut folder_ok: HashMap<i64, bool> = HashMap::new();

    for row in rows {
        let name = row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match row.path.try_exists() {
            Ok(true) => {
                if let Err(e) = remove_file(&row.path) {
                    report.failed.push(format!("{name}: {e}"));
                    continue;
                }
            }
            Ok(false) => {
                let reachable = *folder_ok.entry(row.folder_id).or_insert_with(|| {
                    crate::folders::get(db, row.folder_id).map(|f| f.available).unwrap_or(false)
                });
                if !reachable {
                    report.failed.push(format!("{name}: its folder is not available right now"));
                    continue;
                }
            }
            Err(e) => {
                report.failed.push(format!("{name}: {e}"));
                continue;
            }
        }
        forgotten.push(row.id);
    }

    if !forgotten.is_empty() {
        db.conn()?.execute("DELETE FROM media WHERE id IN (SELECT value FROM json_each(?1))", [ids_json(&forgotten)])?;
        thumbs::remove_cached(paths, &forgotten);
        report.deleted = forgotten.len();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::albums;
    use crate::db::test_db;
    use crate::media::{self, Query, Scope};

    fn add_media(db: &Db, folder_id: i64, path: &Path, taken: &str) -> i64 {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, stage, added_at)
             VALUES (?1, ?2, ?3, 1, 1000, 'photo', 'image/jpeg', ?4, 2, '')",
            params![folder_id, path.to_string_lossy(), path.file_name().unwrap().to_string_lossy(), taken],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    struct Fixture {
        dir: tempfile::TempDir,
        db: Db,
        paths: AppPaths,
        folder_id: i64,
    }

    fn fixture() -> Fixture {
        let (dir, db) = test_db();
        let photos = dir.path().join("Photos");
        std::fs::create_dir_all(&photos).unwrap();
        let folder_id = crate::folders::add(&db, &photos).unwrap().id;
        let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();
        Fixture { dir, db, paths, folder_id }
    }

    impl Fixture {
        fn photo(&self, name: &str, taken: &str) -> i64 {
            let path = self.dir.path().join("Photos").join(name);
            std::fs::write(&path, b"x").unwrap();
            add_media(&self.db, self.folder_id, &path, taken)
        }

        fn ids(&self, q: &Query) -> Vec<i64> {
            media::query(&self.db, q).unwrap().iter().map(|i| i.id).collect()
        }
    }

    fn scope(scope: Scope) -> Query {
        Query { scope, ..Query::default() }
    }

    #[test]
    fn favorites() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        let b = f.photo("b.jpg", "2024-01-02T10:00:00");
        assert_eq!(set_favorite(&f.db, &[a, 999], true).unwrap(), 1);
        assert_eq!(set_favorite(&f.db, &[a], true).unwrap(), 0, "already a favorite");
        assert_eq!(f.ids(&scope(Scope::Favorites)), vec![a]);
        let all = media::timeline(&f.db).unwrap();
        assert_eq!(all.iter().map(|i| (i.id, i.f)).collect::<Vec<_>>(), vec![(b, false), (a, true)]);
        set_favorite(&f.db, &[a], false).unwrap();
        assert!(f.ids(&scope(Scope::Favorites)).is_empty());
    }

    #[test]
    fn trash_and_restore() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        let b = f.photo("b.jpg", "2024-01-02T10:00:00");
        set_favorite(&f.db, &[a], true).unwrap();

        assert_eq!(trash(&f.db, &[a, a]).unwrap(), 1);
        assert_eq!(f.ids(&Query::default()), vec![b]);
        assert_eq!(f.ids(&scope(Scope::Trash)), vec![a]);
        assert!(f.ids(&scope(Scope::Favorites)).is_empty(), "trashed items leave favorites");

        assert_eq!(restore(&f.db, &[a, b]).unwrap(), 1);
        assert_eq!(f.ids(&Query::default()), vec![b, a]);
        assert_eq!(f.ids(&scope(Scope::Favorites)), vec![a], "still a favorite after restore");
        assert!(f.ids(&scope(Scope::Trash)).is_empty());
    }

    #[test]
    fn emptying_the_trash_only_removes_trashed_items() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        let b = f.photo("b.jpg", "2024-01-02T10:00:00");
        let c = f.photo("c.jpg", "2024-01-03T10:00:00");
        trash(&f.db, &[a, c]).unwrap();

        // `b` is not in the trash, so asking for it does nothing.
        let rows = select_rows(&f.db, "trashed_at IS NOT NULL AND id IN (SELECT value FROM json_each(?1))", Some(&ids_json(&[a, b])))
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a]);

        let report = delete_rows(&f.db, &f.paths, rows, &|p| std::fs::remove_file(p).map_err(|e| e.to_string())).unwrap();
        assert_eq!((report.deleted, report.failed.len()), (1, 0));
        assert!(!f.dir.path().join("Photos/a.jpg").exists());
        assert!(f.dir.path().join("Photos/b.jpg").exists());
        assert!(f.dir.path().join("Photos/c.jpg").exists());
        assert_eq!(f.ids(&scope(Scope::Trash)), vec![c]);
        assert_eq!(f.ids(&Query::default()), vec![b]);
    }

    #[test]
    fn failed_deletes_stay_in_the_trash() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        let b = f.photo("b.jpg", "2024-01-02T10:00:00");
        trash(&f.db, &[a, b]).unwrap();
        let rows = select_rows(&f.db, "trashed_at IS NOT NULL", None).unwrap();
        let report = delete_rows(&f.db, &f.paths, rows, &|p| {
            if p.ends_with("a.jpg") { Err("in use".into()) } else { std::fs::remove_file(p).map_err(|e| e.to_string()) }
        })
        .unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(report.failed, vec!["a.jpg: in use".to_owned()]);
        assert_eq!(f.ids(&scope(Scope::Trash)), vec![a]);
    }

    #[test]
    fn files_already_gone_are_dropped_unless_the_folder_is_unavailable() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        trash(&f.db, &[a]).unwrap();
        std::fs::remove_file(f.dir.path().join("Photos/a.jpg")).unwrap();
        let never = |_: &Path| -> std::result::Result<(), String> { panic!("file does not exist") };

        // Folder unplugged: we can't know the file is really gone.
        std::fs::remove_dir(f.dir.path().join("Photos")).unwrap();
        let rows = select_rows(&f.db, "trashed_at IS NOT NULL", None).unwrap();
        let report = delete_rows(&f.db, &f.paths, rows, &never).unwrap();
        assert_eq!((report.deleted, report.failed.len()), (0, 1));

        std::fs::create_dir(f.dir.path().join("Photos")).unwrap();
        let rows = select_rows(&f.db, "trashed_at IS NOT NULL", None).unwrap();
        let report = delete_rows(&f.db, &f.paths, rows, &never).unwrap();
        assert_eq!((report.deleted, report.failed.len()), (1, 0));
    }

    #[test]
    fn purge_selects_only_expired_items() {
        let f = fixture();
        let old = f.photo("old.jpg", "2024-01-01T10:00:00");
        let recent = f.photo("recent.jpg", "2024-01-02T10:00:00");
        trash(&f.db, &[old, recent]).unwrap();
        let long_ago = (Utc::now() - Duration::days(TRASH_DAYS + 1)).format(TIME_FORMAT).to_string();
        f.db.conn().unwrap().execute("UPDATE media SET trashed_at = ?2 WHERE id = ?1", params![old, long_ago]).unwrap();

        let cutoff = (Utc::now() - Duration::days(TRASH_DAYS)).format(TIME_FORMAT).to_string();
        let rows = select_rows(&f.db, "trashed_at IS NOT NULL AND trashed_at < ?1", Some(&cutoff)).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![old]);
    }

    #[test]
    fn albums_lifecycle() {
        let f = fixture();
        let a = f.photo("a.jpg", "2024-01-01T10:00:00");
        let b = f.photo("b.jpg", "2024-01-02T10:00:00");
        let c = f.photo("c.jpg", "2024-01-03T10:00:00");

        assert!(albums::create(&f.db, "  ").is_err());
        let trip = albums::create(&f.db, " Trip ").unwrap();
        assert_eq!((trip.name.as_str(), trip.count, trip.cover_id), ("Trip", 0, None));

        assert_eq!(albums::add_items(&f.db, trip.id, &[a, b, 999]).unwrap(), 2);
        assert_eq!(albums::add_items(&f.db, trip.id, &[a, c]).unwrap(), 1, "a is already in");
        let album = albums::get(&f.db, trip.id).unwrap();
        assert_eq!((album.count, album.cover_id), (3, Some(c)), "newest item is the default cover");

        let q = Query { scope: Scope::Album, album: Some(trip.id), ..Query::default() };
        assert_eq!(f.ids(&q), vec![c, b, a]);

        albums::set_cover(&f.db, trip.id, a).unwrap();
        assert_eq!(albums::get(&f.db, trip.id).unwrap().cover_id, Some(a));
        let other = albums::create(&f.db, "Other").unwrap();
        assert!(albums::set_cover(&f.db, other.id, a).is_err(), "a is not in Other");

        // Trashed items disappear from the album, and come back on restore.
        trash(&f.db, &[a]).unwrap();
        let album = albums::get(&f.db, trip.id).unwrap();
        assert_eq!((album.count, album.cover_id), (2, Some(c)), "cover falls back when trashed");
        assert_eq!(f.ids(&q), vec![c, b]);
        restore(&f.db, &[a]).unwrap();
        assert_eq!(albums::get(&f.db, trip.id).unwrap().cover_id, Some(a));

        assert_eq!(albums::remove_items(&f.db, trip.id, &[a, b]).unwrap(), 2);
        assert_eq!(f.ids(&q), vec![c]);
        assert_eq!(albums::for_media(&f.db, c).unwrap()[0].name, "Trip");
        assert!(albums::for_media(&f.db, a).unwrap().is_empty());

        assert!(matches!(albums::delete(&f.db, 999), Err(crate::Error::NotFound)));
        albums::delete(&f.db, trip.id).unwrap();
        assert_eq!(f.ids(&Query::default()).len(), 3, "photos survive their album");
        assert_eq!(albums::list(&f.db).unwrap().len(), 1);
        assert!(matches!(
            media::query(&f.db, &Query { scope: Scope::Album, album: Some(trip.id), ..Query::default() }),
            Err(crate::Error::NotFound)
        ));
    }

    #[test]
    fn search() {
        let f = fixture();
        let a = f.photo("IMG_2024_beach.jpg", "2024-06-01T10:00:00");
        let b = f.photo("sunset.mp4", "2023-12-24T18:00:00");
        let c = f.photo("café.jpg", "2024-06-02T09:00:00");
        {
            let conn = f.db.conn().unwrap();
            conn.execute("UPDATE media SET camera_make = 'OnePlus', camera_model = 'A6003' WHERE id = ?1", [a]).unwrap();
            conn.execute("UPDATE media SET kind = 'video' WHERE id = ?1", [b]).unwrap();
        }
        let q = |text: &str| Query { q: Some(text.into()), ..Query::default() };

        assert_eq!(f.ids(&q("beach")), vec![a]);
        assert_eq!(f.ids(&q("BEA")), vec![a], "prefix, any case");
        assert_eq!(f.ids(&q("img 2024")), vec![a], "all words must match");
        assert_eq!(f.ids(&q("img_2024")), vec![a], "punctuation splits words");
        assert_eq!(f.ids(&q("photos")), vec![c, a, b], "folder name matches");
        assert_eq!(f.ids(&q("oneplus a6003")), vec![a], "camera");
        assert_eq!(f.ids(&q("cafe")), vec![c], "accents are ignored");
        assert!(f.ids(&q("nothing")).is_empty());
        assert_eq!(f.ids(&q("\" OR * NEAR(")).len(), 0, "operators in the text are harmless");
        assert_eq!(f.ids(&q("   ")).len(), 3, "blank text means no text filter");

        // Filters combine.
        let kind = |k: &str| Query { kind: Some(k.into()), ..Query::default() };
        assert_eq!(f.ids(&kind("video")), vec![b]);
        assert!(media::query(&f.db, &kind("audio")).is_err());
        let range = |from: &str, to: &str| Query { from: Some(from.into()), to: Some(to.into()), ..Query::default() };
        assert_eq!(f.ids(&range("2024-06-01", "2024-06-01")), vec![a], "range includes both end days");
        assert_eq!(f.ids(&range("2024-01-01", "2024-12-31")), vec![c, a]);
        assert!(media::query(&f.db, &range("yesterday", "")).is_err());
        let cam = Query { camera: Some("OnePlus A6003".into()), ..Query::default() };
        assert_eq!(f.ids(&cam), vec![a]);
        assert_eq!(media::cameras(&f.db).unwrap(), vec!["OnePlus A6003"]);

        // Album names are searchable, and follow renames and removals.
        let album = albums::create(&f.db, "Holiday Greece").unwrap();
        albums::add_items(&f.db, album.id, &[b]).unwrap();
        assert_eq!(f.ids(&q("greece")), vec![b]);
        albums::rename(&f.db, album.id, "Summer").unwrap();
        assert!(f.ids(&q("greece")).is_empty());
        assert_eq!(f.ids(&q("summer")), vec![b]);
        albums::remove_items(&f.db, album.id, &[b]).unwrap();
        assert!(f.ids(&q("summer")).is_empty());
        albums::add_items(&f.db, album.id, &[b]).unwrap();
        albums::delete(&f.db, album.id).unwrap();
        assert!(f.ids(&q("summer")).is_empty(), "deleting an album removes its name from the index");

        // Trashed items are not found.
        trash(&f.db, &[a]).unwrap();
        assert!(f.ids(&q("beach")).is_empty());

        // Renames and deletes keep the index current.
        f.db.conn().unwrap().execute("UPDATE media SET path = replace(path, 'sunset', 'dawn') WHERE id = ?1", [b]).unwrap();
        assert_eq!(f.ids(&q("dawn")), vec![b]);
        assert!(f.ids(&q("sunset")).is_empty());
        f.db.conn().unwrap().execute("DELETE FROM media WHERE id = ?1", [b]).unwrap();
        assert!(f.ids(&q("dawn")).is_empty());
    }
}
