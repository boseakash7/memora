//! Albums: named collections of photos and videos. Items stay where they are on disk.
//! Items shown in an album are sorted by date like everywhere else; there is no manual order.

use chrono::Utc;
use r2d2_sqlite::rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::db::Db;
use crate::{Error, Result, sync};

const MAX_NAME: usize = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    /// Visible items (not trashed, not missing).
    pub count: i64,
    /// Cover photo: the chosen one, else the newest item. None for an empty album.
    pub cover_id: Option<i64>,
    /// Modification time (seconds) of the cover's file, for the thumbnail URL.
    pub cover_m: Option<i64>,
}

/// An album an item belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct AlbumRef {
    pub id: i64,
    pub name: String,
}

use crate::media::VISIBLE_M as VISIBLE;

fn select_albums(filter: &str) -> String {
    format!(
        "SELECT x.id, x.name, x.created_at, x.cnt, x.cover,
                (SELECT mtime / 1000 FROM media WHERE id = x.cover)
         FROM (
            SELECT a.id, a.name, a.created_at,
                (SELECT count(*) FROM album_items i JOIN media m ON m.id = i.media_id
                  WHERE i.album_id = a.id AND {VISIBLE}) AS cnt,
                coalesce(
                    (SELECT m.id FROM media m WHERE m.id = a.cover_media_id AND {VISIBLE}),
                    (SELECT m.id FROM album_items i JOIN media m ON m.id = i.media_id
                      WHERE i.album_id = a.id AND {VISIBLE} AND m.stage >= 1
                      ORDER BY m.taken_at DESC, m.id DESC LIMIT 1)) AS cover
            FROM albums a {filter}
         ) x
         ORDER BY x.created_at DESC, x.id DESC"
    )
}

fn from_row(r: &Row) -> r2d2_sqlite::rusqlite::Result<Album> {
    Ok(Album {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        count: r.get(3)?,
        cover_id: r.get(4)?,
        cover_m: r.get(5)?,
    })
}

pub fn list(db: &Db) -> Result<Vec<Album>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&select_albums(""))?;
    Ok(stmt.query_map([], from_row)?.collect::<std::result::Result<_, _>>()?)
}

pub fn get(db: &Db, id: i64) -> Result<Album> {
    let conn = db.conn()?;
    conn.query_row(&select_albums("WHERE a.id = ?1"), [id], from_row)
        .optional()?
        .ok_or(Error::NotFound)
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::invalid("give the album a name"));
    }
    if name.chars().count() > MAX_NAME {
        return Err(Error::invalid(format!("album names can have at most {MAX_NAME} characters")));
    }
    Ok(name.to_owned())
}

/// `[1,2,3]` for `json_each`, which lets one statement take any number of ids.
fn ids_json(ids: &[i64]) -> String {
    serde_json::to_string(ids).expect("ids serialize")
}

pub fn create(db: &Db, name: &str) -> Result<Album> {
    let name = clean_name(name)?;
    let id = {
        let conn = db.conn()?;
        conn.execute(
            "INSERT INTO albums (name, created_at) VALUES (?1, ?2)",
            params![name, Utc::now().to_rfc3339()],
        )?;
        conn.last_insert_rowid()
    };
    get(db, id)
}

pub fn rename(db: &Db, id: i64, name: &str) -> Result<Album> {
    let name = clean_name(name)?;
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE albums SET name = ?2 WHERE id = ?1", params![id, name])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    sync::record_album(&tx, id, false)?;
    tx.commit()?;
    drop(conn);
    get(db, id)
}

/// Use an item of the album as its cover.
pub fn set_cover(db: &Db, id: i64, media_id: i64) -> Result<Album> {
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE albums SET cover_media_id = ?2
         WHERE id = ?1 AND EXISTS (SELECT 1 FROM album_items WHERE album_id = ?1 AND media_id = ?2)",
        params![id, media_id],
    )?;
    if n == 0 {
        drop(tx);
        drop(conn);
        get(db, id)?; // NotFound if the album is unknown
        return Err(Error::invalid("that item is not in the album"));
    }
    sync::record_album_cover(&tx, id, media_id)?;
    tx.commit()?;
    drop(conn);
    get(db, id)
}

/// Deleting an album never touches its photos.
pub fn delete(db: &Db, id: i64) -> Result<()> {
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    // Recorded while its items still show which synced folders know it.
    sync::record_album(&tx, id, true)?;
    // Remove the items first so the search index drops the album's name from them.
    tx.execute("DELETE FROM album_items WHERE album_id = ?1", [id])?;
    let n = tx.execute("DELETE FROM albums WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    tx.commit()?;
    Ok(())
}

/// Add items to an album. Items already in it, and unknown ids, are skipped.
/// Returns how many were added.
pub fn add_items(db: &Db, id: i64, media_ids: &[i64]) -> Result<usize> {
    get(db, id)?;
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let new: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT m.id FROM media m WHERE m.id IN (SELECT value FROM json_each(?2)) AND m.missing = 0
             AND NOT EXISTS (SELECT 1 FROM album_items WHERE album_id = ?1 AND media_id = m.id)",
        )?;
        stmt.query_map(params![id, ids_json(media_ids)], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    tx.execute(
        "INSERT OR IGNORE INTO album_items (album_id, media_id, added_at)
         SELECT ?1, value, ?3 FROM json_each(?2)",
        params![id, ids_json(&new), Utc::now().to_rfc3339()],
    )?;
    sync::record_album_items(&tx, id, &new, true)?;
    tx.commit()?;
    Ok(new.len())
}

/// Take items out of an album (the photos themselves stay). Returns how many were removed.
pub fn remove_items(db: &Db, id: i64, media_ids: &[i64]) -> Result<usize> {
    get(db, id)?;
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let gone: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT media_id FROM album_items WHERE album_id = ?1 AND media_id IN (SELECT value FROM json_each(?2))",
        )?;
        stmt.query_map(params![id, ids_json(media_ids)], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    tx.execute(
        "DELETE FROM album_items WHERE album_id = ?1 AND media_id IN (SELECT value FROM json_each(?2))",
        params![id, ids_json(&gone)],
    )?;
    sync::record_album_items(&tx, id, &gone, false)?;
    tx.commit()?;
    Ok(gone.len())
}

/// Albums that contain an item.
pub fn for_media(db: &Db, media_id: i64) -> Result<Vec<AlbumRef>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.name FROM album_items i JOIN albums a ON a.id = i.album_id
         WHERE i.media_id = ?1 ORDER BY a.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([media_id], |r| Ok(AlbumRef { id: r.get(0)?, name: r.get(1)? }))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
