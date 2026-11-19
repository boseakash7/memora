use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;

struct Harness {
    _dir: tempfile::TempDir,
    dir_path: std::path::PathBuf,
    app: Router,
    token: String,
    library: Library,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let paths = photos_core::AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let db = photos_core::Db::open(&paths.db_path).unwrap();
    let library = Library::start(db, paths).unwrap();
    let state = AppState::new(library.clone());
    let token = state.token().to_owned();
    Harness { dir_path: dir.path().to_path_buf(), _dir: dir, app: router(state), token, library }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.library.shutdown();
    }
}

impl Harness {
    async fn call(&self, method: &str, uri: &str, auth: bool, body: Option<Value>) -> (StatusCode, Value) {
        let (status, _, bytes) = self.raw(method, uri, auth, body, &[]).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn raw(
        &self,
        method: &str,
        uri: &str,
        auth: bool,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        if auth {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, headers, bytes.to_vec())
    }
}

#[tokio::test]
async fn health_is_public_but_api_needs_token() {
    let h = harness();
    assert_eq!(h.call("GET", "/api/health", false, None).await.0, StatusCode::OK);
    assert_eq!(h.call("GET", "/api/folders", false, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(h.call("GET", "/api/folders?token=wrong", false, None).await.0, StatusCode::UNAUTHORIZED);
    let uri = format!("/api/folders?token={}", h.token);
    assert_eq!(h.call("GET", &uri, false, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn folder_lifecycle() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();

    let (status, folder) = h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = folder["id"].as_i64().unwrap();

    let (status, err) = h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("already"));

    let (_, list) = h.call("GET", "/api/folders", true, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let uri = format!("/api/folders/{id}");
    assert_eq!(h.call("DELETE", &uri, true, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(h.call("DELETE", &uri, true, None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn library_info_in_the_folder() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::new(40, 30).save(photos.join("beach.jpg")).unwrap();

    let (_, p) = h.call("POST", "/api/folders/probe", true, Some(json!({"path": photos}))).await;
    assert_eq!((p["root"].is_null(), p["pcs"].as_array().unwrap().len()), (true, 0), "{p}");

    let (status, folder) = h.call("POST", "/api/folders", true, Some(json!({"path": photos, "keepInfo": true}))).await;
    assert_eq!(status, StatusCode::CREATED, "{folder}");
    assert_eq!(folder["sync"], true);
    assert_eq!(folder["libraryInfo"]["writable"], true, "{folder}");
    assert!(photos.join(".memora").join("memora.json").is_file());
    let id = folder["id"].as_i64().unwrap();

    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    let item = all[0]["id"].as_i64().unwrap();
    h.call("POST", "/api/media/favorite", true, Some(json!({"ids": [item], "favorite": true}))).await;
    let (status, _) = h.call("POST", "/api/library/sync", true, None).await;
    assert_eq!(status, StatusCode::OK);
    let logs: Vec<_> = std::fs::read_dir(photos.join(".memora").join("changes")).unwrap().collect();
    assert_eq!(logs.len(), 1);
    let log = std::fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
    assert!(log.contains(r#""f":"fav","p":"beach.jpg""#), "{log}");

    // A second PC would see it in the probe.
    let (_, p) = h.call("POST", "/api/folders/probe", true, Some(json!({"path": photos}))).await;
    assert_eq!(p["pcs"].as_array().unwrap().len(), 1);

    // The finished index is kept there too (on by default) and can be switched off.
    assert_eq!(folder["keepIndex"], true);
    let (_, f) = h.call("GET", "/api/folders", true, None).await;
    assert!(f[0]["libraryInfo"]["indexBytes"].as_u64().unwrap() > 0, "{f}");
    let uri = format!("/api/folders/{id}");
    let (_, f) = h.call("PATCH", &uri, true, Some(json!({"keepIndex": false}))).await;
    assert_eq!((f["keepIndex"].as_bool(), f["libraryInfo"]["indexBytes"].is_null()), (Some(false), true), "{f}");
    assert_eq!(std::fs::read_dir(photos.join(".memora").join("index")).unwrap().count(), 0);

    let (_, off) = h.call("PATCH", &uri, true, Some(json!({"keepInfo": false}))).await;
    assert_eq!((off["sync"].as_bool(), off["libraryInfo"].is_null()), (Some(false), true));
    assert!(photos.join(".memora").is_dir(), "switching off keeps the folder");
    assert_eq!(h.call("PATCH", &uri, true, Some(json!({"nope": 1}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn backup_and_restore() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::new(40, 30).save(photos.join("beach.jpg")).unwrap();
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    let id = all[0]["id"].as_i64().unwrap();
    h.call("POST", "/api/media/favorite", true, Some(json!({"ids": [id], "favorite": true}))).await;

    let file = h.dir_path.join("mine.memora-backup");
    let (status, summary) = h.call("POST", "/api/backup", true, Some(json!({"path": file}))).await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["favorites"], 1);
    let (_, read) = h.call("POST", "/api/backup/read", true, Some(json!({"path": file}))).await;
    assert_eq!(read, summary);

    h.call("POST", "/api/media/favorite", true, Some(json!({"ids": [id], "favorite": false}))).await;
    let (status, restored) = h.call("POST", "/api/backup/restore", true, Some(json!({"path": file}))).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!((restored["items"].as_u64(), restored["favorites"].as_u64(), restored["settings"].as_bool()), (Some(1), Some(1), Some(false)));
    let (_, m) = h.call("GET", &format!("/api/media/{id}"), true, None).await;
    assert_eq!(m["favorite"], true);

    let bad = h.dir_path.join("bad.memora-backup");
    std::fs::write(&bad, "nope").unwrap();
    let (status, err) = h.call("POST", "/api/backup/read", true, Some(json!({"path": bad}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
}

#[tokio::test]
async fn locate_a_moved_folder() {
    let h = harness();
    let photos = h.dir_path.join("E").join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::new(40, 30).save(photos.join("beach.jpg")).unwrap();
    let (_, folder) = h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let folder_id = folder["id"].as_i64().unwrap();
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    let id = all[0]["id"].as_i64().unwrap();
    h.call("POST", "/api/media/favorite", true, Some(json!({"ids": [id], "favorite": true}))).await;

    // The drive comes back as F:.
    let moved = h.dir_path.join("F").join("Photos");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&photos, &moved).unwrap();
    let (_, list) = h.call("GET", "/api/folders", true, None).await;
    assert_eq!(list[0]["available"], false);

    let uri = format!("/api/folders/{folder_id}/locate");
    let (status, err) = h.call("POST", &uri, true, Some(json!({"path": h.dir_path}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, located) = h.call("POST", &uri, true, Some(json!({"path": moved}))).await;
    assert_eq!(status, StatusCode::OK, "{located}");
    assert_eq!(located["available"], true);
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());

    let (_, m) = h.call("GET", &format!("/api/media/{id}"), true, None).await;
    assert!(m["path"].as_str().unwrap().contains(r"\F\Photos\beach.jpg"), "{m}");
    assert_eq!((m["favorite"].as_bool(), m["missing"].as_bool()), (Some(true), Some(false)));
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(all.as_array().unwrap().len(), 1, "same item, not indexed again");
    assert_eq!(h.call("POST", "/api/folders/999/locate", true, Some(json!({"path": moved}))).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn settings_patch() {
    let h = harness();
    let (status, s) = h.call("PATCH", "/api/settings", true, Some(json!({"theme": "dark"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["theme"], "dark");
    let (status, _) = h.call("PATCH", "/api/settings", true, Some(json!({"lanPort": 1}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cors_preflight_allows_ui_origin() {
    let h = harness();
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/api/folders")
        .header(header::ORIGIN, "http://tauri.localhost")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "http://tauri.localhost"
    );
}

#[tokio::test]
async fn timeline_thumbnails_and_originals() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::from_pixel(640, 480, image::Rgb([0, 128, 255])).save(photos.join("blue.jpg")).unwrap();
    image::RgbImage::new(64, 32).save(photos.join("scan.tif")).unwrap();

    let (status, _) = h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());

    let (_, status_json) = h.call("GET", "/api/library/status", true, None).await;
    assert_eq!(status_json["total"], 2);
    assert_eq!(status_json["pending"], 0);

    let (status, items) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 2);
    let blue = items.iter().find(|i| i["w"] == 640).unwrap()["id"].as_i64().unwrap();
    let tif = items.iter().find(|i| i["w"] == 64).unwrap()["id"].as_i64().unwrap();

    let (_, details) = h.call("GET", &format!("/api/media/{blue}"), true, None).await;
    assert_eq!(details["name"], "blue.jpg");
    assert_eq!(details["kind"], "photo");
    assert_eq!(h.call("GET", "/api/media/99999", true, None).await.0, StatusCode::NOT_FOUND);

    let (status, info) = h.call("GET", &format!("/api/media/{blue}/info"), true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["place"], Value::Null);
    assert_eq!(info["iso"], Value::Null);
    assert_eq!(h.call("GET", "/api/media/99999/info", true, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.call("GET", &format!("/api/media/{blue}/info"), false, None).await.0, StatusCode::UNAUTHORIZED);

    // Thumbnails work with the token in the query string, as <img src> sends it.
    let uri = format!("/api/media/{blue}/thumb?v=1&token={}", h.token);
    let (status, headers, body) = h.raw("GET", &uri, false, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert!(headers[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    let thumb = image::load_from_memory(&body).unwrap();
    assert_eq!((thumb.width(), thumb.height()), (533, 400));

    // Originals honour Range requests (video seeking depends on it).
    let (status, headers, body) =
        h.raw("GET", &format!("/api/media/{blue}/original"), true, None, &[("range", "bytes=0-9")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 10);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");

    // TIFF is shown through a converted JPEG; JPEG is served as-is.
    let (status, headers, body) = h.raw("GET", &format!("/api/media/{tif}/display"), true, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(image::guess_format(&body).unwrap(), image::ImageFormat::Jpeg);
    let (_, headers, _) = h.raw("GET", &format!("/api/media/{blue}/display"), true, None, &[]).await;
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");

    assert_eq!(h.call("POST", "/api/library/rescan", true, None).await.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn video_storyboard_and_frame() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::from_pixel(64, 48, image::Rgb([0, 128, 255])).save(photos.join("blue.jpg")).unwrap();
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=3"])
        .args(["-c:v", "libx264", "-g", "5", "-pix_fmt", "yuv420p"])
        .arg(photos.join("clip.mp4"))
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("FFmpeg not found; skipping");
        return;
    }
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(60))).await.unwrap());
    let (_, items) = h.call("GET", "/api/timeline", true, None).await;
    let items = items.as_array().unwrap();
    let video = items.iter().find(|i| i["v"] == true).unwrap()["id"].as_i64().unwrap();
    let photo = items.iter().find(|i| i["v"] != true).unwrap()["id"].as_i64().unwrap();

    // The layout header must be readable from the UI's origin.
    let uri = format!("/api/media/{video}/storyboard?v=1&token={}", h.token);
    let (status, headers, body) = h.raw("GET", &uri, false, None, &[("origin", "http://tauri.localhost")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers["x-storyboard"], "frames=3;interval=1.000000;cols=3;rows=1");
    assert!(headers[header::ACCESS_CONTROL_EXPOSE_HEADERS].to_str().unwrap().contains("x-storyboard"));
    let img = image::load_from_memory(&body).unwrap();
    assert_eq!((img.width(), img.height()), (720, 180));
    assert!(!h.call("GET", &format!("/api/media/{photo}/storyboard"), true, None).await.0.is_success());

    let (status, headers, body) = h.raw("GET", &format!("/api/media/{video}/frame?t=1.5"), true, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    // PNG signature, then the IHDR chunk's width and height.
    assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n");
    let size = |at: usize| u32::from_be_bytes(body[at..at + 4].try_into().unwrap());
    assert_eq!((size(16), size(20)), (320, 240));
}

#[tokio::test]
async fn organize_favorites_albums_trash_and_search() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    for name in ["beach.jpg", "city.jpg", "forest.jpg"] {
        image::RgbImage::new(40, 30).save(photos.join(name)).unwrap();
    }
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());

    let ids_of = |items: &Value| -> Vec<(String, i64)> {
        items.as_array().unwrap().iter().map(|i| (String::new(), i["id"].as_i64().unwrap())).collect()
    };
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(ids_of(&all).len(), 3);
    let mut by_name = std::collections::HashMap::new();
    for (_, id) in ids_of(&all) {
        let (_, m) = h.call("GET", &format!("/api/media/{id}"), true, None).await;
        by_name.insert(m["name"].as_str().unwrap().to_owned(), id);
    }
    let (beach, city, forest) = (by_name["beach.jpg"], by_name["city.jpg"], by_name["forest.jpg"]);
    let count = |v: &Value| v.as_array().unwrap().len();
    let rev = || h.library.status().revision;

    // Favorites.
    let before = rev();
    let (status, r) = h.call("POST", "/api/media/favorite", true, Some(json!({"ids": [beach, city], "favorite": true}))).await;
    assert_eq!((status, r["changed"].as_i64()), (StatusCode::OK, Some(2)));
    assert!(rev() > before, "the UI is told to reload");
    let (_, favs) = h.call("GET", "/api/timeline?scope=favorites", true, None).await;
    assert_eq!(count(&favs), 2);
    assert_eq!(favs[0]["f"], true);
    let (_, m) = h.call("GET", &format!("/api/media/{beach}"), true, None).await;
    assert_eq!(m["favorite"], true);

    // Paths of several items, in the order asked (unknown ids skipped).
    let (status, paths) = h.call("POST", "/api/media/paths", true, Some(json!({"ids": [forest, 999_999, beach]}))).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = paths
        .as_array()
        .unwrap()
        .iter()
        .map(|p| std::path::Path::new(p.as_str().unwrap()).file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["forest.jpg", "beach.jpg"]);

    // Albums.
    let (status, album) = h.call("POST", "/api/albums", true, Some(json!({"name": "Holiday", "ids": [beach, forest]}))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(album["count"], 2);
    let album_id = album["id"].as_i64().unwrap();
    assert_eq!(h.call("POST", "/api/albums", true, Some(json!({"name": " "}))).await.0, StatusCode::BAD_REQUEST);

    let (_, in_album) = h.call("GET", &format!("/api/timeline?scope=album&album={album_id}"), true, None).await;
    assert_eq!(count(&in_album), 2);
    assert_eq!(h.call("GET", "/api/timeline?scope=album", true, None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(h.call("GET", "/api/timeline?scope=album&album=999", true, None).await.0, StatusCode::NOT_FOUND);

    let (_, r) = h.call("POST", &format!("/api/albums/{album_id}/items"), true, Some(json!({"ids": [city, beach]}))).await;
    assert_eq!(r["changed"], 1);
    let (_, albums_of) = h.call("GET", &format!("/api/media/{city}/albums"), true, None).await;
    assert_eq!(albums_of[0]["name"], "Holiday");

    let (status, a) = h.call("PATCH", &format!("/api/albums/{album_id}"), true, Some(json!({"name": "Summer", "coverMediaId": city}))).await;
    assert_eq!((status, a["name"].as_str(), a["coverId"].as_i64()), (StatusCode::OK, Some("Summer"), Some(city)));
    let (_, list) = h.call("GET", "/api/albums", true, None).await;
    assert_eq!((list[0]["name"].as_str(), list[0]["count"].as_i64()), (Some("Summer"), Some(3)));

    let (_, r) = h.call("DELETE", &format!("/api/albums/{album_id}/items"), true, Some(json!({"ids": [forest]}))).await;
    assert_eq!(r["changed"], 1);

    // Search, including the album name and filters.
    let (_, found) = h.call("GET", "/api/timeline?q=summer", true, None).await;
    assert_eq!(count(&found), 2);
    let (_, found) = h.call("GET", "/api/timeline?q=forest", true, None).await;
    assert_eq!(count(&found), 1);
    let (_, found) = h.call("GET", "/api/timeline?q=&kind=video", true, None).await;
    assert_eq!(count(&found), 0);
    assert_eq!(h.call("GET", "/api/timeline?kind=audio", true, None).await.0, StatusCode::BAD_REQUEST);
    let (status, cams) = h.call("GET", "/api/cameras", true, None).await;
    assert_eq!((status, count(&cams)), (StatusCode::OK, 0));

    // Trash: hidden everywhere, restorable, and deleting only works on trashed items.
    let (_, r) = h.call("POST", "/api/media/trash", true, Some(json!({"ids": [beach, forest]}))).await;
    assert_eq!(r["changed"], 2);
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(count(&all), 1);
    let (_, trashed) = h.call("GET", "/api/timeline?scope=trash", true, None).await;
    assert_eq!(count(&trashed), 2);
    let (_, list) = h.call("GET", "/api/albums", true, None).await;
    assert_eq!(list[0]["count"], 1, "trashed items leave albums");
    assert_eq!(h.library.status().total, 1);

    let (_, r) = h.call("POST", "/api/media/restore", true, Some(json!({"ids": [beach]}))).await;
    assert_eq!(r["changed"], 1);
    assert_eq!(h.library.status().total, 2);

    // Deleting only ever applies to items in the trash: `city` is not, so its file is untouched.
    // (Moving files to the real Recycle Bin is covered in photos-core with a stand-in.)
    let (status, r) = h.call("POST", "/api/trash/empty", true, Some(json!({"ids": [city]}))).await;
    assert_eq!((status, r["deleted"].as_i64(), r["failed"].as_array().map(Vec::len)), (StatusCode::OK, Some(0), Some(0)));
    assert!(photos.join("city.jpg").exists());
    let (_, trashed) = h.call("GET", "/api/timeline?scope=trash", true, None).await;
    assert_eq!(count(&trashed), 1);

    // Archive: only on the Archive page, out of the counts, back on unarchive.
    let before = rev();
    let (_, r) = h.call("POST", "/api/media/archive", true, Some(json!({"ids": [city]}))).await;
    assert_eq!(r["changed"], 1);
    assert!(rev() > before);
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(ids_of(&all).iter().map(|x| x.1).collect::<Vec<_>>(), [beach]);
    let (_, archived) = h.call("GET", "/api/timeline?scope=archive", true, None).await;
    assert_eq!(ids_of(&archived).iter().map(|x| x.1).collect::<Vec<_>>(), [city]);
    let (_, favs) = h.call("GET", "/api/timeline?scope=favorites", true, None).await;
    assert_eq!(count(&favs), 1, "archived favorite left Favorites");
    let (_, m) = h.call("GET", &format!("/api/media/{city}"), true, None).await;
    assert_eq!(m["archived"], true);
    assert_eq!(h.library.status().total, 1);
    let (_, r) = h.call("POST", "/api/media/unarchive", true, Some(json!({"ids": [city, beach]}))).await;
    assert_eq!(r["changed"], 1);
    assert_eq!(h.library.status().total, 2);

    // Unauthorized and unknown ids.
    assert_eq!(h.call("POST", "/api/media/trash", false, Some(json!({"ids": [1]}))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(h.call("DELETE", "/api/albums/999", true, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.call("DELETE", &format!("/api/albums/{album_id}"), true, None).await.0, StatusCode::NO_CONTENT);
    assert!(photos.join("beach.jpg").exists(), "deleting an album keeps its photos");
}

#[tokio::test]
async fn places_map_and_duplicates() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    let scene = image::RgbImage::from_fn(200, 150, |x, y| image::Rgb([(x * 5) as u8, (y * 7) as u8, ((x ^ y) * 9) as u8]));
    scene.save(photos.join("a.jpg")).unwrap();
    std::fs::copy(photos.join("a.jpg"), photos.join("b.jpg")).unwrap();
    image::RgbImage::from_fn(90, 90, |x, y| image::Rgb([(x * y) as u8, (x * 11) as u8, 40])).save(photos.join("c.jpg")).unwrap();
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());

    let (status, groups) = h.call("GET", "/api/duplicates", true, None).await;
    assert_eq!(status, StatusCode::OK);
    let groups = groups.as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["kind"], "exact");
    assert_eq!(groups[0]["items"].as_array().unwrap().len(), 2);
    assert_eq!(groups[0]["items"][0]["keeper"], true);
    assert_eq!(groups[0]["items"][1]["keeper"], false);

    // No GPS in these files: no places, no map points.
    let (_, places) = h.call("GET", "/api/places", true, None).await;
    assert_eq!(places.as_array().unwrap().len(), 0);
    let (_, points) = h.call("GET", "/api/map/points", true, None).await;
    assert_eq!(points.as_array().unwrap().len(), 0);
    assert_eq!(h.call("GET", "/api/places/99", true, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.call("GET", "/api/timeline?bbox=1,2,3", true, None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(h.call("GET", "/api/timeline?bbox=10,5,20,95", true, None).await.0, StatusCode::BAD_REQUEST);
    let (status, items) = h.call("GET", "/api/timeline?bbox=-180,-90,180,90", true, None).await;
    assert_eq!((status, items.as_array().unwrap().len()), (StatusCode::OK, 0));
}

#[tokio::test]
async fn people_names_merges_and_search() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        image::RgbImage::from_pixel(400, 300, image::Rgb([90, 120, 160])).save(photos.join(name)).unwrap();
    }
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, items) = h.call("GET", "/api/timeline", true, None).await;
    let ids: Vec<i64> = items.as_array().unwrap().iter().map(|i| i["id"].as_i64().unwrap()).collect();

    // Two people as recognition would leave them: P1 in a and b, P2 in c.
    let (faces, p1, p2) = {
        let conn = h.library.db().conn().unwrap();
        conn.execute("INSERT INTO persons (id, created_at) VALUES (1, 'now'), (2, 'now')", []).unwrap();
        let faces: Vec<i64> = [(ids[0], 1), (ids[1], 1), (ids[2], 2)]
            .iter()
            .map(|(m, p)| {
                conn.execute_batch(&format!(
                    "INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id)
                     VALUES ({m}, 0.4, 0.3, 0.2, 0.3, 0.9, zeroblob(512), {p})"
                ))
                .unwrap();
                conn.last_insert_rowid()
            })
            .collect();
        (faces, 1, 2)
    };

    let (status, people) = h.call("GET", "/api/people", true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(people.as_array().unwrap().iter().map(|p| p["count"].as_i64().unwrap()).collect::<Vec<_>>(), [2, 1]);

    // Name P1; searching the name finds their photos (the file names don't match).
    let (status, p) = h.call("PATCH", &format!("/api/people/{p1}"), true, Some(json!({"name": "Ria Sen"}))).await;
    assert_eq!((status, p["name"].as_str()), (StatusCode::OK, Some("Ria Sen")));
    let (_, found) = h.call("GET", "/api/timeline?q=ria", true, None).await;
    assert_eq!(found.as_array().unwrap().len(), 2);
    let (_, found) = h.call("GET", &format!("/api/timeline?person={p2}"), true, None).await;
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(h.call("PATCH", &format!("/api/people/{p1}"), true, Some(json!({"nope": 1}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // A name can be removed again; leaving the field out keeps it.
    let (_, p) = h.call("PATCH", &format!("/api/people/{p1}"), true, Some(json!({"hidden": false}))).await;
    assert_eq!(p["name"], "Ria Sen");

    // Faces in a photo, and their picture (made from the photo on first request).
    let (_, in_a) = h.call("GET", &format!("/api/media/{}/faces", ids[0]), true, None).await;
    assert_eq!(in_a[0]["person"]["name"], "Ria Sen");
    let (status, headers, body) = h.raw("GET", &format!("/api/faces/{}/thumb", faces[0]), true, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    let img = image::load_from_memory(&body).unwrap();
    assert_eq!((img.width(), img.height()), (photos_core::thumbs::FACE_SIZE, photos_core::thumbs::FACE_SIZE));
    assert_eq!(h.call("GET", "/api/faces/999/thumb", true, None).await.0, StatusCode::NOT_FOUND);

    // "Not this person", then merge P2 into P1.
    let (status, _) = h.call("PUT", &format!("/api/faces/{}/person", faces[1]), true, Some(json!({"person": null}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.call("GET", &format!("/api/people/{p1}"), true, None).await.1["count"], 1);
    let (status, merged) = h.call("POST", &format!("/api/people/{p1}/merge"), true, Some(json!({"from": [p2]}))).await;
    assert_eq!((status, merged["count"].as_i64()), (StatusCode::OK, Some(2)));
    assert_eq!(h.call("GET", &format!("/api/people/{p2}"), true, None).await.0, StatusCode::NOT_FOUND);
    // A face given a new person by name.
    let (status, p) = h.call("PUT", &format!("/api/faces/{}/person", faces[1]), true, Some(json!({"newName": "Dev"}))).await;
    assert_eq!((status, p["name"].as_str()), (StatusCode::OK, Some("Dev")));
    let (status, _) = h.call("PUT", &format!("/api/faces/{}/person", faces[1]), true, Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // No recognition worker in tests.
    let (status, ai) = h.call("GET", "/api/ai", true, None).await;
    assert_eq!((status, ai), (StatusCode::OK, Value::Null));
}

#[tokio::test]
async fn locked_folder() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::from_pixel(640, 480, image::Rgb([0, 128, 255])).save(photos.join("private.jpg")).unwrap();
    image::RgbImage::new(40, 30).save(photos.join("beach.jpg")).unwrap();
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, d) = h.call("GET", &format!("/api/media/{}", all[0]["id"]), true, None).await;
    let private = if d["name"] == "private.jpg" { all[0]["id"].as_i64().unwrap() } else { all[1]["id"].as_i64().unwrap() };

    // Not set up: needs setup first; the API needs the token like everything else.
    let (_, st) = h.call("GET", "/api/vault", true, None).await;
    assert_eq!((st["exists"].as_bool(), st["unlocked"].as_bool()), (Some(false), Some(false)));
    assert_eq!(h.call("GET", "/api/vault/items", false, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(h.call("GET", "/api/vault/items", true, None).await.0, StatusCode::LOCKED);
    assert_eq!(h.call("POST", "/api/vault/setup", true, Some(json!({"password": "123"}))).await.0, StatusCode::BAD_REQUEST);
    let (status, key) = h.call("POST", "/api/vault/setup", true, Some(json!({"password": "vault password"}))).await;
    assert_eq!(status, StatusCode::OK);
    let recovery = key["recoveryKey"].as_str().unwrap().to_owned();
    assert_eq!(recovery.len(), 39);

    let (status, moved) = h.call("POST", "/api/vault/move-in", true, Some(json!({"ids": [private]}))).await;
    assert_eq!((status, moved["moved"].as_u64()), (StatusCode::OK, Some(1)), "{moved}");
    assert!(!photos.join("private.jpg").exists());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(all.as_array().unwrap().len(), 1, "gone from the library");
    assert_eq!(h.call("GET", &format!("/api/media/{private}/thumb"), true, None).await.0, StatusCode::NOT_FOUND);

    let (status, headers, body) = h.raw("GET", "/api/vault/items", true, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store", "never cached by the webview");
    let items: Value = serde_json::from_slice(&body).unwrap();
    let item = &items[0];
    assert_eq!((item["name"].as_str(), item["width"].as_u64()), (Some("private.jpg"), Some(640)), "{item}");
    let id = item["id"].as_str().unwrap().to_owned();
    let size = item["size"].as_u64().unwrap();

    let (status, headers, thumb) = h.raw("GET", &format!("/api/vault/items/{id}/thumb"), true, None, &[]).await;
    assert_eq!((status, headers[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/jpeg"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(&thumb[..2], &[0xFF, 0xD8]);
    let (status, headers, part) =
        h.raw("GET", &format!("/api/vault/items/{id}/original"), true, None, &[("range", "bytes=2-11")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], format!("bytes 2-11/{size}"));
    assert_eq!(part.len(), 10);
    let (_, _, whole) = h.raw("GET", &format!("/api/vault/items/{id}/original"), true, None, &[]).await;
    assert_eq!(whole.len() as u64, size);
    assert_eq!(&whole[2..12], &part[..]);
    let (status, _, _) =
        h.raw("GET", &format!("/api/vault/items/{id}/original"), true, None, &[("range", &format!("bytes={size}-"))]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);

    // Locked: every item answer is 423 until unlocked again.
    h.call("POST", "/api/vault/lock", true, None).await;
    assert_eq!(h.call("GET", &format!("/api/vault/items/{id}/thumb"), true, None).await.0, StatusCode::LOCKED);
    assert_eq!(h.call("POST", "/api/vault/unlock", true, Some(json!({"password": "nope nope"}))).await.0, StatusCode::FORBIDDEN);
    let (status, st) = h.call("POST", "/api/vault/recover", true, Some(json!({"recoveryKey": recovery, "password": "brand new pw"}))).await;
    assert_eq!((status, st["count"].as_u64()), (StatusCode::OK, Some(1)), "{st}");

    // Out again: back in the folder and in the library.
    let (_, out) = h.call("POST", "/api/vault/move-out", true, Some(json!({"ids": [id], "path": photos}))).await;
    assert_eq!(out["moved"], 1, "{out}");
    assert_eq!(std::fs::read(photos.join("private.jpg")).unwrap(), whole);
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn photo_editing() {
    let h = harness();
    let photos = h.dir_path.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    image::RgbImage::from_fn(120, 80, |x, _| image::Rgb([(x * 2) as u8, 90, 160])).save(photos.join("street.jpg")).unwrap();
    h.call("POST", "/api/folders", true, Some(json!({"path": photos}))).await;
    let lib = h.library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(std::time::Duration::from_secs(30))).await.unwrap());
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    let id = all[0]["id"].as_i64().unwrap();
    assert_eq!((all[0]["w"].as_u64(), all[0]["h"].as_u64()), (Some(120), Some(80)));

    let (status, f) = h.call("GET", "/api/edit/filters", true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(f["filters"].as_array().unwrap().len() >= 20);
    assert_eq!(f["subjects"], false, "no recognition in tests");

    // A preview is a JPEG of the edited photo at the size asked for.
    let edit = json!({"rotate": 1, "adjust": {"exposure": 30}, "filter": "chrome"});
    let (status, headers, body) =
        h.raw("POST", &format!("/api/media/{id}/edit/preview"), true, Some(json!({"edit": edit, "max": 60})), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let img = image::load_from_memory(&body).unwrap();
    assert_eq!((img.width(), img.height()), (40, 60));

    let (status, e) = h.call("POST", &format!("/api/media/{id}/edit/enhance"), true, Some(json!({"edit": {}}))).await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert!(e["adjust"]["vibrance"].as_f64().is_some() && !e["suggested"].as_array().unwrap().is_empty(), "{e}");

    // Saving turns the grid item and the viewer's picture.
    let (status, saved) = h.call("PUT", &format!("/api/media/{id}/edit"), true, Some(json!({"edit": edit}))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!((saved["w"].as_u64(), saved["h"].as_u64()), (Some(80), Some(120)));
    let (_, all) = h.call("GET", "/api/timeline", true, None).await;
    assert_eq!((all[0]["w"].as_u64(), all[0]["m"].as_i64()), (Some(80), saved["m"].as_i64()));
    let (_, _, body) = h.raw("GET", &format!("/api/media/{id}/display"), true, None, &[]).await;
    let shown = image::load_from_memory(&body).unwrap();
    assert_eq!((shown.width(), shown.height()), (80, 120));
    let (_, _, body) = h.raw("GET", &format!("/api/media/{id}/thumb"), true, None, &[]).await;
    assert_eq!(image::load_from_memory(&body).unwrap().width(), 80);
    let (_, got) = h.call("GET", &format!("/api/media/{id}/edit"), true, None).await;
    assert_eq!(got["edit"]["filter"], "chrome");

    // Background blur without the subject model is refused with a reason.
    let (status, err) = h.call("POST", &format!("/api/media/{id}/edit/subject"), true, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err["error"].as_str().unwrap().contains("model"));

    // Export writes a new file; never the original.
    let original = photos.join("street.jpg");
    let before = std::fs::read(&original).unwrap();
    let (status, _) = h.call("POST", &format!("/api/media/{id}/edit/export"), true, Some(json!({"path": original}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let copy = h.dir_path.join("street-edited.jpg");
    let (status, out) = h.call("POST", &format!("/api/media/{id}/edit/export"), true, Some(json!({"path": copy}))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!((out["width"].as_u64(), out["height"].as_u64()), (Some(80), Some(120)));
    assert!(copy.is_file());
    assert_eq!(std::fs::read(&original).unwrap(), before, "the original is untouched");

    // Reverting brings the original back everywhere.
    let (_, saved) = h.call("PUT", &format!("/api/media/{id}/edit"), true, Some(json!({"edit": null}))).await;
    assert_eq!((saved["w"].as_u64(), saved["edit"].is_null()), (Some(120), true));
    let (_, _, body) = h.raw("GET", &format!("/api/media/{id}/display"), true, None, &[]).await;
    assert_eq!(body, before);
}

#[tokio::test]
async fn ai_features_install_state_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = photos_core::AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let db = photos_core::Db::open(&paths.db_path).unwrap();
    let library = Library::start(db, paths).unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let state = AppState::new(library.clone()).with_ai(photos_ml::Ai::start(library.clone(), vec![models.clone()]));
    let h = Harness { dir_path: dir.path().to_path_buf(), _dir: dir, app: router(state.clone()), token: state.token().to_owned(), library };

    let (status, list) = h.call("GET", "/api/ai/features", true, None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list.as_array().unwrap().iter().map(|f| f["feature"].as_str().unwrap()).collect();
    assert_eq!(names, ["faces", "search", "blur"]);
    assert!(list.as_array().unwrap().iter().all(|f| f["installed"] == false && f["job"].is_null() && f["size"].as_u64().unwrap() > 0));

    // Model files that are there (here: placed by hand) count as installed and can be removed.
    for f in photos_ml::models::FACES {
        std::fs::write(models.join(f.path), b"x").unwrap();
    }
    let (_, list) = h.call("GET", "/api/ai/features", true, None).await;
    assert_eq!((list[0]["installed"].clone(), list[0]["removable"].clone()), (json!(true), json!(true)));
    let (status, list) = h.call("DELETE", "/api/ai/features/faces", true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["installed"], false);
    assert!(!models.join(photos_ml::models::FACES[0].path).exists());

    assert_eq!(h.call("POST", "/api/ai/features/nonsense/download", true, None).await.0, StatusCode::BAD_REQUEST);
}
