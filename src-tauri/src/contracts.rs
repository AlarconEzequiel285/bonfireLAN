//! Mirror of `src/types.ts`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Players {
    pub online: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredServer {
    pub id: String,
    pub ip: String,
    pub port: u16,
    pub name: String,
    pub mc_version: String,
    pub loader: String, // "fabric" | "vanilla" | "unknown"
    pub players: Players,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon_base64: Option<String>,
    pub bonfirelan_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub state: String, // "ok" | "warn" | "missing" | "incompatible"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RamCheck {
    pub state: String,
    pub total_mb: u64,
    pub recommended_mb: u64,
    pub assigned_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub java: Check,
    pub minecraft: Check,
    pub loader: Check,
    pub ram: RamCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModEntry {
    pub name: String,
    pub version: String,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Absolute, or relative to the manifest host when `source` is "lan".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod_id: Option<String>,
    /// From fabric.mod.json: "*" | "client" | "server".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerManifest {
    pub server_name: String,
    pub minecraft: String,
    pub loader: String,
    pub fabric_version: String,
    pub recommended_ram: u64,
    pub mods: Vec<ModEntry>,
}
