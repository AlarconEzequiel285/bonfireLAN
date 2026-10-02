//! Offline-mode launch: resolves the profile chain (Fabric inherits from
//! vanilla), builds the classpath, extracts natives and spawns Java with
//! auto-connect.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::process::Command;

const EXIT_EVENT: &str = "launch:exited";

// Only catches early crashes; anything later (world load, connecting) shows up
// through EXIT_EVENT and the log.
const CRASH_WATCH_MS: u64 = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchConfig {
    pub java_path: String,
    pub mc_version: String,
    pub ram_mb: u64,
    pub server_ip: String,
    pub server_port: u16,
    pub use_fabric: bool,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Per-server instance dir. Versions, libraries and assets still come from
    /// the shared `.minecraft`.
    #[serde(default)]
    pub game_dir: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub success: bool,
    pub message: String,
    pub pid: Option<u32>,
    pub log_path: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LaunchExit {
    code: Option<i32>,
    log_tail: String,
    log_path: String,
}

pub async fn launch(app: AppHandle, config: LaunchConfig) -> Result<LaunchResult, String> {
    let mc_dir = minecraft_dir()?;
    let versions_dir = mc_dir.join("versions");
    let libraries_dir = mc_dir.join("libraries");
    let assets_dir = mc_dir.join("assets");

    let game_dir = match config.game_dir.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => {
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            dir
        }
        None => mc_dir.clone(),
    };

    let profile_id = if let Some(id) = config.profile_id.clone().filter(|id| !id.is_empty()) {
        id
    } else if config.use_fabric {
        find_fabric_profile(&versions_dir, &config.mc_version).ok_or_else(|| {
            format!(
                "No Fabric profile found for {}.",
                config.mc_version
            )
        })?
    } else {
        config.mc_version.clone()
    };

    let (chain, root_id) = resolve_chain(&versions_dir, &profile_id)?;

    let base_jar = versions_dir
        .join(&root_id)
        .join(format!("{}.jar", root_id));
    if !base_jar.exists() {
        return Err(format!(
            "Minecraft client jar not found ({}).",
            root_id
        ));
    }

    let merged_libs = merge_libraries(&chain);

    let natives_dir = versions_dir.join(&profile_id).join("natives");
    std::fs::create_dir_all(&natives_dir).map_err(|e| e.to_string())?;
    // A stale or wrong-arch dll from a previous launch would shadow the right one.
    clear_dlls(&natives_dir);

    let mut classpath: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut native_jars: Vec<String> = Vec::new();
    let mut unresolved_natives: Vec<String> = Vec::new();

    for lib in &merged_libs {
        if !lib_allowed(&lib.rules) {
            continue;
        }
        if is_natives_lib(lib) {
            if !native_matches_host(lib) {
                continue;
            }
            match natives_jar_path(lib) {
                Some(rel) => {
                    let full = libraries_dir.join(&rel);
                    if full.exists() {
                        extract_natives(&full, &natives_dir).map_err(|e| {
                            format!("Couldn't extract natives from {rel}: {e}")
                        })?;
                        native_jars.push(rel);
                    } else {
                        missing.push(rel);
                    }
                }
                None => unresolved_natives.push(lib.name.clone()),
            }
        } else if let Some(rel) = lib_jar_path(lib) {
            let full = libraries_dir.join(&rel);
            if full.exists() {
                classpath.push(full.to_string_lossy().into_owned());
            } else {
                missing.push(rel);
            }
        }
    }
    classpath.push(base_jar.to_string_lossy().into_owned());

    if !missing.is_empty() {
        return Err(format!(
            "{} library files are missing, e.g. {}",
            missing.len(),
            missing.into_iter().take(3).collect::<Vec<_>>().join(", ")
        ));
    }

    // Without this the JVM dies later with an opaque "Failed to locate library: lwjgl.dll".
    let extracted_dlls = list_dlls(&natives_dir);
    if extracted_dlls.is_empty() {
        return Err(format!(
            "No native libraries were extracted to {}.\nProcessed: [{}]\nUnresolved: [{}]",
            natives_dir.display(),
            native_jars.join(", "),
            unresolved_natives.join(", "),
        ));
    }

    let main_class = chain
        .iter()
        .find_map(|v| v.main_class.clone())
        .ok_or_else(|| "Profile has no mainClass.".to_string())?;

    let asset_index = chain
        .iter()
        .find_map(|v| v.asset_index.as_ref().map(|a| a.id.clone()))
        .or_else(|| chain.iter().find_map(|v| v.assets.clone()))
        .unwrap_or_else(|| "legacy".to_string());

    let username = offline_username(config.username.as_deref());
    let uuid = offline_uuid(&username);

    // Args come from the version JSON. Hardcoding a minimal set drops JVM args
    // newer versions need to open their window.
    let cp_joined = classpath.join(";");
    let placeholders: Vec<(&str, String)> = vec![
        ("${natives_directory}", natives_dir.to_string_lossy().into_owned()),
        ("${library_directory}", libraries_dir.to_string_lossy().into_owned()),
        ("${classpath}", cp_joined.clone()),
        ("${classpath_separator}", ";".to_string()),
        ("${launcher_name}", "bonfireLAN".to_string()),
        ("${launcher_version}", env!("CARGO_PKG_VERSION").to_string()),
        ("${auth_player_name}", username.clone()),
        ("${version_name}", profile_id.clone()),
        ("${game_directory}", game_dir.to_string_lossy().into_owned()),
        ("${assets_root}", assets_dir.to_string_lossy().into_owned()),
        ("${game_assets}", assets_dir.to_string_lossy().into_owned()),
        ("${assets_index_name}", asset_index.clone()),
        ("${auth_uuid}", uuid.clone()),
        ("${auth_access_token}", "0".to_string()),
        ("${auth_session}", "0".to_string()),
        ("${auth_xuid}", String::new()),
        ("${clientid}", String::new()),
        ("${user_type}", "legacy".to_string()),
        ("${version_type}", "release".to_string()),
        ("${user_properties}", "{}".to_string()),
        ("${profile_name}", profile_id.clone()),
    ];

    let mut jvm_args: Vec<String> = Vec::new();
    let mut game_args: Vec<String> = Vec::new();
    let mut has_modern = false;

    // Base profile first, so Fabric's args extend vanilla's.
    for vj in chain.iter().rev() {
        if let Some(a) = &vj.arguments {
            has_modern = true;
            for entry in &a.jvm {
                collect_arg(entry, &mut jvm_args, &placeholders);
            }
            for entry in &a.game {
                collect_arg(entry, &mut game_args, &placeholders);
            }
        }
    }

    if !has_modern {
        // <= 1.12
        jvm_args.push(format!("-Djava.library.path={}", natives_dir.to_string_lossy()));
        jvm_args.push("-cp".into());
        jvm_args.push(cp_joined.clone());
        if let Some(mc_args) = chain.iter().find_map(|v| v.minecraft_arguments.clone()) {
            for tok in mc_args.split_whitespace() {
                game_args.push(substitute(tok, &placeholders));
            }
        }
    }

    if !jvm_args.iter().any(|a| a == "-cp") {
        jvm_args.push("-cp".into());
        jvm_args.push(cp_joined.clone());
    }

    let mut args: Vec<String> = Vec::new();
    args.push(format!("-Xmx{}m", config.ram_mb));
    args.push(format!("-Xms{}m", config.ram_mb / 2));
    args.extend(jvm_args);
    args.push(main_class);
    args.extend(game_args);

    // --server/--port were removed in 1.20.
    if version_at_least(&root_id, 1, 20) {
        args.push("--quickPlayMultiplayer".into());
        args.push(format!("{}:{}", config.server_ip, config.server_port));
    } else {
        args.push("--server".into());
        args.push(config.server_ip.clone());
        args.push("--port".into());
        args.push(config.server_port.to_string());
    }

    let logs_dir = bonfirelan_dir().join("logs");
    std::fs::create_dir_all(&logs_dir).map_err(|e| e.to_string())?;
    let log_path = logs_dir.join(format!("launch-{}.log", profile_id));

    let header = format!(
        "=== bonfireLAN launch diagnostics ===\n\
         arch: {}\n\
         java: {}\n\
         profile: {}  (root {})\n\
         game_dir: {}\n\
         natives_dir: {}\n\
         natives (.dll): [{}]\n\
         classpath entries: {}\n\
         command:\n{} {}\n\
         ===================================\n\n",
        std::env::consts::ARCH,
        config.java_path,
        profile_id,
        root_id,
        game_dir.display(),
        natives_dir.display(),
        extracted_dlls.join(", "),
        classpath.len(),
        config.java_path,
        args.join(" "),
    );
    std::fs::write(&log_path, header).map_err(|e| e.to_string())?;

    let log_file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;
    let log_file2 = log_file.try_clone().map_err(|e| e.to_string())?;

    let mut child = Command::new(&config.java_path)
        .args(&args)
        .current_dir(&game_dir)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file2))
        .spawn()
        .map_err(|e| format!("Couldn't run Java: {}", e))?;

    let pid = child.id();
    let log_path_str = log_path.to_string_lossy().into_owned();

    match tokio::time::timeout(Duration::from_millis(CRASH_WATCH_MS), child.wait()).await {
        Err(_) => {
            let app = app.clone();
            let log_path = log_path.clone();
            tokio::spawn(async move {
                let code = match child.wait().await {
                    Ok(status) => status.code(),
                    Err(_) => None,
                };
                let tail = read_tail(&log_path, 4000);
                let _ = app.emit(
                    EXIT_EVENT,
                    &LaunchExit {
                        code,
                        log_tail: tail,
                        log_path: log_path.to_string_lossy().into_owned(),
                    },
                );
            });
            Ok(LaunchResult {
                success: true,
                message: "Minecraft is running".to_string(),
                pid,
                log_path: log_path_str,
            })
        }
        Ok(Ok(status)) => {
            let tail = read_tail(&log_path, 4000);
            Err(format!(
                "Minecraft exited right away (code {}). Log: {}\n\n{}",
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                log_path_str,
                tail
            ))
        }
        Ok(Err(e)) => Err(format!("Error waiting for Minecraft: {}", e)),
    }
}

fn minecraft_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA is not set".to_string())?;
    Ok(PathBuf::from(appdata).join(".minecraft"))
}

fn bonfirelan_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(appdata).join("bonfireLAN")
}

fn find_fabric_profile(versions_dir: &Path, mc_version: &str) -> Option<String> {
    let entries = std::fs::read_dir(versions_dir).ok()?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if lower.contains("fabric") && name.contains(mc_version) {
            if versions_dir.join(&name).join(format!("{}.json", name)).exists() {
                return Some(name);
            }
        }
    }
    None
}

/// Child first, base vanilla last. Also returns the root id, whose jar we launch.
fn resolve_chain(versions_dir: &Path, id: &str) -> Result<(Vec<VersionJson>, String), String> {
    let mut chain = Vec::new();
    let mut current = id.to_string();
    let mut guard = 0;

    loop {
        let json_path = versions_dir.join(&current).join(format!("{}.json", current));
        if !json_path.exists() {
            return Err(format!("Profile {} not found ({}).", current, json_path.display()));
        }
        let data = std::fs::read_to_string(&json_path).map_err(|e| e.to_string())?;
        let vj: VersionJson = serde_json::from_str(&data)
            .map_err(|e| format!("Invalid profile {}: {}", current, e))?;

        let inherits = vj.inherits_from.clone();
        chain.push(vj);

        match inherits {
            Some(parent) => current = parent,
            None => break,
        }
        guard += 1;
        if guard > 16 {
            return Err("Profile inheritance chain too long (cycle?).".into());
        }
    }

    Ok((chain, current))
}

/// First occurrence of each group:artifact:classifier wins, so the child's version is kept.
fn merge_libraries(chain: &[VersionJson]) -> Vec<Library> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for vj in chain {
        for lib in &vj.libraries {
            let parts: Vec<&str> = lib.name.split(':').collect();
            let key = format!(
                "{}:{}:{}",
                parts.first().copied().unwrap_or(""),
                parts.get(1).copied().unwrap_or(""),
                parts.get(3).copied().unwrap_or(""),
            );
            if seen.insert(key) {
                out.push(lib.clone());
            }
        }
    }
    out
}

/// Mojang library rules, Windows only. Last matching rule wins.
fn lib_allowed(rules: &Option<Vec<Rule>>) -> bool {
    let rules = match rules {
        Some(r) if !r.is_empty() => r,
        _ => return true,
    };
    let mut allowed = false;
    for rule in rules {
        let matches = match &rule.os {
            Some(os) => os.name.as_deref().map_or(true, |n| n == "windows"),
            None => true,
        };
        if matches {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn is_natives_lib(lib: &Library) -> bool {
    lib.natives.is_some() || lib.name.contains("natives-")
}

fn lib_jar_path(lib: &Library) -> Option<String> {
    if let Some(dl) = &lib.downloads {
        if let Some(a) = &dl.artifact {
            if let Some(p) = &a.path {
                return Some(p.clone());
            }
        }
    }
    maven_to_path(&lib.name)
}

fn natives_jar_path(lib: &Library) -> Option<String> {
    // <= 1.18: a `natives` map plus `classifiers` downloads.
    if let Some(natives) = &lib.natives {
        let key = natives.get("windows")?.replace("${arch}", "64");
        if let Some(dl) = &lib.downloads {
            if let Some(classifiers) = &dl.classifiers {
                if let Some(a) = classifiers.get(&key) {
                    if let Some(p) = &a.path {
                        return Some(p.clone());
                    }
                }
            }
        }
        return maven_to_path(&format!("{}:{}", lib.name, key));
    }
    // 1.19+: natives are their own library with a classifier.
    if lib.name.contains("natives-windows") {
        return lib_jar_path(lib);
    }
    None
}

/// `group:artifact:version[:classifier]` -> relative Maven path.
fn maven_to_path(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let file = match parts.get(3) {
        Some(classifier) => format!("{}-{}-{}.jar", artifact, version, classifier),
        None => format!("{}-{}.jar", artifact, version),
    };
    Some(format!("{}/{}/{}/{}", group, artifact, version, file))
}

fn clear_dlls(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("dll")) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

fn list_dlls(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("dll")) {
                if let Some(n) = p.file_name() {
                    out.push(n.to_string_lossy().into_owned());
                }
            }
        }
    }
    out.sort();
    out
}

fn native_classifier(lib: &Library) -> Option<String> {
    if let Some(natives) = &lib.natives {
        return natives.get("windows").map(|k| k.replace("${arch}", "64"));
    }
    lib.name.split(':').nth(3).map(|s| s.to_string())
}

// Mojang gates the x64, x86 and arm64 Windows natives only on os.name, so
// without this a wrong-arch lwjgl.dll can overwrite the right one.
fn native_matches_host(lib: &Library) -> bool {
    match native_classifier(lib) {
        Some(c) => arch_matches(&c),
        None => true,
    }
}

fn arch_matches(classifier: &str) -> bool {
    let c = classifier.to_lowercase();
    let is_arm = c.contains("arm") || c.contains("aarch");
    let is_x86_64 = c.contains("x86-64") || c.contains("x86_64") || c.contains("x64");
    let is_x86_32 = !is_x86_64 && (c.contains("x86") || c.contains("i386"));
    match std::env::consts::ARCH {
        "x86_64" => !is_arm && !is_x86_32,
        "x86" => is_x86_32,
        "aarch64" | "arm" => is_arm,
        _ => true,
    }
}

fn extract_natives(jar_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(jar_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name.starts_with("META-INF") || !name.to_lowercase().ends_with(".dll") {
            continue;
        }
        let file_name = match Path::new(&name).file_name() {
            Some(f) => f.to_owned(),
            None => continue,
        };
        let out_path = dest.join(file_name);
        let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Minecraft names: 3-16 chars of `[A-Za-z0-9_]`.
fn sanitize_username(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(16)
        .collect();
    (cleaned.len() >= 3).then_some(cleaned)
}

pub fn default_username() -> String {
    sanitize_username(&std::env::var("USERNAME").unwrap_or_default()).unwrap_or_else(|| "Player".into())
}

fn offline_username(custom: Option<&str>) -> String {
    custom.and_then(sanitize_username).unwrap_or_else(default_username)
}

/// Same as Java's `UUID.nameUUIDFromBytes("OfflinePlayer:<name>")`, which is
/// what offline-mode servers use.
fn offline_uuid(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{}", name).as_bytes());
    let mut b = hasher.finalize();
    b[6] = (b[6] & 0x0f) | 0x30; // version 3
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn collect_arg(entry: &ArgEntry, out: &mut Vec<String>, placeholders: &[(&str, String)]) {
    match entry {
        ArgEntry::Plain(s) => out.push(substitute(s, placeholders)),
        ArgEntry::Conditional { rules, value } => {
            if !arg_rules_allow(rules) {
                return;
            }
            match value {
                ArgValue::One(s) => out.push(substitute(s, placeholders)),
                ArgValue::Many(v) => {
                    for s in v {
                        out.push(substitute(s, placeholders));
                    }
                }
            }
        }
    }
}

/// Default-deny, last match wins. Rules gated on `features` (demo, custom
/// resolution, quick play) never match: we add our own auto-connect args.
fn arg_rules_allow(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule.features.is_some() {
            continue;
        }
        let os_matches = match &rule.os {
            Some(os) => os.name.as_deref().map_or(true, |n| n == "windows"),
            None => true,
        };
        if os_matches {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn substitute(s: &str, placeholders: &[(&str, String)]) -> String {
    let mut out = s.to_string();
    for (k, v) in placeholders {
        if out.contains(k) {
            out = out.replace(k, v);
        }
    }
    out
}

fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut it = version.split('.').filter_map(|p| p.parse::<u32>().ok());
    let v_major = it.next().unwrap_or(0);
    let v_minor = it.next().unwrap_or(0);
    (v_major, v_minor) >= (major, minor)
}

fn read_tail(path: &Path, max_chars: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            let chars: Vec<char> = s.chars().collect();
            if chars.len() > max_chars {
                chars[chars.len() - max_chars..].iter().collect()
            } else {
                chars.iter().collect()
            }
        }
        Err(_) => "(couldn't read the launch log)".to_string(),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionJson {
    #[allow(dead_code)]
    id: String,
    inherits_from: Option<String>,
    main_class: Option<String>,
    #[serde(default)]
    libraries: Vec<Library>,
    assets: Option<String>,
    asset_index: Option<AssetIndex>,
    /// 1.13+
    #[serde(default)]
    arguments: Option<Arguments>,
    /// <= 1.12
    minecraft_arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Arguments {
    #[serde(default)]
    jvm: Vec<ArgEntry>,
    #[serde(default)]
    game: Vec<ArgEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ArgEntry {
    Plain(String),
    Conditional { rules: Vec<Rule>, value: ArgValue },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ArgValue {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
struct AssetIndex {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Library {
    name: String,
    #[serde(default)]
    downloads: Option<LibraryDownloads>,
    #[serde(default)]
    natives: Option<HashMap<String, String>>,
    #[serde(default)]
    rules: Option<Vec<Rule>>,
}

#[derive(Debug, Clone, Deserialize)]
struct LibraryDownloads {
    artifact: Option<Artifact>,
    #[serde(default)]
    classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Artifact {
    path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Rule {
    action: String,
    os: Option<RuleOs>,
    #[serde(default)]
    features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RuleOs {
    name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::sanitize_username;

    #[test]
    fn sanitizes_usernames() {
        assert_eq!(sanitize_username("Eze Alarcón!").as_deref(), Some("EzeAlarcn"));
        assert_eq!(sanitize_username("a_very_long_name_here").as_deref(), Some("a_very_long_name"));
        assert_eq!(sanitize_username("ab"), None);
    }
}
