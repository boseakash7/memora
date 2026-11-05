use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use photos_core::Status;
use photos_core::folders::{self, Folder};
use photos_core::settings::{self, Settings};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ApiError, AppState, auth, explore, media, organize};

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/folders", get(list_folders).post(add_folder))
        .route("/folders/{id}", delete(remove_folder))
        .route("/settings", get(get_settings).patch(update_settings))
        .route("/library/status", get(library_status))
        .route("/library/rescan", post(rescan))
        .route("/lan", get(lan_status))
        .route("/lan/qr.svg", get(lan_qr))
        .merge(media::routes())
        .merge(organize::routes())
        .merge(explore::routes())
        .route_layer(middleware::from_fn_with_state(state, auth::require_token))
        // Health check stays public so the UI can tell "server down" from "bad token".
        .route("/health", get(|| async { Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") })) }))
}

/// Database and file work is blocking; keep it off the async worker threads.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> photos_core::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(ApiError::from)
}

async fn list_folders(State(s): State<AppState>) -> ApiResult<Vec<Folder>> {
    Ok(Json(blocking(move || folders::list(s.library.db())).await?))
}

#[derive(Deserialize)]
struct AddFolder {
    path: PathBuf,
}

/// Adding a folder starts scanning it in the background.
async fn add_folder(
    State(s): State<AppState>,
    Json(body): Json<AddFolder>,
) -> Result<(StatusCode, Json<Folder>), ApiError> {
    let folder = blocking(move || s.library.add_folder(&body.path)).await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

async fn remove_folder(State(s): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    blocking(move || s.library.remove_folder(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(State(s): State<AppState>) -> ApiResult<Settings> {
    Ok(Json(blocking(move || settings::load(s.library.db())).await?))
}

async fn update_settings(State(s): State<AppState>, Json(patch): Json<Value>) -> ApiResult<Settings> {
    let lib = s.library.clone();
    let updated = blocking(move || settings::update(lib.db(), &patch)).await?;
    // Start, stop or reconfigure network sharing to match.
    s.lan.apply(&s).await;
    Ok(Json(updated))
}

async fn lan_status(State(s): State<AppState>) -> Json<crate::lan::LanStatus> {
    Json(s.lan.status().await)
}

#[derive(Deserialize)]
struct QrQuery {
    text: String,
}

/// QR code (SVG) for the TV page's address, so a phone can open it.
async fn lan_qr(axum::extract::Query(q): axum::extract::Query<QrQuery>) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    if q.text.len() > 200 {
        return Err(ApiError::BadRequest("text too long".into()));
    }
    let code = qrcode::QrCode::new(q.text.as_bytes()).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build();
    Ok(([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], svg).into_response())
}

async fn library_status(State(s): State<AppState>) -> Json<Status> {
    Json(s.library.status())
}

async fn rescan(State(s): State<AppState>) -> StatusCode {
    s.library.rescan();
    StatusCode::ACCEPTED
}
