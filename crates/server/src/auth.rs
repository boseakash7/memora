use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;

use crate::{ApiError, AppState};

/// Require the per-launch token, either as `Authorization: Bearer <token>`
/// or as `?token=<token>` (needed for `<img src>` / `<video src>`, which can't set headers).
pub async fn require_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let from_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let from_query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));

    match from_header.or(from_query) {
        Some(t) if constant_time_eq(t.as_bytes(), state.token().as_bytes()) => Ok(next.run(req).await),
        _ => Err(ApiError::Unauthorized),
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
