//! Streams FFmpeg's converted video to a client (see `photos_core::transcode`).
//!
//! - FFmpeg is stopped as soon as the client goes away (the response body is dropped).
//! - Seeking starts a new request, so a client's previous conversion is stopped when it
//!   asks for a new one. TVs often open a second connection before closing the first.
//! - At most [`MAX_STREAMS`] conversions run at once.

use std::collections::HashMap;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use futures_core::Stream;
use photos_core::media::Media;
use photos_core::transcode::{self, Container};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::ApiError;

pub const MAX_STREAMS: usize = 3;

pub struct Streams {
    slots: Arc<Semaphore>,
    by_client: Mutex<HashMap<IpAddr, (u64, CancellationToken)>>,
    next: std::sync::atomic::AtomicU64,
}

impl Default for Streams {
    fn default() -> Self {
        Streams {
            slots: Arc::new(Semaphore::new(MAX_STREAMS)),
            by_client: Mutex::new(HashMap::new()),
            next: Default::default(),
        }
    }
}

impl Streams {
    /// Start converting `media` from `start_secs` for `client`, stopping that client's
    /// previous conversion. The response has the stream as its body and no length.
    pub async fn start(
        self: &Arc<Self>,
        media: Media,
        container: Container,
        start_secs: f64,
        client: IpAddr,
    ) -> Result<Response, ApiError> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cancel = CancellationToken::new();
        if let Some((_, old)) = self.by_client.lock().unwrap().insert(client, (id, cancel.clone())) {
            old.cancel();
        }

        // The stopped conversion frees its slot once its task notices; give it a moment.
        let permit = tokio::time::timeout(Duration::from_secs(3), self.slots.clone().acquire_owned())
            .await
            .map_err(|_| ApiError::Busy("Too many videos are being converted at once. Try again shortly.".into()))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let name = media.name.clone();
        // Finding the encoder the first time runs FFmpeg a few times.
        let cmd = tokio::task::spawn_blocking(move || transcode::command(&media, container, start_secs))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let mut child = tokio::process::Command::from(cmd)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ApiError::Internal(format!("cannot start FFmpeg: {e}")))?;
        tracing::info!("converting {name} from {start_secs:.1}s for {client}");

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("ffmpeg ({name}): {line}");
                }
            });
        }
        let stdout = child.stdout.take().ok_or_else(|| ApiError::Internal("FFmpeg has no output".into()))?;

        let body = Conversion {
            out: ReaderStream::with_capacity(stdout, 64 * 1024),
            _child: child,
            _permit: permit,
            cancelled: Box::pin(cancel.cancelled_owned()),
            _done: Done { streams: self.clone(), client, id },
        };
        let mut res = Response::new(Body::from_stream(body));
        *res.status_mut() = StatusCode::OK;
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(container.mime()));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(res)
    }
}

/// The response body: FFmpeg's stdout until it ends, the client leaves or the client
/// starts another conversion. Dropping it kills FFmpeg and frees the slot.
struct Conversion {
    out: ReaderStream<ChildStdout>,
    _child: Child,
    _permit: OwnedSemaphorePermit,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    _done: Done,
}

impl Stream for Conversion {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Pin::new(&mut self.out).poll_next(cx)
    }
}

/// Forgets the client's entry when its conversion ends (unless a newer one replaced it).
struct Done {
    streams: Arc<Streams>,
    client: IpAddr,
    id: u64,
}

impl Drop for Done {
    fn drop(&mut self) {
        let mut map = self.streams.by_client.lock().unwrap();
        if map.get(&self.client).is_some_and(|(id, _)| *id == self.id) {
            map.remove(&self.client);
        }
    }
}

/// Start time from `TimeSeekRange.dlna.org: npt=<start>-[<end>]`, where times are seconds
/// (`12.5`) or `h:mm:ss[.fff]`.
pub fn parse_time_seek(value: &str) -> Option<f64> {
    let npt = value.trim().strip_prefix("npt=")?;
    let start = npt.split('-').next()?.trim();
    parse_npt(start)
}

fn parse_npt(s: &str) -> Option<f64> {
    if s.is_empty() || s == "now" {
        return Some(0.0);
    }
    let mut secs = 0.0;
    for part in s.split(':') {
        let v: f64 = part.parse().ok().filter(|v: &f64| v.is_finite() && *v >= 0.0)?;
        secs = secs * 60.0 + v;
    }
    Some(secs)
}

/// `h:mm:ss.fff`, as DLNA wants durations and times.
pub fn npt(ms: i64) -> String {
    let ms = ms.max(0);
    format!("{}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_seek_header() {
        assert_eq!(parse_time_seek("npt=12.5-"), Some(12.5));
        assert_eq!(parse_time_seek("npt=0:01:02.500-0:02:00"), Some(62.5));
        assert_eq!(parse_time_seek("npt=1:00:00-"), Some(3600.0));
        assert_eq!(parse_time_seek("npt=-"), Some(0.0));
        assert_eq!(parse_time_seek("bytes=0-"), None);
        assert_eq!(parse_time_seek("npt=abc-"), None);
        assert_eq!(npt(3_723_045), "1:02:03.045");
        assert_eq!(npt(5000), "0:00:05.000");
    }
}
