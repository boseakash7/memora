//! SSDP: how TVs find the media server. We answer their `M-SEARCH` requests on
//! 239.255.255.250:1900 and announce ourselves (`NOTIFY ssdp:alive`) on every network
//! adapter, then say goodbye (`ssdp:byebye`) when sharing stops.

use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, SockRef, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::lan::net::{self, LocalAddr};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;
const MAX_AGE: u32 = 1800;
/// Re-announce well within MAX_AGE, and check for new adapters (Wi-Fi reconnects) as often.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(300);
const CHECK_ADAPTERS_EVERY: Duration = Duration::from_secs(30);

pub struct Ssdp {
    stop: CancellationToken,
    announce: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Ssdp {
    pub async fn start(uuid: String, http_port: u16) -> io::Result<Ssdp> {
        let socket = bind()?;
        let stop = CancellationToken::new();
        let announce = Arc::new(Notify::new());
        let task = tokio::spawn(run(socket, Server { uuid, http_port }, stop.clone(), announce.clone()));
        Ok(Ssdp { stop, announce, task })
    }

    /// Announce again now (e.g. after the device name changed).
    pub fn announce(&self) {
        self.announce.notify_one();
    }

    /// Say goodbye and stop.
    pub async fn stop(self) {
        self.stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    }
}

fn bind() -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Windows' own SSDP service listens on this port too; sharing it is how every media
    // server does it.
    s.set_reuse_address(true)?;
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())?;
    s.set_multicast_ttl_v4(4)?;
    s.set_multicast_loop_v4(true)?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s.into())
}

struct Server {
    uuid: String,
    http_port: u16,
}

impl Server {
    fn location(&self, ip: Ipv4Addr) -> String {
        format!("http://{ip}:{}/dlna/description.xml", self.http_port)
    }
}

async fn run(socket: UdpSocket, server: Server, stop: CancellationToken, announce: Arc<Notify>) {
    let mut joined: HashSet<Ipv4Addr> = HashSet::new();
    let mut addrs = refresh(&socket, &mut joined);
    notify_all(&socket, &server, &addrs, true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    notify_all(&socket, &server, &addrs, true).await; // UDP can get lost

    let mut announce_tick = tokio::time::interval(ANNOUNCE_EVERY);
    let mut adapters_tick = tokio::time::interval(CHECK_ADAPTERS_EVERY);
    announce_tick.reset();
    adapters_tick.reset();
    let mut buf = vec![0u8; 4096];
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = announce.notified() => notify_all(&socket, &server, &addrs, true).await,
            _ = announce_tick.tick() => notify_all(&socket, &server, &addrs, true).await,
            _ = adapters_tick.tick() => {
                let fresh = refresh(&socket, &mut joined);
                if fresh != addrs {
                    addrs = fresh;
                    notify_all(&socket, &server, &addrs, true).await;
                }
            }
            got = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = got else { continue };
                let Some(st) = parse_msearch(&String::from_utf8_lossy(&buf[..n])) else { continue };
                let answers = matching(&server.uuid, &st);
                if answers.is_empty() {
                    continue; // looking for something else (routers, renderers...)
                }
                let Some(ip) = net::local_ip_for(from.ip(), &addrs) else { continue };
                let location = server.location(ip);
                for (nt, usn) in answers {
                    let msg = search_response(&nt, &usn, &location);
                    let _ = socket.send_to(msg.as_bytes(), from).await;
                }
                tracing::debug!("answered SSDP search for {st} from {from}");
            }
        }
    }
    for _ in 0..2 {
        notify_all(&socket, &server, &addrs, false).await;
    }
}

/// Current adapters; joins the multicast group on new ones.
fn refresh(socket: &UdpSocket, joined: &mut HashSet<Ipv4Addr>) -> Vec<LocalAddr> {
    let addrs = net::local_addrs();
    for a in &addrs {
        if !joined.contains(&a.ip) {
            match socket.join_multicast_v4(GROUP, a.ip) {
                Ok(()) => {
                    joined.insert(a.ip);
                }
                Err(e) => tracing::debug!("SSDP: cannot join multicast on {} ({}): {e}", a.ip, a.name),
            }
        }
    }
    addrs
}

/// Send `ssdp:alive` (or `ssdp:byebye`) for every target out of every adapter.
async fn notify_all(socket: &UdpSocket, server: &Server, addrs: &[LocalAddr], alive: bool) {
    let to = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    for a in addrs {
        if SockRef::from(socket).set_multicast_if_v4(&a.ip).is_err() {
            continue;
        }
        let location = server.location(a.ip);
        for (nt, usn) in targets(&server.uuid) {
            let _ = socket.send_to(notify(&nt, &usn, &location, alive).as_bytes(), to).await;
        }
    }
}

const SERVER: &str = concat!("Windows/10.0 UPnP/1.0 Memora/", env!("CARGO_PKG_VERSION"));
const DEVICE: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const CDS: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CMS: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

/// (NT, USN) for everything we advertise.
fn targets(uuid: &str) -> Vec<(String, String)> {
    let udn = format!("uuid:{uuid}");
    vec![
        ("upnp:rootdevice".into(), format!("{udn}::upnp:rootdevice")),
        (udn.clone(), udn.clone()),
        (DEVICE.into(), format!("{udn}::{DEVICE}")),
        (CDS.into(), format!("{udn}::{CDS}")),
        (CMS.into(), format!("{udn}::{CMS}")),
    ]
}

/// Targets a search for `st` should get answers for.
fn matching(uuid: &str, st: &str) -> Vec<(String, String)> {
    targets(uuid).into_iter().filter(|(nt, _)| st == "ssdp:all" || nt.eq_ignore_ascii_case(st)).collect()
}

/// The `ST` of an `M-SEARCH` discovery request, if that's what the message is.
fn parse_msearch(msg: &str) -> Option<String> {
    let mut lines = msg.lines();
    if !lines.next()?.trim().to_ascii_uppercase().starts_with("M-SEARCH * HTTP/1.") {
        return None;
    }
    let mut st = None;
    let mut discover = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match name.trim().to_ascii_uppercase().as_str() {
            "ST" => st = Some(value.to_owned()),
            "MAN" => discover = value.trim_matches('"').eq_ignore_ascii_case("ssdp:discover"),
            _ => {}
        }
    }
    st.filter(|_| discover)
}

fn search_response(st: &str, usn: &str, location: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\nEXT:\r\nLOCATION: {location}\r\n\
         SERVER: {SERVER}\r\nST: {st}\r\nUSN: {usn}\r\nCONTENT-LENGTH: 0\r\n\r\n"
    )
}

fn notify(nt: &str, usn: &str, location: &str, alive: bool) -> String {
    if alive {
        format!(
            "NOTIFY * HTTP/1.1\r\nHOST: {GROUP}:{PORT}\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\n\
             LOCATION: {location}\r\nNT: {nt}\r\nNTS: ssdp:alive\r\nSERVER: {SERVER}\r\nUSN: {usn}\r\n\r\n"
        )
    } else {
        format!("NOTIFY * HTTP/1.1\r\nHOST: {GROUP}:{PORT}\r\nNT: {nt}\r\nNTS: ssdp:byebye\r\nUSN: {usn}\r\n\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "4d696e69-444c-164e-9d41-001ec92f0d00";

    #[test]
    fn answers_tv_searches() {
        // As sent by a Samsung TV.
        let samsung = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\n\
                       ST: urn:schemas-upnp-org:device:MediaServer:1\r\nUSER-AGENT: DLNADOC/1.50 SEC_HHP_[TV] Samsung/1.0\r\n\r\n";
        let st = parse_msearch(samsung).unwrap();
        assert_eq!(st, DEVICE);
        let m = matching(UUID, &st);
        assert_eq!(m, [(DEVICE.to_owned(), format!("uuid:{UUID}::{DEVICE}"))]);
        assert_eq!(matching(UUID, "ssdp:all").len(), 5);
        assert_eq!(matching(UUID, &format!("uuid:{UUID}")).len(), 1);
        assert!(matching(UUID, "urn:schemas-upnp-org:device:MediaRenderer:1").is_empty());

        assert_eq!(parse_msearch("NOTIFY * HTTP/1.1\r\nNT: x\r\n\r\n"), None);
        assert_eq!(parse_msearch("M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n"), None, "MAN is required");
        assert_eq!(parse_msearch("m-search * HTTP/1.1\nman: ssdp:discover\nst: ssdp:all\n").as_deref(), Some("ssdp:all"));
    }

    #[test]
    fn message_format() {
        let r = search_response(DEVICE, "uuid:x::y", "http://192.168.0.100:8200/dlna/description.xml");
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("\r\nEXT:\r\n") && r.contains("LOCATION: http://192.168.0.100:8200/dlna/description.xml\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
        let alive = notify(DEVICE, "uuid:x", "http://a/", true);
        assert!(alive.contains("NTS: ssdp:alive\r\n") && alive.contains("LOCATION: http://a/\r\n"));
        let bye = notify(DEVICE, "uuid:x", "http://a/", false);
        assert!(bye.contains("NTS: ssdp:byebye\r\n") && !bye.contains("LOCATION"));
    }
}
