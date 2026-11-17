//! Finding the Jellyfin server on the local network: Jellyfin answers a UDP
//! broadcast of `who is JellyfinServer?` on port 7359 with a small JSON
//! description of itself.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const PORT: u16 = 7359;
const QUESTION: &[u8] = b"who is JellyfinServer?";
const DEFAULT_HTTP_PORT: u16 = 8096;

/// One server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The server's id, as in `System/Info/Public`.
    pub id: String,
    /// Addresses to try, most likely first.
    pub urls: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Reply {
    id: String,
    #[serde(default)]
    address: Option<String>,
}

/// A reply from `from`. The address the server advertises is often useless
/// from another machine (`127.0.0.1` behind a proxy, a container address),
/// so the address the reply came from is tried too, on the advertised port.
fn parse_reply(data: &[u8], from: Ipv4Addr) -> Option<Found> {
    let reply: Reply = serde_json::from_slice(data).ok()?;
    let advertised = reply.address.as_deref().and_then(|a| url::Url::parse(a).ok());
    let mut urls = Vec::new();
    if let Some(u) = &advertised {
        let usable = match u.host() {
            Some(url::Host::Ipv4(ip)) => !ip.is_loopback() && !ip.is_unspecified(),
            Some(url::Host::Ipv6(ip)) => !ip.is_loopback() && !ip.is_unspecified(),
            Some(url::Host::Domain(d)) => d != "localhost",
            None => false,
        };
        if usable {
            urls.push(u.as_str().trim_end_matches('/').to_owned());
        }
    }
    let port = advertised.as_ref().filter(|u| u.scheme() == "http").and_then(|u| u.port_or_known_default());
    for port in [port, Some(DEFAULT_HTTP_PORT)].into_iter().flatten() {
        let url = format!("http://{from}:{port}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    Some(Found { id: reply.id, urls })
}

/// Broadcasts the question and collects answers for `wait`. Blocking: run it
/// off the UI thread. Only answers from private addresses count, the home
/// network is the only place a local route makes sense.
pub fn discover(wait: Duration) -> std::io::Result<Vec<Found>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.send_to(QUESTION, (Ipv4Addr::BROADCAST, PORT))?;
    let deadline = Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(left))?;
        match socket.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) => {
                let ip = *from.ip();
                if !(ip.is_private() || ip.is_link_local()) {
                    continue;
                }
                if let Some(f) = parse_reply(&buf[..n], ip)
                    && !found.contains(&f)
                {
                    found.push(f);
                }
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
            // Windows reports an ICMP "port unreachable" from an earlier
            // send as an error on the next receive; keep listening.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 10);

    #[test]
    fn loopback_advertisement_falls_back_to_the_sender() {
        let f = parse_reply(br#"{"Address":"http://127.0.0.1:8096","Id":"abc","Name":"jellyfin","EndpointAddress":null}"#, FROM).unwrap();
        assert_eq!(f.id, "abc");
        assert_eq!(f.urls, ["http://192.168.1.10:8096"]);
    }

    #[test]
    fn advertised_address_comes_first_and_its_port_is_reused() {
        let f = parse_reply(br#"{"Address":"http://172.17.0.2:8920/","Id":"abc"}"#, FROM).unwrap();
        assert_eq!(f.urls, ["http://172.17.0.2:8920", "http://192.168.1.10:8920", "http://192.168.1.10:8096"]);
    }

    #[test]
    fn same_address_is_listed_once() {
        let f = parse_reply(br#"{"Address":"http://192.168.1.10:8096","Id":"abc"}"#, FROM).unwrap();
        assert_eq!(f.urls, ["http://192.168.1.10:8096"]);
    }

    #[test]
    fn junk_is_ignored() {
        assert!(parse_reply(b"hello", FROM).is_none());
    }
}
