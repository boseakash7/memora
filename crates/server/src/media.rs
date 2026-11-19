//! Timeline data and media files (thumbnails, display copies, originals).

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::media::{self, Media, TimelineItem};
use photos_core::thumbs;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::api::blocking;
use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/timeline", get(timeline))
        .route("/media/{id}", get(details))
        .route("/media/paths", post(paths))
        .route("/media/{id}/info", get(info))
        .route("/media/{id}/thumb", get(thumb))
        .route("/media/{id}/display", get(display))
        .route("/media/{id}/original", get(original))
        .route("/media/{id}/stream.mp4", get(stream_mp4))
        .route("/media/{id}/storyboard", get(storyboard))
        .route("/media/{id}/frame", get(frame))
}

/// Items for a grid. See `media::Query` for the filters (`scope`, `album`, `q`, `from`, ...).
/// Search text also finds people by name and pictures by what they show.
async fn timeline(
    State(s): State<AppState>,
    Query(mut q): Query<media::Query>,
) -> Result<Json<Vec<TimelineItem>>, ApiError> {
    let items = blocking(move || {
        let db = s.library.db();
        if let Some(text) = q.q.clone().filter(|t| !t.trim().is_empty()) {
            q.also = photos_core::people::media_named(db, &text)?;
            if let Some(ai) = &s.ai {
                match ai.search(&text) {
                    Ok(hits) => q.also.extend(hits.into_iter().map(|(id, _)| id)),
                    Err(e) => tracing::warn!("search by content failed: {e}"),
                }
            }
        }
        media::query(db, &q)
    })
    .await?;
    Ok(Json(items))
}

async fn details(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Media>, ApiError> {
    Ok(Json(blocking(move || media::get(s.library.db(), id)).await?))
}

#[derive(serde::Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

/// File paths of several items (for "Copy file paths").
async fn paths(State(s): State<AppState>, Json(body): Json<Ids>) -> Result<Json<Vec<PathBuf>>, ApiError> {
    Ok(Json(blocking(move || media::paths(s.library.db(), &body.ids)).await?))
}

/// Exposure settings, video codec, place name and the like, read from the file on request.
async fn info(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<media::Info>, ApiError> {
    Ok(Json(blocking(move || media::info(s.library.db(), id)).await?))
}

/// Grid thumbnail, generated on demand if the background pipeline hasn't got to it yet.
/// URLs carry the file's mtime (`?v=`), so the browser may cache them for good.
async fn thumb(State(s): State<AppState>, Path(id): Path<i64>, req: Request) -> Result<Response, ApiError> {
    let _slot = s.thumb_slots.acquire().await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let (lib, st) = (s.library.clone(), s.clone());
    let path = blocking(move || {
        let m = media::get(lib.db(), id)?;
        crate::edit::prepare_mask(&st, &m);
        thumbs::ensure_thumb(lib.paths(), &m)
    })
    .await?;
    serve(path, Some("image/jpeg"), "private, max-age=31536000, immutable", req).await
}

/// What the viewer shows: the original, or a JPEG copy for formats the webview can't decode.
async fn display(State(s): State<AppState>, Path(id): Path<i64>, req: Request) -> Result<Response, ApiError> {
    let lib = s.library.clone();
    let (path, mime) = blocking(move || {
        let m = media::get(lib.db(), id)?;
        crate::edit::prepare_mask(&s, &m);
        let path = thumbs::ensure_display(lib.paths(), &m)?;
        let mime = if path == m.path { m.mime } else { "image/jpeg".into() };
        Ok((path, mime))
    })
    .await?;
    serve(path, Some(&mime), "private, no-cache", req).await
}

/// The original file, with HTTP range support for video seeking.
async fn original(State(s): State<AppState>, Path(id): Path<i64>, req: Request) -> Result<Response, ApiError> {
    let m = blocking(move || media::get(s.library.db(), id)).await?;
    serve(m.path, Some(&m.mime), "private, no-cache", req).await
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    /// Start time in seconds.
    t: Option<f64>,
}

/// A video converted to H.264 for when the webview can't play the original (e.g. HEVC
/// without the Windows extension). Seeking means requesting again with `?t=`.
async fn stream_mp4(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<StreamQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    let m = blocking(move || media::get(s.library.db(), id)).await?;
    if m.kind != media::Kind::Video {
        return Err(photos_core::Error::NotFound.into());
    }
    let start = q.t.filter(|t| t.is_finite() && *t > 0.0).unwrap_or(0.0);
    s.streams.start(m, photos_core::transcode::Container::Mp4, start, crate::lan::client_ip(&req)).await
}

/// Small frames of a video in one JPEG, for the viewer's seek-bar preview. Made on first
/// request and cached. `X-Storyboard` gives the layout: `frames=82;interval=0.9938;cols=10;rows=9`
/// (tile `i` shows the video at `i * interval` seconds).
async fn storyboard(State(s): State<AppState>, Path(id): Path<i64>, req: Request) -> Result<Response, ApiError> {
    let _slot = s.thumb_slots.acquire().await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let lib = s.library.clone();
    let (path, layout) = blocking(move || {
        let m = media::get(lib.db(), id)?;
        thumbs::ensure_storyboard(lib.paths(), &m)
    })
    .await?;
    let mut res = serve(path, Some("image/jpeg"), "private, max-age=31536000, immutable", req).await?;
    let value = format!(
        "frames={};interval={:.6};cols={};rows={}",
        layout.frames, layout.interval, layout.cols, layout.rows
    );
    res.headers_mut()
        .insert(STORYBOARD_HEADER, HeaderValue::from_str(&value).map_err(|e| ApiError::Internal(e.to_string()))?);
    Ok(res)
}

pub(crate) const STORYBOARD_HEADER: &str = "x-storyboard";

#[derive(serde::Deserialize)]
struct FrameQuery {
    /// Time in seconds.
    t: f64,
}

/// The video frame at `?t=` seconds as a full-size PNG ("Copy this frame").
async fn frame(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<FrameQuery>,
) -> Result<Response, ApiError> {
    let t = if q.t.is_finite() { q.t.max(0.0) } else { 0.0 };
    let _slot = s.thumb_slots.acquire().await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let png = blocking(move || {
        let m = media::get(s.library.db(), id)?;
        thumbs::video_frame_png(&m, t)
    })
    .await?;
    let mut res = Response::new(Body::from(png));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

pub(crate) async fn serve(path: PathBuf, mime: Option<&str>, cache: &str, req: Request) -> Result<Response, ApiError> {
    let res = ServeFile::new(&path).oneshot(req).await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut res = res.map(Body::new);
    if res.status().is_success() {
        if let Some(mime) = mime.and_then(|m| HeaderValue::from_str(m).ok()) {
            res.headers_mut().insert(header::CONTENT_TYPE, mime);
        }
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_str(cache).map_err(|e| ApiError::Internal(e.to_string()))?);
    }
    Ok(res)
}
