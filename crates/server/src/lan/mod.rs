//! Sharing on the home network: a second, read-only HTTP listener on `0.0.0.0:<port>` that
//! serves the TV web page (`/tv`) and the DLNA media server (`/dlna` + SSDP discovery).
//!
//! It is off by default and follows the settings: [`Lan::apply`] starts, restarts or stops
//! it. Nothing on this listener can change the library, and trashed items are never served.

pub(crate) mod files;
pub(crate) mod net;
mod tv;

use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use futures_core::Stream;
use photos_core::settings::{self, Settings};
use serde::Serialize;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::dlna::{self, ssdp::Ssdp};
use crate::{AppState, api::blocking};

pub(crate) use files::client_ip;

/// What the LAN handlers need to know. Device name, PIN and transcoding can change while
/// running; port and the two switches restart the listener.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LanConfig {
    pub port: u16,
    pub tv_page: bool,
    pub dlna: bool,
    pub device_name: String,
    pub pin: Option<String>,
    pub transcode: bool,
    /// Stable DLNA device id, `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    pub uuid: String,
    /// Signs the TV page's PIN cookie.
    pub secret: String,
}

impl LanConfig {
    fn listener_changed(&self, other: &LanConfig) -> bool {
        (self.port, self.tv_page, self.dlna) != (other.port, other.tv_page, other.dlna)
    }
}

#[derive(Clone)]
pub(crate) struct LanState {
    pub app: AppState,
    cfg: Arc<RwLock<Arc<LanConfig>>>,
    pub pin_guard: Arc<tv::PinGuard>,
}

impl LanState {
    pub fn cfg(&self) -> Arc<LanConfig> {
        self.cfg.read().unwrap().clone()
    }
}

/// Shown in Settings.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanStatus {
    pub running: bool,
    pub port: u16,
    pub tv_page: bool,
    pub dlna: bool,
    /// Why sharing isn't running although it is switched on (e.g. port in use).
    pub error: Option<String>,
    pub addresses: Vec<LanAddress>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanAddress {
    pub name: String,
    pub ip: String,
    pub is_virtual: bool,
}

struct Running {
    cfg: Arc<RwLock<Arc<LanConfig>>>,
    current: LanConfig,
    stop: CancellationToken,
    server: JoinHandle<()>,
    ssdp: Option<Ssdp>,
}

#[derive(Default)]
pub struct Lan {
    running: tokio::sync::Mutex<Option<Running>>,
    error: std::sync::Mutex<Option<String>>,
}

impl Lan {
    /// Bring the listener in line with the saved settings.
    pub async fn apply(&self, app: &AppState) {
        let lib = app.library.clone();
        let wanted = blocking(move || {
            let s = settings::load(lib.db())?;
            let uuid = settings::persistent_secret(lib.db(), "dlnaUuid")?;
            let secret = settings::persistent_secret(lib.db(), "lanSecret")?;
            Ok((s, uuid, secret))
        })
        .await
        .map(|(s, uuid, secret)| wanted_config(&s, &uuid, &secret));
        let wanted = match wanted {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("cannot read sharing settings: {e}");
                return;
            }
        };

        let mut running = self.running.lock().await;
        if let (Some(r), Some(w)) = (running.as_mut(), wanted.as_ref())
            && !r.current.listener_changed(w)
        {
            if r.current != *w {
                let name_changed = r.current.device_name != w.device_name;
                *r.cfg.write().unwrap() = Arc::new(w.clone());
                r.current = w.clone();
                if name_changed && let Some(ssdp) = &r.ssdp {
                    ssdp.announce(); // TVs re-read the description and pick up the new name
                }
            }
            return;
        }

        if let Some(r) = running.take() {
            stop(r).await;
        }
        *self.error.lock().unwrap() = None;
        if let Some(w) = wanted {
            match start(app, w).await {
                Ok(r) => *running = Some(r),
                Err(e) => *self.error.lock().unwrap() = Some(e),
            }
        }
    }

    /// Stop sharing (on app exit): DLNA says goodbye so TVs drop the PC from their list.
    pub async fn shutdown(&self) {
        if let Some(r) = self.running.lock().await.take() {
            stop(r).await;
        }
    }

    pub async fn status(&self) -> LanStatus {
        let running = self.running.lock().await;
        let addresses = tokio::task::spawn_blocking(net::local_addrs)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| LanAddress { name: a.name, ip: a.ip.to_string(), is_virtual: a.is_virtual })
            .collect();
        let (port, tv_page, dlna) = running.as_ref().map(|r| (r.current.port, r.current.tv_page, r.current.dlna)).unwrap_or_default();
        LanStatus {
            running: running.is_some(),
            port,
            tv_page,
            dlna,
            error: self.error.lock().unwrap().clone(),
            addresses,
        }
    }
}

fn wanted_config(s: &Settings, uuid: &str, secret: &str) -> Option<LanConfig> {
    if !s.lan_sharing || !(s.lan_tv_page || s.lan_dlna) {
        return None;
    }
    let u = &uuid[..32];
    Some(LanConfig {
        port: s.lan_port,
        tv_page: s.lan_tv_page,
        dlna: s.lan_dlna,
        device_name: s.device_name.trim().to_owned(),
        pin: s.tv_pin.clone().filter(|p| !p.is_empty()),
        transcode: s.transcode_videos,
        uuid: format!("{}-{}-{}-{}-{}", &u[..8], &u[8..12], &u[12..16], &u[16..20], &u[20..]),
        secret: secret.to_owned(),
    })
}

async fn start(app: &AppState, cfg: LanConfig) -> Result<Running, String> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.port)).await.map_err(|e| {
        tracing::warn!("cannot share on port {}: {e}", cfg.port);
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("Port {} is already used by another program. Choose another port.", cfg.port)
        } else {
            format!("Cannot listen on port {}: {e}", cfg.port)
        }
    })?;

    let shared = Arc::new(RwLock::new(Arc::new(cfg.clone())));
    let stop = CancellationToken::new();
    let state = LanState { app: app.clone(), cfg: shared.clone(), pin_guard: Default::default() };
    let app_router = router(state, &cfg, stop.clone());
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        let service = app_router.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(listener, service).with_graceful_shutdown(shutdown.cancelled_owned()).await {
            tracing::error!("network sharing stopped: {e}");
        }
    });

    let ssdp = if cfg.dlna {
        match Ssdp::start(cfg.uuid.clone(), cfg.port).await {
            Ok(s) => Some(s),
            Err(e) => {
                // The TV page still works; TVs just won't find the media server by themselves.
                tracing::warn!("DLNA discovery unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    // Warm up: finding the video encoder runs FFmpeg a few times.
    tokio::task::spawn_blocking(photos_core::transcode::encoder);
    tracing::info!("sharing on the network at port {} (TV page: {}, DLNA: {})", cfg.port, cfg.tv_page, cfg.dlna);
    Ok(Running { cfg: shared, current: cfg, stop, server, ssdp })
}

async fn stop(r: Running) {
    if let Some(ssdp) = r.ssdp {
        ssdp.stop().await;
    }
    r.stop.cancel();
    let mut server = r.server;
    if tokio::time::timeout(Duration::from_secs(3), &mut server).await.is_err() {
        server.abort();
    }
    tracing::info!("network sharing stopped");
}

fn router(state: LanState, cfg: &LanConfig, stop: CancellationToken) -> Router {
    let mut r = Router::new();
    if cfg.tv_page {
        r = r.merge(tv::routes(state.clone())).route("/", get(|| async { Redirect::temporary("/tv/") }));
    }
    if cfg.dlna {
        r = r.merge(dlna::routes());
    }
    r.layer(middleware::from_fn_with_state(stop, end_on_stop))
        .layer(middleware::from_fn(check_host))
        .with_state(state)
}

/// Refuse requests addressed to a host name other than an IP address or this PC's name.
/// A web page on another site could otherwise point its own domain at this PC's address
/// (DNS rebinding) and read the photos through the visitor's browser.
async fn check_host(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_owned);
    match host {
        Some(h) if !host_allowed(&h) => (StatusCode::MISDIRECTED_REQUEST, "Unknown host name").into_response(),
        _ => next.run(req).await,
    }
}

fn host_allowed(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(v6) => return v6.split(']').next().is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok()),
        None => host.rsplit_once(':').map_or(host, |(name, port)| if port.parse::<u16>().is_ok() { name } else { host }),
    };
    if name.parse::<Ipv4Addr>().is_ok() || name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let pc = std::env::var("COMPUTERNAME").unwrap_or_default();
    let name = name.trim_end_matches('.');
    !pc.is_empty() && (name.eq_ignore_ascii_case(&pc) || name.eq_ignore_ascii_case(&format!("{pc}.local")))
}

/// When sharing is switched off, end every response still streaming (videos) right away.
async fn end_on_stop(State(stop): State<CancellationToken>, req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    let (parts, body) = res.into_parts();
    let body = Body::from_stream(Cut { inner: body.into_data_stream(), stop: Box::pin(stop.cancelled_owned()) });
    Response::from_parts(parts, body)
}

struct Cut {
    inner: axum::body::BodyDataStream,
    stop: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl Stream for Cut {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use std::future::Future;
        if self.stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[cfg(test)]
pub(crate) mod tests;
