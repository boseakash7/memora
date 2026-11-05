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
