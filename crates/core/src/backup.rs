//! Library info as one file (`.memora-backup`, JSON): everything the user made, for every
//! watched folder whether it keeps a `.memora` or not. Favorites, the Archive, the trash,
//! albums (with their covers), people's names and hidden flags, face corrections, the folder
//! list and settings. Not the index or thumbnails: those are rebuilt from the photos.
//!
//! Restoring adds what the backup has and never undoes what was done since: favorites are
//! set, archived and trashed items go back there, albums are made or topped up (by their
//! id), unnamed people get their names. Items are found at the same path in the same folder,
//! else by their hash anywhere in the library. Faces recognition hasn't found yet are kept
//! and applied when it does.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use chrono::Utc;
use r2d2_sqlite::rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::Db;
use crate::media::Stage;
use crate::{Error, Result, albums, folders, organize, settings, sync};

const KIND: &str = "memora-backup";
const FORMAT: u32 = 1;
/// File name extension, without the dot.
pub const EXTENSION: &str = "memora-backup";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub memora: String,
    pub format: u32,
    pub created: String,
    pub app: String,
    pub pc: String,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub folders: Vec<BackupFolder>,
    #[serde(default)]
    pub albums: Vec<BackupAlbum>,
    #[serde(default)]
    pub persons: Vec<BackupPerson>,
    #[serde(default)]
    pub items: Vec<BackupItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFolder {
    pub path: PathBuf,
    #[serde(default)]
    pub keep_info: bool,
    #[serde(default = "yes")]
    pub keep_index: bool,
}

fn yes() -> bool {
    true
}

/// An item: folder (index into `folders`), path inside it, hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemKey {
    pub folder: usize,
    pub p: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupAlbum {
    pub uid: String,
    pub name: String,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<ItemKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupPerson {
    pub uid: String,
    pub name: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupItem {
    #[serde(flatten)]
    pub key: ItemKey,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fav: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash: Option<String>,
    /// Uids of the albums it is in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<BackupFace>,
}

/// A face the user decided about, or one of a person they named or hid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFace {
    /// Box `x,y,w,h` as fractions of the picture.
    pub b: String,
    pub person: Option<String>,
    pub locked: i64,
}

fn rel_path(folder: &Path, path: &str) -> Option<String> {
    let mut prefix = folder.to_string_lossy().to_lowercase();
    if !prefix.ends_with(MAIN_SEPARATOR) {
        prefix.push(MAIN_SEPARATOR);
    }
    path.to_lowercase().strip_prefix(&prefix).map(|r| r.replace(MAIN_SEPARATOR, "/"))
}

// ---------- export ----------

/// Everything the user made, as a backup.
pub fn export(db: &Db) -> Result<Backup> {
    let all = folders::list(db)?;
    let folder_index: HashMap<i64, usize> = all.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    let conn = db.conn()?;

    // Faces worth keeping: decided by the user, or of a person they named or hid.
    let mut faces: HashMap<i64, Vec<BackupFace>> = HashMap::new();
    let mut person_ids: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.media_id, printf('%.3f,%.3f,%.3f,%.3f', f.x, f.y, f.w, f.h), p.uid, f.locked
             FROM faces f LEFT JOIN persons p ON p.id = f.person_id
             WHERE f.locked != 0 OR p.name IS NOT NULL OR p.hidden = 1",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?)))?;
        for row in rows {
            let (media, b, person, locked) = row?;
            if let Some(uid) = &person {
                person_ids.insert(uid.clone());
            }
            faces.entry(media).or_default().push(BackupFace { b, person, locked });
        }
    }
    let mut in_albums: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT i.media_id, a.uid FROM album_items i JOIN albums a ON a.id = i.album_id")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (media, uid) = row?;
            in_albums.entry(media).or_default().push(uid);
        }
    }

    let key_of = |folder_id: i64, path: &str, hash: Option<String>| -> Option<ItemKey> {
        let folder = *folder_index.get(&folder_id)?;
        Some(ItemKey { folder, p: rel_path(&all[folder].path, path)?, h: hash })
    };
    let mut items = Vec::new();
    let mut keys: HashMap<i64, ItemKey> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, folder_id, path, blake3, favorite, archived_at, trashed_at FROM media
             WHERE favorite = 1 OR archived_at IS NOT NULL OR trashed_at IS NOT NULL
                OR id IN (SELECT media_id FROM album_items) OR id IN (SELECT value FROM json_each(?1))",
        )?;
        let face_media: Vec<i64> = faces.keys().copied().collect();
        let rows = stmt.query_map([serde_json::to_string(&face_media)?], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (id, folder_id, path, hash, fav, arch, trash) = row?;
            let Some(key) = key_of(folder_id, &path, hash) else { continue };
            keys.insert(id, key.clone());
            items.push(BackupItem {
                key,
                fav,
                arch,
                trash,
                albums: in_albums.remove(&id).unwrap_or_default(),
                faces: faces.remove(&id).unwrap_or_default(),
            });
        }
    }

    let albums = {
        let mut stmt = conn.prepare("SELECT uid, name, created_at, cover_media_id FROM albums ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (uid, name, created, cover) = row?;
            out.push(BackupAlbum { uid, name, created, cover: cover.and_then(|c| keys.get(&c).cloned()) });
        }
        out
    };
    let persons = {
        let mut stmt = conn.prepare("SELECT uid, name, hidden FROM persons WHERE name IS NOT NULL OR hidden = 1")?;
        let rows = stmt.query_map([], |r| Ok(BackupPerson { uid: r.get(0)?, name: r.get(1)?, hidden: r.get(2)? }))?;
        let mut out: Vec<BackupPerson> = rows.collect::<std::result::Result<_, _>>()?;
        let named: HashSet<String> = out.iter().map(|p| p.uid.clone()).collect();
        // People only referred to by corrected faces (moved to an unnamed person).
        for uid in person_ids.difference(&named) {
            out.push(BackupPerson { uid: uid.clone(), name: None, hidden: false });
        }
        out
    };
    drop(conn);

    Ok(Backup {
        memora: KIND.into(),
        format: FORMAT,
        created: Utc::now().to_rfc3339(),
        app: env!("CARGO_PKG_VERSION").into(),
        pc: std::env::var("COMPUTERNAME").unwrap_or_default(),
        settings: serde_json::to_value(settings::load(db)?)?,
        folders: all
            .iter()
            .map(|f| BackupFolder { path: f.path.clone(), keep_info: f.sync, keep_index: f.keep_index })
            .collect(),
        albums,
        persons,
        items,
    })
}

/// Write a backup file (to a temp file renamed over the target, so a failed write never
/// leaves half a backup). Returns what it holds.
pub fn write(db: &Db, path: &Path) -> Result<Summary> {
    let backup = export(db)?;
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    fs::write(&tmp, serde_json::to_vec(&backup)?)?;
    fs::rename(&tmp, path)?;
    Ok(Summary::of(&backup))
}

pub fn read(path: &Path) -> Result<Backup> {
    let bytes = fs::read(path)?;
    let backup: Backup =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid(format!("not a Memora backup ({e})")))?;
    if backup.memora != KIND {
        return Err(Error::invalid("not a Memora backup"));
    }
    if backup.format > FORMAT {
        return Err(Error::invalid("this backup was made by a newer Memora; update Memora to restore it"));
    }
    Ok(backup)
}

/// What a backup holds, for the UI.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub created: String,
    pub pc: String,
    pub folders: Vec<PathBuf>,
    pub favorites: usize,
    pub archived: usize,
    pub trashed: usize,
    pub albums: usize,
    pub people: usize,
}

impl Summary {
    pub fn of(b: &Backup) -> Summary {
        Summary {
            created: b.created.clone(),
            pc: b.pc.clone(),
            folders: b.folders.iter().map(|f| f.path.clone()).collect(),
            favorites: b.items.iter().filter(|i| i.fav).count(),
            archived: b.items.iter().filter(|i| i.arch.is_some()).count(),
            trashed: b.items.iter().filter(|i| i.trash.is_some()).count(),
            albums: b.albums.len(),
            people: b.persons.iter().filter(|p| p.name.is_some()).count(),
        }
    }
}

// ---------- restore ----------

/// What a restore did.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    /// Items of the backup found in the library.
    pub items: usize,
    /// Items of the backup that are not in the library (folder not added, file gone).
    pub not_found: usize,
    pub favorites: usize,
    pub archived: usize,
    pub trashed: usize,
    /// Albums made, and items added to albums.
    pub albums_created: usize,
    pub album_items: usize,
    /// People who got their name back.
    pub people_named: usize,
    /// Faces applied, and faces waiting for recognition to find them.
    pub faces: usize,
    pub faces_waiting: usize,
    pub settings: bool,
    /// Folders of the backup that were added to the library.
    pub folders_added: usize,
}

/// Finds a backup's items in this library.
struct Finder {
    /// For each backup folder: the watched folder at the same path, by relative path.
    by_path: Vec<HashMap<String, i64>>,
    by_hash: HashMap<String, i64>,
}

impl Finder {
    fn new(conn: &Connection, backup: &Backup) -> Result<Finder> {
        let current = {
            let mut stmt = conn.prepare("SELECT id, path FROM folders")?;
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, PathBuf::from(r.get::<_, String>(1)?))))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut by_path = Vec::new();
        for f in &backup.folders {
            let mut map = HashMap::new();
            let wanted = f.path.to_string_lossy().to_lowercase();
            if let Some((id, path)) = current.iter().find(|(_, p)| p.to_string_lossy().to_lowercase() == wanted) {
                let mut stmt = conn.prepare("SELECT id, path FROM media WHERE folder_id = ?1 AND missing = 0")?;
                for row in stmt.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                    let (mid, p) = row?;
                    if let Some(rel) = rel_path(path, &p) {
                        map.insert(rel, mid);
                    }
                }
            }
            by_path.push(map);
        }
        let mut by_hash = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, blake3 FROM media WHERE missing = 0 AND blake3 IS NOT NULL")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, h) = row?;
            by_hash.entry(h).or_insert(id);
        }
        Ok(Finder { by_path, by_hash })
    }

    fn find(&self, key: &ItemKey) -> Option<i64> {
        self.by_path
            .get(key.folder)
            .and_then(|m| m.get(&key.p.to_lowercase()).copied())
            .or_else(|| key.h.as_ref().and_then(|h| self.by_hash.get(h).copied()))
    }
}

/// Restore a backup into the library (whose folders should be added and scanned first, see
/// `Library::restore_backup`). `with_settings` also restores the settings.
pub fn restore(db: &Db, backup: &Backup, with_settings: bool) -> Result<Restored> {
    let mut report = Restored::default();
    let finder = Finder::new(&*db.conn()?, backup)?;
    let mut found: Vec<(i64, &BackupItem)> = Vec::new();
    for item in &backup.items {
        match finder.find(&item.key) {
            Some(id) => found.push((id, item)),
            None => report.not_found += 1,
        }
    }
    report.items = found.len();

    if with_settings && let Value::Object(saved) = &backup.settings {
        let current = serde_json::to_value(settings::load(db)?)?;
        let patch: serde_json::Map<String, Value> =
            saved.iter().filter(|(k, _)| current.get(k.as_str()).is_some()).map(|(k, v)| (k.clone(), v.clone())).collect();
        settings::update(db, &Value::Object(patch))?;
        report.settings = true;
    }

    // Favorites, Archive, trash: through the usual functions, so synced folders record them.
    let ids = |f: &dyn Fn(&BackupItem) -> bool| -> Vec<i64> { found.iter().filter(|(_, i)| f(i)).map(|(id, _)| *id).collect() };
    report.favorites = organize::set_favorite(db, &ids(&|i| i.fav), true)?;
    report.archived = organize::archive(db, &ids(&|i| i.arch.is_some()))?;
    report.trashed = organize::trash(db, &ids(&|i| i.trash.is_some()))?;

    // Albums, by their id; one made here gets the backup's id, name and date.
    for album in &backup.albums {
        let local: Option<i64> =
            db.conn()?.query_row("SELECT id FROM albums WHERE uid = ?1", [&album.uid], |r| r.get(0)).optional()?;
        let members = ids(&|i| i.albums.contains(&album.uid));
        let id = match local {
            Some(id) => id,
            None => {
                let made = albums::create(db, &album.name)?;
                db.conn()?.execute(
                    "UPDATE albums SET uid = ?2, created_at = ?3 WHERE id = ?1",
                    params![made.id, album.uid, album.created],
                )?;
                report.albums_created += 1;
                made.id
            }
        };
        report.album_items += albums::add_items(db, id, &members)?;
        if let Some(cover) = album.cover.as_ref().and_then(|c| finder.find(c)) {
            let has_cover: bool =
                db.conn()?.query_row("SELECT cover_media_id IS NOT NULL FROM albums WHERE id = ?1", [id], |r| r.get(0))?;
            if !has_cover {
                let _ = albums::set_cover(db, id, cover); // not in the album here: keep the default
            }
        }
    }

    // People and faces.
    let persons: HashMap<&str, Value> =
        backup.persons.iter().map(|p| (p.uid.as_str(), json!({ "name": p.name, "hidden": p.hidden }))).collect();
    let mut conn = db.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (uid, value) in &persons {
        tx.execute("INSERT OR REPLACE INTO restore_persons (uid, value) VALUES (?1, ?2)", params![uid, value.to_string()])?;
    }
    let refs: HashMap<&str, &Value> = persons.iter().map(|(k, v)| (*k, v)).collect();
    let known: HashSet<&str> = refs.keys().copied().collect();
    let mut touched_media = Vec::new();
    for (id, item) in found.iter().filter(|(_, i)| !i.faces.is_empty()) {
        let stage: i64 = tx.query_row("SELECT stage FROM media WHERE id = ?1", [id], |r| r.get(0))?;
        for face in &item.faces {
            let value = json!({ "person": face.person, "locked": face.locked });
            if stage >= Stage::AI {
                sync::apply_face(&tx, *id, &face.b, &value, &refs, &known)?;
                report.faces += 1;
            } else {
                tx.execute("INSERT INTO restore_faces (media_id, box, value) VALUES (?1, ?2, ?3)", params![id, face.b, value.to_string()])?;
                report.faces_waiting += 1;
            }
        }
        touched_media.push(*id);
    }
    // Names for people this PC already has (by id), where it has none.
    for p in &backup.persons {
        tx.execute("UPDATE persons SET name = ?2 WHERE uid = ?1 AND name IS NULL AND ?2 IS NOT NULL", params![p.uid, p.name])?;
        if p.hidden {
            tx.execute("UPDATE persons SET hidden = 1 WHERE uid = ?1", [&p.uid])?;
        }
        if p.name.is_some() {
            let named: bool =
                tx.query_row("SELECT count(*) FROM persons WHERE uid = ?1 AND name IS NOT NULL", [&p.uid], |r| r.get(0))?;
            report.people_named += named as usize;
        }
    }
    tx.execute("DELETE FROM persons WHERE NOT EXISTS (SELECT 1 FROM faces WHERE person_id = persons.id)", [])?;
    record_people(&tx, &touched_media)?;
    tx.commit()?;
    Ok(report)
}

/// Record restored people and faces in synced folders' `.memora`.
fn record_people(conn: &Connection, media: &[i64]) -> Result<()> {
    if media.is_empty() {
        return Ok(());
    }
    let (faces, persons): (Vec<i64>, Vec<Option<i64>>) = {
        let mut stmt = conn.prepare("SELECT id, person_id FROM faces WHERE media_id IN (SELECT value FROM json_each(?1))")?;
        stmt.query_map([serde_json::to_string(media)?], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .unzip()
    };
    for p in persons.into_iter().flatten().collect::<HashSet<_>>() {
        sync::record_person(conn, p)?;
    }
    sync::record_faces(conn, &faces)
}

/// Apply a restored backup's faces to an item recognition just looked at.
pub(crate) fn apply_waiting_faces(conn: &Connection, media_id: i64) -> Result<usize> {
    let waiting: Vec<(String, String)> = {
        let mut stmt = conn.prepare_cached("SELECT box, value FROM restore_faces WHERE media_id = ?1")?;
        stmt.query_map([media_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?
    };
    if waiting.is_empty() {
        return Ok(0);
    }
    let persons: Vec<(String, Value)> = {
        let mut stmt = conn.prepare_cached("SELECT uid, value FROM restore_persons")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|r| r.map(|(u, v)| (u, serde_json::from_str(&v).unwrap_or(Value::Null))))
            .collect::<std::result::Result<_, _>>()?
    };
    let refs: HashMap<&str, &Value> = persons.iter().map(|(u, v)| (u.as_str(), v)).collect();
    let known: HashSet<&str> = refs.keys().copied().collect();
    let mut n = 0;
    for (b, value) in &waiting {
        let value: Value = serde_json::from_str(value).unwrap_or(Value::Null);
        n += sync::apply_face(conn, media_id, b, &value, &refs, &known)?;
    }
    conn.execute("DELETE FROM restore_faces WHERE media_id = ?1", [media_id])?;
    conn.execute("DELETE FROM restore_persons WHERE NOT EXISTS (SELECT 1 FROM restore_faces)", [])?;
    record_people(conn, &[media_id])?;
    Ok(n)
}

#[cfg(test)]
mod tests;
