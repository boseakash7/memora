//! The finished index kept next to the photos: `.memora\index\<device>.db`, a small SQLite
//! file with what indexing and recognition learned about each item (metadata, hashes, place,
//! faces with their person ids, CLIP vector). Another PC that adds the folder copies it for
//! files it has too (same path, size and time) instead of reading, hashing and recognising
//! them again; only thumbnails are made there (they stay in each PC's own cache).
//!
//! Each PC writes only its own file, to a temp file renamed over the old one, so readers
//! only ever see a complete file. Readers open it read-only and immutable (no locks, which
//! NAS shares and sync clients handle badly).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use r2d2_sqlite::rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::db::Db;
use crate::folders::{self, Folder};
use crate::media::Stage;
use crate::places::{self, PlaceName};
use crate::{Result, ai, sync};

const FORMAT: i64 = 1;
/// Bump when indexing reads files differently (dates, sizes, places...), so snapshots made
/// by older versions are not used.
pub const PIPELINE: i64 = 1;

const SCHEMA: &str = r#"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE items (
        path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime INTEGER NOT NULL, blake3 TEXT NOT NULL,
        kind TEXT NOT NULL, mime TEXT NOT NULL, width INTEGER, height INTEGER, duration_ms INTEGER,
        orientation INTEGER NOT NULL, taken_at TEXT NOT NULL, tz_offset INTEGER,
        camera_make TEXT, camera_model TEXT, lat REAL, lon REAL, phash INTEGER,
        country TEXT, admin1 TEXT, city TEXT, video_codec TEXT, audio_codec TEXT,
        recognised INTEGER NOT NULL, clip BLOB
    ) WITHOUT ROWID;
    CREATE TABLE faces (
        path TEXT NOT NULL, x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
        score REAL NOT NULL, embedding BLOB NOT NULL, person TEXT, locked INTEGER NOT NULL
    );
    CREATE INDEX faces_path ON faces(path);
"#;

fn index_dir(root: &Path) -> PathBuf {
    root.join(sync::DIR).join("index")
}

/// This PC's snapshot in a folder.
pub fn own_path(root: &Path, device: &str) -> PathBuf {
    index_dir(root).join(format!("{device}.db"))
}

/// A cheap summary of what a folder's snapshot would hold, to tell when it needs writing.
fn mark(conn: &Connection, folder_id: i64) -> Result<String> {
    Ok(conn.query_row(
        "SELECT count(*), coalesce(sum(m.stage), 0), coalesce(sum(m.mtime % 1000003), 0), coalesce(sum(m.id), 0),
                (SELECT count(*) || '/' || coalesce(sum(coalesce(f.person_id, 0) + f.locked), 0)
                   FROM faces f JOIN media m2 ON m2.id = f.media_id WHERE m2.folder_id = ?1)
         FROM media m WHERE m.folder_id = ?1 AND m.missing = 0 AND m.stage >= ?2 AND m.error IS NULL",
        params![folder_id, Stage::HASH],
        |r| Ok(format!("{PIPELINE}:{}:{}:{}:{}:{}", r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?)),
    )?)
}

/// Write the snapshot of every synced folder whose index changed since its last one and
/// has settled (nothing waiting for indexing). Returns how many were written.
pub fn write_changed(db: &Db) -> Result<usize> {
    let mut written = 0;
    for folder in folders::list(db)?.into_iter().filter(|f| f.sync && f.keep_index) {
        let Some(root) = folder.sync_root.clone().filter(|r| r.is_dir()) else { continue };
        let conn = db.conn()?;
        let waiting: i64 = conn.query_row(
            "SELECT count(*) FROM media WHERE folder_id = ?1 AND missing = 0 AND stage < ?2",
            params![folder.id, Stage::DONE],
            |r| r.get(0),
        )?;
        if waiting > 0 {
            continue;
        }
        let device = sync::device_id(&conn)?;
        let now = mark(&conn, folder.id)?;
        let before: Option<String> = conn.query_row("SELECT index_mark FROM folders WHERE id = ?1", [folder.id], |r| r.get(0))?;
        if before.as_deref() == Some(now.as_str()) && own_path(&root, &device).is_file() {
            continue;
        }
        drop(conn);
        match write(db, &folder, &root) {
            Ok(n) => {
                db.conn()?.execute("UPDATE folders SET index_mark = ?2 WHERE id = ?1", params![folder.id, now])?;
                tracing::info!("wrote the index of {} items to {}", n, root.display());
                written += 1;
            }
            Err(e) => tracing::warn!("cannot write the index to {}: {e}", root.display()),
        }
    }
    Ok(written)
}

/// Write a folder's snapshot. Returns how many items it holds.
pub fn write(db: &Db, folder: &Folder, root: &Path) -> Result<usize> {
    let conn = db.conn()?;
    let device = sync::device_id(&conn)?;
    fs::create_dir_all(index_dir(root))?;
    let target = own_path(root, &device);
    let tmp = target.with_extension("db.tmp");
    let _ = fs::remove_file(&tmp);
    let prefix = {
        let mut s = root.to_string_lossy().to_lowercase();
        if !s.ends_with(std::path::MAIN_SEPARATOR) {
            s.push(std::path::MAIN_SEPARATOR);
        }
        s
    };
    let rel = |path: &str| path.to_lowercase().strip_prefix(&prefix).map(|r| r.replace(std::path::MAIN_SEPARATOR, "/"));

    let mut out = Connection::open(&tmp)?;
    out.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    out.execute_batch(SCHEMA)?;
    let tx = out.transaction()?;
    for (k, v) in [
        ("format", FORMAT.to_string()),
        ("pipeline", PIPELINE.to_string()),
        ("recognition", ai::RECOGNITION.to_owned()),
        ("device", device.clone()),
        ("app", env!("CARGO_PKG_VERSION").to_owned()),
        ("created", Utc::now().to_rfc3339()),
    ] {
        tx.execute("INSERT INTO meta (key, value) VALUES (?1, ?2)", params![k, v])?;
    }
    let mut count = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.path, m.size, m.mtime, m.blake3, m.kind, m.mime, m.width, m.height, m.duration_ms,
                    m.orientation, m.taken_at, m.tz_offset, m.camera_make, m.camera_model, m.lat, m.lon, m.phash,
                    p.country, p.admin1, p.city, m.video_codec, m.audio_codec, m.stage >= ?2 AND m.ai_done = 3, c.vec
             FROM media m LEFT JOIN places p ON p.id = m.place_id LEFT JOIN clip_embeddings c ON c.media_id = m.id
             WHERE m.folder_id = ?1 AND m.missing = 0 AND m.stage >= ?3 AND m.error IS NULL AND m.blake3 IS NOT NULL",
        )?;
        let mut faces = conn.prepare(
            "SELECT f.x, f.y, f.w, f.h, f.score, f.embedding, p.uid, f.locked
             FROM faces f LEFT JOIN persons p ON p.id = f.person_id WHERE f.media_id = ?1",
        )?;
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO items VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        )?;
        let mut insert_face = tx.prepare("INSERT INTO faces VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
        let mut rows = stmt.query(params![folder.id, Stage::AI, Stage::HASH])?;
        while let Some(r) = rows.next()? {
            let Some(path) = rel(&r.get::<_, String>(1)?) else { continue };
            let recognised: bool = r.get(23)?;
            insert.execute(params![
                path,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, Option<i64>>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, Option<f64>>(15)?,
                r.get::<_, Option<f64>>(16)?,
                r.get::<_, Option<i64>>(17)?,
                r.get::<_, Option<String>>(18)?,
                r.get::<_, Option<String>>(19)?,
                r.get::<_, Option<String>>(20)?,
                r.get::<_, Option<String>>(21)?,
                r.get::<_, Option<String>>(22)?,
                recognised,
                r.get::<_, Option<Vec<u8>>>(24)?,
            ])?;
            if recognised {
                let mut fr = faces.query([r.get::<_, i64>(0)?])?;
                while let Some(f) = fr.next()? {
                    insert_face.execute(params![
                        path,
                        f.get::<_, f64>(0)?,
                        f.get::<_, f64>(1)?,
                        f.get::<_, f64>(2)?,
                        f.get::<_, f64>(3)?,
                        f.get::<_, f64>(4)?,
                        f.get::<_, Vec<u8>>(5)?,
                        f.get::<_, Option<String>>(6)?,
                        f.get::<_, i64>(7)?,
                    ])?;
                }
            }
            count += 1;
        }
    }
    tx.commit()?;
    out.close().map_err(|(_, e)| e)?;
    fs::rename(&tmp, &target)?;
    Ok(count)
}

/// Stop keeping the index in the folder: this PC's snapshot is deleted.
pub fn remove_own(db: &Db, folder: &Folder) -> Result<()> {
    if let Some(root) = &folder.sync_root {
        let device = sync::device_id(&*db.conn()?)?;
        let path = own_path(root, &device);
        if path.is_file() {
            fs::remove_file(path)?;
        }
    }
    db.conn()?.execute("UPDATE folders SET index_mark = NULL WHERE id = ?1", [folder.id])?;
    Ok(())
}

/// Size of this PC's snapshot in a folder, if there is one.
pub fn own_size(db: &Db, root: &Path) -> Result<Option<u64>> {
    let device = sync::device_id(&*db.conn()?)?;
    Ok(fs::metadata(own_path(root, &device)).ok().map(|m| m.len()))
}

/// A snapshot that can be used here, newest first.
struct Source {
    conn: Connection,
    created: String,
    /// Same recognition models: its faces and CLIP vectors can be used.
    recognition: bool,
}

fn open_sources(root: &Path) -> Vec<Source> {
    let Ok(dir) = fs::read_dir(index_dir(root)) else { return Vec::new() };
    let mut out: Vec<Source> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .filter_map(|p| match open_source(&p) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("cannot use the index in {}: {e}", p.display());
                None
            }
        })
        .collect();
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

fn open_source(path: &Path) -> Result<Option<Source>> {
    // Read-only and immutable: no lock or journal files next to it.
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().replace('\\', "/").chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    uri.push_str("?immutable=1");
    let conn = Connection::open_with_flags(&uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)?;
    let meta = |k: &str| -> Result<Option<String>> {
        Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0)).optional()?)
    };
    if meta("format")?.as_deref() != Some(&FORMAT.to_string()) || meta("pipeline")?.as_deref() != Some(&PIPELINE.to_string()) {
        return Ok(None);
    }
    let recognition = meta("recognition")?.as_deref() == Some(ai::RECOGNITION);
    let created = meta("created")?.unwrap_or_default();
    Ok(Some(Source { conn, created, recognition }))
}

/// What [`import`] took over.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Imported {
    /// Items that skip reading and hashing.
    pub items: usize,
    /// Of those, items that skip recognition too.
    pub recognised: usize,
}

/// Take over what other PCs' snapshots know about this folder's new items: those at the
/// same path with the same size and modification time. They then only need a thumbnail.
pub fn import(db: &Db, folder_id: i64) -> Result<Imported> {
    let folder = folders::get(db, folder_id)?;
    let Some(root) = folder.sync_root.clone().filter(|r| folder.sync && folder.keep_index && r.is_dir()) else {
        return Ok(Imported::default());
    };
    let sources = open_sources(&root);
    if sources.is_empty() {
        return Ok(Imported::default());
    }
    let prefix = {
        let mut s = root.to_string_lossy().to_lowercase();
        if !s.ends_with(std::path::MAIN_SEPARATOR) {
            s.push(std::path::MAIN_SEPARATOR);
        }
        s
    };
    let pending: Vec<(i64, String, i64, i64)> = {
        let conn = db.conn()?;
        let mut stmt =
            conn.prepare("SELECT id, path, size, mtime FROM media WHERE folder_id = ?1 AND stage = ?2 AND missing = 0")?;
        stmt.query_map(params![folder_id, Stage::NEW], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut done = Imported::default();
    let mut conn = db.conn()?;
    for chunk in pending.chunks(500) {
        let tx = conn.transaction()?;
        for (id, path, size, mtime) in chunk {
            let Some(rel) = path.to_lowercase().strip_prefix(&prefix).map(|r| r.replace(std::path::MAIN_SEPARATOR, "/")) else {
                continue;
            };
            for src in &sources {
                match take(&tx, src, *id, &rel, *size, *mtime)? {
                    Some(recognised) => {
                        done.items += 1;
                        done.recognised += recognised as usize;
                        break;
                    }
                    None => continue,
                }
            }
        }
        tx.commit()?;
    }
    if done.items > 0 {
        tracing::info!("took over the index of {} items ({} recognised) from {}", done.items, done.recognised, root.display());
    }
    Ok(done)
}

/// Copy one item from a snapshot if it has it unchanged. Returns whether its recognition
/// was copied too, or None if the snapshot doesn't have it.
fn take(tx: &Connection, src: &Source, id: i64, rel: &str, size: i64, mtime: i64) -> Result<Option<bool>> {
    let mut stmt = src.conn.prepare_cached(
        "SELECT blake3, kind, mime, width, height, duration_ms, orientation, taken_at, tz_offset, camera_make,
                camera_model, lat, lon, phash, country, admin1, city, video_codec, audio_codec, recognised, clip
         FROM items WHERE path = ?1 AND size = ?2 AND mtime = ?3",
    )?;
    let mut rows = stmt.query(params![rel, size, mtime])?;
    let Some(r) = rows.next()? else { return Ok(None) };
    let place = match (r.get::<_, Option<String>>(14)?, r.get::<_, Option<String>>(15)?, r.get::<_, Option<String>>(16)?) {
        (Some(country), Some(admin1), Some(city)) => Some(places::intern(tx, &PlaceName { country, admin1, city })?),
        _ => None,
    };
    let recognised = src.recognition && r.get::<_, bool>(19)?;
    let n = tx
        .prepare_cached(
            "UPDATE media SET blake3 = ?3, kind = ?4, mime = ?5, width = ?6, height = ?7, duration_ms = ?8,
                 orientation = ?9, taken_at = ?10, tz_offset = ?11, camera_make = ?12, camera_model = ?13,
                 lat = ?14, lon = ?15, phash = ?16, place_id = ?17, video_codec = ?18, audio_codec = ?19,
                 imported = ?20, stage = ?21, error = NULL
             WHERE id = ?1 AND mtime = ?2 AND stage = 0",
        )?
        .execute(params![
            id,
            mtime,
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<i64>>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, Option<f64>>(11)?,
            r.get::<_, Option<f64>>(12)?,
            r.get::<_, Option<i64>>(13)?,
            place,
            r.get::<_, Option<String>>(17)?,
            r.get::<_, Option<String>>(18)?,
            if recognised { 2 } else { 1 },
            Stage::METADATA,
        ])?;
    if n == 0 {
        return Ok(None); // changed meanwhile
    }
    if recognised {
        if let Some(clip) = r.get::<_, Option<Vec<u8>>>(20)? {
            tx.prepare_cached("INSERT OR REPLACE INTO clip_embeddings (media_id, vec) VALUES (?1, ?2)")?.execute(params![id, clip])?;
        }
        let mut faces = src.conn.prepare_cached("SELECT x, y, w, h, score, embedding, person, locked FROM faces WHERE path = ?1")?;
        let mut fr = faces.query([rel])?;
        while let Some(f) = fr.next()? {
            let person = match f.get::<_, Option<String>>(6)? {
                Some(uid) => Some(person_by_uid(tx, &uid)?),
                None => None,
            };
            tx.prepare_cached(
                "INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id, locked) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute(params![
                id,
                f.get::<_, f64>(0)?,
                f.get::<_, f64>(1)?,
                f.get::<_, f64>(2)?,
                f.get::<_, f64>(3)?,
                f.get::<_, f64>(4)?,
                f.get::<_, Vec<u8>>(5)?,
                person,
                f.get::<_, i64>(7)?,
            ])?;
        }
    }
    Ok(Some(recognised))
}

/// The local person with this uid, made if there is none. Names come from the change logs
/// (`sync::apply`), which run right after.
fn person_by_uid(tx: &Connection, uid: &str) -> Result<i64> {
    if let Some(id) = tx.prepare_cached("SELECT id FROM persons WHERE uid = ?1")?.query_row([uid], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    tx.execute("INSERT INTO persons (created_at, uid) VALUES (?1, ?2)", params![Utc::now().to_rfc3339(), uid])?;
    Ok(tx.last_insert_rowid())
}

#[cfg(test)]
mod tests;
