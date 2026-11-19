//! The AI features' models: which are installed, downloading them, removing them.

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use photos_ml::{Ai, Feature, FeatureStatus};

use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/features", get(list))
        .route("/ai/features/{feature}/download", post(download))
        .route("/ai/features/{feature}/pause", post(pause))
        .route("/ai/features/{feature}/cancel", post(cancel))
        .route("/ai/features/{feature}", delete(remove))
}

type Features = Result<Json<Vec<FeatureStatus>>, ApiError>;

fn ai(s: &AppState) -> Result<Ai, ApiError> {
    s.ai.clone().ok_or_else(|| ApiError::BadRequest("AI features are not available in this copy of Memora".into()))
}

/// Run on a blocking thread: finding the blur size may load the graphics card driver.
async fn with<F>(s: AppState, f: F) -> Features
where
    F: FnOnce(&Ai) -> Result<(), ApiError> + Send + 'static,
{
    let ai = ai(&s)?;
    tokio::task::spawn_blocking(move || {
        f(&ai)?;
        Ok(Json(ai.features()))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

async fn list(State(s): State<AppState>) -> Features {
    with(s, |_| Ok(())).await
}

async fn download(State(s): State<AppState>, Path(f): Path<Feature>) -> Features {
    with(s, move |ai| {
        ai.download(f);
        Ok(())
    })
    .await
}

async fn pause(State(s): State<AppState>, Path(f): Path<Feature>) -> Features {
    with(s, move |ai| {
        ai.pause(f);
        Ok(())
    })
    .await
}

async fn cancel(State(s): State<AppState>, Path(f): Path<Feature>) -> Features {
    with(s, move |ai| {
        ai.cancel(f);
        Ok(())
    })
    .await
}

async fn remove(State(s): State<AppState>, Path(f): Path<Feature>) -> Features {
    with(s, move |ai| {
        ai.remove(f).map_err(|e| ApiError::Busy(format!("couldn't delete the model files ({e}); try again in a moment")))
    })
    .await
}
