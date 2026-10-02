//! Client for the host script's manifest
//! (`GET http://<ip>:<port>/bonfirelan/manifest`).

use std::time::Duration;

use crate::contracts::ServerManifest;

const MANIFEST_PORT: u16 = 25580;
const PROBE_TIMEOUT: Duration = Duration::from_millis(600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

fn manifest_url(ip: &str, port: u16) -> String {
    format!("http://{ip}:{port}/bonfirelan/manifest")
}

pub async fn probe(ip: &str) -> Option<String> {
    let url = manifest_url(ip, MANIFEST_PORT);
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if resp.status().is_success() {
        Some(url)
    } else {
        None
    }
}

#[tauri::command]
pub async fn get_manifest(url: String) -> Result<ServerManifest, String> {
    fetch(&url).await
}

pub async fn fetch(url: &str) -> Result<ServerManifest, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("manifest_unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("manifest_http_{}", resp.status().as_u16()));
    }
    resp.json::<ServerManifest>()
        .await
        .map_err(|e| format!("manifest_parse_error: {e}"))
}
