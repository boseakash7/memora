use std::io::{Read, Seek, SeekFrom};

use super::*;
use crate::media::Kind;

const PW: &str = "correct horse";

fn vault(dir: &Path) -> Vault {
    let v = Vault::new(&dir.join("vault"));
    v.setup_with(PW, KdfParams::for_tests()).unwrap();
    v
}

fn info(name: &str, taken: &str) -> Info {
    Info {
        name: name.into(),
        original_path: format!("C:\\Photos\\{name}"),
        kind: Kind::Photo,
        mime: "image/jpeg".into(),
        size: 0,
        mtime: 1_700_000_000_000,
        width: Some(4000),
        height: Some(3000),
        duration_ms: None,
        taken_at: taken.into(),
        tz_offset: Some(330),
        camera: Some("Apple iPhone 12 Pro".into()),
        lat: Some(27.33),
        lon: Some(88.61),
        favorite: false,
        hash: String::new(),
        added_at: "2026-11-18T10:00:00Z".into(),
    }
}

fn add(v: &Vault, dir: &Path, name: &str, taken: &str, bytes: &[u8]) -> String {
    let src = dir.join(name);
    fs::write(&src, bytes).unwrap();
    v.add(NewItem { info: info(name, taken), original: &src, thumb: Some(b"thumb jpeg".to_vec()), display: None }).unwrap()
}

/// Every byte of every file in the locked folder.
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(dir).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        out.extend(fs::read(e.path()).unwrap());
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn set_up_lock_unlock_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::new(&dir.path().join("vault"));
    assert!(!v.state().exists);
    assert!(!dir.path().join("vault").exists(), "nothing is created before setup");
    assert!(matches!(v.list(), Err(Error::Locked)));
    assert!(v.setup_with("short", KdfParams::for_tests()).is_err(), "too short");

    let recovery = v.setup_with(PW, KdfParams::for_tests()).unwrap();
    assert!(v.setup_with(PW, KdfParams::for_tests()).is_err(), "only once");
    assert_eq!(v.state().count, Some(0));
    let a = add(&v, dir.path(), "beach.jpg", "2024-05-01T10:00:00", b"photo one");
    let b = add(&v, dir.path(), "secret party.jpg", "2025-01-01T10:00:00", b"photo two");
    let ids: Vec<String> = v.list().unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(ids, [b.clone(), a.clone()], "newest first");

    v.lock();
    assert!(!v.state().unlocked);
    assert!(matches!(v.list(), Err(Error::Locked)));
    assert!(matches!(v.open_part(&a, Part::Original), Err(Error::Locked)));
    assert!(matches!(v.unlock("wrong password"), Err(Error::WrongPassword)));

    // A fresh process (new Vault) unlocks and lists the same.
    let v = Vault::new(&dir.path().join("vault"));
    v.unlock(PW).unwrap();
    let list = v.list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].info.name, "beach.jpg");
    assert_eq!(list[1].info.size, 9);
    assert_eq!(list[1].info.hash, blake3::hash(b"photo one").to_hex().to_string());
    let mut thumb = Vec::new();
    v.open_part(&a, Part::Thumb).unwrap().0.read_to_end(&mut thumb).unwrap();
    assert_eq!(thumb, b"thumb jpeg");
    let (mut d, mime) = v.open_part(&b, Part::Display).unwrap();
    assert_eq!(mime, "image/jpeg");
    d.seek(SeekFrom::Start(6)).unwrap();
    let mut rest = String::new();
    d.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "two", "no preview: the original, seekable");

    // Recovery key: unlocks and sets a new password; the old one stops working.
    v.lock();
    assert!(matches!(v.recover("AAAA-AAAA", "new password"), Err(Error::WrongPassword)));
    v.recover(&recovery.to_lowercase(), "new password").unwrap();
    assert_eq!(v.list().unwrap().len(), 2);
    v.lock();
    assert!(matches!(v.unlock(PW), Err(Error::WrongPassword)));
    v.unlock("new password").unwrap();

    // Change password (needs the current one) and a new recovery key (old one dead).
    assert!(matches!(v.change_password("nope nope", "third password"), Err(Error::WrongPassword)));
    v.change_password("new password", "third password").unwrap();
    let recovery2 = v.new_recovery_key("third password").unwrap();
    v.lock();
    assert!(matches!(v.recover(&recovery, "fourth password"), Err(Error::WrongPassword)));
    v.recover(&recovery2, "fourth password").unwrap();
}

#[test]
fn nothing_readable_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let secret = b"PLAINTEXT-PHOTO-BYTES-0123456789";
    let id = add(&v, dir.path(), "secret party.jpg", "2025-01-01T10:00:00", secret);
    v.set_favorite(std::slice::from_ref(&id), true).unwrap();
    assert!(v.get(&id).unwrap().info.favorite);
    let bytes = all_bytes(&dir.path().join("vault"));
    for needle in [&secret[..], b"secret party", b"iPhone", b"2025-01-01", b"27.33", b"Photos", b"image/jpeg", b"thumb jpeg"] {
        assert!(!contains(&bytes, needle), "{:?} is readable on disk", String::from_utf8_lossy(needle));
    }
    // The favorite survives a new unlock (it lives in the sealed record).
    v.lock();
    v.unlock(PW).unwrap();
    assert!(v.get(&id).unwrap().info.favorite);
}

#[test]
fn moving_out_puts_the_file_back_and_deletes_it_here() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let id = add(&v, dir.path(), "clip.jpg", "2024-01-01T00:00:00", &bytes);
    let out_dir = dir.path().join("restored");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("clip.jpg"), b"someone else").unwrap();

    let files_before = walkdir::WalkDir::new(v.files_dir()).into_iter().flatten().filter(|e| e.file_type().is_file()).count();
    assert_eq!(files_before, 2, "original + thumbnail");
    let path = v.move_out(&id, &out_dir).unwrap();
    assert_eq!(path, out_dir.join("clip (2).jpg"), "existing file kept");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(out_dir.join("clip.jpg")).unwrap(), b"someone else");
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(), 1_700_000_000_000);
    assert!(v.list().unwrap().is_empty());
    let left = walkdir::WalkDir::new(v.files_dir()).into_iter().flatten().filter(|e| e.file_type().is_file()).count();
    assert_eq!(left, 0, "encrypted files deleted");
    assert!(matches!(v.get(&id), Err(Error::NotFound)));
}

#[test]
fn damaged_records_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let a = add(&v, dir.path(), "a.jpg", "2024-01-01T00:00:00", b"aaaa");
    let b = add(&v, dir.path(), "b.jpg", "2024-01-02T00:00:00", b"bbbb");
    v.lock();
    // Swap the two sealed records: each is bound to its id, so neither reads.
    let conn = Connection::open(dir.path().join("vault").join("vault.db")).unwrap();
    let da: Vec<u8> = conn.query_row("SELECT data FROM items WHERE id = ?1", [&a], |r| r.get(0)).unwrap();
    let db: Vec<u8> = conn.query_row("SELECT data FROM items WHERE id = ?1", [&b], |r| r.get(0)).unwrap();
    conn.execute("UPDATE items SET data = ?2 WHERE id = ?1", params![a, db]).unwrap();
    conn.execute("UPDATE items SET data = ?2 WHERE id = ?1", params![b, da]).unwrap();
    drop(conn);
    v.unlock(PW).unwrap();
    assert!(v.list().unwrap().is_empty(), "swapped records are skipped, not shown as each other");
}

#[test]
fn auto_lock() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    assert!(!v.lock_if_idle(Duration::from_secs(60)));
    std::thread::sleep(Duration::from_millis(30));
    assert!(v.lock_if_idle(Duration::from_millis(20)));
    assert!(!v.state().unlocked);
}

/// A locked folder whose watched folders can change during the test.
fn with_roots(home: &Path, roots: &std::sync::Arc<Mutex<Vec<PathBuf>>>) -> Vault {
    let roots = roots.clone();
    Vault::with_roots(home, move || roots.lock().unwrap().clone())
}

fn add_from(v: &Vault, src: &Path, bytes: &[u8]) -> String {
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(src, bytes).unwrap();
    let mut i = info(&src.file_name().unwrap().to_string_lossy(), "2024-05-01T10:00:00");
    i.original_path = src.to_string_lossy().into_owned();
    v.add(NewItem { info: i, original: src, thumb: Some(b"thumb jpeg".to_vec()), display: None }).unwrap()
}

fn file_count(dir: &Path) -> usize {
    walkdir::WalkDir::new(dir).into_iter().flatten().filter(|e| e.file_type().is_file()).count()
}

#[test]
fn items_stay_in_their_photos_folder() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("My Photos");
    let roots = std::sync::Arc::new(Mutex::new(vec![photos.clone(), photos.join("Phone")]));
    let v = with_roots(&dir.path().join("pc1"), &roots);
    v.setup_with(PW, KdfParams::for_tests()).unwrap();

    // From the watched folder (and from a watched folder inside it): the outer folder's place.
    let a = add_from(&v, &photos.join("Phone").join("secret party.jpg"), b"PLAINTEXT-ONE");
    // From outside every watched folder: home.
    let b = add_from(&v, &dir.path().join("Downloads").join("other.jpg"), b"PLAINTEXT-TWO");
    let place = photos.join(".memora").join("vault");
    assert!(place.join("vault.db").is_file());
    assert!(!photos.join("Phone").join(".memora").exists(), "the outermost folder keeps it");
    assert_eq!(file_count(&place.join("files")), 2, "original + thumbnail next to the photos");
    assert_eq!(file_count(&dir.path().join("pc1").join("files")), 2, "the other one at home");
    let bytes = all_bytes(&place);
    for needle in [&b"PLAINTEXT-ONE"[..], b"secret party", b"iPhone", b"image/jpeg"] {
        assert!(!contains(&bytes, needle), "{:?} is readable on disk", String::from_utf8_lossy(needle));
    }
    let mut got = Vec::new();
    v.open_part(&a, Part::Original).unwrap().0.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"PLAINTEXT-ONE");
    assert_eq!(v.list().unwrap().len(), 2);
    v.set_favorite(std::slice::from_ref(&a), true).unwrap();

    // Another PC that adds the folder: its own (empty) home, the same password opens it.
    let other = Vault::with_roots(&dir.path().join("pc2"), {
        let photos = photos.clone();
        move || vec![photos.clone()]
    });
    assert!(other.state().exists, "the folder brings a locked folder");
    assert!(matches!(other.unlock("wrong password"), Err(Error::WrongPassword)));
    other.unlock(PW).unwrap();
    let list = other.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, a);
    assert!(list[0].info.favorite);
    assert!(matches!(other.get(&b), Err(Error::NotFound)), "home items stay on their PC");

    // Delete and move out work in the folder's place.
    other.move_out(&a, &dir.path().join("out")).unwrap();
    assert_eq!(file_count(&place.join("files")), 0);
}

#[test]
fn a_folder_that_was_away_gets_the_new_password() {
    let dir = tempfile::tempdir().unwrap();
    let (photos, drive) = (dir.path().join("Photos"), dir.path().join("USB"));
    let roots = std::sync::Arc::new(Mutex::new(vec![photos.clone(), drive.clone()]));
    let v = with_roots(&dir.path().join("home"), &roots);
    let recovery = v.setup_with(PW, KdfParams::for_tests()).unwrap();
    add_from(&v, &photos.join("a.jpg"), b"aaaa");
    add_from(&v, &drive.join("b.jpg"), b"bbbb");

    // The drive is unplugged while the password changes.
    v.lock();
    fs::rename(&drive, dir.path().join("USB-away")).unwrap();
    v.unlock(PW).unwrap();
    assert_eq!(v.list().unwrap().len(), 1);
    v.change_password(PW, "second password").unwrap();
    v.lock();
    fs::rename(dir.path().join("USB-away"), &drive).unwrap();

    // Back: the old password no longer opens anything (the newest key data counts), the new
    // one opens both and brings the drive's key data up to date.
    assert!(matches!(v.unlock(PW), Err(Error::WrongPassword)));
    v.unlock("second password").unwrap();
    assert_eq!(v.list().unwrap().len(), 2);
    v.lock();
    // Now only the new password works everywhere: a PC that only sees the drive agrees.
    let drive_only = Vault::with_roots(&dir.path().join("pc2"), {
        let drive = drive.clone();
        move || vec![drive.clone()]
    });
    assert!(matches!(drive_only.unlock(PW), Err(Error::WrongPassword)));
    drive_only.unlock("second password").unwrap();
    assert_eq!(drive_only.list().unwrap().len(), 1);

    // The recovery key sets a new password in every place.
    v.recover(&recovery, "third password").unwrap();
    assert_eq!(v.list().unwrap().len(), 2);
    drive_only.lock();
    drive_only.unlock("third password").unwrap();
}

#[test]
fn items_kept_at_home_move_into_their_folder() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    let roots = std::sync::Arc::new(Mutex::new(Vec::new()));
    let home = dir.path().join("home");
    let v = with_roots(&home, &roots);
    v.setup_with(PW, KdfParams::for_tests()).unwrap();
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let a = add_from(&v, &photos.join("trip").join("a.jpg"), &bytes);
    let b = add_from(&v, &dir.path().join("elsewhere").join("b.jpg"), b"bbbb");
    assert_eq!(file_count(&home.join("files")), 4, "no watched folders yet: all at home");

    // An old locked folder (before places) meets its watched folder.
    roots.lock().unwrap().push(photos.clone());
    assert_eq!(v.relocate().unwrap(), 1);
    assert_eq!(v.relocate().unwrap(), 0, "once");
    assert_eq!(file_count(&home.join("files")), 2, "b stays at home");
    assert_eq!(file_count(&photos.join(".memora").join("vault").join("files")), 2);
    let mut got = Vec::new();
    v.open_part(&a, Part::Original).unwrap().0.read_to_end(&mut got).unwrap();
    assert_eq!(got, bytes);

    // Still there after locking, from the new place.
    v.lock();
    v.unlock(PW).unwrap();
    assert_eq!(v.list().unwrap().len(), 2);
    v.get(&a).unwrap();
    v.get(&b).unwrap();
    let conn = Connection::open(home.join("vault.db")).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "a's record left home");
}
