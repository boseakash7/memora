//! The network listener: read-only, host check, PIN, trash hidden, Archive, DLNA browsing and media.

use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use photos_core::Library;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use super::{LanConfig, LanState, router};
use crate::AppState;

struct Net {
    _dir: tempfile::TempDir,
    library: Library,
    cfg: Arc<RwLock<Arc<LanConfig>>>,
    app: Router,
    video: bool,
}

impl Drop for Net {
    fn drop(&mut self) {
        self.library.shutdown();
    }
}

fn config() -> LanConfig {
    LanConfig {
        port: 8200,
        tv_page: true,
        dlna: true,
        archive: true,
        memories: true,
        device_name: "Test PC".into(),
        pin: None,
        transcode: true,
        uuid: "11111111-2222-3333-4444-555555555555".into(),
        secret: "s3cret".into(),
    }
}

/// A library with two photos and (if FFmpeg is installed) an HEVC video, shared on the network.
async fn net() -> Net {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    std::fs::create_dir_all(photos.join("Trip")).unwrap();
    image::RgbImage::from_pixel(800, 600, image::Rgb([200, 100, 50])).save(photos.join("sunset.jpg")).unwrap();
    image::RgbImage::from_pixel(5000, 1000, image::Rgb([20, 100, 50])).save(photos.join("Trip").join("pano.tif")).unwrap();
    let video = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=3"])
        .args(["-f", "lavfi", "-i", "sine=duration=3", "-c:v", "libx265", "-x265-params", "log-level=none"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "libopus"])
        .arg(photos.join("clip.mkv"))
        .status()
        .is_ok_and(|s| s.success());

    let paths = photos_core::AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let db = photos_core::Db::open(&paths.db_path).unwrap();
    let library = Library::start(db, paths).unwrap();
    library.add_folder(&photos, false).unwrap();
    let lib = library.clone();
    assert!(tokio::task::spawn_blocking(move || lib.wait_idle(Duration::from_secs(60))).await.unwrap());

    let cfg = Arc::new(RwLock::new(Arc::new(config())));
    let state = LanState { app: AppState::new(library.clone()), cfg: cfg.clone(), pin_guard: Default::default() };
    let app = router(state, &config(), CancellationToken::new());
    Net { _dir: dir, library, cfg, app, video }
}

impl Net {
    async fn send(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self.app.clone().oneshot(req.body(Body::from(body.to_owned())).unwrap()).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, headers, bytes.to_vec())
    }

    async fn get(&self, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
        self.send("GET", uri, &[("host", "192.168.0.100:8200")], "").await
    }

    async fn json(&self, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let (status, _, body) = self.send("GET", uri, headers, "").await;
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    async fn browse(&self, object: &str, flag: &str, start: u32, count: u32) -> (StatusCode, String) {
        let body = format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
            <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>{}</ObjectID>
            <BrowseFlag>{flag}</BrowseFlag><Filter>*</Filter><StartingIndex>{start}</StartingIndex>
            <RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria></u:Browse></s:Body></s:Envelope>"#,
            crate::dlna::soap::escape(object)
        );
        let headers = [
            ("host", "192.168.0.100:8200"),
            ("soapaction", "\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\""),
            ("content-type", "text/xml; charset=\"utf-8\""),
        ];
        let (status, _, body) = self.send("POST", "/dlna/cds/control", &headers, &body).await;
        (status, String::from_utf8(body).unwrap())
    }

    fn ids(&self) -> Vec<(i64, String)> {
        let conn = self.library.db().conn().unwrap();
        let mut stmt = conn.prepare("SELECT id, name FROM media ORDER BY id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn id_of(&self, name: &str) -> i64 {
        self.ids().into_iter().find(|(_, n)| n == name).unwrap().0
    }
}

/// The DIDL-Lite inside a Browse response, unescaped once.
fn didl(soap: &str) -> String {
    let start = soap.find("<Result>").unwrap() + 8;
    let end = soap.find("</Result>").unwrap();
    soap[start..end].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn tag(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let start = xml.find(&open).unwrap_or_else(|| panic!("no <{name}> in {xml}")) + open.len();
    xml[start..].split(&format!("</{name}>")).next().unwrap().to_owned()
}

#[tokio::test]
async fn read_only_and_host_checked() {
    let n = net().await;
    // Nothing from the desktop API exists on the network listener.
    assert_eq!(n.send("POST", "/api/media/trash", &[], r#"{"ids":[1]}"#).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get("/api/timeline").await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.send("DELETE", "/tv/api/home", &[], "").await.0, StatusCode::METHOD_NOT_ALLOWED);

    // Addressed by IP or this PC's name only (blocks DNS rebinding).
    assert_eq!(n.send("GET", "/tv/api/hello", &[("host", "evil.example:8200")], "").await.0, StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(n.send("GET", "/tv/api/hello", &[("host", "192.168.0.100:8200")], "").await.0, StatusCode::OK);
    assert_eq!(n.send("GET", "/tv/api/hello", &[("host", "[::1]:8200")], "").await.0, StatusCode::OK);
    assert_eq!(n.send("GET", "/tv/api/hello", &[], "").await.0, StatusCode::OK, "old TVs may not send Host");
    assert!(super::host_allowed("localhost:8200"));
    assert!(!super::host_allowed("192.168.0.100.evil.example"));
}

#[tokio::test]
async fn serves_the_tv_page() {
    let n = net().await;
    let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist-tv/index.html").exists();
    let (status, headers, body) = n.get("/tv/").await;
    if !built {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        return;
    }
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    assert!(String::from_utf8(body).unwrap().contains("/tv/tv.js"));
    assert_eq!(n.get("/tv/tv.js").await.0, StatusCode::OK);
    assert_eq!(n.get("/tv/nope.js").await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get("/tv").await.0, StatusCode::PERMANENT_REDIRECT);
}

#[tokio::test]
async fn tv_page_pin_and_trash() {
    let n = net().await;
    let (status, hello) = n.json("/tv/api/hello", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hello, json!({"name": "Test PC", "pinRequired": false, "authorized": true}));
    let (_, home) = n.json("/tv/api/home", &[]).await;
    let expected = if n.video { 3 } else { 2 };
    assert_eq!(home["total"], expected);

    let sunset = n.id_of("sunset.jpg");
    let (status, details) = n.json(&format!("/tv/api/media/{sunset}"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((details["video"].as_bool(), details["w"].as_u64()), (Some(false), Some(800)));

    // With a PIN, the data needs the cookie that a correct PIN sets.
    *n.cfg.write().unwrap() = Arc::new(LanConfig { pin: Some("2468".into()), ..config() });
    assert_eq!(n.json("/tv/api/home", &[]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(n.get(&format!("/tv/api/media/{sunset}/photo.jpg")).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(n.json("/tv/api/hello", &[]).await.1["pinRequired"], true);
    let (status, _, _) = n.send("POST", "/tv/api/login", &[], r#"{"pin":"1111"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, headers, _) = n.send("POST", "/tv/api/login", &[], r#"{"pin":"2468"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    assert_eq!(n.json("/tv/api/home", &[("cookie", &cookie)]).await.0, StatusCode::OK);

    // A new PIN logs everyone out.
    *n.cfg.write().unwrap() = Arc::new(LanConfig { pin: Some("1357".into()), ..config() });
    assert_eq!(n.json("/tv/api/home", &[("cookie", &cookie)]).await.0, StatusCode::UNAUTHORIZED);
    *n.cfg.write().unwrap() = Arc::new(config());

    // Trashed items are not shared, by id or in lists.
    photos_core::organize::trash(n.library.db(), &[sunset]).unwrap();
    assert_eq!(n.json(&format!("/tv/api/media/{sunset}"), &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get(&format!("/tv/api/media/{sunset}/thumb.jpg")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get(&format!("/dlna/m/{sunset}/photo.jpg")).await.0, StatusCode::NOT_FOUND);
    let (_, all) = n.json("/tv/api/items?scope=all", &[]).await;
    assert!(all.as_array().unwrap().iter().all(|i| i["id"] != sunset));
    assert_eq!(n.json("/tv/api/items?scope=trash", &[]).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn archive_row_and_folder() {
    let n = net().await;
    let sunset = n.id_of("sunset.jpg");
    let (_, root) = n.browse("0", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(tag(&root, "TotalMatches"), "7", "no Archive folder while it is empty");
    let (_, home) = n.json("/tv/api/home", &[]).await;
    assert_eq!(home["archive"], json!([]));

    photos_core::organize::archive(n.library.db(), &[sunset]).unwrap();
    let in_list = |v: &Value| v.as_array().unwrap().iter().any(|i| i["id"] == sunset);

    // Only in the Archive row/folder.
    let (_, home) = n.json("/tv/api/home", &[]).await;
    assert!(in_list(&home["archive"]));
    assert!(!in_list(&home["recent"]));
    let expected = if n.video { 2 } else { 1 };
    assert_eq!(home["total"], expected);
    let (status, archived) = n.json("/tv/api/items?scope=archive", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(in_list(&archived));
    assert!(!in_list(&n.json("/tv/api/items?scope=all", &[]).await.1));
    assert_eq!(n.json(&format!("/tv/api/media/{sunset}"), &[]).await.0, StatusCode::OK, "opens from the Archive row");

    let (_, root) = n.browse("0", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(tag(&root, "TotalMatches"), "8");
    assert!(didl(&root).contains("<dc:title>Archive</dc:title>"));
    let (_, folder) = n.browse("r", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(tag(&folder, "TotalMatches"), "1");
    assert!(didl(&folder).contains("<dc:title>sunset.jpg</dc:title>"));
    assert_eq!(n.browse(&format!("r|{sunset}"), "BrowseMetadata", 0, 0).await.0, StatusCode::OK);
    assert_eq!(n.browse(&format!("t|{sunset}"), "BrowseMetadata", 0, 0).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    let (_, folders) = n.browse("d", "BrowseDirectChildren", 0, 0).await;
    let folder_id = didl(&folders).split("container id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    assert!(!didl(&n.browse(&folder_id, "BrowseDirectChildren", 0, 0).await.1).contains("sunset.jpg"));
    assert_eq!(n.get(&format!("/dlna/m/{sunset}/photo.jpg")).await.0, StatusCode::OK);

    // Switched off: gone from both, by id too.
    *n.cfg.write().unwrap() = Arc::new(LanConfig { archive: false, ..config() });
    let (_, home) = n.json("/tv/api/home", &[]).await;
    assert!(home.get("archive").is_none());
    assert_eq!(n.json("/tv/api/items?scope=archive", &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.json(&format!("/tv/api/media/{sunset}"), &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get(&format!("/dlna/m/{sunset}/photo.jpg")).await.0, StatusCode::NOT_FOUND);
    let (_, root) = n.browse("0", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(tag(&root, "TotalMatches"), "7");
    assert_eq!(n.browse("r", "BrowseDirectChildren", 0, 0).await.0, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn dlna_description_and_browsing() {
    let n = net().await;
    let (status, headers, body) = n.get("/dlna/description.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/xml"));
    let desc = String::from_utf8(body).unwrap();
    assert!(desc.contains("<friendlyName>Test PC</friendlyName>"));
    assert!(desc.contains("<UDN>uuid:11111111-2222-3333-4444-555555555555</UDN>"));
    assert_eq!(n.get("/dlna/cds.xml").await.0, StatusCode::OK);
    assert_eq!(n.get("/dlna/icon.png").await.0, StatusCode::OK);

    // Root: seven folders.
    let (status, root) = n.browse("0", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tag(&root, "TotalMatches"), "7");
    let root = didl(&root);
    for title in ["Timeline", "Albums", "People", "Places", "Favorites", "Videos", "Folders"] {
        assert!(root.contains(&format!("<dc:title>{title}</dc:title>")), "{title} in {root}");
    }

    // Timeline -> year -> month -> items, with URLs on the address the TV used.
    let (_, years) = n.browse("t", "BrowseDirectChildren", 0, 10).await;
    let years = didl(&years);
    let year_id = years.split("container id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let (_, months) = n.browse(&year_id, "BrowseDirectChildren", 0, 10).await;
    let months = didl(&months);
    let month_id = months.split("container id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let (_, items) = n.browse(&month_id, "BrowseDirectChildren", 0, 50).await;
    let total: usize = tag(&items, "TotalMatches").parse().unwrap();
    assert!(total >= 1);
    let items = didl(&items);
    assert!(items.contains("http://192.168.0.100:8200/dlna/m/"));
    assert!(items.contains("object.item.imageItem.photo"));
    assert!(items.contains("DLNA.ORG_PN=JPEG_TN"));
    // Samsung TVs cache thumbnails by file name, so every item needs its own.
    let thumb_urls: Vec<&str> = items.split("/dlna/m/").skip(1).filter_map(|s| s.split('<').next()).filter(|u| u.contains("/tn-")).collect();
    assert!(!thumb_urls.is_empty());
    for url in &thumb_urls {
        let (id, file) = url.split_once('/').unwrap();
        assert_eq!(file, format!("tn-{id}.jpg"), "file name carries the item id");
        assert_eq!(n.get(&format!("/dlna/m/{url}")).await.0, StatusCode::OK, "{url}");
    }
    let names: std::collections::HashSet<_> = thumb_urls.iter().map(|u| u.split_once('/').unwrap().1).collect();
    assert!(thumb_urls.len() < 2 || names.len() > 1, "thumbnail file names differ between items");

    // Paging: one at a time.
    let (_, first) = n.browse(&month_id, "BrowseDirectChildren", 0, 1).await;
    assert_eq!(tag(&first, "NumberReturned"), "1");
    assert_eq!(tag(&first, "TotalMatches"), total.to_string());

    // Folders on disk, including a subfolder.
    let (_, folders) = n.browse("d", "BrowseDirectChildren", 0, 0).await;
    let folder_id = didl(&folders).split("container id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let (_, inside) = n.browse(&folder_id, "BrowseDirectChildren", 0, 0).await;
    let inside = didl(&inside);
    assert!(inside.contains("<dc:title>Trip</dc:title>") && inside.contains("<dc:title>sunset.jpg</dc:title>"));
    let (_, trip) = n.browse(&format!("{folder_id}:Trip"), "BrowseDirectChildren", 0, 0).await;
    let trip = didl(&trip);
    assert!(trip.contains("<dc:title>pano.tif</dc:title>"));
    // A 5000x1000 TIFF becomes a JPEG that fits 4K.
    assert!(trip.contains("resolution=\"3840x768\""), "{trip}");

    // Metadata of an item and of a container.
    let pano = n.id_of("pano.tif");
    let (status, meta) = n.browse(&format!("{folder_id}:Trip|{pano}"), "BrowseMetadata", 0, 0).await;
    assert_eq!(status, StatusCode::OK);
    assert!(didl(&meta).contains(&format!("parentID=\"{folder_id}:Trip\"")));
    let (_, meta) = n.browse("t", "BrowseMetadata", 0, 0).await;
    assert!(didl(&meta).contains("<dc:title>Timeline</dc:title>"));

    // People: only named, visible people are listed; each opens their photos.
    {
        let conn = n.library.db().conn().unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO persons (id, name, hidden, created_at) VALUES (1, 'Ria', 0, ''), (2, NULL, 0, ''), (3, 'Hid', 1, '');
             INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id) VALUES
               ({pano}, 0.1, 0.1, 0.2, 0.2, 0.9, zeroblob(512), 1),
               ({pano}, 0.5, 0.1, 0.2, 0.2, 0.9, zeroblob(512), 2),
               ({pano}, 0.7, 0.1, 0.2, 0.2, 0.9, zeroblob(512), 3);"
        ))
        .unwrap();
    }
    let (_, people) = n.browse("h", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(tag(&people, "TotalMatches"), "1");
    assert!(didl(&people).contains("<dc:title>Ria</dc:title>"));
    let (_, ria) = n.browse("h:1", "BrowseDirectChildren", 0, 0).await;
    assert!(didl(&ria).contains("<dc:title>pano.tif</dc:title>"));
    assert_eq!(n.browse("h:3", "BrowseDirectChildren", 0, 0).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(n.browse("h:2", "BrowseMetadata", 0, 0).await.0, StatusCode::INTERNAL_SERVER_ERROR);

    // Errors are UPnP faults.
    let (status, missing) = n.browse("a:999", "BrowseDirectChildren", 0, 0).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(missing.contains("<errorCode>701</errorCode>"));
    let (_, _, body) = n
        .send("POST", "/dlna/cds/control", &[], r#"<s:Envelope xmlns:s="x"><s:Body><u:X_GetFeatureList xmlns:u="y"/></s:Body></s:Envelope>"#)
        .await;
    assert!(String::from_utf8(body).unwrap().contains("samsung.com_BASICVIEW"));
}

#[tokio::test]
async fn dlna_media_files() {
    let n = net().await;
    let pano = n.id_of("pano.tif");
    let (status, headers, body) = n.get(&format!("/dlna/m/{pano}/photo.jpg")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers["contentFeatures.dlna.org"].to_str().unwrap().split(';').next(), Some("DLNA.ORG_PN=JPEG_LRG"));
    assert_eq!(headers["transferMode.dlna.org"], "Interactive");
    assert_eq!(headers[header::CONTENT_LENGTH].to_str().unwrap(), body.len().to_string(), "length survives the stop guard");
    let img = image::load_from_memory(&body).unwrap();
    assert_eq!((img.width(), img.height()), (3840, 768));

    let (status, _, small) = n.get(&format!("/dlna/m/{pano}/tn.jpg")).await;
    assert_eq!(status, StatusCode::OK);
    let small = image::load_from_memory(&small).unwrap();
    assert!(small.width() <= 160 && small.height() <= 160);

    // A photo has no "original.*" or stream on DLNA.
    assert_eq!(n.get(&format!("/dlna/m/{pano}/original.tif")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get(&format!("/dlna/m/{pano}/stream.ts")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(n.get(&format!("/dlna/m/{pano}/x")).await.0, StatusCode::NOT_FOUND);

    if !n.video {
        eprintln!("FFmpeg not found; skipping video checks");
        return;
    }
    let clip = n.id_of("clip.mkv");
    // HEVC + Opus in MKV: TVs get the converted stream first.
    let (_, videos) = n.browse("v", "BrowseDirectChildren", 0, 0).await;
    let videos = didl(&videos);
    let converted = videos.find("stream.ts").unwrap();
    let original = videos.find("original.mkv").unwrap();
    assert!(converted < original, "{videos}");
    assert!(videos.contains("video/x-mkv:DLNA.ORG_OP=01") && videos.contains("duration=\"0:00:03."), "{videos}");

    // Original with byte ranges.
    let (status, headers, body) = n
        .send("GET", &format!("/dlna/m/{clip}/original.mkv"), &[("range", "bytes=0-99")], "")
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 100);
    assert_eq!(headers[header::CONTENT_TYPE], "video/x-mkv");

    // HEAD on the converted stream doesn't start FFmpeg.
    let (status, headers, body) = n.send("HEAD", &format!("/dlna/m/{clip}/stream.ts"), &[], "").await;
    assert_eq!((status, body.len()), (StatusCode::OK, 0));
    assert_eq!(headers["contentFeatures.dlna.org"], super::super::dlna::didl::VIDEO_CONVERTED);

    // Time seek: starts 1 s in and says so.
    let (status, headers, body) = n
        .send(
            "GET",
            &format!("/dlna/m/{clip}/stream.ts"),
            &[("TimeSeekRange.dlna.org", "npt=1.0-"), ("getMediaInfo.sec", "1")],
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "video/mpeg");
    let ms = photos_core::media::get(n.library.db(), clip).unwrap().duration_ms.unwrap();
    let total = crate::stream::npt(ms);
    assert_eq!(headers["TimeSeekRange.dlna.org"].to_str().unwrap(), format!("npt=0:00:01.000-{total}/{total}"));
    assert_eq!(headers["MediaInfo.sec"].to_str().unwrap(), format!("SEC_Duration={ms};"));
    assert!(body.len() > 188 * 10 && body[0] == 0x47 && body[188] == 0x47, "an MPEG-TS stream");

    // The TV page gets fragmented MP4.
    let (status, _, body) = n.get(&format!("/tv/api/media/{clip}/stream.mp4")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[4..8], b"ftyp");
    let (_, details) = n.json(&format!("/tv/api/media/{clip}"), &[]).await;
    assert_eq!((details["videoCodec"].as_str(), details["audioCodec"].as_str()), (Some("hevc"), Some("opus")));
}

#[tokio::test]
async fn memories_row() {
    let n = net().await;
    let (_, home) = n.json("/tv/api/home", &[]).await;
    assert_eq!(home["memories"], json!([]));

    // Four photos from a year ago today (rows only; the rules don't read the files).
    let day = chrono::Local::now().date_naive() - chrono::Duration::days(365);
    let ids: Vec<i64> = {
        let conn = n.library.db().conn().unwrap();
        (0..4)
            .map(|i| {
                conn.execute(
                    "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, width, height, taken_at, phash, stage, added_at)
                     SELECT id, path || '\\old' || ?1 || '.jpg', 'old.jpg', 1, 1000, 'photo', 'image/jpeg', 800, 600, ?2, ?3, 3, ''
                     FROM folders LIMIT 1",
                    (i, format!("{day}T1{i}:00:00"), i as i64 * 0x0F0F_0F0F_0F0F_i64),
                )
                .unwrap();
                conn.last_insert_rowid()
            })
            .collect()
    };
    crate::memories::rebuild(&AppState::new(n.library.clone())).unwrap();
    let (_, home) = n.json("/tv/api/home", &[]).await;
    let row = home["memories"].as_array().unwrap();
    assert_eq!(row.len(), 1, "{home}");
    let id = row[0]["id"].as_i64().unwrap();
    let (status, items) = n.json(&format!("/tv/api/items?scope=memory&id={id}"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(items.as_array().unwrap().len(), 4);

    // An archived photo leaves the memory on the TV too.
    photos_core::organize::archive(n.library.db(), &[ids[0]]).unwrap();
    let (_, items) = n.json(&format!("/tv/api/items?scope=memory&id={id}"), &[]).await;
    assert_eq!(items.as_array().unwrap().len(), 3);

    *n.cfg.write().unwrap() = Arc::new(LanConfig { memories: false, ..config() });
    let (_, home) = n.json("/tv/api/home", &[]).await;
    assert_eq!(home["memories"], json!([]));
    assert_eq!(n.json(&format!("/tv/api/items?scope=memory&id={id}"), &[]).await.0, StatusCode::NOT_FOUND);
}
