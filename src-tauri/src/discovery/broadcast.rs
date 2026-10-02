//! "Open to LAN" worlds multicast `[MOTD]...[/MOTD][AD]port[/AD]` to
//! 224.0.2.60:4445 every ~1.5s. The host IP is the datagram source.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::{timeout_at, Instant};

const MULTICAST_IP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
const MULTICAST_PORT: u16 = 4445;

pub async fn listen(window: Duration) -> Vec<SocketAddr> {
    let socket = match bind_multicast() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let deadline = Instant::now() + window;
    let mut found: HashSet<SocketAddr> = HashSet::new();
    let mut buf = [0u8; 1024];

    while let Ok(Ok((len, src))) = timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let payload = String::from_utf8_lossy(&buf[..len]);
        if let (Some(port), SocketAddr::V4(v4)) = (parse_announcement(&payload), src) {
            found.insert(SocketAddr::from(SocketAddrV4::new(*v4.ip(), port)));
        }
    }

    found.into_iter().collect()
}

/// `SO_REUSEADDR` lets the real Minecraft client listen at the same time.
fn bind_multicast() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    let bind_addr = SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MULTICAST_PORT));
    socket.bind(&bind_addr.into())?;
    socket.join_multicast_v4(&MULTICAST_IP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_nonblocking(true)?;
    let std_sock: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_sock)
}

/// Some versions put `host:port` in `[AD]`, so take what follows the last colon.
fn parse_announcement(data: &str) -> Option<u16> {
    let ad = between(data, "[AD]", "[/AD]")?;
    ad.rsplit(':').next()?.trim().parse().ok()
}

fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(close)?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::parse_announcement;

    #[test]
    fn parses_port() {
        assert_eq!(parse_announcement("[MOTD]Cumple-Eze SMP[/MOTD][AD]25565[/AD]"), Some(25565));
    }

    #[test]
    fn parses_host_port_form() {
        assert_eq!(parse_announcement("[MOTD]x[/MOTD][AD]192.168.1.5:25566[/AD]"), Some(25566));
    }

    #[test]
    fn rejects_missing_port() {
        assert!(parse_announcement("[MOTD]no port[/MOTD]").is_none());
    }
}
