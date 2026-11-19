use std::path::Path;

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use r2d2_sqlite::rusqlite::Connection;

use crate::Result;

/// Schema migrations, applied in order. `PRAGMA user_version` records how many have run.
/// Never edit a migration that has shipped; add a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1: watched folders and key/value settings
    r#"
    CREATE TABLE folders (
        id        INTEGER PRIMARY KEY,
        path      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        enabled   INTEGER NOT NULL DEFAULT 1,
        added_at  TEXT NOT NULL
    );
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // 2: indexed photos and videos
    r#"
    CREATE TABLE media (
        id            INTEGER PRIMARY KEY,
        folder_id     INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        path          TEXT NOT NULL UNIQUE COLLATE NOCASE,
        name          TEXT NOT NULL,
        size          INTEGER NOT NULL,
        mtime         INTEGER NOT NULL,          -- unix milliseconds
        kind          TEXT NOT NULL CHECK (kind IN ('photo', 'video')),
        mime          TEXT NOT NULL,
        width         INTEGER,                   -- as displayed (after rotation)
        height        INTEGER,
        duration_ms   INTEGER,
        orientation   INTEGER NOT NULL DEFAULT 1, -- EXIF orientation, 1-8
        taken_at      TEXT NOT NULL,             -- local wall-clock time, 'YYYY-MM-DDTHH:MM:SS'
        tz_offset     INTEGER,                   -- minutes east of UTC, when known
        camera_make   TEXT,
        camera_model  TEXT,
        lat           REAL,
        lon           REAL,
        blake3        TEXT,
        phash         INTEGER,
        favorite      INTEGER NOT NULL DEFAULT 0,
        trashed_at    TEXT,
        missing       INTEGER NOT NULL DEFAULT 0,
        stage         INTEGER NOT NULL DEFAULT 0, -- see media::Stage
        error         TEXT,
        added_at      TEXT NOT NULL
    );
    CREATE INDEX media_folder ON media(folder_id);
    CREATE INDEX media_timeline ON media(taken_at DESC, id DESC)
        WHERE missing = 0 AND trashed_at IS NULL;
    CREATE INDEX media_pending ON media(stage) WHERE missing = 0;
    "#,
    // 3: tiled HEIC photos were read as their first 512px tile; process them again
    r#"
    UPDATE media SET stage = 0, error = NULL WHERE mime IN ('image/heic', 'image/heif', 'image/avif');
    "#,
    // 4: albums, favorites/trash indexes, and full-text search.
    // `media_fts` rows share their rowid with `media.id` and are kept current by triggers.
    // `path` covers folder and file names; `albums` lists the names of the albums an item is in.
    r#"
    CREATE TABLE albums (
        id              INTEGER PRIMARY KEY,
        name            TEXT NOT NULL,
        cover_media_id  INTEGER REFERENCES media(id) ON DELETE SET NULL,
        created_at      TEXT NOT NULL
    );
    CREATE TABLE album_items (
        album_id  INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
        media_id  INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        added_at  TEXT NOT NULL,
        PRIMARY KEY (album_id, media_id)
    ) WITHOUT ROWID;
    CREATE INDEX album_items_media ON album_items(media_id);
    CREATE INDEX media_favorites ON media(taken_at DESC, id DESC)
        WHERE favorite = 1 AND missing = 0 AND trashed_at IS NULL;
    CREATE INDEX media_trashed ON media(trashed_at) WHERE trashed_at IS NOT NULL;

    CREATE VIRTUAL TABLE media_fts USING fts5(
        path, camera, albums, tokenize = 'unicode61 remove_diacritics 2'
    );
    INSERT INTO media_fts(rowid, path, camera, albums)
        SELECT id, path, trim(coalesce(camera_make, '') || ' ' || coalesce(camera_model, '')), ''
        FROM media;

    CREATE TRIGGER media_fts_insert AFTER INSERT ON media BEGIN
        INSERT INTO media_fts(rowid, path, camera, albums) VALUES (
            new.id, new.path,
            trim(coalesce(new.camera_make, '') || ' ' || coalesce(new.camera_model, '')), '');
    END;
    CREATE TRIGGER media_fts_update AFTER UPDATE OF path, camera_make, camera_model ON media BEGIN
        UPDATE media_fts SET
            path = new.path,
            camera = trim(coalesce(new.camera_make, '') || ' ' || coalesce(new.camera_model, ''))
        WHERE rowid = new.id;
    END;
    CREATE TRIGGER media_fts_delete AFTER DELETE ON media BEGIN
        DELETE FROM media_fts WHERE rowid = old.id;
    END;
    CREATE TRIGGER album_items_fts_insert AFTER INSERT ON album_items BEGIN
        UPDATE media_fts SET albums = coalesce((
            SELECT group_concat(a.name, ' ') FROM album_items i JOIN albums a ON a.id = i.album_id
            WHERE i.media_id = new.media_id), '')
        WHERE rowid = new.media_id;
    END;
    CREATE TRIGGER album_items_fts_delete AFTER DELETE ON album_items BEGIN
        UPDATE media_fts SET albums = coalesce((
            SELECT group_concat(a.name, ' ') FROM album_items i JOIN albums a ON a.id = i.album_id
            WHERE i.media_id = old.media_id), '')
        WHERE rowid = old.media_id;
    END;
    CREATE TRIGGER albums_fts_rename AFTER UPDATE OF name ON albums BEGIN
        UPDATE media_fts SET albums = coalesce((
            SELECT group_concat(a.name, ' ') FROM album_items i JOIN albums a ON a.id = i.album_id
            WHERE i.media_id = media_fts.rowid), '')
        WHERE rowid IN (SELECT media_id FROM album_items WHERE album_id = new.id);
    END;
    "#,
    // 5: places, duplicate hashes. Existing items sit at stage 2 and now count as pending,
    // so they get hashed and geocoded in the background. The search index gains a `place`
    // column, which means rebuilding it (FTS5 tables cannot be altered).
    r#"
    CREATE TABLE places (
        id      INTEGER PRIMARY KEY,
        country TEXT NOT NULL,                  -- ISO 3166-1 alpha-2 code
        admin1  TEXT NOT NULL,
        city    TEXT NOT NULL,
        UNIQUE (country, admin1, city)
    );
    ALTER TABLE media ADD COLUMN place_id INTEGER REFERENCES places(id);
    CREATE INDEX media_place ON media(place_id) WHERE place_id IS NOT NULL;
    CREATE INDEX media_blake3 ON media(blake3) WHERE blake3 IS NOT NULL;
    CREATE INDEX media_gps ON media(lat, lon) WHERE lat IS NOT NULL;

    DROP TRIGGER media_fts_insert;
    DROP TRIGGER media_fts_update;
    DROP TABLE media_fts;
    CREATE VIRTUAL TABLE media_fts USING fts5(
        path, camera, albums, place, tokenize = 'unicode61 remove_diacritics 2'
    );
    INSERT INTO media_fts(rowid, path, camera, albums, place)
        SELECT m.id, m.path,
            trim(coalesce(m.camera_make, '') || ' ' || coalesce(m.camera_model, '')),
            coalesce((SELECT group_concat(a.name, ' ') FROM album_items i JOIN albums a ON a.id = i.album_id
                      WHERE i.media_id = m.id), ''),
            ''
        FROM media m;
    CREATE TRIGGER media_fts_insert AFTER INSERT ON media BEGIN
        INSERT INTO media_fts(rowid, path, camera, albums, place) VALUES (
            new.id, new.path,
            trim(coalesce(new.camera_make, '') || ' ' || coalesce(new.camera_model, '')), '', '');
    END;
    CREATE TRIGGER media_fts_update AFTER UPDATE OF path, camera_make, camera_model, place_id ON media BEGIN
        UPDATE media_fts SET
            path = new.path,
            camera = trim(coalesce(new.camera_make, '') || ' ' || coalesce(new.camera_model, '')),
            place = coalesce((SELECT city || ' ' || admin1 || ' ' || country FROM places WHERE id = new.place_id), '')
        WHERE rowid = new.id;
    END;
    "#,
    // 6: video codecs, so the TV server knows which videos need converting.
    // NULL means not probed yet (filled on first use for existing videos); '' means no audio track.
    r#"
    ALTER TABLE media ADD COLUMN video_codec TEXT;
    ALTER TABLE media ADD COLUMN audio_codec TEXT;
    "#,
    // 7: people (faces grouped into persons) and CLIP vectors for searching by content.
    // Filled by the recognition worker, which takes items at stage 3 (hashed) to stage 4.
    r#"
    CREATE TABLE persons (
        id             INTEGER PRIMARY KEY,
        name           TEXT,                      -- NULL until the user names them
        cover_face_id  INTEGER REFERENCES faces(id) ON DELETE SET NULL,
        hidden         INTEGER NOT NULL DEFAULT 0,
        created_at     TEXT NOT NULL
    );
    CREATE TABLE faces (
        id         INTEGER PRIMARY KEY,
        media_id   INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL, -- fractions of the displayed picture
        score      REAL NOT NULL,                 -- detector confidence, 0-1
        embedding  BLOB NOT NULL,                 -- 128 x f32 little-endian, unit length
        person_id  INTEGER REFERENCES persons(id) ON DELETE SET NULL,
        locked     INTEGER NOT NULL DEFAULT 0     -- 0 automatic, 1 person set by the user, 2 "not this person"
    );
    CREATE INDEX faces_media ON faces(media_id);
    CREATE INDEX faces_person ON faces(person_id) WHERE person_id IS NOT NULL;
    CREATE TABLE clip_embeddings (
        media_id  INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        vec       BLOB NOT NULL                   -- 512 x f32 little-endian, unit length
    );
    "#,
    // 8: video dates are now read in the time zone where they were taken, and cameras that
    // write local time are recognised. This flag makes the library re-read them once
    // (see `Library::refresh_video_dates`); the rest of the pipeline is not repeated.
    r#"
    INSERT OR REPLACE INTO settings(key, value) VALUES ('_refreshVideoDates', '1');
    "#,
    // 9: Archive. Archived items are shown only on the Archive page (and the TV/DLNA Archive),
    // so the partial indexes for the everyday lists leave them out.
    r#"
    ALTER TABLE media ADD COLUMN archived_at TEXT;
    DROP INDEX media_timeline;
    CREATE INDEX media_timeline ON media(taken_at DESC, id DESC)
        WHERE missing = 0 AND trashed_at IS NULL AND archived_at IS NULL;
    DROP INDEX media_favorites;
    CREATE INDEX media_favorites ON media(taken_at DESC, id DESC)
        WHERE favorite = 1 AND missing = 0 AND trashed_at IS NULL AND archived_at IS NULL;
    CREATE INDEX media_archived ON media(taken_at DESC, id DESC)
        WHERE archived_at IS NOT NULL AND missing = 0 AND trashed_at IS NULL;
    "#,
    // 10: library info kept in a `.memora` folder next to the photos (see `sync`).
    // Albums and people get ids that are the same on every PC.
    r#"
    ALTER TABLE albums ADD COLUMN uid TEXT;
    UPDATE albums SET uid = lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX albums_uid ON albums(uid);
    CREATE TRIGGER albums_uid AFTER INSERT ON albums WHEN new.uid IS NULL BEGIN
        UPDATE albums SET uid = lower(hex(randomblob(16))) WHERE id = new.id;
    END;
    ALTER TABLE persons ADD COLUMN uid TEXT;
    UPDATE persons SET uid = lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX persons_uid ON persons(uid);
    CREATE TRIGGER persons_uid AFTER INSERT ON persons WHEN new.uid IS NULL BEGIN
        UPDATE persons SET uid = lower(hex(randomblob(16))) WHERE id = new.id;
    END;

    ALTER TABLE folders ADD COLUMN sync INTEGER NOT NULL DEFAULT 0; -- keep library info in `.memora`
    ALTER TABLE folders ADD COLUMN sync_root TEXT;                  -- folder holding the `.memora`

    -- The newest known value of every synced field (from this PC or read from another
    -- PC's log). The library tables are kept in step with it.
    CREATE TABLE sync_reg (
        folder_id  INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        field      TEXT NOT NULL,
        item       TEXT NOT NULL,   -- path relative to the `.memora` folder, lowercase, '/'; '' for albums and people
        sub        TEXT NOT NULL,   -- album or person uid, face box; ''
        hash       TEXT,            -- the item's blake3, when known
        clock      TEXT NOT NULL,   -- hybrid logical clock, compares as text
        value      TEXT NOT NULL,   -- JSON
        own        INTEGER NOT NULL DEFAULT 0, -- belongs in this PC's log
        PRIMARY KEY (folder_id, field, item, sub)
    ) WITHOUT ROWID;
    CREATE INDEX sync_reg_hash ON sync_reg(hash) WHERE hash IS NOT NULL;
    -- Lines not yet written to the folder's `.memora` (drive unplugged, read-only...).
    CREATE TABLE sync_outbox (
        id         INTEGER PRIMARY KEY,
        folder_id  INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        line       TEXT NOT NULL
    );
    -- How far each PC's log has been read.
    CREATE TABLE sync_cursor (
        folder_id  INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        file       TEXT NOT NULL,
        gen        TEXT NOT NULL,
        offset     INTEGER NOT NULL,
        PRIMARY KEY (folder_id, file)
    ) WITHOUT ROWID;
    "#,
    // 11: the finished index kept in `.memora` too (see `snapshot`), so another PC can skip
    // reading, hashing and recognising files it already knows.
    r#"
    ALTER TABLE folders ADD COLUMN keep_index INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE folders ADD COLUMN index_mark TEXT;   -- what the last written snapshot covered
    -- 1: metadata and hashes came from a snapshot (only the thumbnail is made here),
    -- 2: recognition too.
    ALTER TABLE media ADD COLUMN imported INTEGER NOT NULL DEFAULT 0;
    "#,
    // 12: a restored backup's face decisions for items recognition hasn't looked at yet
    // (see `backup`); applied when it does.
    r#"
    CREATE TABLE restore_faces (
        media_id  INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        box       TEXT NOT NULL,   -- x,y,w,h
        value     TEXT NOT NULL    -- {"person": uid, "locked": n}
    );
    CREATE INDEX restore_faces_media ON restore_faces(media_id);
    CREATE TABLE restore_persons (
        uid    TEXT PRIMARY KEY,
        value  TEXT NOT NULL       -- {"name", "hidden"}
    ) WITHOUT ROWID;
    "#,
    // 13: the locked folder. Items moved there leave the library; their folder's `.memora`
    // log is rewritten without them (`log_dirty`), and the search index drops their words
    // for good instead of leaving them in old index pages (`secure-delete`, SQLite 3.44+).
    r#"
    ALTER TABLE folders ADD COLUMN log_dirty INTEGER NOT NULL DEFAULT 0;
    INSERT INTO media_fts (media_fts, rank) VALUES ('secure-delete', 1);
    "#,
    // 14: memories (see `memories`). `key` is stable across rebuilds so seen, hidden and
    // removed photos stay; `items` is a JSON list of media ids in order.
    r#"
    CREATE TABLE memories (
        id          INTEGER PRIMARY KEY,
        key         TEXT NOT NULL UNIQUE,
        kind        TEXT NOT NULL,             -- day | trip | person | year | month
        title       TEXT NOT NULL,
        start_day   TEXT NOT NULL,             -- YYYY-MM-DD
        end_day     TEXT NOT NULL,
        cover_id    INTEGER REFERENCES media(id) ON DELETE SET NULL,
        items       TEXT NOT NULL,
        score       REAL NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        seen_at     TEXT,
        hidden      INTEGER NOT NULL DEFAULT 0,
        active      INTEGER NOT NULL DEFAULT 1 -- 0: hidden and no longer qualifies (kept so it stays hidden)
    );
    CREATE TABLE memory_removed (
        key       TEXT NOT NULL,
        media_id  INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        PRIMARY KEY (key, media_id)
    ) WITHOUT ROWID;
    "#,
    // 15: photo edits, saved non-destructively (see `edit`). `edit` is the JSON recipe (NULL:
    // the original); `edited_at` (unix ms) changes picture URLs and cache freshness, also when
    // an edit is reverted; `edit_width`/`edit_height` are the size after cropping and turning.
    r#"
    ALTER TABLE media ADD COLUMN edit TEXT;
    ALTER TABLE media ADD COLUMN edited_at INTEGER;
    ALTER TABLE media ADD COLUMN edit_width INTEGER;
    ALTER TABLE media ADD COLUMN edit_height INTEGER;
    "#,
    // 16: which recognition models looked at an item (bits of `ai::FACES`, `ai::CLIP`). Each
    // model is downloaded on its own now; until then recognition needed both.
    r#"
    ALTER TABLE media ADD COLUMN ai_done INTEGER NOT NULL DEFAULT 0;
    UPDATE media SET ai_done = 3 WHERE stage >= 4;
    "#,
];

pub type Conn = PooledConnection<SqliteConnectionManager>;

/// Shared handle to the library database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    pool: Pool<SqliteConnectionManager>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        // WAL mode is stored in the file, so set it (and migrate) once on a single
        // connection before the pool opens more; doing it per connection causes lock errors.
        {
            let mut conn = Connection::open(path)?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            apply_migrations(&mut conn)?;
        }

        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.execute_batch(
                "PRAGMA busy_timeout = 5000;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;",
            )
        });
        let pool = Pool::builder().max_size(8).min_idle(Some(1)).build(manager)?;
        Ok(Db { pool })
    }

    pub fn conn(&self) -> Result<Conn> {
        Ok(self.pool.get()?)
    }
}

fn apply_migrations(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("test.db")).unwrap();
    (dir, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.db");
        Db::open(&path).unwrap();
        let db = Db::open(&path).unwrap();
        let version: i64 = db
            .conn()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
    }
}
