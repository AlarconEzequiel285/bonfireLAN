mod broadcast;
mod slp;
mod sweep;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::net::lookup_host;
use tokio::task::JoinSet;

use crate::contracts::DiscoveredServer;
use crate::manifest;

const BROADCAST_WINDOW: Duration = Duration::from_millis(2500);
const SERVER_FOUND_EVENT: &str = "scan:server-found";
const DEFAULT_MC_PORT: u16 = 25565;

#[tauri::command]
pub async fn ping_server(ip: String, port: Option<u16>) -> Result<DiscoveredServer, String> {
    let port = port.unwrap_or(DEFAULT_MC_PORT);
    let addr = lookup_host((ip.as_str(), port))
        .await
        .map_err(|_| "invalid_address".to_string())?
        .next()
        .ok_or_else(|| "invalid_address".to_string())?;

    let mut server = slp::ping(addr)
        .await
        .ok_or_else(|| "server_unreachable".to_string())?;

    if let Some(url) = manifest::probe(&server.ip).await {
        server.bonfirelan_ready = true;
        server.manifest_url = Some(url);
        server.loader = "fabric".to_string();
    }
    Ok(server)
}

#[tauri::command]
pub async fn scan_lan(app: AppHandle) -> Result<Vec<DiscoveredServer>, String> {
    let broadcast_task = tokio::spawn(broadcast::listen(BROADCAST_WINDOW));
    let sweep_task = tokio::spawn(sweep::scan());

    let announced = broadcast_task.await.unwrap_or_default();
    let swept = sweep_task.await.unwrap_or_default();
    let candidates: HashSet<SocketAddr> = announced.into_iter().chain(swept).collect();

    let mut set = JoinSet::new();
    for addr in candidates {
        let app = app.clone();
        set.spawn(async move {
            let mut server = slp::ping(addr).await?;
            // Best-effort: a host manifest means Fabric + mod sync.
            if let Some(url) = manifest::probe(&server.ip).await {
                server.bonfirelan_ready = true;
                server.manifest_url = Some(url);
                server.loader = "fabric".to_string();
            }
            let _ = app.emit(SERVER_FOUND_EVENT, &server);
            Some(server)
        });
    }

    let mut servers = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Some(server)) = res {
            servers.push(server);
        }
    }

    servers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(servers)
}
