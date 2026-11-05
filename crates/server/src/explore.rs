//! Places, the map and duplicates.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use photos_core::duplicates::{self, Group};
use photos_core::media::{self, MapPoint};
use photos_core::places::{self, Place};

use crate::api::blocking;
use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/places", get(list_places))
        .route("/places/{id}", get(get_place))
        .route("/map/points", get(map_points))
        .route("/duplicates", get(find_duplicates))
}

async fn list_places(State(s): State<AppState>) -> Result<Json<Vec<Place>>, ApiError> {
    Ok(Json(blocking(move || places::list(s.library.db())).await?))
}

async fn get_place(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Place>, ApiError> {
    Ok(Json(blocking(move || places::get(s.library.db(), id)).await?))
}

/// Every geotagged item as `[id, lat, lon]`; compact, because libraries can be big.
async fn map_points(State(s): State<AppState>) -> Result<Json<Vec<MapPoint>>, ApiError> {
    Ok(Json(blocking(move || media::map_points(s.library.db())).await?))
}

async fn find_duplicates(State(s): State<AppState>) -> Result<Json<Vec<Group>>, ApiError> {
    Ok(Json(blocking(move || duplicates::find(s.library.db())).await?))
}
