//! The locked folder's API. Only on the localhost listener (never routed on the network),
//! behind the per-launch token, and every answer is `Cache-Control: no-store` so the webview
//! never writes a decrypted picture or name to its disk cache. Answers 423 while locked.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::vault::{self, Item, Moved, Part, State as VaultState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::api::blocking;
use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/vault", get(state))
        .route("/vault/setup", post(setup))
        .route("/vault/unlock", post(unlock))
        .route("/vault/recover", post(recover))
        .route("/vault/lock", post(lock))
        .route("/vault/password", post(change_password))
        .route("/vault/recovery-key", post(new_recovery_key))
        .route("/vault/items", get(items))
        .route("/vault/items/{id}", get(item))
        .route("/vault/items/{id}/thumb", get(thumb))
        .route("/vault/items/{id}/display", get(display))
        .route("/vault/items/{id}/original", get(original))
        .route("/vault/move-in", post(move_in))
        .route("/vault/move-out", post(move_out))
        .route("/vault/delete", post(delete))
        .route("/vault/favorite", post(favorite))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// Lock the locked folder when it hasn't been used for the minutes set in Settings.
/// Runs until the process exits.
pub fn spawn_auto_lock(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let lib = state.library.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let minutes = photos_core::settings::load(lib.db()).map(|s| s.vault_lock_minutes).unwrap_or(5);
                if lib.vault().lock_if_idle(Duration::from_secs(u64::from(minutes) * 60)) {
                    tracing::info!("locked folder locked after {minutes} idle minutes");
                }
            })
            .await;
        }
    });
}

async fn state(State(s): State<AppState>) -> Json<VaultState> {
    Json(s.library.vault().state())
}

#[derive(Deserialize)]
struct Password {
    password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryKey {
    recovery_key: String,
}

async fn setup(State(s): State<AppState>, Json(body): Json<Password>) -> ApiResult<RecoveryKey> {
    let key = blocking(move || s.library.vault().setup(&body.password)).await?;
    Ok(Json(RecoveryKey { recovery_key: key.to_string() }))
}

async fn unlock(State(s): State<AppState>, Json(body): Json<Password>) -> ApiResult<VaultState> {
    let lib = s.library.clone();
    blocking(move || lib.vault().unlock(&body.password)).await?;
    relocate_later(&s);
    Ok(Json(s.library.vault().state()))
}

/// After unlocking: items still kept in the data folder move next to their photos (a locked
/// folder from before that, or a watched folder added since). In the background.
fn relocate_later(s: &AppState) {
    let lib = s.library.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = lib.vault().relocate() {
            tracing::warn!("locked items not moved into their folders: {e}");
        }
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recover {
    recovery_key: String,
    password: String,
}

/// Forgot the password: unlock with the recovery key and choose a new password.
async fn recover(State(s): State<AppState>, Json(body): Json<Recover>) -> ApiResult<VaultState> {
    let lib = s.library.clone();
    blocking(move || lib.vault().recover(&body.recovery_key, &body.password)).await?;
    relocate_later(&s);
    Ok(Json(s.library.vault().state()))
}

async fn lock(State(s): State<AppState>) -> Json<VaultState> {
    s.library.vault().lock();
    Json(s.library.vault().state())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePassword {
    old: String,
    new: String,
}

async fn change_password(State(s): State<AppState>, Json(body): Json<ChangePassword>) -> Result<StatusCode, ApiError> {
    blocking(move || s.library.vault().change_password(&body.old, &body.new)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A new recovery key; the old one stops working.
async fn new_recovery_key(State(s): State<AppState>, Json(body): Json<Password>) -> ApiResult<RecoveryKey> {
    let key = blocking(move || s.library.vault().new_recovery_key(&body.password)).await?;
    Ok(Json(RecoveryKey { recovery_key: key.to_string() }))
}

async fn items(State(s): State<AppState>) -> ApiResult<Vec<Item>> {
    Ok(Json(blocking(move || s.library.vault().list()).await?))
}

async fn item(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Item> {
    Ok(Json(blocking(move || s.library.vault().get(&id)).await?))
}

async fn thumb(State(s): State<AppState>, Path(id): Path<String>, req: Request) -> Result<Response, ApiError> {
    serve_part(s, id, Part::Thumb, req.headers()).await
}

async fn display(State(s): State<AppState>, Path(id): Path<String>, req: Request) -> Result<Response, ApiError> {
    serve_part(s, id, Part::Display, req.headers()).await
}

async fn original(State(s): State<AppState>, Path(id): Path<String>, req: Request) -> Result<Response, ApiError> {
    serve_part(s, id, Part::Original, req.headers()).await
}

/// One `bytes=` range out of `len`. None: no (usable) Range header, send it all.
/// `Err(())`: a range that can't be satisfied (416).
fn parse_range(headers: &HeaderMap, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = headers.get(header::RANGE)?.to_str().ok()?.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // several ranges: send the whole file (allowed by RFC 9110)
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return Some(Err(()));
        }
        (len.saturating_sub(n), len.checked_sub(1)?)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { len.saturating_sub(1) } else { b.parse::<u64>().ok()?.min(len.saturating_sub(1)) };
        if start >= len || end < start {
            return Some(Err(()));
        }
        (start, end)
    };
    Some(Ok(range))
}

/// Decrypted bytes come from a blocking thread through a small channel.
struct ChannelBody(mpsc::Receiver<std::io::Result<Bytes>>);

impl futures_core::Stream for ChannelBody {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// Serve an item's file decrypted on the fly, with HTTP Range support (video seeking).
async fn serve_part(s: AppState, id: String, part: Part, headers: &HeaderMap) -> Result<Response, ApiError> {
    let lib = s.library.clone();
    let (mut reader, mime) = blocking(move || lib.vault().open_part(&id, part)).await?;
    let len = reader.len();
    let mut res = Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store");
    let (start, end) = match parse_range(headers, len) {
        Some(Ok((a, b))) => {
            res = res.status(StatusCode::PARTIAL_CONTENT).header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{len}"));
            (a, b + 1)
        }
        Some(Err(())) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::empty())
                .map_err(|e| ApiError::Internal(e.to_string()));
        }
        None => (0, len),
    };
    res = res.header(header::CONTENT_LENGTH, end - start);

    let (tx, rx) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        let result = (|| -> std::io::Result<()> {
            reader.seek(SeekFrom::Start(start))?;
            let mut left = end - start;
            while left > 0 {
                let mut buf = vec![0u8; (left as usize).min(photos_core::vault::crypto::CHUNK)];
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                buf.truncate(n);
                left -= n as u64;
                if tx.blocking_send(Ok(Bytes::from(buf))).is_err() {
                    return Ok(()); // the client went away
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("locked folder file could not be read: {e}");
            let _ = tx.blocking_send(Err(e));
        }
    });
    res.body(Body::from_stream(ChannelBody(rx))).map_err(|e| ApiError::Internal(e.to_string()))
}

#[derive(Deserialize)]
struct MediaIds {
    ids: Vec<i64>,
}

async fn move_in(State(s): State<AppState>, Json(body): Json<MediaIds>) -> ApiResult<Moved> {
    Ok(Json(blocking(move || s.library.move_to_vault(&body.ids)).await?))
}

#[derive(Deserialize)]
struct MoveOut {
    ids: Vec<String>,
    /// Folder to put the files in.
    path: PathBuf,
}

async fn move_out(State(s): State<AppState>, Json(body): Json<MoveOut>) -> ApiResult<Moved> {
    let lib = s.library.clone();
    let report = blocking(move || vault::move_out(lib.vault(), &body.ids, &body.path)).await?;
    if report.moved > 0 {
        // A watched folder: pick the files up right away.
        s.library.rescan();
    }
    Ok(Json(report))
}

#[derive(Deserialize)]
struct VaultIds {
    ids: Vec<String>,
}

/// Delete for good (there is no trash in the locked folder).
async fn delete(State(s): State<AppState>, Json(body): Json<VaultIds>) -> ApiResult<Value> {
    let n = blocking(move || s.library.vault().delete(&body.ids)).await?;
    Ok(Json(json!({ "deleted": n })))
}

#[derive(Deserialize)]
struct Favorite {
    ids: Vec<String>,
    favorite: bool,
}

async fn favorite(State(s): State<AppState>, Json(body): Json<Favorite>) -> ApiResult<Value> {
    let n = blocking(move || s.library.vault().set_favorite(&body.ids, body.favorite)).await?;
    Ok(Json(json!({ "changed": n })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::RANGE, HeaderValue::from_str(v).unwrap());
            m
        };
        assert_eq!(parse_range(&HeaderMap::new(), 100), None);
        assert_eq!(parse_range(&h("bytes=0-9"), 100), Some(Ok((0, 9))));
        assert_eq!(parse_range(&h("bytes=90-"), 100), Some(Ok((90, 99))));
        assert_eq!(parse_range(&h("bytes=90-500"), 100), Some(Ok((90, 99))));
        assert_eq!(parse_range(&h("bytes=-10"), 100), Some(Ok((90, 99))));
        assert_eq!(parse_range(&h("bytes=-500"), 100), Some(Ok((0, 99))));
        assert_eq!(parse_range(&h("bytes=100-"), 100), Some(Err(())));
        assert_eq!(parse_range(&h("bytes=5-2"), 100), Some(Err(())));
        assert_eq!(parse_range(&h("bytes=0-1,5-6"), 100), None);
        assert_eq!(parse_range(&h("items=0-1"), 100), None);
    }
}
