use std::time::Duration;

use super::*;
use crate::ai::{Found, FoundFace};
use crate::people::tests::face_like;
use crate::people::{self, NOT_THIS_PERSON, to_blob};
use crate::{AppPaths, Library, hash, media, scan};

fn png(path: &Path, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(40 + shade as u32, 30, image::Rgb([shade, 60, 0])).save(path).unwrap();
}

fn photos(root: &Path) {
    for (i, name) in ["a.png", "b.png", "c.png", "sub/d.png"].iter().enumerate() {
        png(&root.join(name), i as u8 * 25);
    }
}

fn start(dir: &Path) -> Library {
    let paths = AppPaths::in_dir(dir).unwrap();
    Library::start(Db::open(&paths.db_path).unwrap(), paths).unwrap()
}

fn id(db: &Db, name: &str) -> i64 {
    db.conn().unwrap().query_row("SELECT id FROM media WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

fn flag(db: &Db, name: &str, column: &str) -> bool {
    db.conn()
        .unwrap()
        .query_row(&format!("SELECT {column} IS NOT NULL AND {column} != 0 FROM media WHERE name = ?1"), [name], |r| r.get(0))
        .unwrap()
}

fn add_face(db: &Db, media: i64, rect: [f32; 4], seed: u64) -> i64 {
    let conn = db.conn().unwrap();
    conn.execute("UPDATE media SET stage = 4 WHERE id = ?1", [media]).unwrap();
    conn.execute(
        "INSERT INTO faces (media_id, x, y, w, h, score, embedding) VALUES (?1, ?2, ?3, ?4, ?5, 0.9, ?6)",
        params![media, rect[0], rect[1], rect[2], rect[3], to_blob(&face_like(0, seed))],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// Recognition on the new PC finds a face where the old one had it.
fn recognise(db: &Db, name: &str, rect: [f32; 4]) -> i64 {
    db.conn().unwrap().execute("UPDATE media SET stage = 3 WHERE name = ?1", [name]).unwrap();
    let m = media::get(db, id(db, name)).unwrap();
    let found = Found { faces: vec![FoundFace { rect, score: 0.9, embedding: face_like(0, 7) }], clip: None };
    crate::ai::save(db, &m, &found).unwrap().unwrap().face_ids[0]
}

fn person_of(db: &Db, face: i64) -> (Option<String>, i64) {
    db.conn()
        .unwrap()
        .query_row("SELECT p.name, f.locked FROM faces f LEFT JOIN persons p ON p.id = f.person_id WHERE f.id = ?1", [face], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}

#[test]
fn everything_comes_back_on_a_fresh_install() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Photos");
    photos(&root);
    let file = dir.path().join("library.memora-backup");

    // The old install: favorites, Archive, trash, an album with a cover, named people and a
    // "not this person" correction.
    {
        let old = start(&dir.path().join("old"));
        old.add_folder(&root, false).unwrap();
        assert!(old.wait_idle(Duration::from_secs(60)));
        let db = old.db();
        organize::set_favorite(db, &[id(db, "a.png")], true).unwrap();
        organize::archive(db, &[id(db, "b.png")]).unwrap();
        organize::trash(db, &[id(db, "c.png")]).unwrap();
        let trip = albums::create(db, "Trip").unwrap();
        albums::add_items(db, trip.id, &[id(db, "a.png"), id(db, "d.png")]).unwrap();
        albums::set_cover(db, trip.id, id(db, "d.png")).unwrap();
        let ria = add_face(db, id(db, "a.png"), [0.1, 0.1, 0.2, 0.2], 1);
        let p = people::new_person_for_face(db, ria, Some("Ria")).unwrap();
        people::set_hidden(db, p.id, true).unwrap();
        let stranger = add_face(db, id(db, "d.png"), [0.5, 0.5, 0.2, 0.2], 2);
        people::set_face_person(db, stranger, None).unwrap();
        settings::update(db, &json!({ "theme": "dark" })).unwrap();

        let summary = old.write_backup(&file).unwrap();
        assert_eq!((summary.favorites, summary.archived, summary.trashed, summary.albums, summary.people), (1, 1, 1, 1, 1));
        old.shutdown();
    }

    // A fresh install restores it: the folder comes back too.
    let new = start(&dir.path().join("new"));
    let report = new.restore_backup(&file, true).unwrap();
    assert_eq!(report.folders_added, 1);
    assert_eq!((report.items, report.not_found), (4, 0), "{report:?}");
    assert_eq!((report.favorites, report.archived, report.trashed), (1, 1, 1));
    assert_eq!((report.albums_created, report.album_items), (1, 2));
    assert_eq!((report.faces, report.faces_waiting), (0, 2), "not recognised here yet");
    assert!(report.settings);
    assert_eq!(settings::load(new.db()).unwrap().theme, settings::Theme::Dark);
    assert!(new.wait_idle(Duration::from_secs(60)));
    let db = new.db();
    assert!(flag(db, "a.png", "favorite") && flag(db, "b.png", "archived_at") && flag(db, "c.png", "trashed_at"));
    let album = &albums::list(db).unwrap()[0];
    assert_eq!((album.name.as_str(), album.count, album.cover_id), ("Trip", 2, Some(id(db, "d.png"))));

    // Recognition finds the faces: the name and the correction are applied then.
    let ria = recognise(db, "a.png", [0.11, 0.1, 0.2, 0.2]);
    assert_eq!(person_of(db, ria).0.as_deref(), Some("Ria"));
    assert!(people::list(db, true).unwrap()[0].hidden);
    let stranger = recognise(db, "d.png", [0.5, 0.51, 0.2, 0.2]);
    assert_eq!(person_of(db, stranger), (None, NOT_THIS_PERSON));
    let waiting: i64 = db.conn().unwrap().query_row("SELECT count(*) FROM restore_faces", [], |r| r.get(0)).unwrap();
    assert_eq!(waiting, 0);
    new.shutdown();
}

#[test]
fn restoring_adds_but_never_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Photos");
    photos(&root);
    let db = Db::open(&dir.path().join("library.db")).unwrap();
    let folder = folders::add(&db, &root).unwrap();
    scan::scan_folder(&db, &folder, &mut |_| {}).unwrap();
    organize::set_favorite(&db, &[id(&db, "a.png")], true).unwrap();
    let trip = albums::create(&db, "Trip").unwrap();
    albums::add_items(&db, trip.id, &[id(&db, "a.png")]).unwrap();
    let face = add_face(&db, id(&db, "b.png"), [0.2, 0.2, 0.2, 0.2], 3);
    let p = people::new_person_for_face(&db, face, Some("Sam")).unwrap();
    let file = dir.path().join("x.memora-backup");
    write(&db, &file).unwrap();

    // Since then: a favorite removed, the album renamed and grown, the person renamed.
    organize::set_favorite(&db, &[id(&db, "a.png")], false).unwrap();
    organize::set_favorite(&db, &[id(&db, "c.png")], true).unwrap();
    albums::rename(&db, trip.id, "Trip 2024").unwrap();
    albums::add_items(&db, trip.id, &[id(&db, "c.png")]).unwrap();
    people::rename(&db, p.id, Some("Samir")).unwrap();

    let report = restore(&db, &read(&file).unwrap(), false).unwrap();
    assert!(flag(&db, "a.png", "favorite"), "the backup's favorite is back");
    assert!(flag(&db, "c.png", "favorite"), "and the newer one stays");
    let album = albums::get(&db, trip.id).unwrap();
    assert_eq!((album.name.as_str(), album.count), ("Trip 2024", 2), "same album (by its id), nothing taken out");
    assert_eq!(report.albums_created, 0);
    assert_eq!(people::get(&db, p.id).unwrap().name.as_deref(), Some("Samir"), "names given since stay");
    assert!(!report.settings);
}

#[test]
fn items_are_found_by_hash_when_the_folder_moved() {
    let dir = tempfile::tempdir().unwrap();
    let old_root = dir.path().join("E").join("Photos");
    photos(&old_root);
    let db = Db::open(&dir.path().join("library.db")).unwrap();
    let folder = folders::add(&db, &old_root).unwrap();
    scan::scan_folder(&db, &folder, &mut |_| {}).unwrap();
    let hash_all = |db: &Db| {
        let conn = db.conn().unwrap();
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare("SELECT id, path FROM media WHERE missing = 0").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
        };
        for (id, path) in rows {
            conn.execute("UPDATE media SET blake3 = ?2 WHERE id = ?1", params![id, hash::file_blake3(Path::new(&path)).unwrap()]).unwrap();
        }
    };
    hash_all(&db);
    organize::set_favorite(&db, &[id(&db, "d.png")], true).unwrap();
    let backup = export(&db).unwrap();

    // A new library where the photos are somewhere else, renamed.
    let db2 = Db::open(&dir.path().join("library2.db")).unwrap();
    let new_root = dir.path().join("F").join("Pictures");
    photos(&new_root);
    fs::rename(new_root.join("sub/d.png"), new_root.join("renamed.png")).unwrap();
    let f2 = folders::add(&db2, &new_root).unwrap();
    scan::scan_folder(&db2, &f2, &mut |_| {}).unwrap();
    hash_all(&db2);
    let report = restore(&db2, &backup, false).unwrap();
    assert_eq!((report.items, report.favorites), (1, 1));
    assert!(flag(&db2, "renamed.png", "favorite"));
}

#[test]
fn rejects_files_that_are_not_backups() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.memora-backup");
    fs::write(&file, b"{\"hello\": 1}").unwrap();
    assert!(read(&file).unwrap_err().to_string().contains("not a Memora backup"));
    fs::write(&file, json!({"memora": KIND, "format": 99, "created": "", "app": "", "pc": ""}).to_string()).unwrap();
    assert!(read(&file).unwrap_err().to_string().contains("newer Memora"));
}
