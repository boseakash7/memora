//! Two PCs sharing one photo folder. Each "PC" has its own database and its own copy of the
//! folder at a different path; `.memora` logs are copied between them the way a sync client
//! (or carrying a drive over) would.

use super::*;
use crate::people::tests::face_like;
use crate::people::{self, NOT_THIS_PERSON, to_blob};
use crate::{albums, hash, organize, scan};

struct Pc {
    db: Db,
    root: PathBuf,
    folder: i64,
}

fn png(path: &Path, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(4 + shade as u32, 3, image::Rgb([shade, 0, 0])).save(path).unwrap();
}

/// A PC with its own copy of the photos at `<base>/<name>/Photos`, watching `watch` in it.
fn pc(base: &Path, name: &str, watch: &str) -> Pc {
    let root = base.join(name).join("Photos");
    for (i, file) in ["a.png", "b.png", "sub/c.png"].iter().enumerate() {
        png(&root.join(file), i as u8 * 10);
    }
    let db = Db::open(&base.join(name).join("library.db")).unwrap();
    let folder = folders::add(&db, &root.join(watch)).unwrap();
    scan::scan_folder(&db, &folder, &mut |_| {}).unwrap();
    hash_all(&db);
    Pc { db, root: dunce::canonicalize(&root).unwrap(), folder: folder.id }
}

/// What the hash stage of indexing does.
fn hash_all(db: &Db) {
    let conn = db.conn().unwrap();
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, path FROM media WHERE missing = 0").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    for (id, path) in rows {
        let h = hash::file_blake3(Path::new(&path)).unwrap();
        conn.execute("UPDATE media SET blake3 = ?2, stage = 3 WHERE id = ?1", params![id, h]).unwrap();
    }
}

impl Pc {
    fn id(&self, rel: &str) -> i64 {
        let path = self.root.join(rel.replace('/', "\\"));
        self.db
            .conn()
            .unwrap()
            .query_row("SELECT id FROM media WHERE path = ?1", [path.to_string_lossy()], |r| r.get(0))
            .unwrap()
    }

    fn device(&self) -> String {
        device_id(&self.db.conn().unwrap()).unwrap()
    }

    fn flag(&self, rel: &str, column: &str) -> bool {
        self.db
            .conn()
            .unwrap()
            .query_row(&format!("SELECT {column} IS NOT NULL AND {column} != 0 FROM media WHERE id = ?1"), [self.id(rel)], |r| r.get(0))
            .unwrap()
    }

    /// (name, item names, cover name) of every album, by name.
    fn albums(&self) -> Vec<(String, Vec<String>, Option<String>)> {
        let conn = self.db.conn().unwrap();
        let mut out = Vec::new();
        let mut stmt = conn.prepare("SELECT id, name FROM albums").unwrap();
        let all: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        for (id, name) in all {
            let mut stmt = conn
                .prepare("SELECT m.name FROM album_items i JOIN media m ON m.id = i.media_id WHERE i.album_id = ?1 ORDER BY m.name")
                .unwrap();
            let items = stmt.query_map([id], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
            let cover: Option<String> = conn
                .query_row("SELECT m.name FROM albums a JOIN media m ON m.id = a.cover_media_id WHERE a.id = ?1", [id], |r| r.get(0))
                .optional()
                .unwrap();
            out.push((name, items, cover));
        }
        out.sort();
        out
    }

    /// Write this PC's changes, read everyone's, apply.
    fn sync(&self) -> bool {
        flush(&self.db).unwrap();
        read(&self.db).unwrap();
        apply(&self.db).unwrap()
    }
}

/// Copy `from`'s log (and the `.memora` basics) to `to`, like a sync client would.
fn carry(from: &Pc, to: &Pc) {
    let src = from.root.join(DIR);
    let dst = to.root.join(DIR);
    fs::create_dir_all(dst.join("changes")).unwrap();
    if !dst.join("memora.json").exists() {
        fs::copy(src.join("memora.json"), dst.join("memora.json")).unwrap();
    }
    let log = format!("{}.jsonl", from.device());
    fs::copy(src.join("changes").join(&log), dst.join("changes").join(&log)).unwrap();
}

#[test]
fn edits_travel_between_pcs_with_different_paths() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    assert!(a.root.join(DIR).join("memora.json").is_file());

    organize::set_favorite(&a.db, &[a.id("a.png")], true).unwrap();
    organize::archive(&a.db, &[a.id("b.png")]).unwrap();
    let trip = albums::create(&a.db, "Trip").unwrap();
    albums::add_items(&a.db, trip.id, &[a.id("a.png"), a.id("sub/c.png")]).unwrap();
    albums::set_cover(&a.db, trip.id, a.id("sub/c.png")).unwrap();
    albums::create(&a.db, "Empty").unwrap(); // in no synced folder: stays on this PC
    a.sync();

    // PC 2 adds the same folder (here a copy at another path) and finds the info.
    let b = pc(dir.path(), "F", "");
    carry(&a, &b);
    let p = probe(&b.root);
    assert_eq!(p.root.as_deref(), Some(b.root.as_path()));
    assert_eq!(p.pcs.len(), 1);
    enable(&b.db, b.folder).unwrap();
    assert!(b.sync());
    assert!(b.flag("a.png", "favorite"));
    assert!(b.flag("b.png", "archived_at"));
    assert!(!b.flag("sub/c.png", "favorite"));
    assert_eq!(b.albums(), vec![("Trip".into(), vec!["a.png".into(), "c.png".into()], Some("c.png".into()))]);
    assert!(!b.sync(), "nothing new the second time");

    // PC 2 changes things; PC 1 picks them up.
    let b_trip = albums::list(&b.db).unwrap()[0].id;
    albums::rename(&b.db, b_trip, "Trip 2024").unwrap();
    albums::remove_items(&b.db, b_trip, &[b.id("a.png")]).unwrap();
    organize::set_favorite(&b.db, &[b.id("a.png")], false).unwrap();
    organize::unarchive(&b.db, &[b.id("b.png")]).unwrap();
    organize::trash(&b.db, &[b.id("sub/c.png")]).unwrap();
    b.sync();
    carry(&b, &a);
    assert!(a.sync());
    assert!(!a.flag("a.png", "favorite"));
    assert!(!a.flag("b.png", "archived_at"));
    assert!(a.flag("sub/c.png", "trashed_at"));
    let names: Vec<String> = a.albums().into_iter().map(|x| x.0).collect();
    assert_eq!(names, ["Empty", "Trip 2024"]);
    assert_eq!(a.albums()[1].1, vec!["c.png".to_owned()]);

    // Both rename at "the same time": the later change wins on both PCs.
    albums::rename(&a.db, trip.id, "From A").unwrap();
    albums::rename(&b.db, b_trip, "From B").unwrap();
    a.sync();
    b.sync();
    carry(&a, &b);
    carry(&b, &a);
    a.sync();
    b.sync();
    assert_eq!(a.albums()[1].0, "From B");
    assert_eq!(b.albums()[0].0, "From B");

    // Restoring from the trash and deleting the album travel too.
    organize::restore(&a.db, &[a.id("sub/c.png")]).unwrap();
    albums::delete(&a.db, trip.id).unwrap();
    a.sync();
    carry(&a, &b);
    assert!(b.sync());
    assert!(!b.flag("sub/c.png", "trashed_at"));
    assert!(b.albums().is_empty());
}

#[test]
fn half_written_lines_wait_and_renamed_files_are_found_by_hash() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    organize::set_favorite(&a.db, &[a.id("a.png")], true).unwrap();
    organize::set_favorite(&a.db, &[a.id("b.png")], true).unwrap();
    a.sync();

    let b = pc(dir.path(), "F", "");
    // On PC 2 the file has another name (renamed before syncing).
    fs::rename(b.root.join("b.png"), b.root.join("b renamed.png")).unwrap();
    scan::scan_folder(&b.db, &folders::get(&b.db, b.folder).unwrap(), &mut |_| {}).unwrap();
    hash_all(&b.db);

    // The log arrives cut off in the middle of its last line.
    carry(&a, &b);
    let log = b.root.join(DIR).join("changes").join(format!("{}.jsonl", a.device()));
    let full = fs::read(&log).unwrap();
    fs::write(&log, &full[..full.len() - 10]).unwrap();
    enable(&b.db, b.folder).unwrap();
    b.sync();
    assert!(b.flag("a.png", "favorite"), "complete lines are applied");
    assert!(!b.flag("b renamed.png", "favorite"), "the cut-off line waits");

    fs::write(&log, &full).unwrap();
    assert!(b.sync());
    assert!(b.flag("b renamed.png", "favorite"), "found by its hash");
}

#[test]
fn existing_info_fills_gaps_but_does_not_override() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    organize::set_favorite(&a.db, &[a.id("a.png")], true).unwrap();
    organize::set_favorite(&a.db, &[a.id("a.png")], false).unwrap();
    a.sync();

    // PC 2 had favorites of its own before switching on.
    let b = pc(dir.path(), "F", "");
    organize::set_favorite(&b.db, &[b.id("a.png"), b.id("sub/c.png")], true).unwrap();
    carry(&a, &b);
    enable(&b.db, b.folder).unwrap();
    b.sync();
    assert!(!b.flag("a.png", "favorite"), "PC 1 decided about a.png");
    assert!(b.flag("sub/c.png", "favorite"), "nobody decided about c.png before");

    carry(&b, &a);
    a.sync();
    assert!(a.flag("sub/c.png", "favorite"));
    assert!(!a.flag("a.png", "favorite"));
}

#[test]
fn subfolder_uses_the_parents_info_and_unplugged_drives_keep_changes() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    organize::set_favorite(&a.db, &[a.id("sub/c.png")], true).unwrap();
    a.sync();

    // PC 2 only watches the subfolder.
    let b = pc(dir.path(), "F", "sub");
    carry(&a, &b);
    let folder = enable(&b.db, b.folder).unwrap();
    assert_eq!(folder.sync_root.as_deref(), Some(b.root.as_path()));
    b.sync();
    assert!(b.flag("sub/c.png", "favorite"));

    // PC 2's drive is unplugged: the change waits and is written when it is back.
    organize::archive(&b.db, &[b.id("sub/c.png")]).unwrap();
    let away = dir.path().join("F-away");
    fs::rename(dir.path().join("F").join("Photos"), &away).unwrap();
    flush(&b.db).unwrap();
    let st = status(&b.db, &folders::get(&b.db, b.folder).unwrap()).unwrap().unwrap();
    assert_eq!((st.unsent, st.writable), (1, false));
    fs::rename(&away, dir.path().join("F").join("Photos")).unwrap();
    flush(&b.db).unwrap();
    let st = status(&b.db, &folders::get(&b.db, b.folder).unwrap()).unwrap().unwrap();
    assert_eq!((st.unsent, st.writable), (0, true));
    assert_eq!(st.other_pcs.len(), 1);

    carry(&b, &a);
    a.sync();
    assert!(a.flag("sub/c.png", "archived_at"));
}

#[test]
fn clocks_from_a_pc_that_runs_ahead_are_respected() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    // Another PC whose clock is a year ahead favorited a.png.
    let future = format_clock(Utc::now().timestamp_millis() as u64 + 365 * 86_400_000, 0, "zzfuture");
    let header = Header { memora: FORMAT, device: "zzfuture".into(), name: "Fast PC".into(), generation: "g1".into() };
    let line = Line { c: future.clone(), f: "fav".into(), p: "a.png".into(), s: String::new(), h: None, v: json!(true) };
    fs::write(
        a.root.join(DIR).join("changes").join("zzfuture.jsonl"),
        format!("{}\n{}\n", serde_json::to_string(&header).unwrap(), serde_json::to_string(&line).unwrap()),
    )
    .unwrap();
    assert!(a.sync());
    assert!(a.flag("a.png", "favorite"));
    // Unfavoriting here afterwards still wins, though the wall clock is behind.
    organize::set_favorite(&a.db, &[a.id("a.png")], false).unwrap();
    a.sync();
    assert!(!a.flag("a.png", "favorite"));
    let clock: String = a
        .db
        .conn()
        .unwrap()
        .query_row("SELECT clock FROM sync_reg WHERE field = 'fav' AND item = 'a.png'", [], |r| r.get(0))
        .unwrap();
    assert!(clock > future);
}

#[test]
fn logs_are_compacted_and_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    enable(&a.db, a.folder).unwrap();
    // The first write makes the log; later changes are appended to it.
    organize::set_favorite(&a.db, &[a.id("b.png")], true).unwrap();
    organize::set_favorite(&a.db, &[a.id("b.png")], false).unwrap();
    a.sync();
    for i in 0..300 {
        organize::set_favorite(&a.db, &[a.id("a.png")], i % 2 == 0).unwrap();
    }
    a.sync();
    let b = pc(dir.path(), "F", "");
    carry(&a, &b);
    enable(&b.db, b.folder).unwrap();
    b.sync();
    assert!(!b.flag("a.png", "favorite"));

    let log = a.root.join(DIR).join("changes").join(format!("{}.jsonl", a.device()));
    let before = fs::read_to_string(&log).unwrap().lines().count();
    compact(&a.db).unwrap();
    let after = fs::read_to_string(&log).unwrap().lines().count();
    assert!(before > 300 && after == 3, "{before} -> {after}");

    // PC 2 sees a new generation of the log and reads it from the start.
    organize::set_favorite(&a.db, &[a.id("b.png")], true).unwrap();
    a.sync();
    carry(&a, &b);
    assert!(b.sync());
    assert!(b.flag("b.png", "favorite"));
    assert!(!b.flag("a.png", "favorite"));
}

fn add_face(db: &Db, media: i64, rect: [f32; 4], seed: u64) -> i64 {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO faces (media_id, x, y, w, h, score, embedding) VALUES (?1, ?2, ?3, ?4, ?5, 0.9, ?6)",
        params![media, rect[0], rect[1], rect[2], rect[3], to_blob(&face_like(0, seed))],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn person_name(db: &Db, face: i64) -> Option<String> {
    db.conn()
        .unwrap()
        .query_row("SELECT p.name FROM faces f JOIN persons p ON p.id = f.person_id WHERE f.id = ?1", [face], |r| r.get(0))
        .optional()
        .unwrap()
        .flatten()
}

#[test]
fn people_names_and_corrections_travel() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    let b = pc(dir.path(), "F", "");
    let boxes = [[0.1, 0.1, 0.2, 0.2], [0.5, 0.1, 0.2, 0.2]];
    // The same faces were found on both PCs (boxes a little different) and grouped
    // automatically, each PC into a person of its own.
    let mut fb = Vec::new();
    for (i, rel) in ["a.png", "b.png", "sub/c.png"].iter().enumerate() {
        add_face(&a.db, a.id(rel), boxes[0], i as u64);
        let nudged = [boxes[0][0] + 0.01, boxes[0][1], boxes[0][2], boxes[0][3]];
        fb.push(add_face(&b.db, b.id(rel), nudged, i as u64 + 10));
    }
    let stranger_a = add_face(&a.db, a.id("a.png"), boxes[1], 99);
    let stranger_b = add_face(&b.db, b.id("a.png"), boxes[1], 98);
    people::cluster(&a.db).unwrap();
    people::cluster(&b.db).unwrap();
    assert!(person_name(&b.db, fb[0]).is_none());

    enable(&a.db, a.folder).unwrap();
    let pa = people::list(&a.db, false).unwrap()[0].id;
    people::rename(&a.db, pa, Some("Ria")).unwrap();
    people::set_face_person(&a.db, stranger_a, None).unwrap(); // "not this person"
    a.sync();

    carry(&a, &b);
    enable(&b.db, b.folder).unwrap();
    assert!(b.sync());
    for f in &fb {
        assert_eq!(person_name(&b.db, *f).as_deref(), Some("Ria"), "PC 2's own group got the name");
    }
    assert_eq!(people::list(&b.db, true).unwrap().len(), 1);
    let locked: i64 = b.db.conn().unwrap().query_row("SELECT locked FROM faces WHERE id = ?1", [stranger_b], |r| r.get(0)).unwrap();
    assert_eq!(locked, NOT_THIS_PERSON);

    // Hiding on PC 2 travels back.
    let pb = people::list(&b.db, false).unwrap()[0].id;
    people::set_hidden(&b.db, pb, true).unwrap();
    b.sync();
    carry(&b, &a);
    a.sync();
    assert!(people::get(&a.db, pa).unwrap().hidden);
}

#[test]
fn faces_found_later_get_their_names_when_recognised() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "E", "");
    let face = add_face(&a.db, a.id("a.png"), [0.3, 0.3, 0.2, 0.2], 1);
    enable(&a.db, a.folder).unwrap();
    let p = people::new_person_for_face(&a.db, face, Some("Sam")).unwrap();
    assert_eq!(p.name.as_deref(), Some("Sam"));
    a.sync();

    // PC 2 has not looked for faces yet.
    let b = pc(dir.path(), "F", "");
    carry(&a, &b);
    enable(&b.db, b.folder).unwrap();
    b.sync();
    assert!(people::list(&b.db, true).unwrap().is_empty());

    // Recognition finds the face; the name is there before any grouping runs.
    let m = crate::media::get(&b.db, b.id("a.png")).unwrap();
    let found = crate::ai::Found {
        faces: vec![crate::ai::FoundFace { rect: [0.31, 0.3, 0.2, 0.2], score: 0.9, embedding: face_like(0, 5) }],
        clip: None,
    };
    let saved = crate::ai::save(&b.db, &m, &found).unwrap().unwrap();
    assert_eq!(person_name(&b.db, saved.face_ids[0]).as_deref(), Some("Sam"));
}

/// Open a copy of a real library (migrations run), record what its first folder has as
/// at switch-on, and check applying it changes nothing. No file is written: `MEMORA_DB_COPY=path\library.db cargo test -p photos-core -- --ignored real_library_upgrade --nocapture`
#[test]
#[ignore]
fn real_library_upgrade() {
    let path = PathBuf::from(std::env::var("MEMORA_DB_COPY").expect("set MEMORA_DB_COPY"));
    let db = Db::open(&path).unwrap();
    let conn = db.conn().unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    println!("version {}", count("PRAGMA user_version"));
    println!("albums {} (no uid {})", count("SELECT count(*) FROM albums"), count("SELECT count(*) FROM albums WHERE uid IS NULL"));
    println!("persons {} (no uid {})", count("SELECT count(*) FROM persons"), count("SELECT count(*) FROM persons WHERE uid IS NULL"));
    println!("media {}, favorites {}", count("SELECT count(*) FROM media"), count("SELECT count(*) FROM media WHERE favorite = 1"));
    // Only the database: `enable` would also create `.memora` in the real photo folder.
    let folder = folders::list(&db).unwrap()[0].clone();
    conn.execute("UPDATE folders SET sync = 1, sync_root = ?2 WHERE id = ?1", params![folder.id, folder.path.to_string_lossy()]).unwrap();
    let started = std::time::Instant::now();
    seed(&conn, folder.id).unwrap();
    println!("seeded {} values in {:.1?}", count("SELECT count(*) FROM sync_reg"), started.elapsed());
    let started = std::time::Instant::now();
    drop(conn);
    let changed = apply(&db).unwrap();
    println!("apply over the whole library: changed {changed} in {:.1?}", started.elapsed());
    assert!(!changed, "switching on must not change the library");
}

#[test]
fn photo_edits_travel() {
    let dir = tempfile::tempdir().unwrap();
    let a = pc(dir.path(), "G", "");
    enable(&a.db, a.folder).unwrap();
    let edit = crate::edit::Edit {
        rotate: 1,
        filter: Some("chrome".into()),
        adjust: crate::edit::Adjust { exposure: 20.0, ..Default::default() },
        ..Default::default()
    };
    crate::edit::save(&a.db, a.id("a.png"), Some(edit.clone())).unwrap();
    a.sync();

    let b = pc(dir.path(), "H", "");
    carry(&a, &b);
    enable(&b.db, b.folder).unwrap();
    assert!(b.sync());
    let got = crate::edit::get(&b.db, b.id("a.png")).unwrap();
    assert_eq!(got.edit, Some(edit));
    assert!(got.edited_at.is_some());
    assert!(!b.sync(), "nothing new the second time");

    // Reverting on the second PC reaches the first.
    crate::edit::save(&b.db, b.id("a.png"), None).unwrap();
    b.sync();
    carry(&b, &a);
    assert!(a.sync());
    assert_eq!(crate::edit::get(&a.db, a.id("a.png")).unwrap().edit, None);
}
