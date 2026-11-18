//! HTTP server embedded in the desktop app.
//!
//! The desktop UI talks to it over `127.0.0.1` with a per-launch token. Sharing on the
//! home network (TV page, DLNA) runs on a separate listener; see [`lan`].

mod api;
mod auth;
mod dlna;
mod error;
mod explore;
pub mod lan;
mod media;
mod organize;
mod people;
mod stream;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, Method, header};
use photos_core::Library;
use tokio::sync::Semaphore;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub library: Library,
    token: Arc<str>,
    /// Limits on-demand thumbnail work so a fast scroll can't swamp the CPU.
    thumb_slots: Arc<Semaphore>,
    /// Video conversions in progress.
    streams: Arc<stream::Streams>,
    /// Sharing on the home network.
    pub lan: Arc<lan::Lan>,
    /// Face and content recognition; None when not started (tests).
    pub ai: Option<photos_ml::Ai>,
}

impl AppState {
    pub fn new(library: Library) -> Self {
        let slots = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 4);
        AppState {
            library,
            token: uuid::Uuid::new_v4().simple().to_string().into(),
            thumb_slots: Arc::new(Semaphore::new(slots)),
            streams: Default::default(),
            lan: Default::default(),
            ai: None,
        }
    }

    pub fn with_ai(mut self, ai: photos_ml::Ai) -> Self {
        self.ai = Some(ai);
        self
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

/// Origins the desktop UI is served from: the Tauri webview on Windows, and Vite in dev.
const UI_ORIGINS: &[&str] = &["http://tauri.localhost", "tauri://localhost", "http://localhost:5173"];

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(UI_ORIGINS.iter().map(|o| HeaderValue::from_static(o))))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::PUT, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, HeaderName::from_static("range")])
        .expose_headers([HeaderName::from_static(media::STORYBOARD_HEADER)]);

    Router::new()
        .nest("/api", api::routes(state.clone()))
        .layer(cors)
        .with_state(state)
}

pub struct LocalServer {
    pub addr: SocketAddr,
    pub token: String,
    state: AppState,
}

impl LocalServer {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop sharing on the network (DLNA says goodbye). Call when the app exits.
    pub async fn shutdown(&self) {
        self.state.lan.shutdown().await;
    }
}

/// Start the server on a random free localhost port. Runs until the process exits.
pub async fn start_local(library: Library, ai: photos_ml::Ai) -> std::io::Result<LocalServer> {
    let state = AppState::new(library).with_ai(ai);
    let token = state.token().to_owned();
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("local server stopped: {e}");
        }
    });
    tracing::info!("local server listening on http://{addr}");
    // Resume sharing if it was on last time. Problems (port in use) show in Settings.
    state.lan.apply(&state).await;
    Ok(LocalServer { addr, token, state })
}

#[cfg(test)]
mod tests;
