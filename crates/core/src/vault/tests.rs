use std::fs;
use std::time::Duration;

use super::*;
use crate::media::Query;
use crate::{Library, albums, organize};

fn png(path: &Path, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(40 + shade as u32, 30, image::Rgb([shade, 90, 0])).save(path).unwrap();
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Every file under `dir` whose bytes contain `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| fs::read(e.path()).is_ok_and(|b| contains(&b, needle.as_bytes())))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn id_of(lib: &Library, name: &str) -> i64 {
    lib.db().conn().unwrap().query_row("SELECT id FROM media WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn moved_items_leave_no_trace_in_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    png(&photos.join("beach.png"), 10);
    png(&photos.join("zz-private-moment.png"), 80);
    png(&photos.join("sub").join("dog.png"), 160);
    let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let lib = Library::start(Db::open(&paths.db_path).unwrap(), paths.clone()).unwrap();
    lib.add_folder(&photos, true).unwrap();
    assert!(lib.wait_idle(Duration::from_secs(60)));

    let secret = id_of(&lib, "zz-private-moment.png");
    let beach = id_of(&lib, "beach.png");
    organize::set_favorite(lib.db(), &[secret], true).unwrap();
    let album = albums::create(lib.db(), "Holiday").unwrap();
    albums::add_items(lib.db(), album.id, &[secret, beach]).unwrap();
    albums::set_cover(lib.db(), album.id, secret).unwrap();
    lib.sync_now().unwrap();
    let memora = photos.join(sync::DIR);
    assert!(!files_containing(&memora, "zz-private-moment").is_empty(), "the log and index snapshot know it first");
    let thumb = thumbs::thumb_path(&paths, secret);
    assert!(thumb.is_file());

    // Locked: refused, nothing changes.
    assert!(matches!(lib.move_to_vault(&[secret]), Err(Error::Locked)));
    lib.vault().setup_with("vault password", crypto::KdfParams::for_tests()).unwrap();
    let report = lib.move_to_vault(&[secret]).unwrap();
    assert_eq!((report.moved, report.failed.len()), (1, 0), "{report:?}");

    // In the locked folder, with what the library knew.
    let items = lib.vault().list().unwrap();
    assert_eq!(items.len(), 1);
    // Kept next to the photos, not in the data folder.
    let count = |d: &Path| walkdir::WalkDir::new(d).into_iter().flatten().filter(|e| e.file_type().is_file()).count();
    assert_eq!(count(&Vault::place_in(&photos).join("files")), 2, "original + thumbnail");
    assert_eq!(count(&lib.vault().dir().join("files")), 0);
    let item = &items[0];
    assert_eq!(item.info.name, "zz-private-moment.png");
    assert!(item.info.favorite);
    assert_eq!((item.info.width, item.info.height), (Some(120), Some(30)));
    let mut thumb_bytes = Vec::new();
    std::io::Read::read_to_end(&mut lib.vault().open_part(&item.id, Part::Thumb).unwrap().0, &mut thumb_bytes).unwrap();
    assert_eq!(&thumb_bytes[..2], &[0xFF, 0xD8], "thumbnail is a JPEG");

    // Gone from disk and from the library: lists, search, album, cover, caches.
    assert!(!photos.join("zz-private-moment.png").exists(), "original deleted");
    assert!(!thumb.exists(), "cached thumbnail deleted");
    assert!(matches!(media::get(lib.db(), secret), Err(Error::NotFound)));
    let ids = |q: Query| media::query(lib.db(), &q).unwrap().into_iter().map(|i| i.id).collect::<Vec<_>>();
    assert!(!ids(Query::default()).contains(&secret));
    assert!(ids(Query { q: Some("private".into()), ..Query::default() }).is_empty());
    let album = albums::get(lib.db(), album.id).unwrap();
    assert_eq!(album.count, 1);
    assert_eq!(album.cover_id, Some(beach), "cover falls back to the other photo");

    // Not a word of it left in this PC's `.memora` files or the library's database files.
    let found = files_containing(&memora, "zz-private-moment");
    assert!(found.is_empty(), "still in {found:?}");
    let hash = item.info.hash.clone();
    assert!(files_containing(&memora, &hash).is_empty(), "hash still in the library info");
    let found = files_containing(&paths.data_dir, "zz-private-moment");
    assert!(found.is_empty(), "still in {found:?}");
    assert!(files_containing(&memora, "beach.png").len() >= 2, "the other items are still there");

    // Not picked up again by a rescan.
    lib.rescan();
    assert!(lib.wait_idle(Duration::from_secs(60)));
    assert_eq!(ids(Query::default()).len(), 2);

    // Moving it out puts the file back in a folder; a watched folder indexes it as new.
    let report = move_out(lib.vault(), std::slice::from_ref(&item.id), &photos).unwrap();
    assert_eq!(report.moved, 1);
    assert_eq!(report.paths, [photos.join("zz-private-moment.png")]);
    assert!(lib.vault().list().unwrap().is_empty());
    lib.rescan();
    assert!(lib.wait_idle(Duration::from_secs(60)));
    assert_eq!(ids(Query::default()).len(), 3);
    lib.shutdown();
}

#[test]
fn an_original_that_cant_be_deleted_stays_in_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    png(&photos.join("busy.png"), 10);
    let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let lib = Library::start(Db::open(&paths.db_path).unwrap(), paths).unwrap();
    lib.add_folder(&photos, false).unwrap();
    assert!(lib.wait_idle(Duration::from_secs(60)));
    let id = id_of(&lib, "busy.png");
    lib.vault().setup_with("vault password", crypto::KdfParams::for_tests()).unwrap();

    // A file another program holds open (sharing reads only) can be read but not deleted.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let held = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(photos.join("busy.png")).unwrap();
        let report = lib.move_to_vault(&[id]).unwrap();
        drop(held);
        assert_eq!(report.moved, 0);
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].contains("couldn't be deleted"), "{report:?}");
        assert!(media::get(lib.db(), id).is_ok(), "still in the library");
        assert!(photos.join("busy.png").is_file());
        assert!(lib.vault().list().unwrap().is_empty(), "the encrypted copy was dropped");
        let files = walkdir::WalkDir::new(Vault::place_in(&photos).join("files")).into_iter().flatten();
        assert_eq!(files.filter(|e| e.file_type().is_file()).count(), 0);
    }
    lib.shutdown();
}
