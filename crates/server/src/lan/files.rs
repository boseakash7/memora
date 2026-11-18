//! Media files for the network: thumbnails, TV-sized photos, originals and converted
//! videos. Only visible items (not trashed or missing; archived ones only while the Archive
//! is shared) are served.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use photos_core::media::{self, Kind, Media};
use photos_core::thumbs;
use photos_core::transcode::Container;

use super::LanState;
use crate::api::blocking;
use crate::media::serve;
use crate::ApiError;

/// Files one item offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {
    /// Grid thumbnail (400 px tall).
    Thumb,
    /// At most 160x160, for DLNA.
    Small,
    /// Photos: upright JPEG of at most 4K.
    Photo,
    Original,
    Converted(Container),
}

pub fn client_ip(req: &Request) -> IpAddr {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| super::net::to_v4(c.0.ip()))
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// The visible item, with its video codecs filled in.
pub async fn visible(s: &LanState, id: i64) -> Result<Media, ApiError> {
    let db = s.app.library.db().clone();
    let archive = s.cfg().archive;
    blocking(move || {
        let mut m = media::get_visible(&db, id, archive)?;
        media::ensure_codecs(&db, &mut m)?;
        Ok(m)
    })
    .await
}

/// Serve `file` of item `id`. `start_secs` is where a converted video starts.
pub async fn send(s: &LanState, id: i64, file: File, start_secs: f64, req: Request) -> Result<Response, ApiError> {
    let m = visible(s, id).await?;
    let lib = s.app.library.clone();
    match file {
        File::Thumb | File::Small | File::Photo => {
            if file == File::Photo && m.kind == Kind::Video {
                return Err(photos_core::Error::NotFound.into());
            }
            let _slot = s.app.thumb_slots.acquire().await.map_err(|e| ApiError::Internal(e.to_string()))?;
            let path = blocking(move || match file {
                File::Thumb => thumbs::ensure_thumb(lib.paths(), &m),
                File::Small => thumbs::ensure_small(lib.paths(), &m),
                _ => thumbs::ensure_tv(lib.paths(), &m),
            })
            .await?;
            serve(path, Some("image/jpeg"), "max-age=86400", req).await
        }
        File::Original => {
            let mime = if m.kind == Kind::Video { dlna_video_mime(&m.mime) } else { m.mime.as_str() }.to_owned();
            serve(m.path, Some(&mime), "no-cache", req).await
        }
        File::Converted(container) => {
            if m.kind != Kind::Video || !s.cfg().transcode {
                return Err(photos_core::Error::NotFound.into());
            }
            if req.method() == Method::HEAD {
                // TVs check the type before playing; don't start FFmpeg for that.
                let mut res = StatusCode::OK.into_response();
                res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(container.mime()));
                return Ok(res);
            }
            let client = client_ip(&req);
            s.app.streams.start(m, container, start_secs, client).await
        }
    }
}

/// MIME types TVs expect for video originals. Samsung TVs know MKV as `video/x-mkv`, and
/// QuickTime files are MP4s as far as a TV is concerned.
pub fn dlna_video_mime(mime: &str) -> &str {
    match mime {
        "video/quicktime" => "video/mp4",
        "video/x-matroska" => "video/x-mkv",
        "video/mp2t" => "video/vnd.dlna.mpeg-tts",
        m => m,
    }
}
