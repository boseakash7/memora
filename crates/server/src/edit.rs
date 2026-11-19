//! The photo editor: saved edits, live previews, Enhance, the subject mask for background
//! blur, and exporting an edited copy. Edits are recipes (see `photos_core::edit`); the
//! file itself is never changed.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::edit::filters::{self, Scene};
use photos_core::edit::{self, Adjust, Edit, Saved, session};
use photos_core::media::{self, Kind, Media};
use photos_core::{AppPaths, thumbs};
use serde::{Deserialize, Serialize};

use crate::api::blocking;
use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/edit/filters", get(filter_list))
        .route("/media/{id}/edit", get(get_edit).put(save_edit))
        .route("/media/{id}/edit/preview", post(preview))
        .route("/media/{id}/edit/enhance", post(enhance))
        .route("/media/{id}/edit/subject", post(subject))
        .route("/media/{id}/edit/export", post(export))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Filters {
    filters: Vec<filters::Listed>,
    /// Background blur is possible (the subject model is installed).
    subjects: bool,
    /// Finding the subject runs on a graphics card (None: not known yet).
    subjects_gpu: Option<bool>,
}

async fn filter_list(State(s): State<AppState>) -> Json<Filters> {
    let subjects = s.ai.as_ref().is_some_and(|ai| ai.can_find_subjects());
    let subjects_gpu = s.ai.as_ref().filter(|_| subjects).and_then(|ai| ai.subjects_on_gpu());
    Json(Filters { filters: filters::catalog(), subjects, subjects_gpu })
}

async fn get_edit(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Saved>, ApiError> {
    Ok(Json(blocking(move || edit::get(s.library.db(), id)).await?))
}

#[derive(Deserialize)]
struct SaveBody {
    edit: Option<Edit>,
}

/// What the grid needs after a save: the item's new size and picture version.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedView {
    #[serde(flatten)]
    saved: Saved,
    w: u32,
    h: u32,
    /// Picture version in seconds (the timeline's `m`).
    m: i64,
}

async fn save_edit(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<SaveBody>) -> Result<Json<SavedView>, ApiError> {
    let st = s.clone();
    let view = blocking(move || {
        let lib = &st.library;
        let saved = edit::save(lib.db(), id, body.edit)?;
        let m = media::get(lib.db(), id)?;
        // Make the pictures now, so the grid and viewer show the edit straight away.
        prepare_mask(&st, &m);
        let _ = thumbs::make_thumb(lib.paths(), &m);
        lib.notify_changed();
        let (w, h) = m.shown_size().unwrap_or((0, 0));
        Ok(SavedView { saved, w, h, m: m.version() / 1000 })
    })
    .await?;
    Ok(Json(view))
}

/// Make the subject mask an edit's background blur needs, if recognition can (an edit
/// synced from another PC may arrive without one). Problems only mean no blur.
pub(crate) fn prepare_mask(s: &AppState, m: &Media) {
    let Some(e) = m.edit() else { return };
    if e.blur <= 0.0 || thumbs::has_mask(s.library.paths(), m) {
        return;
    }
    let Some(ai) = s.ai.as_ref().filter(|ai| ai.can_find_subjects()) else { return };
    if let Err(e) = session::ensure_mask(s.library.paths(), m, |img| ai.subject_mask(img).map_err(|e| e.to_string())) {
        tracing::warn!("no subject mask for {}: {e}", m.id);
    }
}

fn photo(paths_db: &photos_core::Db, id: i64) -> photos_core::Result<Media> {
    let m = media::get(paths_db, id)?;
    if m.kind != Kind::Photo {
        return Err(photos_core::Error::media("only photos can be edited"));
    }
    Ok(m)
}

#[derive(Deserialize)]
struct PreviewBody {
    edit: Edit,
    /// Longest side wanted, in pixels.
    max: u32,
}

/// The photo with an edit that isn't saved yet, as JPEG. The editor asks for one whenever
/// a slider moves.
async fn preview(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<PreviewBody>) -> Result<Response, ApiError> {
    let jpeg = blocking(move || {
        let lib = &s.library;
        let m = photo(lib.db(), id)?;
        session::preview(lib.paths(), &s.editor, &m, &body.edit.normalized(), body.max)
    })
    .await?;
    let mut res = Response::new(Body::from(jpeg));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[derive(Deserialize)]
struct EnhanceBody {
    edit: Edit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Enhanced {
    adjust: Adjust,
    /// What recognition says the photo shows (None until it has looked).
    scene: Option<Scene>,
    /// Filters that suit the scene, best first.
    suggested: Vec<&'static str>,
}

/// Enhance: slider values for this photo, and filters that suit it.
async fn enhance(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<EnhanceBody>) -> Result<Json<Enhanced>, ApiError> {
    let out = blocking(move || {
        let lib = &s.library;
        let m = photo(lib.db(), id)?;
        let scene = s.ai.as_ref().and_then(|ai| match ai.scene(id) {
            Ok(scene) => scene,
            Err(e) => {
                tracing::warn!("scene of {id}: {e}");
                None
            }
        });
        let adjust = session::enhance(lib.paths(), &s.editor, &m, &body.edit.normalized(), scene)?;
        let suggested = filters::suggested(scene.unwrap_or(Scene::Other)).to_vec();
        Ok(Enhanced { adjust, scene, suggested })
    })
    .await?;
    Ok(Json(out))
}

/// Find the photo's main subject (for background blur). Takes a few seconds the first time.
async fn subject(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(ai) = s.ai.clone().filter(|ai| ai.can_find_subjects()) else {
        return Err(ApiError::Busy("Background blur needs the subject model, which isn't installed.".into()));
    };
    blocking(move || {
        let lib = &s.library;
        let m = photo(lib.db(), id)?;
        session::ensure_mask(lib.paths(), &m, |img| ai.subject_mask(img).map_err(|e| e.to_string()))
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ExportBody {
    /// The edit to export; the saved one when missing.
    edit: Option<Edit>,
    path: PathBuf,
    quality: Option<u8>,
}

#[derive(Serialize)]
struct Exported {
    path: PathBuf,
    width: u32,
    height: u32,
}

/// Save the edited photo as a new JPEG file.
async fn export(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<ExportBody>) -> Result<Json<Exported>, ApiError> {
    if !body.path.is_absolute() {
        return Err(ApiError::BadRequest("the file must be a full path".into()));
    }
    let out = blocking(move || {
        let lib = &s.library;
        let m = photo(lib.db(), id)?;
        let e = match body.edit {
            Some(e) => e.normalized(),
            None => m.edit.clone().unwrap_or_default(),
        };
        if e.blur > 0.0 {
            prepare_mask(&s, &Media { edit: Some(e.clone()), ..m.clone() });
        }
        let paths: &AppPaths = lib.paths();
        let (width, height) = session::export(paths, &m, &e, &body.path, body.quality.unwrap_or(92))?;
        Ok(Exported { path: body.path, width, height })
    })
    .await?;
    Ok(Json(out))
}
