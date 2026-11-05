//! The TV web page (`/tv`): a page for TV browsers and phones, plus the read-only API it
//! uses. With a PIN set, everything but the page itself, `hello` and `login` needs the PIN
//! cookie.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::albums::{self, Album};
use photos_core::media::{self, Kind, Scope, TimelineItem};
use photos_core::places::{self, Place};
use photos_core::transcode::Container;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::files::{self, File};
use super::{LanState, client_ip};
use crate::ApiError;
use crate::api::blocking;

/// Built by `npm run build` in `ui/` (`vite.tv.config.ts`). In debug builds the files are
/// read from disk, so rebuilding the page needs no Rust rebuild.
#[derive(rust_embed::Embed)]
/// The folder is relative to `crates/server/Cargo.toml`; `build.rs` makes sure it exists.
#[folder = "../../ui/dist-tv/"]
struct Assets;

const COOKIE: &str = "lp_tv";

pub fn routes(state: LanState) -> Router<LanState> {
    let api = Router::new()
        .route("/home", get(home))
        .route("/items", get(items))
        .route("/media/{id}", get(details))
        .route("/media/{id}/{file}", get(media_file))
        .route_layer(middleware::from_fn_with_state(state, require_pin))
        .route("/hello", get(hello))
        .route("/login", post(login));
    Router::new()
        .nest("/tv/api", api)
        .route("/tv", get(|| async { Redirect::permanent("/tv/") }))
        .route("/tv/", get(|| asset("index.html")))
        .route("/tv/{*path}", get(|Path(path): Path<String>| asset(path)))
}

async fn asset(path: impl AsRef<str>) -> Response {
    let path = path.as_ref();
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_of(path);
            // The files are small; always fetching them means an updated app never leaves a
            // TV running an old page.
            (
                [(header::CONTENT_TYPE, mime.to_owned()), (header::CACHE_CONTROL, "no-cache".to_owned())],
                file.data,
            )
                .into_response()
        }
        None if path == "index.html" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The TV page isn't built. Run `npm run build` in the ui folder.",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

// ---------- PIN ----------

/// Cookie value for a PIN: a keyed hash, so it can't be turned back into the PIN, survives
/// restarts, and stops working when the PIN changes.
fn pin_cookie(secret: &str, pin: &str) -> String {
    let key = blake3::hash(secret.as_bytes());
    blake3::keyed_hash(key.as_bytes(), pin.as_bytes()).to_hex().to_string()
}

fn cookie_value<'a>(req_headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    req_headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(name)?.strip_prefix('='))
}

fn authorized(s: &LanState, headers: &axum::http::HeaderMap) -> bool {
    let cfg = s.cfg();
    match &cfg.pin {
        None => true,
        Some(pin) => cookie_value(headers, COOKIE).is_some_and(|c| {
            crate::auth::constant_time_eq(c.as_bytes(), pin_cookie(&cfg.secret, pin).as_bytes())
        }),
    }
}

async fn require_pin(State(s): State<LanState>, req: Request, next: Next) -> Response {
    if authorized(&s, req.headers()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({ "error": "PIN required" }))).into_response()
    }
}

/// Slows down PIN guessing: after 5 wrong PINs from an address, it must wait a minute.
#[derive(Default)]
pub struct PinGuard {
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

const MAX_TRIES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

impl PinGuard {
    /// Seconds to wait before another try, if locked out.
    fn locked(&self, ip: IpAddr) -> Option<u64> {
        let map = self.failures.lock().unwrap();
        let (n, last) = map.get(&ip)?;
        let left = LOCKOUT.checked_sub(last.elapsed())?;
        (*n >= MAX_TRIES).then(|| left.as_secs() + 1)
    }

    fn failed(&self, ip: IpAddr) {
        let mut map = self.failures.lock().unwrap();
        let entry = map.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() > LOCKOUT {
            entry.0 = 0;
        }
        entry.0 += 1;
        entry.1 = Instant::now();
    }

    fn succeeded(&self, ip: IpAddr) {
        self.failures.lock().unwrap().remove(&ip);
    }
}

async fn hello(State(s): State<LanState>, headers: axum::http::HeaderMap) -> Json<Value> {
    let cfg = s.cfg();
    Json(json!({
        "name": cfg.device_name,
        "pinRequired": cfg.pin.is_some(),
        "authorized": authorized(&s, &headers),
    }))
}

#[derive(Deserialize)]
struct Login {
    pin: String,
}

async fn login(State(s): State<LanState>, req: Request) -> Response {
    let ip = client_ip(&req);
    if let Some(wait) = s.pin_guard.locked(ip) {
        let msg = format!("Too many wrong PINs. Try again in {wait} seconds.");
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": msg }))).into_response();
    }
    let body = axum::body::to_bytes(req.into_body(), 4096).await.unwrap_or_default();
    let Ok(Login { pin }) = serde_json::from_slice::<Login>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Send the PIN as JSON" }))).into_response();
    };
    let cfg = s.cfg();
    let Some(expected) = &cfg.pin else { return StatusCode::NO_CONTENT.into_response() };
    if !crate::auth::constant_time_eq(pin.trim().as_bytes(), expected.as_bytes()) {
        s.pin_guard.failed(ip);
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Wrong PIN" }))).into_response();
    }
    s.pin_guard.succeeded(ip);
    let cookie = format!(
        "{COOKIE}={}; Path=/tv; Max-Age=31536000; HttpOnly; SameSite=Strict",
        pin_cookie(&cfg.secret, expected)
    );
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).expect("cookie is ASCII"));
    res
}

// ---------- data ----------

/// Everything the home screen shows, in one request.
#[derive(Serialize)]
struct Home {
    name: String,
    total: usize,
    recent: Vec<TimelineItem>,
    favorites: Vec<TimelineItem>,
    videos: Vec<TimelineItem>,
    albums: Vec<Album>,
    places: Vec<TvPlace>,
    transcode: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TvPlace {
    #[serde(flatten)]
    place: Place,
    /// TV browsers may not know country names (`Intl.DisplayNames`), so we send them.
    country_name: String,
}

const ROW: usize = 30;

async fn home(State(s): State<LanState>) -> Result<Json<Home>, ApiError> {
    let cfg = s.cfg();
    let db = s.app.library.db().clone();
    let home = blocking(move || {
        let all = media::query(&db, &media::Query::default())?;
        let favorites = media::query(&db, &media::Query { scope: Scope::Favorites, ..Default::default() })?;
        let videos: Vec<TimelineItem> = all.iter().filter(|i| i.v).take(ROW).cloned().collect();
        let mut places = places::list(&db)?;
        places.sort_by(|a, b| b.count.cmp(&a.count));
        places.truncate(ROW);
        Ok(Home {
            name: cfg.device_name.clone(),
            total: all.len(),
            recent: all.into_iter().take(ROW).collect(),
            favorites: favorites.into_iter().take(ROW).collect(),
            videos,
            albums: albums::list(&db)?.into_iter().filter(|a| a.count > 0).collect(),
            places: places
                .into_iter()
                .map(|p| TvPlace { country_name: crate::dlna::countries::name(&p.country).to_owned(), place: p })
                .collect(),
            transcode: cfg.transcode,
        })
    })
    .await?;
    Ok(Json(home))
}

#[derive(Deserialize)]
struct ItemsQuery {
    /// `all`, `favorites`, `videos`, `album` or `place`.
    scope: String,
    id: Option<i64>,
}

async fn items(State(s): State<LanState>, Query(q): Query<ItemsQuery>) -> Result<Json<Vec<TimelineItem>>, ApiError> {
    let mut query = media::Query::default();
    match q.scope.as_str() {
        "all" => {}
        "favorites" => query.scope = Scope::Favorites,
        "videos" => query.kind = Some("video".into()),
        "album" => {
            query.scope = Scope::Album;
            query.album = Some(q.id.ok_or_else(|| ApiError::BadRequest("album id missing".into()))?);
        }
        "place" => query.place = Some(q.id.ok_or_else(|| ApiError::BadRequest("place id missing".into()))?),
        other => return Err(ApiError::BadRequest(format!("unknown list: {other}"))),
    }
    let db = s.app.library.db().clone();
    Ok(Json(blocking(move || media::query(&db, &query)).await?))
}

/// What the viewer needs about one item.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Details {
    id: i64,
    name: String,
    video: bool,
    taken_at: String,
    w: Option<u32>,
    h: Option<u32>,
    duration_ms: Option<i64>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    mime: String,
    /// Converted playback is available (setting on).
    can_convert: bool,
}

async fn details(State(s): State<LanState>, Path(id): Path<i64>) -> Result<Json<Details>, ApiError> {
    let m = files::visible(&s, id).await?;
    Ok(Json(Details {
        id: m.id,
        name: m.name,
        video: m.kind == Kind::Video,
        taken_at: m.taken_at,
        w: m.width,
        h: m.height,
        duration_ms: m.duration_ms,
        video_codec: m.video_codec,
        audio_codec: m.audio_codec,
        mime: m.mime,
        can_convert: s.cfg().transcode,
    }))
}

#[derive(Deserialize)]
struct StreamQuery {
    /// Start time in seconds.
    t: Option<f64>,
}

async fn media_file(
    State(s): State<LanState>,
    Path((id, file)): Path<(i64, String)>,
    Query(q): Query<StreamQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    let file = match file.as_str() {
        "thumb.jpg" => File::Thumb,
        "photo.jpg" => File::Photo,
        "original" => File::Original,
        "stream.mp4" => File::Converted(Container::Mp4),
        _ => return Err(photos_core::Error::NotFound.into()),
    };
    let start = q.t.filter(|t| t.is_finite() && *t > 0.0).unwrap_or(0.0);
    files::send(&s, id, file, start, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_cookie_depends_on_pin_and_secret() {
        let a = pin_cookie("secret", "1234");
        assert_eq!(a.len(), 64);
        assert_eq!(a, pin_cookie("secret", "1234"));
        assert_ne!(a, pin_cookie("secret", "1235"));
        assert_ne!(a, pin_cookie("other", "1234"));
    }

    #[test]
    fn reads_cookies() {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; lp_tv=abc; lp_tv_x=2"));
        assert_eq!(cookie_value(&h, COOKIE), Some("abc"));
        assert_eq!(cookie_value(&h, "b"), None);
    }

    #[test]
    fn lockout_after_five_wrong_pins() {
        let g = PinGuard::default();
        let ip: IpAddr = "192.168.0.5".parse().unwrap();
        for _ in 0..4 {
            g.failed(ip);
        }
        assert_eq!(g.locked(ip), None);
        g.failed(ip);
        assert!(g.locked(ip).is_some_and(|s| s <= 60));
        assert_eq!(g.locked("192.168.0.6".parse().unwrap()), None);
        g.succeeded(ip);
        assert_eq!(g.locked(ip), None);
    }
}
