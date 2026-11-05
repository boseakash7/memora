//! DLNA/UPnP media server, so TVs list this PC as a media source and browse it with the
//! remote. Written for Samsung TVs first (they ask for a few extras), following the
//! UPnP AV ContentDirectory:1 and ConnectionManager:1 specs.

pub(crate) mod countries;
pub(crate) mod didl;
pub(crate) mod soap;
pub(crate) mod ssdp;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use photos_core::media::Kind;
use photos_core::transcode::Container;

use crate::ApiError;
use crate::api::blocking;
use crate::lan::files::{self, File};
use crate::lan::{LanState, client_ip, net};
use crate::stream::{npt, parse_time_seek};
use soap::escape;

const CDS: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CMS: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

pub fn routes() -> Router<LanState> {
    Router::new()
        .route("/dlna/description.xml", get(description))
        .route("/dlna/cds.xml", get(|| async { xml(CDS_SCPD.to_owned()) }))
        .route("/dlna/cms.xml", get(|| async { xml(CMS_SCPD.to_owned()) }))
        .route("/dlna/cds/control", post(control))
        .route("/dlna/cms/control", post(control))
        .route("/dlna/cds/event", any(events))
        .route("/dlna/cms/event", any(events))
        .route("/dlna/icon.png", get(icon))
        .route("/dlna/m/{id}/{file}", get(media_file))
}

fn xml(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")], body).into_response()
}

/// `http://<our address as the TV sees it>:<port>`, for URLs in the XML.
fn base_url(s: &LanState, req_headers: &HeaderMap, peer: std::net::IpAddr) -> String {
    let port = s.cfg().port;
    if let Some(host) = req_headers.get(header::HOST).and_then(|h| h.to_str().ok()).filter(|h| !h.is_empty()) {
        return format!("http://{host}");
    }
    let ip = net::local_ip_for(peer, &net::local_addrs()).unwrap_or(std::net::Ipv4Addr::LOCALHOST);
    format!("http://{ip}:{port}")
}

async fn description(State(s): State<LanState>) -> Response {
    let cfg = s.cfg();
    let presentation = if cfg.tv_page { "<presentationURL>/tv/</presentationURL>" } else { "" };
    xml(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0" xmlns:dlna="urn:schemas-dlna-org:device-1-0" xmlns:sec="http://www.sec.co.kr/dlna">
<specVersion><major>1</major><minor>0</minor></specVersion>
<device>
<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
<dlna:X_DLNADOC>DMS-1.50</dlna:X_DLNADOC>
<friendlyName>{name}</friendlyName>
<manufacturer>Memora</manufacturer>
<modelDescription>Offline photo and video library</modelDescription>
<modelName>Memora</modelName>
<modelNumber>{version}</modelNumber>
<serialNumber>{uuid}</serialNumber>
<UDN>uuid:{uuid}</UDN>
<sec:ProductCap>smi,DCM10,getMediaInfo.sec</sec:ProductCap>
<sec:X_ProductCap>smi,DCM10,getMediaInfo.sec</sec:X_ProductCap>
<iconList><icon><mimetype>image/png</mimetype><width>128</width><height>128</height><depth>24</depth><url>/dlna/icon.png</url></icon></iconList>
<serviceList>
<service><serviceType>{CDS}</serviceType><serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId><SCPDURL>/dlna/cds.xml</SCPDURL><controlURL>/dlna/cds/control</controlURL><eventSubURL>/dlna/cds/event</eventSubURL></service>
<service><serviceType>{CMS}</serviceType><serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId><SCPDURL>/dlna/cms.xml</SCPDURL><controlURL>/dlna/cms/control</controlURL><eventSubURL>/dlna/cms/event</eventSubURL></service>
</serviceList>
{presentation}
</device>
</root>"#,
        name = escape(&cfg.device_name),
        version = env!("CARGO_PKG_VERSION"),
        uuid = cfg.uuid,
    ))
}

async fn icon() -> Response {
    const ICON: &[u8] = include_bytes!("../../../../src-tauri/icons/128x128.png");
    ([(header::CONTENT_TYPE, "image/png")], ICON).into_response()
}

/// Event subscriptions. We never change in ways TVs need to hear about between browses,
/// but some TVs give up on a server that refuses to subscribe.
async fn events(req: Request) -> Response {
    match req.method().as_str() {
        "SUBSCRIBE" => {
            let sid = req
                .headers()
                .get("SID")
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_str(&format!("uuid:{}", uuid::Uuid::new_v4())).expect("ascii"));
            let mut res = StatusCode::OK.into_response();
            res.headers_mut().insert("SID", sid);
            res.headers_mut().insert("TIMEOUT", HeaderValue::from_static("Second-1800"));
            res
        }
        "UNSUBSCRIBE" => StatusCode::OK.into_response(),
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// How many items a TV gets per Browse when it asks for "all" (0).
const MAX_PAGE: u64 = 500;

async fn control(State(s): State<LanState>, req: Request) -> Response {
    let peer = client_ip(&req);
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024).await.unwrap_or_default();
    let Some(call) = soap::parse(&String::from_utf8_lossy(&body)) else {
        return soap_error(402, "Invalid request");
    };
    let agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("-");
    tracing::debug!("DLNA {peer} ({agent}): {} {:?}", call.action, call.args);
    let cfg = s.cfg();
    let reply = match call.action.as_str() {
        "Browse" => {
            let urls = didl::Urls { base: base_url(&s, &headers, peer), transcode: cfg.transcode };
            return browse(&s, &call, urls).await;
        }
        "GetSystemUpdateID" => soap::response(CDS, "GetSystemUpdateID", &[("Id", &update_id(&s).to_string())]),
        "GetSearchCapabilities" => soap::response(CDS, "GetSearchCapabilities", &[("SearchCaps", "")]),
        "GetSortCapabilities" => soap::response(CDS, "GetSortCapabilities", &[("SortCaps", "")]),
        // Samsung: which containers its "Videos" and "Photos" screens open.
        "X_GetFeatureList" => soap::response(CDS, "X_GetFeatureList", &[("FeatureList", FEATURE_LIST)]),
        // Samsung: remembers where playback stopped. We don't keep that.
        "X_SetBookmark" => soap::response(CDS, "X_SetBookmark", &[]),
        "GetProtocolInfo" => soap::response(CMS, "GetProtocolInfo", &[("Source", PROTOCOLS), ("Sink", "")]),
        "GetCurrentConnectionIDs" => soap::response(CMS, "GetCurrentConnectionIDs", &[("ConnectionIDs", "0")]),
        "GetCurrentConnectionInfo" => soap::response(
            CMS,
            "GetCurrentConnectionInfo",
            &[
                ("RcsID", "-1"),
                ("AVTransportID", "-1"),
                ("ProtocolInfo", ""),
                ("PeerConnectionManager", ""),
                ("PeerConnectionID", "-1"),
                ("Direction", "Output"),
                ("Status", "OK"),
            ],
        ),
        _ => return soap_error(401, "Invalid Action"),
    };
    xml(reply)
}

fn update_id(s: &LanState) -> u32 {
    (s.app.library.status().revision % u32::MAX as u64) as u32
}

fn soap_error(code: u16, description: &str) -> Response {
    let mut res = xml(soap::fault(code, description));
    *res.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    res
}

async fn browse(s: &LanState, call: &soap::Call, urls: didl::Urls) -> Response {
    let id = call.arg("ObjectID").to_owned();
    let flag = call.arg("BrowseFlag").to_owned();
    let start: u64 = call.arg("StartingIndex").parse().unwrap_or(0);
    let count: u64 = call.arg("RequestedCount").parse().unwrap_or(0);
    let count = if count == 0 { MAX_PAGE } else { count.min(MAX_PAGE) };
    let root_title = s.cfg().device_name.clone();
    let db = s.app.library.db().clone();

    let result = blocking(move || match flag.as_str() {
        "BrowseMetadata" => Ok(Some((vec![didl::metadata(&db, &id, &root_title)?], 1))),
        "BrowseDirectChildren" => Ok(Some(didl::children(&db, &id, start, count, &root_title)?)),
        _ => Ok(None),
    })
    .await;
    match result {
        Ok(Some((nodes, total))) => {
            let returned = nodes.len().to_string();
            let body = soap::response(
                CDS,
                "Browse",
                &[
                    ("Result", &didl::didl(&nodes, &urls)),
                    ("NumberReturned", &returned),
                    ("TotalMatches", &total.to_string()),
                    ("UpdateID", &update_id(s).to_string()),
                ],
            );
            xml(body)
        }
        Ok(None) => soap_error(402, "Invalid BrowseFlag"),
        Err(ApiError::Core(photos_core::Error::NotFound)) => soap_error(701, "No such object"),
        Err(e) => {
            tracing::warn!("DLNA browse failed: {e:?}");
            soap_error(501, "Action Failed")
        }
    }
}

/// `/dlna/m/{id}/{file}` with `photo.jpg`, `tn.jpg`, `stream.ts` or `original.<ext>`.
async fn media_file(State(s): State<LanState>, Path((id, file)): Path<(i64, String)>, req: Request) -> Response {
    match send_media(&s, id, &file, req).await {
        Ok(res) => res,
        Err(e) => e.into_response(),
    }
}

async fn send_media(s: &LanState, id: i64, file: &str, req: Request) -> Result<Response, ApiError> {
    let (file, features, mode) = match file {
        "photo.jpg" => (File::Photo, None, "Interactive"),
        "tn.jpg" => (File::Small, Some(didl::THUMB_FEATURES), "Interactive"),
        "stream.ts" => (File::Converted(Container::Ts), Some(didl::VIDEO_CONVERTED), "Streaming"),
        f if f.starts_with("original.") => (File::Original, Some(didl::VIDEO_ORIGINAL), "Streaming"),
        _ => return Err(photos_core::Error::NotFound.into()),
    };
    let m = files::visible(s, id).await?;
    let features = match (file, features) {
        (File::Photo, _) => didl::photo_features(&m),
        (File::Original, _) if m.kind != Kind::Video => return Err(photos_core::Error::NotFound.into()),
        (_, Some(f)) => String::from(f),
        _ => unreachable!(),
    };

    let headers = req.headers().clone();
    let seek_header = headers.get("TimeSeekRange.dlna.org").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let start = seek_header.as_deref().and_then(parse_time_seek).unwrap_or(0.0);
    let is_head = req.method() == Method::HEAD;
    tracing::debug!(
        "DLNA {} {} {id}/{file:?} range={:?} seek={seek_header:?} agent={:?}",
        client_ip(&req),
        req.method(),
        headers.get(header::RANGE),
        headers.get(header::USER_AGENT),
    );
    let mut res = files::send(s, id, file, start, req).await?;

    let h = res.headers_mut();
    let mode = headers
        .get("transferMode.dlna.org")
        .and_then(|v| v.to_str().ok())
        .filter(|m| ["Streaming", "Interactive", "Background"].contains(m))
        .unwrap_or(mode)
        .to_owned();
    h.insert("transferMode.dlna.org", HeaderValue::from_str(&mode).expect("ascii"));
    h.insert("contentFeatures.dlna.org", HeaderValue::from_str(&features).expect("ascii"));
    if file == File::Converted(Container::Ts)
        && let (Some(_), Some(total)) = (&seek_header, m.duration_ms)
    {
        let range = format!("npt={}-{}/{}", npt((start * 1000.0) as i64), npt(total), npt(total));
        h.insert("TimeSeekRange.dlna.org", HeaderValue::from_str(&range).expect("ascii"));
    }
    if headers.contains_key("getMediaInfo.sec")
        && let Some(ms) = m.duration_ms
    {
        h.insert("MediaInfo.sec", HeaderValue::from_str(&format!("SEC_Duration={ms};")).expect("ascii"));
    }
    if is_head {
        h.remove(header::TRANSFER_ENCODING);
    }
    Ok(res)
}

/// Samsung `X_GetFeatureList`: its Videos screen opens our Videos folder, Photos the root.
const FEATURE_LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Features xmlns="urn:schemas-upnp-org:av:avs" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="urn:schemas-upnp-org:av:avs http://www.upnp.org/schemas/av/avs.xsd"><Feature name="samsung.com_BASICVIEW" version="1"><container id="v" type="object.item.videoItem"/><container id="0" type="object.item.imageItem"/></Feature></Features>"#;

const PROTOCOLS: &str = "http-get:*:image/jpeg:*,http-get:*:video/mp4:*,http-get:*:video/x-mkv:*,\
http-get:*:video/mpeg:*,http-get:*:video/vnd.dlna.mpeg-tts:*,http-get:*:video/x-msvideo:*,\
http-get:*:video/webm:*,http-get:*:video/3gpp:*,http-get:*:video/x-ms-wmv:*";

const CDS_SCPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<actionList>
<action><name>Browse</name><argumentList>
<argument><name>ObjectID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ObjectID</relatedStateVariable></argument>
<argument><name>BrowseFlag</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_BrowseFlag</relatedStateVariable></argument>
<argument><name>Filter</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Filter</relatedStateVariable></argument>
<argument><name>StartingIndex</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Index</relatedStateVariable></argument>
<argument><name>RequestedCount</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>
<argument><name>SortCriteria</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_SortCriteria</relatedStateVariable></argument>
<argument><name>Result</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Result</relatedStateVariable></argument>
<argument><name>NumberReturned</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>
<argument><name>TotalMatches</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>
<argument><name>UpdateID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_UpdateID</relatedStateVariable></argument>
</argumentList></action>
<action><name>GetSearchCapabilities</name><argumentList>
<argument><name>SearchCaps</name><direction>out</direction><relatedStateVariable>SearchCapabilities</relatedStateVariable></argument>
</argumentList></action>
<action><name>GetSortCapabilities</name><argumentList>
<argument><name>SortCaps</name><direction>out</direction><relatedStateVariable>SortCapabilities</relatedStateVariable></argument>
</argumentList></action>
<action><name>GetSystemUpdateID</name><argumentList>
<argument><name>Id</name><direction>out</direction><relatedStateVariable>SystemUpdateID</relatedStateVariable></argument>
</argumentList></action>
<action><name>X_GetFeatureList</name><argumentList>
<argument><name>FeatureList</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Featurelist</relatedStateVariable></argument>
</argumentList></action>
<action><name>X_SetBookmark</name><argumentList>
<argument><name>CategoryType</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_CategoryType</relatedStateVariable></argument>
<argument><name>RID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_RID</relatedStateVariable></argument>
<argument><name>ObjectID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ObjectID</relatedStateVariable></argument>
<argument><name>PosSecond</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_PosSec</relatedStateVariable></argument>
</argumentList></action>
</actionList>
<serviceStateTable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_ObjectID</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Result</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_BrowseFlag</name><dataType>string</dataType><allowedValueList><allowedValue>BrowseMetadata</allowedValue><allowedValue>BrowseDirectChildren</allowedValue></allowedValueList></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Filter</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_SortCriteria</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Index</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Count</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_UpdateID</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>SearchCapabilities</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>SortCapabilities</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="yes"><name>SystemUpdateID</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Featurelist</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_CategoryType</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_RID</name><dataType>ui4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_PosSec</name><dataType>ui4</dataType></stateVariable>
</serviceStateTable>
</scpd>"#;

const CMS_SCPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<actionList>
<action><name>GetProtocolInfo</name><argumentList>
<argument><name>Source</name><direction>out</direction><relatedStateVariable>SourceProtocolInfo</relatedStateVariable></argument>
<argument><name>Sink</name><direction>out</direction><relatedStateVariable>SinkProtocolInfo</relatedStateVariable></argument>
</argumentList></action>
<action><name>GetCurrentConnectionIDs</name><argumentList>
<argument><name>ConnectionIDs</name><direction>out</direction><relatedStateVariable>CurrentConnectionIDs</relatedStateVariable></argument>
</argumentList></action>
<action><name>GetCurrentConnectionInfo</name><argumentList>
<argument><name>ConnectionID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ConnectionID</relatedStateVariable></argument>
<argument><name>RcsID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_RcsID</relatedStateVariable></argument>
<argument><name>AVTransportID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_AVTransportID</relatedStateVariable></argument>
<argument><name>ProtocolInfo</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_ProtocolInfo</relatedStateVariable></argument>
<argument><name>PeerConnectionManager</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_ConnectionManager</relatedStateVariable></argument>
<argument><name>PeerConnectionID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_ConnectionID</relatedStateVariable></argument>
<argument><name>Direction</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Direction</relatedStateVariable></argument>
<argument><name>Status</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_ConnectionStatus</relatedStateVariable></argument>
</argumentList></action>
</actionList>
<serviceStateTable>
<stateVariable sendEvents="yes"><name>SourceProtocolInfo</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="yes"><name>SinkProtocolInfo</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="yes"><name>CurrentConnectionIDs</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_ConnectionStatus</name><dataType>string</dataType><allowedValueList><allowedValue>OK</allowedValue><allowedValue>ContentFormatMismatch</allowedValue><allowedValue>InsufficientBandwidth</allowedValue><allowedValue>UnreliableChannel</allowedValue><allowedValue>Unknown</allowedValue></allowedValueList></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_ConnectionManager</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_Direction</name><dataType>string</dataType><allowedValueList><allowedValue>Input</allowedValue><allowedValue>Output</allowedValue></allowedValueList></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_ProtocolInfo</name><dataType>string</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_ConnectionID</name><dataType>i4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_AVTransportID</name><dataType>i4</dataType></stateVariable>
<stateVariable sendEvents="no"><name>A_ARG_TYPE_RcsID</name><dataType>i4</dataType></stateVariable>
</serviceStateTable>
</scpd>"#;
