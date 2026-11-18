use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Core(#[from] photos_core::Error),
    #[error("missing or invalid access token")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(String),
    /// Try again later (e.g. too many videos converting).
    #[error("{0}")]
    Busy(String),
    #[error("internal error")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        use photos_core::Error as E;
        let status = match &self {
            ApiError::Core(E::NotFound) => StatusCode::NOT_FOUND,
            ApiError::Core(E::Invalid(_)) | ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Core(E::FolderExists(_)) => StatusCode::CONFLICT,
            // A photo that can't be decoded is the file's problem, not the server's.
            ApiError::Core(E::Media(_)) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Core(E::Locked) => StatusCode::LOCKED,
            ApiError::Core(E::WrongPassword) => StatusCode::FORBIDDEN,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Busy(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Core(_) | ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("{self:?}");
            "Something went wrong. See the app log for details.".to_owned()
        } else {
            self.to_string()
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
