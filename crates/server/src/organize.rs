//! Favorites, albums, archive and trash. Changes tell the UI to reload its lists.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::albums::{self, Album, AlbumRef};
use photos_core::media;
use photos_core::organize::{self, Deleted};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::blocking;
use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cameras", get(cameras))
        .route("/media/favorite", post(favorite))
        .route("/media/trash", post(trash))
        .route("/media/restore", post(restore))
        .route("/media/archive", post(archive))
        .route("/media/unarchive", post(unarchive))
        .route("/media/{id}/albums", get(albums_of))
        .route("/trash/empty", post(empty_trash))
        .route("/albums", get(list_albums).post(create_album))
        .route("/albums/{id}", get(get_album).patch(update_album).delete(delete_album))
        .route("/albums/{id}/items", post(add_items).delete(remove_items))
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

fn changed(n: usize) -> Json<Value> {
    Json(json!({ "changed": n }))
}

async fn cameras(State(s): State<AppState>) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(blocking(move || media::cameras(s.library.db())).await?))
}

#[derive(Deserialize)]
struct Favorite {
    ids: Vec<i64>,
    favorite: bool,
}

async fn favorite(State(s): State<AppState>, Json(body): Json<Favorite>) -> Result<Json<Value>, ApiError> {
    let lib = s.library;
    let n = blocking(move || {
        let n = organize::set_favorite(lib.db(), &body.ids, body.favorite)?;
        if n > 0 {
            lib.notify_changed();
        }
        Ok(n)
    })
    .await?;
    Ok(changed(n))
}

/// Run a change on some items; open lists reload if anything changed.
async fn change(
    s: AppState,
    ids: Vec<i64>,
    f: fn(&photos_core::db::Db, &[i64]) -> photos_core::Result<usize>,
) -> Result<Json<Value>, ApiError> {
    let lib = s.library;
    let n = blocking(move || {
        let n = f(lib.db(), &ids)?;
        if n > 0 {
            lib.notify_changed();
        }
        Ok(n)
    })
    .await?;
    Ok(changed(n))
}

async fn trash(State(s): State<AppState>, Json(body): Json<Ids>) -> Result<Json<Value>, ApiError> {
    change(s, body.ids, organize::trash).await
}

async fn restore(State(s): State<AppState>, Json(body): Json<Ids>) -> Result<Json<Value>, ApiError> {
    change(s, body.ids, organize::restore).await
}

async fn archive(State(s): State<AppState>, Json(body): Json<Ids>) -> Result<Json<Value>, ApiError> {
    change(s, body.ids, organize::archive).await
}

async fn unarchive(State(s): State<AppState>, Json(body): Json<Ids>) -> Result<Json<Value>, ApiError> {
    change(s, body.ids, organize::unarchive).await
}

#[derive(Deserialize)]
struct EmptyTrash {
    /// Only these items; everything in the trash when left out.
    ids: Option<Vec<i64>>,
}

/// Send trashed files to the Windows Recycle Bin and forget the items.
async fn empty_trash(State(s): State<AppState>, Json(body): Json<EmptyTrash>) -> Result<Json<Deleted>, ApiError> {
    let lib = s.library;
    let report = blocking(move || {
        let report = organize::empty_trash(lib.db(), lib.paths(), body.ids.as_deref())?;
        if report.deleted > 0 {
            lib.notify_changed();
        }
        Ok(report)
    })
    .await?;
    Ok(Json(report))
}

async fn albums_of(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Vec<AlbumRef>>, ApiError> {
    Ok(Json(blocking(move || albums::for_media(s.library.db(), id)).await?))
}

async fn list_albums(State(s): State<AppState>) -> Result<Json<Vec<Album>>, ApiError> {
    Ok(Json(blocking(move || albums::list(s.library.db())).await?))
}

async fn get_album(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Album>, ApiError> {
    Ok(Json(blocking(move || albums::get(s.library.db(), id)).await?))
}

#[derive(Deserialize)]
struct NewAlbum {
    name: String,
    /// Items to start the album with.
    #[serde(default)]
    ids: Vec<i64>,
}

async fn create_album(
    State(s): State<AppState>,
    Json(body): Json<NewAlbum>,
) -> Result<(StatusCode, Json<Album>), ApiError> {
    let lib = s.library;
    let album = blocking(move || {
        let album = albums::create(lib.db(), &body.name)?;
        if !body.ids.is_empty() {
            albums::add_items(lib.db(), album.id, &body.ids)?;
        }
        lib.notify_changed();
        albums::get(lib.db(), album.id)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(album)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumPatch {
    name: Option<String>,
    cover_media_id: Option<i64>,
}

async fn update_album(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<AlbumPatch>,
) -> Result<Json<Album>, ApiError> {
    let lib = s.library;
    let album = blocking(move || {
        if let Some(name) = &body.name {
            albums::rename(lib.db(), id, name)?;
        }
        if let Some(cover) = body.cover_media_id {
            albums::set_cover(lib.db(), id, cover)?;
        }
        lib.notify_changed();
        albums::get(lib.db(), id)
    })
    .await?;
    Ok(Json(album))
}

async fn delete_album(State(s): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    let lib = s.library;
    blocking(move || {
        albums::delete(lib.db(), id)?;
        lib.notify_changed();
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_items(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Ids>,
) -> Result<Json<Value>, ApiError> {
    let lib = s.library;
    let n = blocking(move || {
        let n = albums::add_items(lib.db(), id, &body.ids)?;
        lib.notify_changed();
        Ok(n)
    })
    .await?;
    Ok(changed(n))
}

async fn remove_items(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Ids>,
) -> Result<Json<Value>, ApiError> {
    let lib = s.library;
    let n = blocking(move || {
        let n = albums::remove_items(lib.db(), id, &body.ids)?;
        lib.notify_changed();
        Ok(n)
    })
    .await?;
    Ok(changed(n))
}
