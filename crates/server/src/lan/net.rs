//! This PC's IPv4 addresses on the local network.

use std::net::{IpAddr, Ipv4Addr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAddr {
    /// Adapter name as Windows shows it, e.g. `Wi-Fi`.
    pub name: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// Hyper-V / WSL / VirtualBox / VPN adapters: real devices on the home network can't
    /// reach these addresses, so they are listed last.
    pub is_virtual: bool,
}

impl LocalAddr {
    pub fn contains(&self, other: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        u32::from(self.ip) & mask == u32::from(other) & mask
    }
}

/// Usable addresses (up, not loopback or link-local), real adapters first.
pub fn local_addrs() -> Vec<LocalAddr> {
    let mut addrs: Vec<LocalAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.is_oper_up() && !i.is_loopback() && !i.is_link_local())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(LocalAddr {
                is_virtual: is_virtual(&i.name),
                name: i.name,
                ip: v4.ip,
                netmask: v4.netmask,
            }),
            _ => None,
        })
        .collect();
    addrs.sort_by_key(|a| (a.is_virtual, !a.ip.is_private()));
    addrs
}

fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["vethernet", "wsl", "hyper-v", "virtualbox", "vmware", "vpn", "tailscale", "zerotier", "loopback", "docker"]
        .iter()
        .any(|v| n.contains(v))
}

/// The address of ours that `peer` can reach: the one on the same subnet, else whatever
/// Windows would route through (connecting a UDP socket sends nothing).
pub fn local_ip_for(peer: IpAddr, addrs: &[LocalAddr]) -> Option<Ipv4Addr> {
    let IpAddr::V4(peer) = to_v4(peer) else { return None };
    if peer.is_loopback() {
        return Some(Ipv4Addr::LOCALHOST);
    }
    if let Some(a) = addrs.iter().find(|a| a.contains(peer)) {
        return Some(a.ip);
    }
    let sock = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((peer, 1900)).ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// `::ffff:1.2.3.4` to `1.2.3.4`.
pub fn to_v4(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_match_and_virtual_adapters() {
        let wifi = LocalAddr {
            name: "Wi-Fi".into(),
            ip: Ipv4Addr::new(192, 168, 0, 100),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            is_virtual: false,
        };
        let wsl = LocalAddr {
            name: "vEthernet (WSL (Hyper-V firewall))".into(),
            ip: Ipv4Addr::new(172, 29, 128, 1),
            netmask: Ipv4Addr::new(255, 255, 240, 0),
            is_virtual: true,
        };
        assert!(is_virtual(&wsl.name) && !is_virtual(&wifi.name));
        let addrs = [wsl, wifi];
        assert_eq!(local_ip_for("192.168.0.42".parse().unwrap(), &addrs), Some(Ipv4Addr::new(192, 168, 0, 100)));
        assert_eq!(local_ip_for("172.29.130.7".parse().unwrap(), &addrs), Some(Ipv4Addr::new(172, 29, 128, 1)));
        assert_eq!(local_ip_for("::ffff:127.0.0.1".parse().unwrap(), &addrs), Some(Ipv4Addr::LOCALHOST));
    }
}
