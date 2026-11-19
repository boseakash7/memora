//! Library info that travels with the photos: a hidden `.memora` folder in a watched folder
//! keeps favorites, the Archive, the trash, albums and people names, so another PC that adds
//! the same folder (external drive, NAS, synced folder) shows the same library.
//!
//! ```text
//! <folder>\.memora\
//!   memora.json              format version, folder id, created by / when
//!   changes\<device>.jsonl   one PC's edits; each PC appends only to its own file
//! ```
//!
//! Each log starts with a header line (`{"memora":1,"device":..,"name":..,"gen":..}`), then
//! one line per change: `{"c":clock,"f":field,"p":item,"s":sub,"h":hash,"v":value}`.
//! `p` is the item's path relative to the folder holding `.memora` (lowercase, `/`), `h` its
//! blake3 hash, so a renamed file is still recognised. Fields:
//!
//! | `f` | key | value |
//! |---|---|---|
//! | `fav` | item | true / false |
//! | `arch`, `trash` | item | time it was archived / trashed, or null |
//! | `edit` | item | the photo's edit recipe (see `edit::Edit`), or null for the original |
//! | `album` | `s` = album uid | `{"name","created"}`, null once deleted |
//! | `album_item` | item + album uid | true / false |
//! | `album_cover` | album uid | `{"p","h"}` of the cover |
//! | `person` | `s` = person uid | `{"name","hidden"}` |
//! | `face` | item + `s` = box `x,y,w,h` | `{"person": uid or null, "locked": 0/1/2}` |
//!
//! Merging is "last change wins" per field, by a hybrid logical clock (wall time, counter,
//! device id), so clocks that are a bit off don't matter. Every known value is kept in
//! `sync_reg`, and [`apply`] makes the library tables match it. The local database stays the
//! working copy: edits are recorded in the same transaction as the change ([`record`]) and
//! written to the folder later ([`flush`]), so an unplugged drive loses nothing.
//!
//! Nothing is ever written by two PCs, so this works the same on a USB drive, a NAS and a
//! cloud-synced folder; a half-synced log just ends in an incomplete line that waits.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use r2d2_sqlite::rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::Db;
use crate::folders::{self, Folder};
use crate::people::AUTO;
use crate::{Error, Result};

/// Name of the folder kept next to the photos.
pub const DIR: &str = ".memora";
const FORMAT: u32 = 1;
/// A log this many times longer than what it still needs is rewritten.
const COMPACT_RATIO: usize = 3;

// ---------- device and clock ----------

/// Host name, so a data folder copied to another PC gets a device id of its own.
fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

/// This PC's id in `.memora` logs. A new one is made if the database moves to another PC.
pub fn device_id(conn: &Connection) -> Result<String> {
    let host = host_name();
    let stored: Option<String> =
        conn.query_row("SELECT value FROM settings WHERE key = '_syncDevice'", [], |r| r.get(0)).optional()?;
    if let Some(v) = stored.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        && v["host"] == host.as_str()
        && let Some(id) = v["id"].as_str()
    {
        return Ok(id.to_owned());
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('_syncDevice', ?1)",
        [json!({ "id": id, "host": host }).to_string()],
    )?;
    Ok(id)
}

/// Last clock handed out or seen: (unix ms, counter). Shared by all libraries in the process,
/// which only makes it more monotonic.
static CLOCK: Mutex<(u64, u64)> = Mutex::new((0, 0));

fn format_clock(ms: u64, n: u64, device: &str) -> String {
    format!("{ms:013}.{n:08}.{device}")
}

/// A clock newer than every clock handed out or seen before.
fn next_clock(device: &str) -> String {
    let now = Utc::now().timestamp_millis().max(0) as u64;
    let mut c = CLOCK.lock().unwrap();
    if now > c.0 {
        *c = (now, 0);
    } else if c.1 >= 99_999_999 {
        *c = (c.0 + 1, 0);
    } else {
        c.1 += 1;
    }
    format_clock(c.0, c.1, device)
}

/// Remember a clock from another PC, so our next change sorts after it.
fn observe(clock: &str) {
    let mut parts = clock.split('.');
    let (Some(Ok(ms)), Some(Ok(n))) = (parts.next().map(str::parse::<u64>), parts.next().map(str::parse::<u64>)) else {
        return;
    };
    let mut c = CLOCK.lock().unwrap();
    if (ms, n) > *c {
        *c = (ms, n);
    }
}

/// Clock of values that were there before syncing was switched on: anything recorded
/// elsewhere wins over them.
fn seed_clock(device: &str) -> String {
    format_clock(0, 0, device)
}

// ---------- paths ----------

/// The folder holding the `.memora` that covers `path`: `path` itself or a parent.
pub fn find_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|p| p.join(DIR).join("memora.json").is_file()).map(Path::to_path_buf)
}

fn root_prefix(root: &Path) -> String {
    let mut s = root.to_string_lossy().to_lowercase();
    if !s.ends_with(MAIN_SEPARATOR) {
        s.push(MAIN_SEPARATOR);
    }
    s
}

/// `path` relative to the `.memora` folder's parent, as stored in logs.
fn rel_of(prefix: &str, path: &str) -> Option<String> {
    path.to_lowercase().strip_prefix(prefix).map(|r| r.replace(MAIN_SEPARATOR, "/"))
}

fn log_path(root: &Path, device: &str) -> PathBuf {
    root.join(DIR).join("changes").join(format!("{device}.jsonl"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    memora: u32,
    device: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "gen")]
    generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Line {
    c: String,
    f: String,
    #[serde(default)]
    p: String,
    #[serde(default)]
    s: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h: Option<String>,
    v: Value,
}

/// Create `.memora` (hidden) with its `memora.json` if it is not there yet.
fn init_dir(root: &Path) -> Result<()> {
    let dir = root.join(DIR);
    let new = !dir.is_dir();
    fs::create_dir_all(dir.join("changes"))?;
    if new {
        hide(&dir);
    }
    let info = dir.join("memora.json");
    if !info.is_file() {
        let body = json!({
            "format": FORMAT,
            "id": uuid::Uuid::new_v4().to_string(),
            "createdBy": host_name(),
            "createdAt": Utc::now().to_rfc3339(),
            "about": "Library info for Memora (favorites, albums, people). Safe to delete; photos are not affected.",
        });
        fs::write(&info, serde_json::to_vec_pretty(&body)?)?;
    }
    Ok(())
}

pub(crate) fn hide(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("attrib").arg("+h").arg(dir).creation_flags(CREATE_NO_WINDOW).status();
    }
    #[cfg(not(windows))]
    let _ = dir;
}

/// Whether this PC can write its log in `root`.
fn writable(root: &Path, device: &str) -> bool {
    let log = log_path(root, device);
    if log.is_file() {
        return OpenOptions::new().append(true).open(&log).is_ok();
    }
    if init_dir(root).is_err() {
        return false;
    }
    let probe = log.with_extension("probe");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

// ---------- recording local changes ----------

/// One synced value to record.
pub(crate) struct Rec {
    folder: i64,
    field: &'static str,
    item: String,
    sub: String,
    hash: Option<String>,
    value: Value,
    /// Use this clock instead of a new one (copying a value to another folder, seeding).
    clock: Option<String>,
}

/// An item in a synced folder.
pub(crate) struct ItemRef {
    pub id: i64,
    pub folder: i64,
    pub rel: String,
    pub hash: Option<String>,
}

impl ItemRef {
    fn rec(&self, field: &'static str, sub: &str, value: Value) -> Rec {
        Rec { folder: self.folder, field, item: self.rel.clone(), sub: sub.to_owned(), hash: self.hash.clone(), value, clock: None }
    }
}

/// The items among `ids` that are in synced folders.
pub(crate) fn items(conn: &Connection, ids: &[i64]) -> Result<Vec<ItemRef>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT m.id, m.folder_id, m.path, m.blake3, f.sync_root FROM media m JOIN folders f ON f.id = m.folder_id
         WHERE f.sync = 1 AND f.sync_root IS NOT NULL AND m.id IN (SELECT value FROM json_each(?1))",
    )?;
    let rows = stmt.query_map([serde_json::to_string(ids)?], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, folder, path, hash, root) = row?;
        if let Some(rel) = rel_of(&root_prefix(Path::new(&root)), &path) {
            out.push(ItemRef { id, folder, rel, hash });
        }
    }
    Ok(out)
}

/// Store values as the newest known and queue them for the folders' logs.
pub(crate) fn record(conn: &Connection, recs: &[Rec]) -> Result<()> {
    if recs.is_empty() {
        return Ok(());
    }
    let device = device_id(conn)?;
    for r in recs {
        let clock = r.clock.clone().unwrap_or_else(|| next_clock(&device));
        conn.prepare_cached(
            "INSERT INTO sync_reg (folder_id, field, item, sub, hash, clock, value, own)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
             ON CONFLICT DO UPDATE SET hash = coalesce(excluded.hash, hash), clock = excluded.clock,
                 value = excluded.value, own = 1
             WHERE excluded.clock > sync_reg.clock",
        )?
        .execute(params![r.folder, r.field, r.item, r.sub, r.hash, clock, r.value.to_string()])?;
        let line = Line { c: clock, f: r.field.into(), p: r.item.clone(), s: r.sub.clone(), h: r.hash.clone(), v: r.value.clone() };
        conn.prepare_cached("INSERT INTO sync_outbox (folder_id, line) VALUES (?1, ?2)")?
            .execute(params![r.folder, serde_json::to_string(&line)?])?;
    }
    Ok(())
}

/// Record a per-item field (`fav`, `arch`, `trash`) for items in synced folders.
pub(crate) fn record_media(conn: &Connection, ids: &[i64], field: &'static str, value: Value) -> Result<()> {
    let recs: Vec<Rec> = items(conn, ids)?.iter().map(|i| i.rec(field, "", value.clone())).collect();
    record(conn, &recs)
}

/// Forget what is recorded for items that are leaving the library for the locked folder, so
/// their names and hashes leave this PC's `.memora` logs: the logs of their folders are
/// rewritten on the next flush (kept marked until the folder can be written). Other PCs' logs
/// can't be changed from here.
pub(crate) fn forget_items(conn: &Connection, ids: &[i64]) -> Result<()> {
    for it in items(conn, ids)? {
        conn.prepare_cached("DELETE FROM sync_reg WHERE folder_id = ?1 AND item = ?2")?.execute(params![it.folder, it.rel])?;
        conn.prepare_cached(
            "DELETE FROM sync_reg WHERE folder_id = ?1 AND field = 'album_cover' AND json_extract(value, '$.p') = ?2",
        )?
        .execute(params![it.folder, it.rel])?;
        conn.prepare_cached(
            "DELETE FROM sync_outbox WHERE folder_id = ?1 AND (json_extract(line, '$.p') = ?2
                 OR (json_extract(line, '$.f') = 'album_cover' AND json_extract(line, '$.v.p') = ?2))",
        )?
        .execute(params![it.folder, it.rel])?;
        conn.prepare_cached("UPDATE folders SET log_dirty = 1 WHERE id = ?1")?.execute([it.folder])?;
    }
    Ok(())
}

/// Synced folders that already hold a value for an album or person.
fn entity_folders(conn: &Connection, field: &str, uid: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.folder_id FROM sync_reg r JOIN folders f ON f.id = r.folder_id
         WHERE f.sync = 1 AND r.field = ?1 AND r.item = '' AND r.sub = ?2",
    )?;
    Ok(stmt.query_map(params![field, uid], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
}

/// Make sure `folder` knows an album or person before something in it refers to it: copy
/// the newest value another folder has (keeping its clock), or record the current one.
fn ensure_entity(conn: &Connection, field: &'static str, uid: &str, folder: i64, current: &dyn Fn() -> Result<Value>) -> Result<()> {
    let known: bool = conn
        .prepare_cached("SELECT count(*) FROM sync_reg WHERE folder_id = ?1 AND field = ?2 AND item = '' AND sub = ?3")?
        .query_row(params![folder, field, uid], |r| r.get(0))?;
    if known {
        return Ok(());
    }
    let best: Option<(String, String)> = conn
        .prepare_cached("SELECT clock, value FROM sync_reg WHERE field = ?1 AND item = '' AND sub = ?2 ORDER BY clock DESC LIMIT 1")?
        .query_row(params![field, uid], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let (clock, value) = match best {
        Some((clock, value)) => (Some(clock), serde_json::from_str(&value)?),
        None => (None, current()?),
    };
    record(conn, &[Rec { folder, field, item: String::new(), sub: uid.to_owned(), hash: None, value, clock }])
}

fn album_value(conn: &Connection, album_id: i64) -> Result<Option<(String, Value)>> {
    Ok(conn
        .query_row("SELECT uid, name, created_at FROM albums WHERE id = ?1", [album_id], |r| {
            Ok((r.get::<_, String>(0)?, json!({ "name": r.get::<_, String>(1)?, "created": r.get::<_, String>(2)? })))
        })
        .optional()?)
}

/// An album was renamed (or is about to be deleted: `deleted`). Recorded in every synced
/// folder that has its items or knows it.
pub(crate) fn record_album(conn: &Connection, album_id: i64, deleted: bool) -> Result<()> {
    let Some((uid, value)) = album_value(conn, album_id)? else { return Ok(()) };
    let mut folders: HashSet<i64> = entity_folders(conn, "album", &uid)?.into_iter().collect();
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT m.folder_id FROM album_items i JOIN media m ON m.id = i.media_id
         JOIN folders f ON f.id = m.folder_id WHERE i.album_id = ?1 AND f.sync = 1",
    )?;
    folders.extend(stmt.query_map([album_id], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?);
    let value = if deleted { Value::Null } else { value };
    let recs: Vec<Rec> = folders
        .into_iter()
        .map(|folder| Rec { folder, field: "album", item: String::new(), sub: uid.clone(), hash: None, value: value.clone(), clock: None })
        .collect();
    record(conn, &recs)
}

/// Items were added to (`member`) or taken out of an album.
pub(crate) fn record_album_items(conn: &Connection, album_id: i64, media_ids: &[i64], member: bool) -> Result<()> {
    let refs = items(conn, media_ids)?;
    if refs.is_empty() {
        return Ok(());
    }
    let Some((uid, value)) = album_value(conn, album_id)? else { return Ok(()) };
    for folder in refs.iter().map(|r| r.folder).collect::<HashSet<_>>() {
        ensure_entity(conn, "album", &uid, folder, &|| Ok(value.clone()))?;
    }
    let recs: Vec<Rec> = refs.iter().map(|r| r.rec("album_item", &uid, json!(member))).collect();
    record(conn, &recs)
}

pub(crate) fn record_album_cover(conn: &Connection, album_id: i64, media_id: i64) -> Result<()> {
    let Some(item) = items(conn, &[media_id])?.into_iter().next() else { return Ok(()) };
    let Some((uid, value)) = album_value(conn, album_id)? else { return Ok(()) };
    ensure_entity(conn, "album", &uid, item.folder, &|| Ok(value.clone()))?;
    let cover = json!({ "p": item.rel, "h": item.hash });
    record(conn, &[Rec { folder: item.folder, field: "album_cover", item: String::new(), sub: uid, hash: None, value: cover, clock: None }])
}

fn person_value(conn: &Connection, person_id: i64) -> Result<Option<(String, Value)>> {
    Ok(conn
        .query_row("SELECT uid, name, hidden FROM persons WHERE id = ?1", [person_id], |r| {
            Ok((r.get::<_, String>(0)?, json!({ "name": r.get::<_, Option<String>>(1)?, "hidden": r.get::<_, bool>(2)? })))
        })
        .optional()?)
}

/// A face's box as written by `printf('%.3f,%.3f,%.3f,%.3f', x, y, w, h)` in the queries.
fn parse_face_key(s: &str) -> Option<[f32; 4]> {
    let v: Vec<f32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
    v.try_into().ok()
}

/// Record who these faces are (a person the user chose, "not this person", or the person
/// they were grouped into). Faces whose recorded value is already this are skipped.
/// A face as recorded: media id, box key, locked, person id and uid.
type FaceRow = (i64, String, i64, Option<i64>, Option<String>);

pub(crate) fn record_faces(conn: &Connection, face_ids: &[i64]) -> Result<()> {
    if face_ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<FaceRow> = {
        let mut stmt = conn.prepare_cached(
            "SELECT f.media_id, printf('%.3f,%.3f,%.3f,%.3f', f.x, f.y, f.w, f.h), f.locked, f.person_id, p.uid
             FROM faces f LEFT JOIN persons p ON p.id = f.person_id WHERE f.id IN (SELECT value FROM json_each(?1))",
        )?;
        stmt.query_map([serde_json::to_string(face_ids)?], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let media: Vec<i64> = rows.iter().map(|r| r.0).collect::<HashSet<_>>().into_iter().collect();
    let refs: HashMap<i64, ItemRef> = items(conn, &media)?.into_iter().map(|r| (r.id, r)).collect();
    let mut recs = Vec::new();
    let mut ensured: HashSet<(i64, i64)> = HashSet::new();
    for (media_id, sub, locked, person, uid) in rows {
        let Some(item) = refs.get(&media_id) else { continue };
        let value = json!({ "person": uid, "locked": locked });
        let current: Option<String> = conn
            .prepare_cached("SELECT value FROM sync_reg WHERE folder_id = ?1 AND field = 'face' AND item = ?2 AND sub = ?3")?
            .query_row(params![item.folder, item.rel, sub], |r| r.get(0))
            .optional()?;
        if current.and_then(|c| serde_json::from_str::<Value>(&c).ok()).as_ref() == Some(&value) {
            continue;
        }
        if let (Some(p), Some(uid)) = (person, uid.as_deref())
            && ensured.insert((item.folder, p))
        {
            ensure_entity(conn, "person", uid, item.folder, &|| Ok(person_value(conn, p)?.map(|v| v.1).unwrap_or(Value::Null)))?;
        }
        recs.push(item.rec("face", &sub, value));
    }
    record(conn, &recs)
}

/// A person was named, hidden or got faces merged in: record the person in every synced
/// folder with their faces, and which faces are theirs (so other PCs can find them).
pub(crate) fn record_person(conn: &Connection, person_id: i64) -> Result<()> {
    let Some((uid, value)) = person_value(conn, person_id)? else { return Ok(()) };
    let faces: Vec<i64> = {
        let mut stmt = conn.prepare_cached("SELECT id FROM faces WHERE person_id = ?1")?;
        stmt.query_map([person_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    let mut folders: HashSet<i64> = entity_folders(conn, "person", &uid)?.into_iter().collect();
    let media: Vec<i64> = {
        let mut stmt = conn.prepare_cached("SELECT DISTINCT media_id FROM faces WHERE person_id = ?1")?;
        stmt.query_map([person_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    folders.extend(items(conn, &media)?.iter().map(|i| i.folder));
    let recs: Vec<Rec> = folders
        .into_iter()
        .map(|folder| Rec { folder, field: "person", item: String::new(), sub: uid.clone(), hash: None, value: value.clone(), clock: None })
        .collect();
    record(conn, &recs)?;
    record_faces(conn, &faces)
}

// ---------- switching on and off ----------

/// What is known about a folder before it is added.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    /// The folder with an existing `.memora` covering this one (itself or a parent).
    pub root: Option<PathBuf>,
    /// Names of the PCs that wrote library info there.
    pub pcs: Vec<String>,
}

pub fn probe(path: &Path) -> Probe {
    let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = find_root(&path);
    let pcs = root.as_deref().map(|r| pc_names(r, None)).unwrap_or_default();
    Probe { root, pcs }
}

/// Names of the PCs with a log in `root`, except `skip`.
fn pc_names(root: &Path, skip: Option<&str>) -> Vec<String> {
    let Ok(dir) = fs::read_dir(root.join(DIR).join("changes")) else { return Vec::new() };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| read_header(&e.path()).ok().flatten())
        .filter(|h| Some(h.device.as_str()) != skip)
        .map(|h| if h.name.is_empty() { h.device } else { h.name })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Start keeping a folder's library info in `.memora` (its own, or a parent's if there is
/// one). What the folder's items already have (favorites, albums...) is recorded with the
/// oldest possible clock, so it fills gaps but never overrides what other PCs recorded.
/// A folder that can't be written keeps the info on this PC (see [`status`]).
pub fn enable(db: &Db, folder_id: i64) -> Result<Folder> {
    let folder = folders::get(db, folder_id)?;
    if folder.sync {
        return Ok(folder);
    }
    let root = prepare_root(&folder.path);
    let mut conn = db.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE folders SET sync = 1, sync_root = ?2 WHERE id = ?1", params![folder_id, root.to_string_lossy()])?;
    seed(&tx, folder_id)?;
    tx.commit()?;
    drop(conn);
    folders::get(db, folder_id)
}

/// The folder whose `.memora` will hold `path`'s library info (a parent's if it has one,
/// else its own, created now if it can be).
pub(crate) fn prepare_root(path: &Path) -> PathBuf {
    let root = find_root(path).unwrap_or_else(|| path.to_path_buf());
    if let Err(e) = init_dir(&root) {
        tracing::warn!("cannot write library info to {}: {e}", root.display());
    }
    root
}

/// Stop syncing a folder. Its `.memora` stays; what this PC knows of it is forgotten, so
/// switching on again starts from what the folder holds then.
pub fn disable(db: &Db, folder_id: i64) -> Result<Folder> {
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE folders SET sync = 0, index_mark = NULL WHERE id = ?1", [folder_id])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    for table in ["sync_reg", "sync_outbox", "sync_cursor"] {
        tx.execute(&format!("DELETE FROM {table} WHERE folder_id = ?1"), [folder_id])?;
    }
    tx.commit()?;
    drop(conn);
    folders::get(db, folder_id)
}

/// Stop syncing and move the folder's `.memora` (every PC's info) to the Recycle Bin.
/// Only for a `.memora` in the folder itself, not one shared with a parent.
pub fn remove_info(db: &Db, folder_id: i64) -> Result<Folder> {
    let folder = folders::get(db, folder_id)?;
    let dir = folder.path.join(DIR);
    let folder = disable(db, folder_id)?;
    if dir.is_dir() {
        // The locked folder's items stay (they would be lost, or readable by no one, in the
        // Recycle Bin): only the library info goes.
        let vault = dir.join(crate::vault::PLACE_DIR);
        let targets: Vec<PathBuf> = if vault.exists() {
            fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| *p != vault).collect()
        } else {
            vec![dir.clone()]
        };
        for t in targets {
            trash::delete(&t).map_err(|e| Error::invalid(format!("cannot remove {}: {e}", t.display())))?;
        }
    }
    Ok(folder)
}

/// The folder moved: find its `.memora` again.
pub fn relocated(db: &Db, folder_id: i64) -> Result<()> {
    let folder = folders::get(db, folder_id)?;
    if folder.sync {
        let root = find_root(&folder.path).unwrap_or(folder.path);
        db.conn()?.execute("UPDATE folders SET sync_root = ?2 WHERE id = ?1", params![folder_id, root.to_string_lossy()])?;
    }
    Ok(())
}

fn seed(conn: &Connection, folder_id: i64) -> Result<()> {
    let device = device_id(conn)?;
    let seed = seed_clock(&device);
    let ids = |sql: &str| -> Result<Vec<i64>> {
        let mut stmt = conn.prepare(sql)?;
        Ok(stmt.query_map([folder_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
    };
    let all = ids("SELECT id FROM media WHERE folder_id = ?1")?;
    let refs: HashMap<i64, ItemRef> = items(conn, &all)?.into_iter().map(|r| (r.id, r)).collect();
    let mut recs = Vec::new();
    let mut push = |r: Rec| recs.push(Rec { clock: Some(seed.clone()), ..r });

    let mut stmt = conn.prepare("SELECT id, favorite, archived_at, trashed_at, edit FROM media WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, bool>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (id, fav, arch, trash, edit) = row?;
        let Some(item) = refs.get(&id) else { continue };
        if let Some(e) = edit.as_deref().and_then(crate::edit::parse) {
            push(item.rec("edit", "", serde_json::to_value(e)?));
        }
        if fav {
            push(item.rec("fav", "", json!(true)));
        }
        if let Some(t) = arch {
            push(item.rec("arch", "", json!(t)));
        }
        if let Some(t) = trash {
            push(item.rec("trash", "", json!(t)));
        }
    }
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT i.album_id, i.media_id, a.uid, a.name, a.created_at, a.cover_media_id = i.media_id
         FROM album_items i JOIN albums a ON a.id = i.album_id JOIN media m ON m.id = i.media_id WHERE m.folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder_id], |r| {
        Ok((r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, bool>(5)?))
    })?;
    let mut albums: HashMap<String, Value> = HashMap::new();
    for row in rows {
        let (media, uid, name, created, is_cover) = row?;
        let Some(item) = refs.get(&media) else { continue };
        albums.entry(uid.clone()).or_insert_with(|| json!({ "name": name, "created": created }));
        push(item.rec("album_item", &uid, json!(true)));
        if is_cover {
            push(Rec {
                folder: folder_id,
                field: "album_cover",
                item: String::new(),
                sub: uid,
                hash: None,
                value: json!({ "p": item.rel, "h": item.hash }),
                clock: None,
            });
        }
    }
    drop(stmt);

    // Faces the user decided about, and the faces of people they named or hid.
    let faces = ids(
        "SELECT f.id FROM faces f JOIN media m ON m.id = f.media_id LEFT JOIN persons p ON p.id = f.person_id
         WHERE m.folder_id = ?1 AND (f.locked != 0 OR p.name IS NOT NULL OR p.hidden = 1)",
    )?;
    let mut stmt = conn.prepare(
        "SELECT f.media_id, printf('%.3f,%.3f,%.3f,%.3f', f.x, f.y, f.w, f.h), f.locked, p.uid, p.name, p.hidden
         FROM faces f LEFT JOIN persons p ON p.id = f.person_id WHERE f.id IN (SELECT value FROM json_each(?1))",
    )?;
    let rows = stmt.query_map([serde_json::to_string(&faces)?], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<bool>>(5)?,
        ))
    })?;
    let mut persons: HashMap<String, Value> = HashMap::new();
    for row in rows {
        let (media, sub, locked, uid, name, hidden) = row?;
        let Some(item) = refs.get(&media) else { continue };
        if let Some(uid) = &uid {
            persons.entry(uid.clone()).or_insert_with(|| json!({ "name": name, "hidden": hidden.unwrap_or(false) }));
        }
        push(item.rec("face", &sub, json!({ "person": uid, "locked": locked })));
    }
    drop(stmt);

    for (field, map) in [("album", albums), ("person", persons)] {
        for (uid, value) in map {
            push(Rec { folder: folder_id, field, item: String::new(), sub: uid, hash: None, value, clock: None });
        }
    }
    record(conn, &recs)
}

// ---------- writing and reading logs ----------

/// Write queued changes to the folders' `.memora`. Folders that can't be reached or written
/// keep theirs queued.
pub fn flush(db: &Db) -> Result<()> {
    let pending: Vec<i64> = {
        let conn = db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT folder_id FROM sync_outbox UNION SELECT id FROM folders WHERE log_dirty = 1 AND sync = 1",
        )?;
        stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    for folder_id in pending {
        if let Err(e) = flush_folder(db, folder_id) {
            tracing::debug!("library info for folder {folder_id} not written yet: {e}");
        }
    }
    Ok(())
}

fn sync_root(db: &Db, folder_id: i64) -> Result<Option<PathBuf>> {
    let folder = folders::get(db, folder_id)?;
    Ok(folder.sync_root.filter(|r| folder.sync && r.is_dir()))
}

fn flush_folder(db: &Db, folder_id: i64) -> Result<()> {
    let Some(root) = sync_root(db, folder_id)? else { return Ok(()) };
    let conn = db.conn()?;
    let device = device_id(&conn)?;
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, line FROM sync_outbox WHERE folder_id = ?1 ORDER BY id")?;
        stmt.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?
    };
    let dirty: bool = conn.query_row("SELECT log_dirty FROM folders WHERE id = ?1", [folder_id], |r| r.get(0))?;
    if rows.is_empty() && !dirty {
        return Ok(());
    }
    let last = rows.last().map_or(0, |r| r.0);
    let log = log_path(&root, &device);
    if dirty || read_header(&log)?.is_none_or(|h| h.device != device) {
        // No log yet (or an unusable one), or items left for the locked folder: write
        // everything this PC knows for the folder.
        rewrite_log(&conn, folder_id, &root, &device)?;
        conn.execute("UPDATE folders SET log_dirty = 0 WHERE id = ?1", [folder_id])?;
    } else {
        let mut text = String::new();
        for (_, line) in &rows {
            text.push_str(line);
            text.push('\n');
        }
        let mut f = OpenOptions::new().append(true).open(&log)?;
        f.write_all(text.as_bytes())?;
        f.sync_data()?;
    }
    conn.execute("DELETE FROM sync_outbox WHERE folder_id = ?1 AND id <= ?2", params![folder_id, last])?;
    Ok(())
}

/// Write this PC's log for a folder from scratch (new, or compacted): a header with a new
/// generation, then this PC's current values. Other PCs see the new generation and read
/// it again from the start.
fn rewrite_log(conn: &Connection, folder_id: i64, root: &Path, device: &str) -> Result<()> {
    init_dir(root)?;
    let header = Header { memora: FORMAT, device: device.to_owned(), name: host_name(), generation: uuid::Uuid::new_v4().simple().to_string() };
    let mut text = serde_json::to_string(&header)? + "\n";
    let mut stmt = conn.prepare(
        "SELECT clock, field, item, sub, hash, value FROM sync_reg WHERE folder_id = ?1 AND own = 1 ORDER BY clock",
    )?;
    let rows = stmt.query_map([folder_id], |r| {
        Ok((Line { c: r.get(0)?, f: r.get(1)?, p: r.get(2)?, s: r.get(3)?, h: r.get(4)?, v: Value::Null }, r.get::<_, String>(5)?))
    })?;
    for row in rows {
        let (mut line, value) = row?;
        line.v = serde_json::from_str(&value)?;
        text.push_str(&serde_json::to_string(&line)?);
        text.push('\n');
    }
    let log = log_path(root, device);
    let tmp = log.with_extension("jsonl.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &log)?;
    Ok(())
}

/// Rewrite this PC's logs that have grown much longer than what they still say.
pub fn compact(db: &Db) -> Result<()> {
    flush(db)?;
    for folder in folders::list(db)?.into_iter().filter(|f| f.sync) {
        let Some(root) = sync_root(db, folder.id)? else { continue };
        let conn = db.conn()?;
        let device = device_id(&conn)?;
        let log = log_path(&root, &device);
        let Ok(text) = fs::read(&log) else { continue };
        let lines = text.iter().filter(|&&b| b == b'\n').count();
        let needed: i64 =
            conn.query_row("SELECT count(*) FROM sync_reg WHERE folder_id = ?1 AND own = 1", [folder.id], |r| r.get(0))?;
        if lines > (needed as usize + 50) * COMPACT_RATIO {
            let pending: i64 = conn.query_row("SELECT count(*) FROM sync_outbox WHERE folder_id = ?1", [folder.id], |r| r.get(0))?;
            if pending == 0 {
                rewrite_log(&conn, folder.id, &root, &device)?;
                tracing::info!("compacted library info in {}: {lines} lines to {needed}", root.display());
            }
        }
    }
    Ok(())
}

/// The header of a log, None if the file is missing or its first line is incomplete.
fn read_header(path: &Path) -> Result<Option<Header>> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut buf = vec![0u8; 512];
    let n = f.read(&mut buf)?;
    let Some(end) = buf[..n].iter().position(|&b| b == b'\n') else { return Ok(None) };
    Ok(serde_json::from_slice(&buf[..end]).ok())
}

/// Read what other PCs (and this one, earlier) wrote to the synced folders since last time.
/// Returns true if anything new was learned; then [`apply`] brings the library up to date.
pub fn read(db: &Db) -> Result<bool> {
    let mut learned = false;
    for folder in folders::list(db)?.into_iter().filter(|f| f.sync) {
        let Some(root) = sync_root(db, folder.id)? else { continue };
        let Ok(dir) = fs::read_dir(root.join(DIR).join("changes")) else { continue };
        for entry in dir.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            match read_log(db, folder.id, &path) {
                Ok(n) => learned |= n,
                Err(e) => tracing::warn!("cannot read {}: {e}", path.display()),
            }
        }
    }
    Ok(learned)
}

fn read_log(db: &Db, folder_id: i64, path: &Path) -> Result<bool> {
    let Some(header) = read_header(path)? else { return Ok(false) };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut conn = db.conn()?;
    let me = device_id(&conn)?;
    let cursor: Option<(String, i64)> = conn
        .query_row("SELECT gen, offset FROM sync_cursor WHERE folder_id = ?1 AND file = ?2", params![folder_id, name], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut offset = match cursor {
        Some((g, offset)) if g == header.generation && offset as u64 <= len => offset as u64,
        _ => 0,
    };
    if offset == len {
        return Ok(false);
    }
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    // Only complete lines; a half-synced last line waits for the rest.
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return Ok(false) };
    let own = header.device == me;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut learned = false;
    for raw in buf[..end].split(|&b| b == b'\n') {
        let Ok(line) = serde_json::from_slice::<Line>(raw) else { continue }; // the header, or damaged
        observe(&line.c);
        let n = tx
            .prepare_cached(
                "INSERT INTO sync_reg (folder_id, field, item, sub, hash, clock, value, own)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET hash = coalesce(excluded.hash, hash), clock = excluded.clock,
                     value = excluded.value, own = excluded.own
                 WHERE excluded.clock > sync_reg.clock",
            )?
            .execute(params![folder_id, line.f, line.p, line.s, line.h, line.c, line.v.to_string(), own])?;
        learned |= n > 0;
    }
    offset += end as u64 + 1;
    tx.execute(
        "INSERT OR REPLACE INTO sync_cursor (folder_id, file, gen, offset) VALUES (?1, ?2, ?3, ?4)",
        params![folder_id, name, header.generation, offset as i64],
    )?;
    tx.commit()?;
    Ok(learned)
}

// ---------- applying ----------

struct Reg {
    folder: i64,
    field: String,
    item: String,
    sub: String,
    hash: Option<String>,
    clock: String,
    value: Value,
}

/// Lookup of a folder's items by relative path and by hash.
#[derive(Default)]
struct Index {
    by_path: HashMap<String, i64>,
    by_hash: HashMap<String, Vec<i64>>,
}

impl Index {
    /// The items a value is about: the one at its path, else every copy with its hash.
    fn resolve(&self, item: &str, hash: Option<&str>) -> Vec<i64> {
        if let Some(&id) = self.by_path.get(item) {
            return vec![id];
        }
        hash.and_then(|h| self.by_hash.get(h)).cloned().unwrap_or_default()
    }
}

fn load_index(conn: &Connection, folder_id: i64, root: &Path) -> Result<Index> {
    let prefix = root_prefix(root);
    let mut index = Index::default();
    // Rows of files that are gone would hide the renamed file their value is about.
    let mut stmt = conn.prepare("SELECT id, path, blake3 FROM media WHERE folder_id = ?1 AND missing = 0")?;
    let rows = stmt.query_map([folder_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
    for row in rows {
        let (id, path, hash) = row?;
        if let Some(rel) = rel_of(&prefix, &path) {
            index.by_path.insert(rel, id);
        }
        if let Some(h) = hash {
            index.by_hash.entry(h).or_default().push(id);
        }
    }
    Ok(index)
}

fn load_regs(conn: &Connection, sql_filter: &str, args: &[&dyn r2d2_sqlite::rusqlite::ToSql]) -> Result<Vec<Reg>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.folder_id, r.field, r.item, r.sub, r.hash, r.clock, r.value FROM sync_reg r
         JOIN folders f ON f.id = r.folder_id WHERE f.sync = 1 {sql_filter}"
    ))?;
    let rows = stmt.query_map(args, |r| {
        Ok(Reg {
            folder: r.get(0)?,
            field: r.get(1)?,
            item: r.get(2)?,
            sub: r.get(3)?,
            hash: r.get(4)?,
            clock: r.get(5)?,
            value: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Make the library match the newest known values of every synced folder. Safe to run any
/// time (it only changes what differs). Returns true if anything visible changed.
pub fn apply(db: &Db) -> Result<bool> {
    let mut conn = db.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = apply_all(&tx)?;
    tx.commit()?;
    Ok(changed > 0)
}

fn apply_all(conn: &Connection) -> Result<usize> {
    let roots: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, sync_root FROM folders WHERE sync = 1 AND sync_root IS NOT NULL")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?
    };
    if roots.is_empty() {
        return Ok(0);
    }
    let mut indexes: HashMap<i64, Index> = HashMap::new();
    for (id, root) in &roots {
        indexes.insert(*id, load_index(conn, *id, Path::new(root))?);
    }
    let regs = load_regs(conn, "", &[])?;

    // Newest value per album/person field, and per item field (by the item it resolves to).
    let mut entity: HashMap<(&str, &str), &Reg> = HashMap::new();
    let mut per_item: HashMap<(&str, &str, i64), &Reg> = HashMap::new();
    for r in &regs {
        if r.item.is_empty() {
            let e = entity.entry((r.field.as_str(), r.sub.as_str())).or_insert(r);
            if r.clock > e.clock {
                *e = r;
            }
            continue;
        }
        let Some(index) = indexes.get(&r.folder) else { continue };
        for id in index.resolve(&r.item, r.hash.as_deref()) {
            let e = per_item.entry((r.field.as_str(), r.sub.as_str(), id)).or_insert(r);
            if r.clock > e.clock {
                *e = r;
            }
        }
    }

    let mut changed = 0;
    // Favorites, Archive, trash.
    for ((field, _, id), r) in &per_item {
        let (column, value) = match *field {
            "fav" => ("favorite", Value::Bool(r.value.as_bool().unwrap_or(false))),
            "arch" => ("archived_at", r.value.clone()),
            "trash" => ("trashed_at", r.value.clone()),
            _ => continue,
        };
        let value: Option<String> = match value {
            Value::Bool(b) => Some(if b { "1" } else { "0" }.into()),
            Value::String(s) => Some(s),
            _ => None,
        };
        let sql = if column == "favorite" {
            "UPDATE media SET favorite = CAST(?2 AS INTEGER) WHERE id = ?1 AND favorite != CAST(?2 AS INTEGER)".to_owned()
        } else {
            format!("UPDATE media SET {column} = ?2 WHERE id = ?1 AND {column} IS NOT ?2")
        };
        changed += conn.prepare_cached(&sql)?.execute(params![id, value])?;
    }

    // Edits.
    for ((field, _, id), r) in &per_item {
        if *field == "edit" {
            changed += apply_edit(conn, *id, &r.value)?;
        }
    }

    // Albums: create, rename, delete.
    let mut album_ids: HashMap<&str, i64> = HashMap::new();
    for ((field, uid), r) in &entity {
        if *field != "album" {
            continue;
        }
        let local: Option<(i64, String)> = conn
            .prepare_cached("SELECT id, name FROM albums WHERE uid = ?1")?
            .query_row([uid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match (r.value.get("name").and_then(Value::as_str), local) {
            (None, Some((id, _))) => {
                conn.execute("DELETE FROM album_items WHERE album_id = ?1", [id])?;
                changed += conn.execute("DELETE FROM albums WHERE id = ?1", [id])?;
            }
            (None, None) => {}
            (Some(name), Some((id, old))) => {
                if old != name {
                    changed += conn.execute("UPDATE albums SET name = ?2 WHERE id = ?1", params![id, name])?;
                }
                album_ids.insert(*uid, id);
            }
            (Some(name), None) => {
                let created = r.value.get("created").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| Utc::now().to_rfc3339());
                conn.execute("INSERT INTO albums (name, created_at, uid) VALUES (?1, ?2, ?3)", params![name, created, uid])?;
                album_ids.insert(*uid, conn.last_insert_rowid());
                changed += 1;
            }
        }
    }
    for ((field, uid, id), r) in &per_item {
        if *field != "album_item" {
            continue;
        }
        let Some(&album) = album_ids.get(uid) else { continue };
        changed += if r.value.as_bool().unwrap_or(false) {
            conn.prepare_cached("INSERT OR IGNORE INTO album_items (album_id, media_id, added_at) VALUES (?1, ?2, ?3)")?
                .execute(params![album, id, Utc::now().to_rfc3339()])?
        } else {
            conn.prepare_cached("DELETE FROM album_items WHERE album_id = ?1 AND media_id = ?2")?.execute(params![album, id])?
        };
    }
    for ((field, uid), r) in &entity {
        if *field != "album_cover" {
            continue;
        }
        let (Some(&album), Some(index)) = (album_ids.get(uid), indexes.get(&r.folder)) else { continue };
        let p = r.value.get("p").and_then(Value::as_str).unwrap_or_default();
        let Some(&cover) = index.resolve(p, r.value.get("h").and_then(Value::as_str)).first() else { continue };
        changed += conn.execute(
            "UPDATE albums SET cover_media_id = ?2 WHERE id = ?1 AND cover_media_id IS NOT ?2
             AND EXISTS (SELECT 1 FROM album_items WHERE album_id = ?1 AND media_id = ?2)",
            params![album, cover],
        )?;
    }

    // People: names and hidden for those this PC has; faces.
    let persons: HashMap<&str, &Value> =
        entity.iter().filter(|((f, _), _)| *f == "person").map(|((_, uid), r)| (*uid, &r.value)).collect();
    for (uid, value) in &persons {
        changed += conn.prepare_cached(
            "UPDATE persons SET name = ?2, hidden = ?3 WHERE uid = ?1 AND (name IS NOT ?2 OR hidden != ?3)",
        )?
        .execute(params![uid, value.get("name").and_then(Value::as_str), value.get("hidden").and_then(Value::as_bool).unwrap_or(false)])?;
    }
    let known_uids: HashSet<&str> = persons.keys().copied().collect();
    for ((field, sub, id), r) in &per_item {
        if *field == "face" {
            changed += apply_face(conn, *id, sub, &r.value, &persons, &known_uids)?;
        }
    }
    if changed > 0 {
        conn.execute("DELETE FROM persons WHERE NOT EXISTS (SELECT 1 FROM faces WHERE person_id = persons.id)", [])?;
    }
    Ok(changed)
}

/// Make an item's edit match a recorded one (a recipe, or null for the original).
fn apply_edit(conn: &Connection, id: i64, value: &Value) -> Result<usize> {
    let wanted = serde_json::from_value::<crate::edit::Edit>(value.clone())
        .ok()
        .map(crate::edit::Edit::normalized)
        .filter(|e| !e.is_identity());
    let current: Option<Option<String>> =
        conn.prepare_cached("SELECT edit FROM media WHERE id = ?1 AND kind = 'photo'")?.query_row([id], |r| r.get(0)).optional()?;
    let Some(current) = current else { return Ok(0) };
    if current.as_deref().and_then(crate::edit::parse) == wanted {
        return Ok(0);
    }
    let (json, at, w, h) = crate::edit::columns(conn, id, wanted.as_ref())?;
    Ok(conn
        .prepare_cached("UPDATE media SET edit = ?2, edited_at = ?3, edit_width = ?4, edit_height = ?5 WHERE id = ?1")?
        .execute(params![id, json, at, w, h])?)
}

/// Apply the recorded faces of one item that recognition just looked at, so names and
/// corrections are in place before automatic grouping runs.
pub(crate) fn apply_faces_for(conn: &Connection, media_id: i64) -> Result<usize> {
    let Some(item) = items(conn, &[media_id])?.into_iter().next() else { return Ok(0) };
    let regs = load_regs(
        conn,
        "AND r.folder_id = ?1 AND r.field = 'face' AND (r.item = ?2 OR (r.hash IS NOT NULL AND r.hash = ?3))",
        &[&item.folder, &item.rel, &item.hash],
    )?;
    // A value at the item's path wins over one found by hash (a copy elsewhere).
    let at_path = regs.iter().any(|r| r.item == item.rel);
    let mut newest: HashMap<&str, &Reg> = HashMap::new();
    for r in regs.iter().filter(|r| !at_path || r.item == item.rel) {
        let e = newest.entry(r.sub.as_str()).or_insert(r);
        if r.clock > e.clock {
            *e = r;
        }
    }
    if newest.is_empty() {
        return Ok(0);
    }
    let people = load_regs(conn, "AND r.field = 'person' AND r.item = ''", &[])?;
    let mut persons: HashMap<&str, &Reg> = HashMap::new();
    for r in &people {
        let e = persons.entry(r.sub.as_str()).or_insert(r);
        if r.clock > e.clock {
            *e = r;
        }
    }
    let persons: HashMap<&str, &Value> = persons.into_iter().map(|(k, r)| (k, &r.value)).collect();
    let known: HashSet<&str> = persons.keys().copied().collect();
    let mut changed = 0;
    for (sub, r) in newest {
        changed += apply_face(conn, media_id, sub, &r.value, &persons, &known)?;
    }
    Ok(changed)
}

/// The local person with this uid, made (with its recorded name) if this PC has none yet.
fn person_for(conn: &Connection, uid: &str, persons: &HashMap<&str, &Value>) -> Result<i64> {
    if let Some(id) = conn.prepare_cached("SELECT id FROM persons WHERE uid = ?1")?.query_row([uid], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    let value = persons.get(uid);
    conn.execute(
        "INSERT INTO persons (name, hidden, created_at, uid) VALUES (?1, ?2, ?3, ?4)",
        params![
            value.and_then(|v| v.get("name")).and_then(Value::as_str),
            value.and_then(|v| v.get("hidden")).and_then(Value::as_bool).unwrap_or(false),
            Utc::now().to_rfc3339(),
            uid
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Apply who one face is. Faces are matched by position (the same face found on another
/// PC has nearly the same box). A face the user set on another PC is set the same here.
/// For a face that was grouped automatically there, an unnamed group this PC made on its
/// own is folded into that person, so their name covers it.
pub(crate) fn apply_face(conn: &Connection, media_id: i64, sub: &str, value: &Value, persons: &HashMap<&str, &Value>, known: &HashSet<&str>) -> Result<usize> {
    let Some(rect) = parse_face_key(sub) else { return Ok(0) };
    let faces: Vec<(i64, [f32; 4], Option<i64>, i64)> = {
        let mut stmt = conn.prepare_cached("SELECT id, x, y, w, h, person_id, locked FROM faces WHERE media_id = ?1")?;
        stmt.query_map([media_id], |r| Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?], r.get(5)?, r.get(6)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let Some(&(face, _, current, locked_now)) =
        faces.iter().map(|f| (crate::ai::iou(f.1, rect), f)).filter(|(o, _)| *o > 0.5).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, f)| f)
    else {
        return Ok(0); // not found here (yet)
    };
    let locked = value.get("locked").and_then(Value::as_i64).unwrap_or(AUTO);
    let uid = value.get("person").and_then(Value::as_str);
    if locked != AUTO {
        let target = match uid {
            Some(uid) => Some(person_for(conn, uid, persons)?),
            None => None,
        };
        return Ok(conn
            .prepare_cached("UPDATE faces SET person_id = ?2, locked = ?3 WHERE id = ?1 AND (person_id IS NOT ?2 OR locked != ?3)")?
            .execute(params![face, target, locked])?);
    }
    let Some(uid) = uid else { return Ok(0) };
    if locked_now != AUTO {
        return Ok(0); // decided by the user on this PC
    }
    match current {
        None => {
            let target = person_for(conn, uid, persons)?;
            Ok(conn.execute("UPDATE faces SET person_id = ?2 WHERE id = ?1", params![face, target])?)
        }
        Some(p) => {
            let (p_uid, p_name, p_hidden): (String, Option<String>, bool) =
                conn.query_row("SELECT uid, name, hidden FROM persons WHERE id = ?1", [p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            if p_uid == uid || p_name.is_some() || p_hidden || known.contains(p_uid.as_str()) {
                return Ok(0);
            }
            let target = person_for(conn, uid, persons)?;
            let n = conn.execute("UPDATE faces SET person_id = ?2 WHERE person_id = ?1", params![p, target])?;
            conn.execute("DELETE FROM persons WHERE id = ?1", [p])?;
            Ok(n)
        }
    }
}

// ---------- status ----------

/// How a folder's library info is kept, for Settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The folder holding `.memora`.
    pub root: PathBuf,
    /// False when this PC can't write there (read-only drive or share): the info stays on
    /// this PC until it can.
    pub writable: bool,
    /// Other PCs that keep their info there.
    pub other_pcs: Vec<String>,
    /// Changes not written to the folder yet (unplugged or read-only).
    pub unsent: u64,
    /// Size of this PC's index snapshot there, when the index is kept too.
    pub index_bytes: Option<u64>,
}

pub fn status(db: &Db, folder: &Folder) -> Result<Option<Status>> {
    let Some(root) = folder.sync_root.clone().filter(|_| folder.sync) else { return Ok(None) };
    let conn = db.conn()?;
    let device = device_id(&conn)?;
    let unsent: i64 = conn.query_row("SELECT count(*) FROM sync_outbox WHERE folder_id = ?1", [folder.id], |r| r.get(0))?;
    let reachable = root.is_dir();
    Ok(Some(Status {
        writable: reachable && writable(&root, &device),
        other_pcs: if reachable { pc_names(&root, Some(&device)) } else { Vec::new() },
        index_bytes: if folder.keep_index { crate::snapshot::own_size(db, &root)? } else { None },
        root,
        unsent: unsent as u64,
    }))
}

/// True if a changed path is inside a `.memora` folder.
pub fn is_info_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str().eq_ignore_ascii_case(DIR))
}

/// True if a path is inside a `.memoraault` (the locked folder's place in a watched folder).
pub fn is_vault_path(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str()).collect();
    parts.windows(2).any(|w| w[0].eq_ignore_ascii_case(DIR) && w[1].eq_ignore_ascii_case(crate::vault::PLACE_DIR))
}

#[cfg(test)]
mod tests;
