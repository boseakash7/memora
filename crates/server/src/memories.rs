//! Memories API, and keeping memories up to date in the background.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use photos_core::memories::{self, Card, Memory};
use serde::Deserialize;

use crate::api::blocking;
use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/memories", get(list))
        .route("/memories/rebuild", post(rebuild_now))
        .route("/memories/{id}", get(one).patch(update))
        .route("/memories/{id}/seen", post(seen))
        .route("/memories/{id}/remove", post(remove))
}

/// Pictures that look like screenshots or documents, when recognition is available.
/// Search favours precision, so only clear cases are left out.
const JUNK_PROMPTS: &[&str] = &[
    "a screenshot of a phone screen",
    "a screenshot of a computer screen",
    "a photo of a document or a page of text",
    "a photo of a receipt",
];

fn junk(s: &AppState) -> HashSet<i64> {
    let Some(ai) = &s.ai else { return HashSet::new() };
    let mut out = HashSet::new();
    for prompt in JUNK_PROMPTS {
        match ai.search(prompt) {
            Ok(hits) => out.extend(hits.into_iter().map(|(id, _)| id)),
            Err(_) => return HashSet::new(), // recognition off or models missing
        }
    }
    out
}

/// Rebuild memories for today. Blocking.
pub(crate) fn rebuild(s: &AppState) -> photos_core::Result<memories::Rebuilt> {
    let today = chrono::Local::now().date_naive();
    let started = Instant::now();
    let r = memories::rebuild(s.library.db(), today, &junk(s))?;
    tracing::info!("memories: {} ({} new, {} gone) in {:?}", r.memories, r.added, r.removed, started.elapsed());
    Ok(r)
}

/// Keep memories current: at start, when the day changes, and after the library changed
/// (at most every 10 minutes, once indexing is quiet). Runs until the process exits.
pub fn spawn_refresh(state: AppState) {
    tokio::spawn(async move {
        let mut built: Option<(chrono::NaiveDate, u64, Instant)> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let status = state.library.status();
            let today = chrono::Local::now().date_naive();
            let busy = status.scanning || status.pending > 0;
            let due = match built {
                None => !busy || status.total > 0,
                Some((day, _, _)) if day != today => true,
                Some((_, rev, at)) => rev != status.revision && !busy && at.elapsed() >= Duration::from_secs(600),
            };
            if !due {
                continue;
            }
            let s = state.clone();
            let enabled = {
                let lib = s.library.clone();
                tokio::task::spawn_blocking(move || photos_core::settings::load(lib.db()).map(|c| c.memories).unwrap_or(true))
                    .await
                    .unwrap_or(true)
            };
            if enabled {
                match tokio::task::spawn_blocking(move || rebuild(&s)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::warn!("memories not rebuilt: {e}"),
                    Err(e) => tracing::warn!("memories not rebuilt: {e}"),
                }
            }
            built = Some((today, status.revision, Instant::now()));
        }
    });
}

#[derive(Deserialize)]
struct ListQuery {
    /// Also hidden ones (the Memories page's "Show hidden").
    #[serde(default)]
    hidden: bool,
}

/// Empty while memories are switched off in Settings.
async fn list(State(s): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Card>> {
    Ok(Json(
        blocking(move || {
            let db = s.library.db();
            if !photos_core::settings::load(db)?.memories {
                return Ok(Vec::new());
            }
            memories::list(db, q.hidden)
        })
        .await?,
    ))
}

async fn one(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Memory> {
    Ok(Json(blocking(move || memories::get(s.library.db(), id)).await?))
}

async fn rebuild_now(State(s): State<AppState>) -> Result<StatusCode, ApiError> {
    blocking(move || rebuild(&s).map(|_| ())).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    hidden: bool,
}

async fn update(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<Patch>) -> ApiResult<Memory> {
    Ok(Json(
        blocking(move || {
            memories::set_hidden(s.library.db(), id, body.hidden)?;
            memories::get(s.library.db(), id)
        })
        .await?,
    ))
}

async fn seen(State(s): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    blocking(move || memories::mark_seen(s.library.db(), id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

/// Take photos out of a memory (they stay out when it is rebuilt).
async fn remove(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<Ids>) -> ApiResult<Memory> {
    Ok(Json(blocking(move || memories::remove_items(s.library.db(), id, &body.ids)).await?))
}
