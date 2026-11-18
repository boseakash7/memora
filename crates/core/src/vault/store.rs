//! The locked folder's storage: encrypted files plus `vault.db`, in one or more places.
//!
//! Places: every watched folder keeps the items that came from it in
//! `<folder>\.memora\vault\`, next to the photos, so they stay on that drive, go along with copies
//! and backups of the folder, and open on another PC that adds the folder (with the password).
//! Items from outside any watched folder stay in `<data folder>\vault\` ("home").
//!
//! Nothing in a `vault.db` is readable without the password: `meta` holds only the key-wrapping
//! data (salt, Argon2 settings, the master key sealed by the password and by the recovery key,
//! and a key id that tells which places share a master key) and `items` a random id plus the
//! sealed record. Names, dates, places and sizes are inside the records, so listing and sorting
//! happen in memory after unlocking. Every place of one locked folder holds the same key data;
//! a place whose copy is older (the password was changed while its drive was away) gets the
//! current one at the next unlock.
//!
//! Files: `files\<2 hex>\<32 hex>`, every file with its own random name (an item's original,
//! thumbnail and preview can't be told apart from another item's).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use r2d2_sqlite::rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::crypto::{self, Decryptor, KdfParams, SecretKey};
use crate::{Error, Result};

const FORMAT: u32 = 1;
const PASSWORD_AAD: &[u8] = b"memora vault master key / password";
const RECOVERY_AAD: &[u8] = b"memora vault master key / recovery key";
/// Shortest password accepted.
pub const MIN_PASSWORD: usize = 6;
/// The folder inside a watched folder's `.memora`.
pub const PLACE_DIR: &str = "vault";

/// How the master key is stored (the `keys` row of `meta`). No secrets in plain text.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFile {
    format: u32,
    kdf: KdfParams,
    /// Master key sealed by the password's key, hex.
    by_password: String,
    /// Master key sealed by the recovery key's key, hex.
    by_recovery: String,
    /// Derived from the master key (reveals nothing about it); the same in every place of
    /// one locked folder. Empty in files from before places.
    #[serde(default)]
    key_id: String,
    /// When the password or recovery key last changed (unix ms): among places sharing a key,
    /// the newest key data counts, so an old password stops working everywhere.
    #[serde(default)]
    changed: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The key data to try: the newest of each key (and each copy from before key ids).
fn newest(keys: &[(PathBuf, KeyFile, String)]) -> Vec<&KeyFile> {
    let mut out: Vec<&KeyFile> = Vec::new();
    for (_, kf, _) in keys {
        if kf.key_id.is_empty() {
            if !out.iter().any(|o| o.by_password == kf.by_password) {
                out.push(kf);
            }
            continue;
        }
        match out.iter_mut().find(|o| o.key_id == kf.key_id) {
            Some(o) if kf.changed > o.changed => *o = kf,
            Some(_) => {}
            None => out.push(kf),
        }
    }
    out
}

fn key_id(master: &[u8; crypto::KEY_LEN]) -> String {
    crypto::hex(&blake3::derive_key("memora vault key id v1", master)[..16])
}

/// Watched folders (where places can be), asked for when needed.
type Roots = Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// What the locked folder knows about an item (kept sealed in `vault.db`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub name: String,
    /// Where the file was before it was moved in.
    pub original_path: String,
    pub kind: crate::media::Kind,
    pub mime: String,
    pub size: u64,
    /// File modification time, unix milliseconds (put back when it is moved out).
    pub mtime: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub taken_at: String,
    pub tz_offset: Option<i32>,
    pub camera: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub favorite: bool,
    /// BLAKE3 of the original file.
    pub hash: String,
    pub added_at: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    info: Info,
    /// The item's file key, hex.
    key: String,
    original: String,
    thumb: Option<String>,
    /// JPEG preview for photos the webview can't show (HEIC, RAW, TIFF).
    display: Option<String>,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl Record {
    fn file_key(&self) -> Result<SecretKey> {
        let bytes = Zeroizing::new(crypto::unhex(&self.key).ok_or_else(|| Error::invalid("bad item key"))?);
        let key: [u8; crypto::KEY_LEN] =
            bytes.as_slice().try_into().map_err(|_| Error::invalid("bad item key"))?;
        Ok(Zeroizing::new(key))
    }

    fn files(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.original).chain(self.thumb.iter()).chain(self.display.iter())
    }
}

/// An item as the UI lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    #[serde(flatten)]
    pub info: Info,
    pub has_display: bool,
}

/// Which of an item's files to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Original,
    Thumb,
    /// The JPEG preview, or the original when there is none.
    Display,
}

/// An item and the place its files are in.
struct Entry {
    place: PathBuf,
    rec: Record,
}

struct Unlocked {
    /// The master key the password opened, and its key data (written to new places).
    master: SecretKey,
    key: KeyFile,
    /// Every place that is open, with its master key (the same one, unless a folder brought a
    /// locked folder made elsewhere that the same password opens).
    masters: HashMap<PathBuf, SecretKey>,
    items: Vec<(String, Entry)>,
    last_used: Instant,
}

struct Inner {
    conns: HashMap<PathBuf, Connection>,
    unlocked: Option<Unlocked>,
}

/// The locked folder. Cheap to share (`Arc`).
pub struct Vault {
    home: PathBuf,
    roots: Roots,
    inner: Mutex<Inner>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// A locked folder has been set up.
    pub exists: bool,
    pub unlocked: bool,
    /// Items, when unlocked.
    pub count: Option<usize>,
}

/// What an item to move in brings.
pub struct NewItem<'a> {
    pub info: Info,
    pub original: &'a Path,
    pub thumb: Option<Vec<u8>>,
    pub display: Option<Vec<u8>>,
}

fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(Error::invalid(format!("use at least {MIN_PASSWORD} characters")));
    }
    Ok(())
}

fn has_db(place: &Path) -> bool {
    place.join("vault.db").is_file()
}

fn file_path(place: &Path, name: &str) -> PathBuf {
    place.join("files").join(&name[..2]).join(name)
}

/// Case-insensitive "is `path` inside `root`" (Windows paths).
fn inside(path: &Path, root: &Path) -> bool {
    let p = path.to_string_lossy().to_lowercase();
    let mut r = root.to_string_lossy().to_lowercase();
    if !r.ends_with(std::path::MAIN_SEPARATOR) {
        r.push(std::path::MAIN_SEPARATOR);
    }
    p.starts_with(&r)
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

fn read_key(conn: &Connection) -> Result<Option<(KeyFile, String)>> {
    let Some(text) =
        conn.query_row("SELECT value FROM meta WHERE key = 'keys'", [], |r| r.get::<_, String>(0)).optional()?
    else {
        return Ok(None);
    };
    let kf: KeyFile = serde_json::from_str(&text)?;
    if kf.format > FORMAT {
        return Err(Error::invalid("this locked folder was made by a newer version of Memora"));
    }
    Ok(Some((kf, text)))
}

fn write_key(conn: &Connection, kf: &KeyFile) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('keys', ?1) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [serde_json::to_string(kf)?],
    )?;
    Ok(())
}

fn open_master(sealed_hex: &str, key: &[u8; crypto::KEY_LEN], aad: &[u8]) -> Result<SecretKey> {
    let sealed = crypto::unhex(sealed_hex).ok_or_else(|| Error::invalid("damaged key data"))?;
    let plain = crypto::open(key, aad, &sealed).map_err(|_| Error::WrongPassword)?;
    let master: [u8; crypto::KEY_LEN] = plain.as_slice().try_into().map_err(|_| Error::invalid("damaged key data"))?;
    Ok(Zeroizing::new(master))
}

impl Inner {
    /// The place's database (created with its tables if it isn't there).
    fn conn(&mut self, place: &Path) -> Result<&Connection> {
        if !self.conns.contains_key(place) {
            let conn = Connection::open(place.join("vault.db"))?;
            conn.busy_timeout(Duration::from_secs(5))?;
            // Deleted records are overwritten with zeros, not left in free pages.
            conn.execute_batch(
                "PRAGMA secure_delete = ON;
                 CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY, data BLOB NOT NULL) WITHOUT ROWID;",
            )?;
            self.conns.insert(place.to_path_buf(), conn);
        }
        Ok(&self.conns[place])
    }
}

impl Vault {
    /// The locked folder whose home place is `home` (usually `<data folder>\vault`), with no
    /// watched folders. Nothing is created until it is set up.
    pub fn new(home: &Path) -> Vault {
        Vault::with_roots(home, Vec::new)
    }

    /// `roots` lists the watched folders; items from one of them are kept in its
    /// `.memora\vault`.
    pub fn with_roots(home: &Path, roots: impl Fn() -> Vec<PathBuf> + Send + Sync + 'static) -> Vault {
        Vault {
            home: home.to_path_buf(),
            roots: Box::new(roots),
            inner: Mutex::new(Inner { conns: HashMap::new(), unlocked: None }),
        }
    }

    /// The home place.
    pub fn dir(&self) -> &Path {
        &self.home
    }

    #[cfg(test)]
    pub(crate) fn files_dir(&self) -> PathBuf {
        self.home.join("files")
    }

    /// The place inside a watched folder.
    pub fn place_in(root: &Path) -> PathBuf {
        root.join(crate::sync::DIR).join(PLACE_DIR)
    }

    /// Home first, then each watched folder's place.
    fn places(&self) -> Vec<PathBuf> {
        let mut out = vec![self.home.clone()];
        for root in (self.roots)() {
            let place = Self::place_in(&root);
            if !out.iter().any(|p| same(p, &place)) {
                out.push(place);
            }
        }
        out
    }

    /// Where an item from `original` is kept: the place of the outermost watched folder holding
    /// it (when that folder is reachable), else home.
    fn place_for(&self, original: &Path) -> PathBuf {
        let mut roots: Vec<PathBuf> = (self.roots)().into_iter().filter(|r| inside(original, r)).collect();
        roots.sort_by_key(|r| r.as_os_str().len());
        roots.into_iter().find(|r| r.is_dir()).map(|r| Self::place_in(&r)).unwrap_or_else(|| self.home.clone())
    }

    /// Places that exist on disk now.
    fn existing(&self) -> Vec<PathBuf> {
        self.places().into_iter().filter(|p| has_db(p)).collect()
    }

    pub fn exists(&self) -> bool {
        !self.existing().is_empty()
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> State {
        let exists = self.exists();
        let inner = self.lock_inner();
        let unlocked = inner.unlocked.as_ref();
        State { exists, unlocked: unlocked.is_some(), count: unlocked.map(|u| u.items.len()) }
    }

    /// Set up a new locked folder. Returns the recovery key (shown once) and leaves it unlocked.
    pub fn setup(&self, password: &str) -> Result<Zeroizing<String>> {
        self.setup_with(password, KdfParams::new())
    }

    pub(crate) fn setup_with(&self, password: &str, kdf: KdfParams) -> Result<Zeroizing<String>> {
        check_password(password)?;
        if self.exists() {
            return Err(Error::invalid("a locked folder already exists"));
        }
        let mut inner = self.lock_inner();
        let master = crypto::random_key();
        let recovery = crypto::new_recovery_key();
        let rk = crypto::parse_recovery_key(&recovery).expect("a new recovery key reads back");
        let key = KeyFile {
            format: FORMAT,
            by_password: crypto::hex(&crypto::seal(&*crypto::password_key(password, &kdf)?, PASSWORD_AAD, master.as_ref())),
            by_recovery: crypto::hex(&crypto::seal(&crypto::recovery_wrap_key(&rk), RECOVERY_AAD, master.as_ref())),
            kdf,
            key_id: key_id(&master),
            changed: now_ms(),
        };
        Self::create_place(&mut inner, &self.home, &key)?;
        let masters = HashMap::from([(self.home.clone(), master.clone())]);
        inner.unlocked = Some(Unlocked { master, key, masters, items: Vec::new(), last_used: Instant::now() });
        Ok(recovery)
    }

    /// Make a place's folder and database with the key data.
    fn create_place(inner: &mut Inner, place: &Path, key: &KeyFile) -> Result<()> {
        let parent_existed = place.parent().is_some_and(Path::exists);
        fs::create_dir_all(place.join("files"))?;
        crate::sync::hide(place);
        if !parent_existed && let Some(parent) = place.parent() {
            crate::sync::hide(parent);
        }
        write_key(inner.conn(place)?, key)
    }

    /// Every place's key data, read from disk.
    fn read_keys(&self, inner: &mut Inner) -> Vec<(PathBuf, KeyFile, String)> {
        let mut out = Vec::new();
        for place in self.existing() {
            match inner.conn(&place).and_then(read_key) {
                Ok(Some((kf, text))) => out.push((place, kf, text)),
                Ok(None) => {}
                Err(e) => tracing::warn!("locked folder in {} can't be read: {e}", place.display()),
            }
        }
        out
    }

    /// Unlock with the password.
    pub fn unlock(&self, password: &str) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(u) = inner.unlocked.as_mut() {
            u.last_used = Instant::now();
            return Ok(());
        }
        let keys = self.read_keys(&mut inner);
        if keys.is_empty() {
            return Err(Error::invalid("the locked folder is not set up"));
        }
        // Try the current key data of each key (usually all places hold the same).
        let mut opened: Vec<(SecretKey, KeyFile)> = Vec::new();
        for kf in newest(&keys) {
            if let Ok(master) = open_master(&kf.by_password, &*crypto::password_key(password, &kf.kdf)?, PASSWORD_AAD) {
                let mut kf = kf.clone();
                kf.key_id = key_id(&master);
                if !opened.iter().any(|(m, _)| *m == master) {
                    opened.push((master, kf));
                }
            }
        }
        if opened.is_empty() {
            return Err(Error::WrongPassword);
        }
        self.open_places(&mut inner, &keys, &opened)?;
        drop(inner);
        self.remove_strays();
        Ok(())
    }

    /// Unlock with the recovery key and set a new password.
    pub fn recover(&self, recovery: &str, new_password: &str) -> Result<()> {
        check_password(new_password)?;
        let rk = crypto::parse_recovery_key(recovery).ok_or(Error::WrongPassword)?;
        let wrap = crypto::recovery_wrap_key(&rk);
        let mut inner = self.lock_inner();
        let keys = self.read_keys(&mut inner);
        if keys.is_empty() {
            return Err(Error::invalid("the locked folder is not set up"));
        }
        let (master, mut kf) = newest(&keys)
            .into_iter()
            .find_map(|kf| open_master(&kf.by_recovery, &wrap, RECOVERY_AAD).ok().map(|m| (m, kf.clone())))
            .ok_or(Error::WrongPassword)?;
        kf.kdf = KdfParams { salt: crypto::hex(&crypto::random_bytes::<16>()), ..kf.kdf };
        kf.by_password =
            crypto::hex(&crypto::seal(&*crypto::password_key(new_password, &kf.kdf)?, PASSWORD_AAD, master.as_ref()));
        kf.key_id = key_id(&master);
        kf.changed = now_ms();
        inner.unlocked = None;
        self.open_places(&mut inner, &keys, &[(master, kf)])?;
        drop(inner);
        self.remove_strays();
        Ok(())
    }

    /// Open every place whose master key is one of `opened` (the first is the main one): its key
    /// data is brought up to date, then its records are read.
    fn open_places(&self, inner: &mut Inner, keys: &[(PathBuf, KeyFile, String)], opened: &[(SecretKey, KeyFile)]) -> Result<()> {
        let (master, key) = opened[0].clone();
        let mut masters = HashMap::new();
        for (place, kf, text) in keys {
            // The same key data, or an older copy of it (same key id, or one from before key ids
            // whose master the recovery key or password opened).
            // (Sealed keys have random nonces, so an equal one comes from the same key data.)
            let found = opened.iter().find(|(_, current)| {
                (!kf.key_id.is_empty() && kf.key_id == current.key_id)
                    || kf.by_password == current.by_password
                    || kf.by_recovery == current.by_recovery
            });
            let Some((m, current)) = found else {
                tracing::warn!("the locked folder in {} uses another password; it stays locked", place.display());
                continue;
            };
            if serde_json::to_string(current)? != *text
                && current.changed >= kf.changed
                && let Err(e) = write_key(inner.conn(place)?, current)
            {
                tracing::warn!("cannot update the key data in {}: {e}", place.display());
            }
            masters.insert(place.clone(), m.clone());
        }
        let mut items = Vec::new();
        for (place, m) in &masters {
            let conn = inner.conn(place)?;
            let mut stmt = conn.prepare("SELECT id, data FROM items")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
            for row in rows {
                let (id, data) = row?;
                match crypto::open(m, id.as_bytes(), &data) {
                    Ok(plain) => match serde_json::from_slice::<Record>(&plain) {
                        Ok(rec) => items.push((id, Entry { place: place.clone(), rec })),
                        Err(e) => tracing::warn!("locked folder item {id} can't be read: {e}"),
                    },
                    Err(_) => tracing::warn!("locked folder item {id} is damaged"),
                }
            }
        }
        sort(&mut items);
        inner.unlocked = Some(Unlocked { master, key, masters, items, last_used: Instant::now() });
        Ok(())
    }

    /// Delete files that no record points to (left by a move that was interrupted).
    fn remove_strays(&self) {
        let (known, places): (HashSet<String>, Vec<PathBuf>) = {
            let inner = self.lock_inner();
            let Some(u) = inner.unlocked.as_ref() else { return };
            (u.items.iter().flat_map(|(_, e)| e.rec.files().cloned()).collect(), u.masters.keys().cloned().collect())
        };
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        for place in places {
            let Ok(dirs) = fs::read_dir(place.join("files")) else { continue };
            for entry in dirs.flatten().filter_map(|d| fs::read_dir(d.path()).ok()).flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < hour_ago);
                if !known.contains(&name) && old {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Forget the keys and the decrypted list.
    pub fn lock(&self) {
        let mut inner = self.lock_inner();
        inner.unlocked = None;
        // Unplugged drives shouldn't stay open.
        inner.conns.clear();
    }

    /// Lock if nothing used the locked folder for `idle`. Returns true if it locked now.
    pub fn lock_if_idle(&self, idle: Duration) -> bool {
        let mut inner = self.lock_inner();
        if inner.unlocked.as_ref().is_some_and(|u| u.last_used.elapsed() >= idle) {
            inner.unlocked = None;
            inner.conns.clear();
            return true;
        }
        false
    }

    /// Run `f` with the unlocked state; counts as use (for auto-lock).
    fn with_unlocked<T>(&self, f: impl FnOnce(&mut Unlocked, &mut Inner) -> Result<T>) -> Result<T> {
        let mut inner = self.lock_inner();
        let Some(mut u) = inner.unlocked.take() else { return Err(Error::Locked) };
        u.last_used = Instant::now();
        let result = f(&mut u, &mut inner);
        inner.unlocked = Some(u);
        result
    }

    /// Write new key data to every place of this locked folder.
    fn rekey(u: &mut Unlocked, inner: &mut Inner, mut key: KeyFile) -> Result<()> {
        key.changed = now_ms().max(u.key.changed + 1);
        let places: Vec<PathBuf> = u.masters.iter().filter(|(_, m)| **m == u.master).map(|(p, _)| p.clone()).collect();
        for place in places {
            write_key(inner.conn(&place)?, &key)?;
        }
        u.key = key;
        Ok(())
    }

    fn check(u: &Unlocked, password: &str) -> Result<()> {
        // Asking for the password stops someone at an unlocked PC from taking over.
        let m = open_master(&u.key.by_password, &*crypto::password_key(password, &u.key.kdf)?, PASSWORD_AAD)?;
        if *m != *u.master {
            return Err(Error::WrongPassword);
        }
        Ok(())
    }

    pub fn change_password(&self, old: &str, new: &str) -> Result<()> {
        check_password(new)?;
        self.with_unlocked(|u, inner| {
            Self::check(u, old)?;
            let mut kf = u.key.clone();
            kf.kdf = KdfParams { salt: crypto::hex(&crypto::random_bytes::<16>()), ..kf.kdf };
            kf.by_password = crypto::hex(&crypto::seal(&*crypto::password_key(new, &kf.kdf)?, PASSWORD_AAD, u.master.as_ref()));
            Self::rekey(u, inner, kf)
        })
    }

    /// Make a new recovery key (the old one stops working). Needs the password.
    pub fn new_recovery_key(&self, password: &str) -> Result<Zeroizing<String>> {
        self.with_unlocked(|u, inner| {
            Self::check(u, password)?;
            let recovery = crypto::new_recovery_key();
            let rk = crypto::parse_recovery_key(&recovery).expect("a new recovery key reads back");
            let mut kf = u.key.clone();
            kf.by_recovery = crypto::hex(&crypto::seal(&crypto::recovery_wrap_key(&rk), RECOVERY_AAD, u.master.as_ref()));
            Self::rekey(u, inner, kf)?;
            Ok(recovery)
        })
    }

    /// Every item, newest first.
    pub fn list(&self) -> Result<Vec<Item>> {
        self.with_unlocked(|u, _| Ok(u.items.iter().map(|(id, e)| item(id, &e.rec)).collect()))
    }

    pub fn get(&self, id: &str) -> Result<Item> {
        self.with_unlocked(|u, _| u.items.iter().find(|(i, _)| i == id).map(|(id, e)| item(id, &e.rec)).ok_or(Error::NotFound))
    }

    /// Where the locked items are: each place with its item count (home is the data folder).
    pub fn places_in_use(&self) -> Result<Vec<(PathBuf, usize)>> {
        self.with_unlocked(|u, _| {
            let mut out: Vec<(PathBuf, usize)> = Vec::new();
            for (_, e) in &u.items {
                match out.iter_mut().find(|(p, _)| *p == e.place) {
                    Some((_, n)) => *n += 1,
                    None => out.push((e.place.clone(), 1)),
                }
            }
            Ok(out)
        })
    }

    /// Open one of an item's files for reading (decrypted on the fly, seekable).
    /// Returns the reader and its content type.
    pub fn open_part(&self, id: &str, part: Part) -> Result<(Decryptor<BufReader<File>>, String)> {
        let (key, path, mime) = self.with_unlocked(|u, _| {
            let (_, e) = u.items.iter().find(|(i, _)| i == id).ok_or(Error::NotFound)?;
            let r = &e.rec;
            let (name, mime) = match part {
                Part::Original => (Some(&r.original), r.info.mime.clone()),
                Part::Thumb => (r.thumb.as_ref(), "image/jpeg".to_owned()),
                Part::Display => match &r.display {
                    Some(d) => (Some(d), "image/jpeg".to_owned()),
                    None => (Some(&r.original), r.info.mime.clone()),
                },
            };
            let name = name.ok_or(Error::NotFound)?;
            Ok((r.file_key()?, file_path(&e.place, name), mime))
        })?;
        let file = BufReader::new(File::open(path)?);
        Ok((Decryptor::new(&key, file)?, mime))
    }

    /// The place for an item from `original`, created if needed. Returns it with its master key.
    fn ready_place(&self, original: &Path) -> Result<(PathBuf, SecretKey)> {
        let place = self.place_for(original);
        self.with_unlocked(|u, inner| {
            if let Some(m) = u.masters.get(&place) {
                return Ok((place.clone(), m.clone()));
            }
            if has_db(&place) {
                // There already, but its key didn't open (another locked folder, or an old
                // password): don't mix.
                return Err(Error::invalid(format!(
                    "{} has a locked folder with another password",
                    place.parent().and_then(Path::parent).unwrap_or(&place).display()
                )));
            }
            Self::create_place(inner, &place, &u.key)?;
            u.masters.insert(place.clone(), u.master.clone());
            Ok((place.clone(), u.master.clone()))
        })
    }

    /// Encrypt `data` into a new file in `place`; returns its name.
    fn write_new(place: &Path, key: &[u8; crypto::KEY_LEN], src: impl Read) -> Result<(String, crypto::Written)> {
        let name = crypto::random_id();
        let path = file_path(place, &name);
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("tmp");
        let result = (|| {
            let mut out = BufWriter::new(File::create(&tmp)?);
            let written = crypto::encrypt_stream(key, src, &mut out)?;
            out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            fs::rename(&tmp, &path)?;
            Ok::<_, Error>(written)
        })();
        match result {
            Ok(w) => Ok((name, w)),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Encrypt an item into the locked folder (the place of the watched folder it is in) and
    /// check that it decrypts to the same bytes. The original is not touched. Returns the new
    /// item's id.
    pub fn add(&self, new: NewItem) -> Result<String> {
        let (place, master) = self.ready_place(new.original)?;
        let key = crypto::random_key();
        let mut written: Vec<String> = Vec::new();
        let result = (|| {
            let (original, w) = Self::write_new(&place, &key, BufReader::new(File::open(new.original)?))?;
            written.push(original.clone());
            let check = Decryptor::new(&key, BufReader::new(File::open(file_path(&place, &original))?))?.hash_all()?;
            if check != w.hash {
                return Err(Error::invalid("the encrypted copy didn't check out"));
            }
            let mut part = |bytes: Option<Vec<u8>>| -> Result<Option<String>> {
                let Some(bytes) = bytes.map(Zeroizing::new) else { return Ok(None) };
                let (name, _) = Self::write_new(&place, &key, bytes.as_slice())?;
                written.push(name.clone());
                Ok(Some(name))
            };
            let thumb = part(new.thumb)?;
            let display = part(new.display)?;
            let mut info = new.info;
            info.hash = w.hash;
            info.size = w.len;
            let rec = Record { info, key: crypto::hex(key.as_ref()), original, thumb, display };
            let id = crypto::random_id();
            let sealed = crypto::seal(&master, id.as_bytes(), &Zeroizing::new(serde_json::to_vec(&rec)?));
            self.with_unlocked(|u, inner| {
                inner.conn(&place)?.execute("INSERT INTO items (id, data) VALUES (?1, ?2)", params![id, sealed])?;
                u.items.push((id.clone(), Entry { place: place.clone(), rec }));
                sort(&mut u.items);
                Ok(())
            })?;
            Ok(id)
        })();
        if result.is_err() {
            for name in &written {
                let _ = fs::remove_file(file_path(&place, name));
            }
        }
        result
    }

    /// Move items kept at home into the watched folder they came from (items set up before
    /// places, or whose folder was added later). Returns how many moved.
    pub fn relocate(&self) -> Result<usize> {
        let work: Vec<(String, PathBuf)> = self.with_unlocked(|u, _| {
            Ok(u.items
                .iter()
                .filter(|(_, e)| e.place == self.home)
                .map(|(id, e)| (id.clone(), PathBuf::from(&e.rec.info.original_path)))
                .collect())
        })?;
        let mut moved = 0;
        for (id, original) in work {
            if self.place_for(&original) == self.home {
                continue;
            }
            match self.relocate_one(&id, &original) {
                Ok(true) => moved += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("locked item not moved next to its folder yet: {e}"),
            }
        }
        if moved > 0 {
            tracing::info!("moved {moved} locked items into their folders");
        }
        Ok(moved)
    }

    fn relocate_one(&self, id: &str, original: &Path) -> Result<bool> {
        let (place, master) = self.ready_place(original)?;
        let Some(names) = self.with_unlocked(|u, _| {
            Ok(u.items.iter().find(|(i, e)| i == id && e.place == self.home).map(|(_, e)| e.rec.files().cloned().collect::<Vec<_>>()))
        })?
        else {
            return Ok(false);
        };
        // Copy the encrypted files as they are (each is sealed with the item's own key).
        let mut copied = Vec::new();
        let result = (|| {
            for name in &names {
                let (from, to) = (file_path(&self.home, name), file_path(&place, name));
                fs::create_dir_all(to.parent().unwrap())?;
                let tmp = to.with_extension("tmp");
                let n = fs::copy(&from, &tmp)?;
                fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
                if n != fs::metadata(&from)?.len() {
                    let _ = fs::remove_file(&tmp);
                    return Err(Error::invalid("copy came out short"));
                }
                fs::rename(&tmp, &to)?;
                copied.push(to);
            }
            self.with_unlocked(|u, inner| {
                let Some((_, e)) = u.items.iter_mut().find(|(i, e)| i == id && e.place == self.home) else {
                    return Err(Error::NotFound);
                };
                let sealed = crypto::seal(&master, id.as_bytes(), &Zeroizing::new(serde_json::to_vec(&e.rec)?));
                inner.conn(&place)?.execute("INSERT INTO items (id, data) VALUES (?1, ?2)", params![id, sealed])?;
                inner.conn(&self.home)?.execute("DELETE FROM items WHERE id = ?1", [id])?;
                e.place = place.clone();
                Ok(())
            })
        })();
        if let Err(e) = result {
            for p in &copied {
                let _ = fs::remove_file(p);
            }
            return if matches!(e, Error::NotFound) { Ok(false) } else { Err(e) };
        }
        for name in &names {
            let _ = fs::remove_file(file_path(&self.home, name));
        }
        Ok(true)
    }

    /// Decrypt an item into `dir` (under its own name, made unique) with its old modification
    /// time, check it, then remove it from the locked folder. Returns the new file's path.
    pub fn move_out(&self, id: &str, dir: &Path) -> Result<PathBuf> {
        let (key, src_path, info) = self.with_unlocked(|u, _| {
            let (_, e) = u.items.iter().find(|(i, _)| i == id).ok_or(Error::NotFound)?;
            Ok((e.rec.file_key()?, file_path(&e.place, &e.rec.original), e.rec.info.clone()))
        })?;
        fs::create_dir_all(dir)?;
        let dest = unique_path(dir, &info.name);
        let tmp = dest.with_file_name(format!(".{}.memora-tmp", crypto::random_id()));
        let result = (|| {
            let mut src = Decryptor::new(&key, BufReader::new(File::open(&src_path)?))?;
            let mut out = BufWriter::new(File::create(&tmp)?);
            let mut hasher = blake3::Hasher::new();
            let mut buf = Zeroizing::new(vec![0u8; crypto::CHUNK]);
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            let file = out.into_inner().map_err(|e| e.into_error())?;
            if info.mtime > 0 {
                let t = std::time::UNIX_EPOCH + Duration::from_millis(info.mtime as u64);
                let _ = file.set_modified(t);
            }
            file.sync_all()?;
            drop(file);
            if hasher.finalize().to_hex().as_str() != info.hash {
                return Err(Error::invalid("the decrypted copy didn't match the original"));
            }
            // Don't replace a file that appeared meanwhile.
            if dest.exists() {
                return Err(Error::invalid(format!("{} already exists", dest.display())));
            }
            fs::rename(&tmp, &dest)?;
            Ok::<_, Error>(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.delete(&[id.to_owned()])?;
        Ok(dest)
    }

    /// Delete items for good (record first, then the encrypted files).
    pub fn delete(&self, ids: &[String]) -> Result<usize> {
        let removed: Vec<Entry> = self.with_unlocked(|u, inner| {
            let mut out = Vec::new();
            for id in ids {
                if let Some(pos) = u.items.iter().position(|(i, _)| i == id) {
                    inner.conn(&u.items[pos].1.place)?.execute("DELETE FROM items WHERE id = ?1", [id])?;
                    out.push(u.items.remove(pos).1);
                }
            }
            Ok(out)
        })?;
        for e in &removed {
            for name in e.rec.files() {
                let _ = fs::remove_file(file_path(&e.place, name));
            }
        }
        Ok(removed.len())
    }

    /// Set or clear the favorite mark.
    pub fn set_favorite(&self, ids: &[String], favorite: bool) -> Result<usize> {
        self.update(ids, |info| info.favorite = favorite)
    }

    fn update(&self, ids: &[String], change: impl Fn(&mut Info)) -> Result<usize> {
        self.with_unlocked(|u, inner| {
            let mut n = 0;
            for (id, e) in u.items.iter_mut().filter(|(i, _)| ids.contains(i)) {
                let master = u.masters.get(&e.place).ok_or(Error::Locked)?;
                change(&mut e.rec.info);
                let sealed = crypto::seal(master, id.as_bytes(), &Zeroizing::new(serde_json::to_vec(&e.rec)?));
                inner.conn(&e.place)?.execute("UPDATE items SET data = ?2 WHERE id = ?1", params![id.as_str(), sealed])?;
                n += 1;
            }
            Ok(n)
        })
    }

    /// Bytes the locked folder takes on disk (every reachable place).
    pub fn disk_size(&self) -> u64 {
        self.existing()
            .iter()
            .flat_map(|p| walkdir::WalkDir::new(p).into_iter().filter_map(|e| e.ok()))
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }
}

fn item(id: &str, r: &Record) -> Item {
    Item { id: id.to_owned(), info: r.info.clone(), has_display: r.display.is_some() }
}

fn sort(items: &mut [(String, Entry)]) {
    items.sort_by(|a, b| b.1.rec.info.taken_at.cmp(&a.1.rec.info.taken_at).then_with(|| b.0.cmp(&a.0)));
}

/// `dir\name`, or `dir\name (2).ext`... if that is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests;
