//! The locked folder ("vault"): photos and videos encrypted on disk, readable only in Memora
//! after the password. Moving an item in takes it out of the library completely (database,
//! caches, search, people, `.memora` library info) and permanently deletes the original.
//! Never shown on the network, TV or DLNA.

pub mod crypto;
mod store;

use std::path::{Path, PathBuf};

use r2d2_sqlite::rusqlite::params;
use serde::Serialize;

pub use store::{Info, Item, MIN_PASSWORD, NewItem, PLACE_DIR, Part, State, Vault};

use crate::db::Db;
use crate::media::{self, Kind};
use crate::{AppPaths, Error, Result, sync, thumbs};

/// What a move into or out of the locked folder did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Moved {
    pub moved: usize,
    /// Items that stayed where they were, with the reason.
    pub failed: Vec<String>,
    /// Moving out: where the files went.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
}

/// Move library items into the locked folder. Per item: encrypt it with its thumbnail and
/// preview, check the encrypted copy, delete the original for good (not the Recycle Bin, which
/// would keep a readable copy), then remove everything the library knew about it.
/// An item whose original can't be deleted stays in the library (its encrypted copy is dropped).
pub fn move_in(db: &Db, paths: &AppPaths, vault: &Vault, ids: &[i64]) -> Result<Moved> {
    if !vault.state().unlocked {
        return Err(Error::Locked);
    }
    let mut report = Moved::default();
    for &id in ids {
        let m = match media::get(db, id) {
            Ok(m) => m,
            Err(Error::NotFound) => continue,
            Err(e) => return Err(e),
        };
        let fail = |report: &mut Moved, why: String| report.failed.push(format!("{}: {why}", m.name));
        if m.missing || !m.path.is_file() {
            fail(&mut report, "the file is not available".into());
            continue;
        }
        let read = |p: Result<PathBuf>| p.ok().and_then(|p| std::fs::read(p).ok());
        let thumb = read(thumbs::ensure_thumb(paths, &m));
        let display = (m.kind == Kind::Photo && media::needs_display_copy(&m.mime))
            .then(|| read(thumbs::ensure_display(paths, &m)))
            .flatten();
        let camera = [m.camera_make.as_deref(), m.camera_model.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let info = Info {
            name: m.name.clone(),
            original_path: m.path.to_string_lossy().into_owned(),
            kind: m.kind,
            mime: m.mime.clone(),
            size: m.size as u64,
            mtime: m.mtime,
            width: m.width,
            height: m.height,
            duration_ms: m.duration_ms,
            taken_at: m.taken_at.clone(),
            tz_offset: m.tz_offset,
            camera: (!camera.trim().is_empty()).then(|| camera.trim().to_owned()),
            lat: m.lat,
            lon: m.lon,
            favorite: m.favorite,
            hash: String::new(),
            added_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        };
        let vault_id = match vault.add(NewItem { info, original: &m.path, thumb, display }) {
            Ok(v) => v,
            Err(e) => {
                fail(&mut report, e.to_string());
                continue;
            }
        };
        if let Err(e) = std::fs::remove_file(&m.path) {
            let _ = vault.delete(&[vault_id]);
            fail(&mut report, format!("the original couldn't be deleted ({e}), so it stays in the library"));
            continue;
        }
        purge(db, paths, &[id])?;
        report.moved += 1;
    }
    if report.moved > 0 {
        scrub(db);
    }
    Ok(report)
}

/// Rebuild the database file and empty its write-ahead log, so no copy of a removed item's
/// name or place is left in free space (rows that grew while indexing leave their old bytes
/// behind in the page). A few seconds for a very large library; best effort.
fn scrub(db: &Db) {
    let result = db.conn().and_then(|c| {
        c.execute_batch("VACUUM")?;
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!("cannot compact the library database after moving to the locked folder: {e}");
    }
}

/// Remove items from the library and everything derived from them: rows (search index, album
/// entries, faces and CLIP vectors go with them), cached pictures, face pictures, and this
/// PC's `.memora` values. Deleted rows are overwritten (`secure_delete`); see also [`scrub`].
fn purge(db: &Db, paths: &AppPaths, ids: &[i64]) -> Result<()> {
    let json = serde_json::to_string(ids)?;
    let mut conn = db.conn()?;
    let faces: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM faces WHERE media_id IN (SELECT value FROM json_each(?1))")?;
        stmt.query_map([&json], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    let was: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0))?;
    conn.pragma_update(None, "secure_delete", 1)?;
    let result = (|| {
        let tx = conn.transaction()?;
        sync::forget_items(&tx, ids)?;
        tx.execute("DELETE FROM media WHERE id IN (SELECT value FROM json_each(?1))", params![json])?;
        tx.commit()?;
        Ok::<_, Error>(())
    })();
    conn.pragma_update(None, "secure_delete", was)?;
    result?;
    thumbs::remove_cached(paths, ids);
    for face in faces {
        let _ = std::fs::remove_file(thumbs::face_path(paths, face));
    }
    Ok(())
}

/// Decrypt items back into `dir` (each under its own name, made unique) and remove them from
/// the locked folder. If `dir` is a watched folder, the library picks the files up as new.
pub fn move_out(vault: &Vault, ids: &[String], dir: &Path) -> Result<Moved> {
    let mut report = Moved::default();
    for id in ids {
        let name = vault.get(id)?.info.name;
        match vault.move_out(id, dir) {
            Ok(p) => {
                report.moved += 1;
                report.paths.push(p);
            }
            Err(e) => report.failed.push(format!("{name}: {e}")),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
