use std::time::Duration;

use super::*;
use crate::people::tests::face_like;
use crate::people::to_blob;
use crate::{Library, AppPaths, scan};

fn png(path: &Path, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(40 + shade as u32, 30, image::Rgb([shade, 90, 0])).save(path).unwrap();
}

/// Copy a folder with its files' times (as copying to a drive or a sync client does).
fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let target = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).unwrap();
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn open(dir: &Path) -> Db {
    fs::create_dir_all(dir).unwrap();
    Db::open(&dir.join("library.db")).unwrap()
}

fn add(db: &Db, root: &Path) -> Folder {
    let folder = folders::add(db, root).unwrap();
    scan::scan_folder(db, &folder, &mut |_| {}).unwrap();
    sync::enable(db, folder.id).unwrap()
}

/// What indexing and recognition would have stored on PC 1.
fn index_by_hand(db: &Db) {
    let conn = db.conn().unwrap();
    conn.execute("INSERT INTO places (country, admin1, city) VALUES ('IN', 'Karnataka', 'Bangalore')", []).unwrap();
    conn.execute(
        "UPDATE media SET width = 40, height = 30, taken_at = '2024-05-01T10:00:00', camera_make = 'Canon',
             lat = 12.9, lon = 77.6, blake3 = 'hash-' || name, phash = 42, place_id = 1, stage = 4",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO persons (name, created_at) VALUES ('Ria', 'now')", []).unwrap();
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM media ORDER BY name").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    for (i, id) in ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id) VALUES (?1, 0.1, 0.2, 0.3, 0.3, 0.9, ?2, 1)",
            params![id, to_blob(&face_like(0, i as u64))],
        )
        .unwrap();
        conn.execute("INSERT INTO clip_embeddings (media_id, vec) VALUES (?1, ?2)", params![id, to_blob(&vec![0.5; 512])]).unwrap();
    }
}

fn row(db: &Db, name: &str) -> (i64, i64, Option<String>, Option<String>, i64, i64) {
    db.conn()
        .unwrap()
        .query_row(
            "SELECT m.stage, m.imported, m.camera_make, p.city,
                    (SELECT count(*) FROM faces WHERE media_id = m.id), (SELECT count(*) FROM clip_embeddings WHERE media_id = m.id)
             FROM media m LEFT JOIN places p ON p.id = m.place_id WHERE m.name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap()
}

#[test]
fn second_pc_takes_over_unchanged_files() {
    let dir = tempfile::tempdir().unwrap();
    let photos_a = dir.path().join("E").join("Photos");
    for (i, name) in ["a.png", "b.png", "sub/c.png"].iter().enumerate() {
        png(&photos_a.join(name), i as u8 * 20);
    }
    let a = open(&dir.path().join("pc1"));
    let fa = add(&a, &photos_a);
    index_by_hand(&a);
    let uid: String = a.conn().unwrap().query_row("SELECT uid FROM persons", [], |r| r.get(0)).unwrap();
    assert_eq!(write_changed(&a).unwrap(), 1);
    assert_eq!(write_changed(&a).unwrap(), 0, "nothing changed since");
    let root_a = fa.sync_root.clone().unwrap();
    assert!(own_size(&a, &root_a).unwrap().unwrap() > 0);

    // PC 2 gets a copy of the folder; one file there is different.
    let photos_b = dir.path().join("F").join("Photos");
    copy_dir(&photos_a, &photos_b);
    png(&photos_b.join("b.png"), 200);
    let b = open(&dir.path().join("pc2"));
    let fb = add(&b, &photos_b);
    assert_eq!(import(&b, fb.id).unwrap(), Imported { items: 2, recognised: 2 });
    assert_eq!(row(&b, "a.png"), (Stage::METADATA, 2, Some("Canon".into()), Some("Bangalore".into()), 1, 1));
    assert_eq!(row(&b, "c.png").1, 2);
    assert_eq!(row(&b, "b.png"), (Stage::NEW, 0, None, None, 0, 0), "a changed file is indexed here");
    let b_uid: String = b
        .conn()
        .unwrap()
        .query_row("SELECT p.uid FROM faces f JOIN persons p ON p.id = f.person_id LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(b_uid, uid, "same person as on PC 1, so names from the change log fit");
    assert_eq!(import(&b, fb.id).unwrap(), Imported::default(), "only new items");

    // A snapshot from a PC with other recognition models brings everything but the faces.
    let c = open(&dir.path().join("pc3"));
    let photos_c = dir.path().join("G").join("Photos");
    copy_dir(&photos_a, &photos_c);
    let snap = own_path(&photos_c.join(""), &sync::device_id(&a.conn().unwrap()).unwrap());
    let fixed = Connection::open(&snap).unwrap();
    fixed.execute("UPDATE meta SET value = 'other-models' WHERE key = 'recognition'", []).unwrap();
    drop(fixed);
    let fc = add(&c, &photos_c);
    assert_eq!(import(&c, fc.id).unwrap(), Imported { items: 3, recognised: 0 });
    assert_eq!(row(&c, "a.png"), (Stage::METADATA, 1, Some("Canon".into()), Some("Bangalore".into()), 0, 0));

    // Switching it off deletes this PC's snapshot.
    remove_own(&a, &folders::get(&a, fa.id).unwrap()).unwrap();
    assert!(own_size(&a, &root_a).unwrap().is_none());
}

#[test]
fn snapshots_from_other_versions_are_not_used() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    png(&photos.join("a.png"), 1);
    let a = open(&dir.path().join("pc1"));
    let fa = add(&a, &photos);
    index_by_hand(&a);
    write_changed(&a).unwrap();
    let snap = own_path(fa.sync_root.as_ref().unwrap(), &sync::device_id(&a.conn().unwrap()).unwrap());
    Connection::open(&snap).unwrap().execute("UPDATE meta SET value = '0' WHERE key = 'pipeline'", []).unwrap();
    // Not even a damaged file stops indexing.
    fs::write(snap.with_file_name("broken.db"), b"not a database").unwrap();

    let b = open(&dir.path().join("pc2"));
    folders::remove(&a, fa.id).unwrap();
    let fb = add(&b, &photos);
    assert_eq!(import(&b, fb.id).unwrap(), Imported::default());
}

#[test]
fn second_library_skips_indexing_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let photos_a = dir.path().join("E").join("Photos");
    for (i, name) in ["a.jpg", "b.png", "sub/c.png"].iter().enumerate() {
        png(&photos_a.join(name), i as u8 * 30);
    }
    let start = |name: &str| {
        let paths = AppPaths::in_dir(&dir.path().join(name)).unwrap();
        Library::start(Db::open(&paths.db_path).unwrap(), paths).unwrap()
    };
    let a = start("pc1");
    a.add_folder(&photos_a, true).unwrap();
    assert!(a.wait_idle(Duration::from_secs(60)));
    a.sync_now().unwrap();
    let hashes = |lib: &Library| -> Vec<(String, String)> {
        let conn = lib.db().conn().unwrap();
        let mut stmt = conn.prepare("SELECT name, blake3 FROM media ORDER BY name").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    let expected = hashes(&a);
    a.shutdown();

    let photos_b = dir.path().join("F").join("Photos");
    copy_dir(&photos_a, &photos_b);
    let b = start("pc2");
    b.add_folder(&photos_b, true).unwrap();
    assert!(b.wait_idle(Duration::from_secs(60)));
    assert_eq!(hashes(&b), expected);
    let conn = b.db().conn().unwrap();
    let (stage, imported): (i64, i64) =
        conn.query_row("SELECT min(stage), min(imported) FROM media", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((stage, imported), (Stage::HASH, 1), "taken over, then only the thumbnail was made");
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM media").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert!(ids.iter().all(|id| crate::thumbs::thumb_path(b.paths(), *id).is_file()));
    b.shutdown();
}
