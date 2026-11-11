//! People (faces grouped by person) and the recognition worker's status.

use axum::extract::{Path, Query, Request, State};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use photos_core::people::{self, Face, Person};
use photos_core::{media, thumbs};
use photos_ml::AiStatus;
use serde::Deserialize;

use crate::api::blocking;
use crate::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/people", get(list))
        .route("/people/{id}", get(get_person).patch(update))
        .route("/people/{id}/merge", post(merge))
        .route("/faces/{id}/thumb", get(face_thumb))
        .route("/faces/{id}/person", put(set_face_person))
        .route("/media/{id}/faces", get(faces_of))
        .route("/ai", get(ai_status))
        .route("/ai/refresh", post(ai_refresh))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    hidden: bool,
}

async fn list(State(s): State<AppState>, Query(q): Query<ListQuery>) -> Result<Json<Vec<Person>>, ApiError> {
    Ok(Json(blocking(move || people::list(s.library.db(), q.hidden)).await?))
}

async fn get_person(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Person>, ApiError> {
    Ok(Json(blocking(move || people::get(s.library.db(), id)).await?))
}

/// `{"name": "Ria"}` (null removes it), `{"hidden": true}`, `{"coverFace": 12}`; any combination.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Update {
    #[serde(default, with = "serde_with_option")]
    name: Option<Option<String>>,
    hidden: Option<bool>,
    cover_face: Option<i64>,
}

/// Tells a missing field (`None`) from an explicit `null` (`Some(None)`).
mod serde_with_option {
    use serde::{Deserialize, Deserializer};
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(d).map(Some)
    }
}

async fn update(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<Update>) -> Result<Json<Person>, ApiError> {
    let lib = s.library;
    let person = blocking(move || {
        let db = lib.db();
        let mut p = people::get(db, id)?;
        if let Some(name) = &body.name {
            p = people::rename(db, id, name.as_deref())?;
        }
        if let Some(hidden) = body.hidden {
            p = people::set_hidden(db, id, hidden)?;
        }
        if let Some(face) = body.cover_face {
            p = people::set_cover(db, id, face)?;
        }
        lib.notify_changed();
        Ok(p)
    })
    .await?;
    Ok(Json(person))
}

#[derive(Deserialize)]
struct Merge {
    /// People to fold into the one in the URL.
    from: Vec<i64>,
}

async fn merge(State(s): State<AppState>, Path(id): Path<i64>, Json(body): Json<Merge>) -> Result<Json<Person>, ApiError> {
    let lib = s.library;
    let person = blocking(move || {
        let p = people::merge(lib.db(), &body.from, id)?;
        lib.notify_changed();
        Ok(p)
    })
    .await?;
    Ok(Json(person))
}

/// Square picture of a face. Made when the face was found; remade from the photo if missing.
async fn face_thumb(State(s): State<AppState>, Path(id): Path<i64>, req: Request) -> Result<Response, ApiError> {
    let _slot = s.thumb_slots.acquire().await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let lib = s.library.clone();
    let path = blocking(move || {
        let path = thumbs::face_path(lib.paths(), id);
        if !path.is_file() {
            let (media_id, rect) = people::media_of_face(lib.db(), id)?;
            let m = media::get(lib.db(), media_id)?;
            let img = thumbs::analysis_image(lib.paths(), &m)?;
            thumbs::write_face(lib.paths(), id, &img, rect)?;
        }
        Ok(path)
    })
    .await?;
    // A face id always shows the same picture: a changed photo gets new face ids.
    crate::media::serve(path, Some("image/jpeg"), "private, max-age=31536000, immutable", req).await
}

/// `{"person": 3}` puts the face with that person, `{"person": null}` means "not this
/// person", `{"newName": "Ria"}` starts a new person.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetPerson {
    #[serde(default, with = "serde_with_id")]
    person: Option<Option<i64>>,
    new_name: Option<String>,
}

mod serde_with_id {
    use serde::{Deserialize, Deserializer};
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
        Option::<i64>::deserialize(d).map(Some)
    }
}

async fn set_face_person(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetPerson>,
) -> Result<Json<Option<Person>>, ApiError> {
    let lib = s.library;
    let person = blocking(move || {
        let db = lib.db();
        let person = match (body.person, body.new_name) {
            (Some(_), Some(_)) | (None, None) => return Err(photos_core::Error::invalid("give either person or newName")),
            (None, Some(name)) => Some(people::new_person_for_face(db, id, Some(&name))?),
            (Some(Some(p)), None) => {
                people::set_face_person(db, id, Some(p))?;
                Some(people::get(db, p)?)
            }
            (Some(None), None) => {
                people::set_face_person(db, id, None)?;
                None
            }
        };
        lib.notify_changed();
        Ok(person)
    })
    .await?;
    Ok(Json(person))
}

async fn faces_of(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Json<Vec<Face>>, ApiError> {
    Ok(Json(blocking(move || people::faces_of(s.library.db(), id)).await?))
}

async fn ai_status(State(s): State<AppState>) -> Result<Json<Option<AiStatus>>, ApiError> {
    Ok(Json(s.ai.as_ref().map(|ai| ai.status())))
}

/// Look for model files again (e.g. after copying them in).
async fn ai_refresh(State(s): State<AppState>) -> Result<Json<Option<AiStatus>>, ApiError> {
    if let Some(ai) = &s.ai {
        ai.refresh();
    }
    ai_status(State(s)).await
}
