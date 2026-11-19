use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use photos_core::Status;
use photos_core::folders::{self, Folder};
use photos_core::settings::{self, Settings};
use photos_core::{backup, sync};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, AppState, auth, explore, media, organize, people};

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/folders", get(list_folders).post(add_folder))
        .route("/folders/probe", post(probe_folder))
        .route("/folders/{id}", delete(remove_folder).patch(update_folder))
        .route("/folders/{id}/locate", post(locate_folder))
        .route("/folders/{id}/info", delete(remove_folder_info))
        .route("/library/sync", post(sync_now))
        .route("/backup", post(write_backup))
        .route("/backup/read", post(read_backup))
        .route("/backup/restore", post(restore_backup))
        .route("/settings", get(get_settings).patch(update_settings))
        .route("/library/status", get(library_status))
        .route("/library/rescan", post(rescan))
        .route("/lan", get(lan_status))
        .route("/lan/qr.svg", get(lan_qr))
        .merge(media::routes())
        .merge(organize::routes())
        .merge(explore::routes())
        .merge(people::routes())
        .merge(crate::vault::routes())
        .merge(crate::memories::routes())
        .merge(crate::edit::routes())
        .merge(crate::models::routes())
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

/// A watched folder, and how its library info is kept (`.memora`) when it is.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderView {
    #[serde(flatten)]
    folder: Folder,
    library_info: Option<sync::Status>,
}

fn view(db: &photos_core::Db, folder: Folder) -> photos_core::Result<FolderView> {
    let library_info = sync::status(db, &folder)?;
    Ok(FolderView { folder, library_info })
}

async fn list_folders(State(s): State<AppState>) -> ApiResult<Vec<FolderView>> {
    Ok(Json(
        blocking(move || {
            let db = s.library.db();
            folders::list(db)?.into_iter().map(|f| view(db, f)).collect()
        })
        .await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddFolder {
    path: PathBuf,
    /// Keep the library info in a `.memora` folder inside it too.
    #[serde(default)]
    keep_info: bool,
}

/// Adding a folder starts scanning it in the background.
async fn add_folder(
    State(s): State<AppState>,
    Json(body): Json<AddFolder>,
) -> Result<(StatusCode, Json<FolderView>), ApiError> {
    let lib = s.library;
    let folder = blocking(move || {
        let folder = lib.add_folder(&body.path, body.keep_info)?;
        view(lib.db(), folder)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

#[derive(Deserialize)]
struct ProbeFolder {
    path: PathBuf,
}

/// Before adding: does the folder already have library info from other PCs?
async fn probe_folder(Json(body): Json<ProbeFolder>) -> ApiResult<sync::Probe> {
    Ok(Json(blocking(move || Ok(sync::probe(&body.path))).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FolderPatch {
    keep_info: Option<bool>,
    /// While keeping library info: the finished index too (on by default).
    keep_index: Option<bool>,
}

async fn update_folder(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<FolderPatch>) -> ApiResult<FolderView> {
    let lib = s.library;
    Ok(Json(
        blocking(move || {
            let mut folder = folders::get(lib.db(), id)?;
            if let Some(keep) = body.keep_info {
                folder = lib.set_keep_info(id, keep)?;
            }
            if let Some(keep) = body.keep_index {
                folder = lib.set_keep_index(id, keep)?;
            }
            view(lib.db(), folder)
        })
        .await?,
    ))
}

/// Stop keeping library info in the folder and move its `.memora` to the Recycle Bin.
async fn remove_folder_info(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<FolderView> {
    let lib = s.library;
    Ok(Json(
        blocking(move || {
            let folder = lib.remove_folder_info(id)?;
            view(lib.db(), folder)
        })
        .await?,
    ))
}

/// Write a backup of everything the user made (favorites, albums, people...) to `path`.
async fn write_backup(State(s): State<AppState>, Json(body): Json<ProbeFolder>) -> ApiResult<backup::Summary> {
    Ok(Json(blocking(move || s.library.write_backup(&body.path)).await?))
}

/// What a backup file holds, before restoring it.
async fn read_backup(Json(body): Json<ProbeFolder>) -> ApiResult<backup::Summary> {
    Ok(Json(blocking(move || Ok(backup::Summary::of(&backup::read(&body.path)?))).await?))
}

#[derive(Deserialize)]
struct RestoreBackup {
    path: PathBuf,
    /// Restore the settings (theme, TV sharing...) too.
    #[serde(default)]
    settings: bool,
}

async fn restore_backup(State(s): State<AppState>, Json(body): Json<RestoreBackup>) -> ApiResult<backup::Restored> {
    let lib = s.library.clone();
    let restored = blocking(move || lib.restore_backup(&body.path, body.settings)).await?;
    // Settings may have switched network sharing or recognition.
    if restored.settings {
        s.lan.apply(&s).await;
        if let Some(ai) = &s.ai {
            ai.refresh();
        }
    }
    Ok(Json(restored))
}

/// Write and read library info now instead of waiting for the next round.
async fn sync_now(State(s): State<AppState>) -> ApiResult<Value> {
    let changed = blocking(move || s.library.sync_now()).await?;
    Ok(Json(json!({ "changed": changed })))
}

/// The folder moved or its drive letter changed: point it at its new place.
async fn locate_folder(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<ProbeFolder>,
) -> ApiResult<FolderView> {
    let lib = s.library;
    Ok(Json(
        blocking(move || {
            let folder = lib.locate_folder(id, &body.path)?;
            view(lib.db(), folder)
        })
        .await?,
    ))
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
    // Recognition switched on/off or between GPU and CPU.
    if let Some(ai) = &s.ai {
        ai.refresh();
    }
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
