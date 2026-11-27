//! The host half of the sandbox proxy: resolve a name once, check every
//! address, and dial only a checked address. The container runs with no
//! network, so this is its one way out.
//!
//! Closed means private, loopback, link-local, metadata, CGNAT, IPv6 ULA,
//! multicast, unspecified, documentation, benchmark or broadcast, the
//! IPv4-mapped and NAT64 forms of each, and any address this host owns. One
//! closed address in an answer refuses the whole answer, so a rebinding name
//! that mixes a public and a private address gets nothing.

use super::{Refusal, Res};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The only ports the proxy dials.
pub const PORTS: [u16; 2] = [80, 443];
const DIAL_BOUND: Duration = Duration::from_secs(10);

fn closed_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        // CGNAT 100.64.0.0/10, which tailnets also use.
        || (a == 100 && (b & 0xc0) == 64)
        // IETF protocol assignments 192.0.0.0/24, benchmark 198.18.0.0/15.
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b & 0xfe) == 18)
        // Reserved 240.0.0.0/4.
        || a >= 240
}

fn closed_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return closed_v4(v4);
    }
    let s = ip.segments();
    // NAT64 64:ff9b::/96 carries an IPv4 address in its low 32 bits.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return closed_v4(v4);
    }
    // The local-use NAT64 prefix 64:ff9b:1::/48 (RFC 8215) maps to whatever
    // the local translator chooses, so all of it is closed.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
        return true;
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // ULA fc00::/7, link-local fe80::/10, site-local fec0::/10.
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // Documentation 2001:db8::/32, and the IPv4-compatible ::/96.
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        || s[0..6] == [0, 0, 0, 0, 0, 0]
}

/// Whether the proxy refuses `ip`. `owned` is this host's own addresses.
pub fn closed(ip: IpAddr, owned: &[IpAddr]) -> bool {
    owned.contains(&ip)
        || match ip {
            IpAddr::V4(v4) => closed_v4(v4),
            IpAddr::V6(v6) => closed_v6(v6),
        }
}

/// The addresses a remote engine host resolves to. The proxy dials from the
/// fno host, so without this a worker could reach its own VM's services by
/// the VM's public address. A failed lookup answers empty.
pub fn remote_addresses(engine_host: &str) -> Vec<IpAddr> {
    use std::net::ToSocketAddrs as _;
    let Ok(dest) = super::engine::SshHost::parse(engine_host) else {
        return Vec::new();
    };
    (dest.host.as_str(), dest.port.unwrap_or(22))
        .to_socket_addrs()
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// Every address on this host's interfaces. A read failure answers empty,
/// which still leaves every private range closed.
pub fn host_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    // SAFETY: getifaddrs fills a list we walk read-only and free once.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let addr = (*cur).ifa_addr;
            if !addr.is_null() {
                match i32::from((*addr).sa_family) {
                    libc::AF_INET => {
                        let sin = &*(addr as *const libc::sockaddr_in);
                        out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                            sin.sin_addr.s_addr,
                        ))));
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(addr as *const libc::sockaddr_in6);
                        out.push(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

/// Resolve `host:port` once and check the whole answer. The error names the
/// reason and never echoes guest text unescaped.
pub fn checked_addrs(host: &str, port: u16, owned: &[IpAddr]) -> Res<Vec<SocketAddr>> {
    if !PORTS.contains(&port) {
        return Err(Refusal(format!(
            "port {port} is closed; the proxy dials 80 and 443"
        )));
    }
    if host.is_empty() || host.len() > 253 || !host.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Refusal(format!(
            "host {} is not a name the proxy resolves",
            host.escape_debug()
        )));
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", host.escape_debug()))?
        .collect();
    if addrs.is_empty() {
        return Err(Refusal(format!(
            "resolve {}: no address",
            host.escape_debug()
        )));
    }
    if let Some(bad) = addrs.iter().find(|a| closed(a.ip(), owned)) {
        return Err(Refusal(format!(
            "{} resolves to {}, a closed address; the whole answer is refused",
            host.escape_debug(),
            bad.ip()
        )));
    }
    Ok(addrs)
}

/// Dial the first checked address that answers. The socket connects to the
/// address that passed the check, never to a fresh resolve of the name.
pub fn dial(host: &str, port: u16, owned: &[IpAddr]) -> Res<TcpStream> {
    let addrs = checked_addrs(host, port, owned)?;
    let mut last = String::new();
    for a in &addrs {
        match TcpStream::connect_timeout(a, DIAL_BOUND) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(Refusal(format!("dial {}: {last}", host.escape_debug())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every closed range refuses, in v4, mapped and NAT64 forms; a public
    /// address and a host-owned public address split as they should; and a
    /// closed port or a literal private host refuses before any dial.
    #[test]
    fn closed_ranges_refuse_and_public_passes() {
        let shut = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "192.0.0.8",
            "240.0.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "64:ff9b:1::101:101",
            "2001:db8::1",
            "ff02::1",
        ];
        for s in shut {
            assert!(closed(s.parse().unwrap(), &[]), "{s} must be closed");
        }
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "64:ff9b::101:101",
        ] {
            assert!(!closed(s.parse().unwrap(), &[]), "{s} must be open");
        }
        let mine: IpAddr = "203.0.114.7".parse().unwrap();
        assert!(closed(mine, &[mine]), "a host-owned address is closed");
        assert!(checked_addrs("1.1.1.1", 22, &[])
            .unwrap_err()
            .0
            .contains("port 22"));
        assert!(checked_addrs("169.254.169.254", 80, &[])
            .unwrap_err()
            .0
            .contains("closed address"));
        assert!(checked_addrs("localhost", 443, &[]).is_err());
        assert!(checked_addrs("a\u{1b}[2J", 443, &[])
            .unwrap_err()
            .0
            .contains("\\u{1b}"));
    }
}
