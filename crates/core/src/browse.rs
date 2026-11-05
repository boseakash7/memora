//! Groupings for browsing the library as a tree (the DLNA server): years, months and the
//! folders on disk. Only visible items count (indexed, not missing, not in the trash).

use r2d2_sqlite::rusqlite::params;

use crate::db::Db;
use crate::{Result, folders};

const VISIBLE: &str = "missing = 0 AND trashed_at IS NULL AND stage >= 1";

/// A year (`2024`) or month (`2024-03`) with its item count and newest item.
#[derive(Debug, Clone, PartialEq)]
pub struct Period {
    pub key: String,
    pub count: u64,
    pub cover: i64,
}

fn periods(db: &Db, len: usize, prefix: &str) -> Result<Vec<Period>> {
    let conn = db.conn()?;
    // SQLite returns `id` from the row that has max(taken_at): the newest item.
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT substr(taken_at, 1, ?1) AS k, count(*), id, max(taken_at) FROM media
         WHERE {VISIBLE} AND taken_at LIKE ?2 || '%'
         GROUP BY k ORDER BY k DESC"
    ))?;
    let rows = stmt.query_map(params![len as i64, prefix], |r| {
        Ok(Period { key: r.get(0)?, count: r.get::<_, i64>(1)? as u64, cover: r.get(2)? })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Years with items, newest first.
pub fn years(db: &Db) -> Result<Vec<Period>> {
    periods(db, 4, "")
}

/// Months of a year (`YYYY-MM`), newest first.
pub fn months(db: &Db, year: &str) -> Result<Vec<Period>> {
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Vec::new());
    }
    periods(db, 7, &format!("{year}-"))
}

/// What a folder on disk holds: subfolders (name, items inside them at any depth) and the
/// ids of the items directly in it. Both sorted by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub dirs: Vec<(String, u64)>,
    pub items: Vec<i64>,
}

/// List `rel` (a path relative to the watched folder, `""` for the folder itself, parts
/// separated by `/` or `\`).
pub fn folder(db: &Db, folder_id: i64, rel: &str) -> Result<Listing> {
    let root = folders::get(db, folder_id)?;
    let mut prefix = root.path.to_string_lossy().trim_end_matches(['\\', '/']).to_owned();
    for part in rel.split(['\\', '/']).filter(|p| !p.is_empty()) {
        prefix.push('\\');
        prefix.push_str(part);
    }
    prefix.push('\\');

    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT id, path FROM media
         WHERE folder_id = ?1 AND {VISIBLE} AND substr(replace(path, '/', '\\'), 1, ?2) = ?3 COLLATE NOCASE"
    ))?;
    let rows = stmt.query_map(params![folder_id, prefix.chars().count() as i64, prefix], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;

    let mut dirs: std::collections::BTreeMap<String, (String, u64)> = Default::default();
    let mut items: Vec<(String, i64)> = Vec::new();
    for row in rows {
        let (id, path) = row?;
        let rest: String = path.chars().skip(prefix.chars().count()).collect();
        match rest.split_once(['\\', '/']) {
            Some((dir, _)) => dirs.entry(dir.to_lowercase()).or_insert_with(|| (dir.to_owned(), 0)).1 += 1,
            None => items.push((rest.to_lowercase(), id)),
        }
    }
    items.sort();
    Ok(Listing {
        dirs: dirs.into_values().collect(),
        items: items.into_iter().map(|(_, id)| id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn add(db: &Db, folder: i64, path: &str, taken_at: &str, trashed: bool) {
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, stage, trashed_at, added_at)
                 VALUES (?1, ?2, 'x', 1, 1, 'photo', 'image/jpeg', ?3, 3, ?4, '')",
                params![folder, path, taken_at, trashed.then_some("2026-01-01")],
            )
            .unwrap();
    }

    #[test]
    fn years_months_and_folders() {
        let (dir, db) = test_db();
        let root = dir.path().join("Pics");
        std::fs::create_dir_all(&root).unwrap();
        let f = folders::add(&db, &root).unwrap();
        let r = f.path.to_string_lossy().to_string();
        add(&db, f.id, &format!(r"{r}\b.jpg"), "2024-03-05T10:00:00", false);
        add(&db, f.id, &format!(r"{r}\A.jpg"), "2024-03-01T10:00:00", false);
        add(&db, f.id, &format!(r"{r}\Trip\c.jpg"), "2023-12-31T10:00:00", false);
        add(&db, f.id, &format!(r"{r}\Trip\Day 2\d.jpg"), "2024-01-02T10:00:00", false);
        add(&db, f.id, &format!(r"{r}\Old\gone.jpg"), "2022-01-01T10:00:00", true);

        let years = years(&db).unwrap();
        assert_eq!(years.iter().map(|y| (y.key.as_str(), y.count)).collect::<Vec<_>>(), [("2024", 3), ("2023", 1)]);
        let march = &months(&db, "2024").unwrap()[0];
        assert_eq!((march.key.as_str(), march.count, march.cover), ("2024-03", 2, 1));
        assert!(months(&db, "20%").unwrap().is_empty());

        let top = folder(&db, f.id, "").unwrap();
        assert_eq!(top.dirs, [("Trip".to_owned(), 2)], "trashed-only folders are hidden");
        assert_eq!(top.items, [2, 1], "sorted by name");
        let trip = folder(&db, f.id, "trip").unwrap();
        assert_eq!((trip.dirs, trip.items), (vec![("Day 2".to_owned(), 1)], vec![3]));
        assert_eq!(folder(&db, f.id, "Trip/Day 2").unwrap().items, [4]);
    }
}
