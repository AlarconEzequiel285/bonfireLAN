//! One game dir per server under `%APPDATA%\bonfireLAN\instances\`. Versions,
//! libraries and assets stay in the shared `.minecraft`, so a new server only
//! downloads its mods.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceMeta {
    pub server_id: String,
    pub server_name: String,
    pub mc_version: String,
    pub loader: String,
    pub fabric_version: Option<String>,
    pub ram_mb: u64,
}

pub fn instances_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(appdata).join("bonfireLAN").join("instances")
}

/// `192.168.1.5:25565` -> `192.168.1.5_25565` (`:` isn't valid in Windows paths).
pub fn sanitize_id(server_id: &str) -> String {
    let s: String = server_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() { "server".to_string() } else { s }
}

pub fn instance_dir(server_id: &str) -> PathBuf {
    instances_dir().join(sanitize_id(server_id))
}

pub fn ensure_instance(meta: &InstanceMeta) -> std::io::Result<PathBuf> {
    let dir = instance_dir(&meta.server_id);
    std::fs::create_dir_all(dir.join("mods"))?;
    std::fs::create_dir_all(dir.join("config"))?;

    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(dir.join("instance.json"), json)?;

    Ok(dir)
}

pub fn mods_dir(server_id: &str) -> PathBuf {
    instance_dir(server_id).join("mods")
}

#[cfg(test)]
mod tests {
    use super::sanitize_id;

    #[test]
    fn sanitizes_server_ids_for_windows_paths() {
        assert_eq!(sanitize_id("192.168.1.5:25565"), "192.168.1.5_25565");
        assert_eq!(sanitize_id("../../evil"), "_.._evil");
        assert_eq!(sanitize_id(""), "server");
    }
}
