//! Dedicated servers don't broadcast, so probe 25565 on every host of the local
//! subnets. Anything bigger than a /22 is skipped.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use std::sync::Arc;

const DEFAULT_MC_PORT: u16 = 25565;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const MAX_CONCURRENCY: usize = 256;
const MAX_HOSTS: u32 = 1024;

pub async fn scan() -> Vec<SocketAddr> {
    let hosts = enumerate_hosts();
    if hosts.is_empty() {
        return Vec::new();
    }

    let sem = Arc::new(Semaphore::new(MAX_CONCURRENCY));
    let mut set = JoinSet::new();

    for ip in hosts {
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.ok()?;
            let addr = SocketAddr::new(IpAddr::V4(ip), DEFAULT_MC_PORT);
            match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                Ok(Ok(_stream)) => Some(addr),
                _ => None,
            }
        });
    }

    let mut open = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Some(addr)) = res {
            open.push(addr);
        }
    }
    open
}

fn enumerate_hosts() -> Vec<Ipv4Addr> {
    let mut hosts = Vec::new();
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(i) => i,
        Err(_) => return hosts,
    };

    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            let ip = u32::from(v4.ip);
            let mask = u32::from(v4.netmask);
            if mask == 0 {
                continue;
            }
            let network = ip & mask;
            let broadcast = network | !mask;
            // Usable host count, excluding network + broadcast addresses.
            let count = broadcast.saturating_sub(network);
            if count < 2 || count > MAX_HOSTS {
                continue;
            }
            for h in (network + 1)..broadcast {
                hosts.push(Ipv4Addr::from(h));
            }
        }
    }

    hosts.sort_unstable();
    hosts.dedup();
    hosts
}
