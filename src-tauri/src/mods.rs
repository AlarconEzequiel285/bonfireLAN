//! Makes a server's instance match the host's manifest. Jars the player added
//! by hand are never deleted: we only remove what's listed in MANAGED_FILE.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use tauri::AppHandle;

use crate::contracts::{ModEntry, ServerManifest};
use crate::instance::{self, InstanceMeta};
use crate::manifest;
use crate::provision::{download_file, emit, file_sha1, http_client};

const MANAGED_FILE: &str = ".bonfirelan-managed.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSyncResult {
    pub game_dir: String,
    pub manifest: ServerManifest,
    pub installed: Vec<String>,
    pub up_to_date: Vec<String>,
    pub removed: Vec<String>,
    pub skipped_server_only: Vec<String>,
}

#[tauri::command]
pub async fn sync_mods(
    app: AppHandle,
    server_id: String,
    manifest_url: String,
    ram_mb: Option<u64>,
) -> Result<ModSyncResult, String> {
    emit(&app, "mods", "Reading mods…", 0, 0);
    let manifest = manifest::fetch(&manifest_url).await?;

    let meta = InstanceMeta {
        server_id: server_id.clone(),
        server_name: manifest.server_name.clone(),
        mc_version: manifest.minecraft.clone(),
        loader: manifest.loader.clone(),
        fabric_version: Some(manifest.fabric_version.clone()).filter(|v| !v.is_empty()),
        ram_mb: ram_mb.unwrap_or(manifest.recommended_ram),
    };
    let game_dir = instance::ensure_instance(&meta).map_err(|e| e.to_string())?;
    let mods_dir = instance::mods_dir(&server_id);

    let (required, server_only): (Vec<&ModEntry>, Vec<&ModEntry>) =
        manifest.mods.iter().partition(|m| !is_server_only(m));

    let local = local_hashes(&mods_dir);
    let managed = load_managed(&game_dir);
    let plan = plan_sync(&required, &local, &managed);

    let client = http_client()?;
    let total = plan.download.len() as u64;
    let mut installed = Vec::new();
    for (i, m) in plan.download.iter().enumerate() {
        emit(&app, "mods", format!("Downloading {}…", m.name), i as u64, total);
        let url = resolve_url(&manifest_url, m.url.as_deref().unwrap_or_default())
            .ok_or_else(|| format!("Mod {} has no valid download URL.", m.name))?;
        let file = target_file_name(m);
        let hash = m.hash.to_lowercase();
        download_file(&client, &url, &mods_dir.join(&file), valid_sha1(&hash), true)
            .await
            .map_err(|e| format!("Couldn't download mod {}: {e}", m.name))?;
        installed.push(file);
    }

    let mut removed = Vec::new();
    for file in &plan.remove {
        let path = mods_dir.join(file);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Couldn't remove old mod {file}: {e}"))?;
        }
        removed.push(file.clone());
    }

    let mut new_managed: HashSet<String> = plan.keep_managed;
    new_managed.extend(installed.iter().cloned());
    save_managed(&game_dir, &new_managed)?;

    let up_to_date = plan.up_to_date.iter().map(|m| m.name.clone()).collect();
    let skipped_server_only = server_only.iter().map(|m| m.name.clone()).collect();

    emit(&app, "mods", "Mods ready", 1, 1);
    Ok(ModSyncResult {
        game_dir: game_dir.to_string_lossy().into_owned(),
        manifest,
        installed,
        up_to_date,
        removed,
        skipped_server_only,
    })
}

struct SyncPlan<'a> {
    download: Vec<&'a ModEntry>,
    up_to_date: Vec<&'a ModEntry>,
    keep_managed: HashSet<String>,
    remove: Vec<String>,
}

/// `local` maps sha1 -> file name of jars currently in `mods/`; `managed` is
/// the set of file names bonfireLAN installed previously.
fn plan_sync<'a>(
    required: &[&'a ModEntry],
    local: &HashMap<String, String>,
    managed: &HashSet<String>,
) -> SyncPlan<'a> {
    let mut download = Vec::new();
    let mut up_to_date = Vec::new();
    let mut still_needed: HashSet<String> = HashSet::new();

    for &m in required {
        let hash = m.hash.to_lowercase();
        match local.get(&hash) {
            Some(file) if !hash.is_empty() => {
                still_needed.insert(file.clone());
                up_to_date.push(m);
            }
            _ => {
                still_needed.insert(target_file_name(m));
                download.push(m);
            }
        }
    }

    let mut remove: Vec<String> =
        managed.iter().filter(|f| !still_needed.contains(*f)).cloned().collect();
    remove.sort();
    let keep_managed = managed.iter().filter(|f| still_needed.contains(*f)).cloned().collect();

    SyncPlan { download, up_to_date, keep_managed, remove }
}

fn is_server_only(m: &ModEntry) -> bool {
    m.environment.as_deref().map(|e| e.eq_ignore_ascii_case("server")).unwrap_or(false)
}

/// File name to save a mod under. Only a bare `*.jar` name from the manifest is
/// trusted; anything with a path component falls back to `<sha1>.jar`.
fn target_file_name(m: &ModEntry) -> String {
    let candidate = m.file_name.as_deref().unwrap_or_default();
    let safe = !candidate.is_empty()
        && !candidate.contains(['/', '\\', ':'])
        && !candidate.starts_with('.')
        && candidate.to_lowercase().ends_with(".jar");
    if safe {
        candidate.to_string()
    } else {
        format!("{}.jar", m.hash.to_lowercase())
    }
}

/// Relative URLs (`/bonfirelan/mods/x.jar`) resolve against the manifest's host.
fn resolve_url(manifest_url: &str, mod_url: &str) -> Option<String> {
    if mod_url.is_empty() {
        return None;
    }
    let base = reqwest::Url::parse(manifest_url).ok()?;
    let url = base.join(mod_url).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

fn valid_sha1(hash: &str) -> Option<&str> {
    (hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

fn local_hashes(mods_dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(mods_dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_jar = path.extension().map(|e| e.eq_ignore_ascii_case("jar")).unwrap_or(false);
        if !is_jar || !path.is_file() {
            continue;
        }
        if let (Some(hash), Some(name)) = (file_sha1(&path), path.file_name()) {
            out.insert(hash, name.to_string_lossy().into_owned());
        }
    }
    out
}

fn load_managed(game_dir: &Path) -> HashSet<String> {
    std::fs::read_to_string(game_dir.join(MANAGED_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_managed(game_dir: &Path, managed: &HashSet<String>) -> Result<(), String> {
    let mut list: Vec<&String> = managed.iter().collect();
    list.sort();
    let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    std::fs::write(game_dir.join(MANAGED_FILE), json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, hash: &str, file: &str, env: &str) -> ModEntry {
        ModEntry {
            name: name.into(),
            version: "1".into(),
            hash: hash.into(),
            source: Some("lan".into()),
            url: Some(format!("/bonfirelan/mods/{file}")),
            file_name: Some(file.into()),
            mod_id: None,
            environment: Some(env.into()),
            size: None,
        }
    }

    #[test]
    fn plans_downloads_keeps_and_removals() {
        let a = entry("A", "aaa", "a.jar", "*");
        let b = entry("B", "bbb", "b-2.jar", "client");
        let required = vec![&a, &b];

        // a.jar already present; b-1.jar is an old managed version; user.jar is
        // a mod the player added by hand.
        let local: HashMap<String, String> = [
            ("aaa".to_string(), "a.jar".to_string()),
            ("old".to_string(), "b-1.jar".to_string()),
            ("usr".to_string(), "user.jar".to_string()),
        ]
        .into();
        let managed: HashSet<String> = ["a.jar".to_string(), "b-1.jar".to_string()].into();

        let plan = plan_sync(&required, &local, &managed);
        assert_eq!(plan.up_to_date.iter().map(|m| &m.name).collect::<Vec<_>>(), ["A"]);
        assert_eq!(plan.download.iter().map(|m| &m.name).collect::<Vec<_>>(), ["B"]);
        assert_eq!(plan.remove, vec!["b-1.jar".to_string()]); // never user.jar
        assert!(plan.keep_managed.contains("a.jar"));
    }

    #[test]
    fn detects_server_only_mods() {
        assert!(is_server_only(&entry("S", "h", "s.jar", "server")));
        assert!(!is_server_only(&entry("C", "h", "c.jar", "*")));
    }

    #[test]
    fn rejects_unsafe_file_names() {
        let hash = "810b2b0195371a012906241d8b85a32a1d6de53c";
        assert_eq!(target_file_name(&entry("x", hash, "ok-1.0+mc.jar", "*")), "ok-1.0+mc.jar");
        for bad in ["../evil.jar", "a\\b.jar", "C:x.jar", ".hidden.jar", "notajar.zip"] {
            assert_eq!(target_file_name(&entry("x", hash, bad, "*")), format!("{hash}.jar"));
        }
    }

    #[test]
    fn resolves_relative_urls_against_the_manifest_host() {
        let m = "http://192.168.1.5:25580/bonfirelan/manifest";
        assert_eq!(
            resolve_url(m, "/bonfirelan/mods/fabric-api-0.116.17%2B1.21.1.jar").as_deref(),
            Some("http://192.168.1.5:25580/bonfirelan/mods/fabric-api-0.116.17%2B1.21.1.jar")
        );
        assert_eq!(
            resolve_url(m, "https://cdn.modrinth.com/x.jar").as_deref(),
            Some("https://cdn.modrinth.com/x.jar")
        );
        assert_eq!(resolve_url(m, "file:///C:/secret"), None);
        assert_eq!(resolve_url(m, ""), None);
    }
}
