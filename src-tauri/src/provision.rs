//! Downloads vanilla Minecraft (from Mojang) and Fabric (from Fabric Meta) so
//! no official launcher is needed. Files already on disk with a matching hash
//! are skipped, so it's cheap to run before every launch.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const VERSION_MANIFEST: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
const RESOURCES_BASE: &str = "https://resources.download.minecraft.net";
const FABRIC_MAVEN: &str = "https://maven.fabricmc.net/";
const PROGRESS_EVENT: &str = "provision:progress";

// Assets are thousands of tiny files, but the connection is shared at a LAN.
const PARALLEL: usize = 12;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    phase: String,
    message: String,
    current: u64,
    total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareResult {
    pub profile_id: String,
}

pub(crate) fn emit(app: &AppHandle, phase: &str, message: impl Into<String>, current: u64, total: u64) {
    let _ = app.emit(
        PROGRESS_EVENT,
        &Progress {
            phase: phase.to_string(),
            message: message.into(),
            current,
            total,
        },
    );
}

/// Without `fabric_version` (or if Fabric Meta doesn't know it) the latest
/// stable loader is used.
#[tauri::command]
pub async fn prepare_instance(
    app: AppHandle,
    mc_version: String,
    use_fabric: bool,
    fabric_version: Option<String>,
) -> Result<PrepareResult, String> {
    let mc_dir = minecraft_dir()?;
    let client = http_client()?;

    ensure_vanilla(&app, &client, &mc_dir, &mc_version).await?;

    let profile_id = if use_fabric {
        ensure_fabric(&app, &client, &mc_dir, &mc_version, fabric_version.as_deref()).await?
    } else {
        mc_version.clone()
    };

    emit(&app, "done", "Ready", 1, 1);
    Ok(PrepareResult { profile_id })
}

async fn ensure_vanilla(
    app: &AppHandle,
    client: &reqwest::Client,
    mc_dir: &Path,
    version: &str,
) -> Result<(), String> {
    emit(app, "vanilla", format!("Looking up Minecraft {version}…"), 0, 0);

    let manifest: VersionManifest = client
        .get(VERSION_MANIFEST)
        .send()
        .await
        .map_err(|e| format!("Couldn't read Mojang's version manifest: {e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| format!("Version {version} doesn't exist."))?;

    let version_dir = mc_dir.join("versions").join(version);
    std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;
    let json_path = version_dir.join(format!("{version}.json"));

    let vj: VanillaVersion = if json_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&json_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
    } else {
        let text = client
            .get(&entry.url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        std::fs::write(&json_path, &text).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())?
    };

    if let Some(client_dl) = &vj.downloads.client {
        emit(app, "vanilla", "Downloading Minecraft…", 0, 0);
        let jar_path = version_dir.join(format!("{version}.jar"));
        download_file(client, &client_dl.url, &jar_path, client_dl.sha1.as_deref(), true)
            .await?;
    }

    let libs_dir = mc_dir.join("libraries");
    let mut jobs: Vec<DownloadJob> = Vec::new();
    for lib in &vj.libraries {
        if !lib_allowed(&lib.rules) {
            continue;
        }
        if let Some(dl) = &lib.downloads {
            if let Some(a) = &dl.artifact {
                if let (Some(path), Some(url)) = (&a.path, &a.url) {
                    jobs.push(DownloadJob {
                        url: url.clone(),
                        dest: libs_dir.join(path),
                        sha1: a.sha1.clone(),
                    });
                }
            }
            // Pre-1.19 natives live under `classifiers`.
            if let Some(natives) = &lib.natives {
                if let Some(key) = natives.get("windows") {
                    let key = key.replace("${arch}", "64");
                    if let Some(classifiers) = &dl.classifiers {
                        if let Some(a) = classifiers.get(&key) {
                            if let (Some(path), Some(url)) = (&a.path, &a.url) {
                                jobs.push(DownloadJob {
                                    url: url.clone(),
                                    dest: libs_dir.join(path),
                                    sha1: a.sha1.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    download_all(app, client, jobs, "vanilla", "Downloading libraries", true).await?;

    ensure_assets(app, client, mc_dir, &vj).await?;

    Ok(())
}

async fn ensure_assets(
    app: &AppHandle,
    client: &reqwest::Client,
    mc_dir: &Path,
    vj: &VanillaVersion,
) -> Result<(), String> {
    let asset_index = match &vj.asset_index {
        Some(a) => a,
        None => return Ok(()),
    };

    let indexes_dir = mc_dir.join("assets").join("indexes");
    std::fs::create_dir_all(&indexes_dir).map_err(|e| e.to_string())?;
    let index_path = indexes_dir.join(format!("{}.json", asset_index.id));

    let text = if index_path.exists() {
        std::fs::read_to_string(&index_path).map_err(|e| e.to_string())?
    } else {
        emit(app, "assets", "Downloading asset index…", 0, 0);
        let t = client
            .get(&asset_index.url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        std::fs::write(&index_path, &t).map_err(|e| e.to_string())?;
        t
    };

    let index: AssetIndexFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let objects_dir = mc_dir.join("assets").join("objects");

    let mut jobs: Vec<DownloadJob> = Vec::new();
    for obj in index.objects.values() {
        if obj.hash.len() < 2 {
            continue;
        }
        let prefix = &obj.hash[0..2];
        jobs.push(DownloadJob {
            url: format!("{RESOURCES_BASE}/{prefix}/{}", obj.hash),
            dest: objects_dir.join(prefix).join(&obj.hash),
            sha1: Some(obj.hash.clone()),
        });
    }
    // Assets are named by their hash, so an existing file doesn't need re-hashing.
    download_all(app, client, jobs, "assets", "Downloading assets", false).await
}

async fn ensure_fabric(
    app: &AppHandle,
    client: &reqwest::Client,
    mc_dir: &Path,
    game_version: &str,
    wanted_loader: Option<&str>,
) -> Result<String, String> {
    emit(app, "fabric", "Resolving Fabric…", 0, 0);

    let loaders: Vec<FabricLoaderEntry> = client
        .get(format!("{FABRIC_META}/versions/loader/{game_version}"))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Fabric: {e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let pinned = wanted_loader.filter(|v| !v.is_empty());
    let loader = pinned
        .and_then(|v| loaders.iter().find(|l| l.loader.version == v))
        .or_else(|| loaders.iter().find(|l| l.loader.stable))
        .or_else(|| loaders.first())
        .ok_or_else(|| format!("No Fabric loader for {game_version}."))?;
    let loader_version = &loader.loader.version;

    let profile_url =
        format!("{FABRIC_META}/versions/loader/{game_version}/{loader_version}/profile/json");
    let profile_text = client
        .get(&profile_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let profile: FabricProfile =
        serde_json::from_str(&profile_text).map_err(|e| e.to_string())?;
    let profile_id = profile.id.clone();

    let dir = mc_dir.join("versions").join(&profile_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{profile_id}.json")), &profile_text)
        .map_err(|e| e.to_string())?;

    // Fabric Meta gives Maven coordinates without hashes.
    let libs_dir = mc_dir.join("libraries");
    let mut jobs: Vec<DownloadJob> = Vec::new();
    for lib in &profile.libraries {
        let rel = maven_to_path(&lib.name)
            .ok_or_else(|| format!("Invalid Fabric library: {}", lib.name))?;
        let base = lib.url.clone().unwrap_or_else(|| FABRIC_MAVEN.to_string());
        let base = if base.ends_with('/') { base } else { format!("{base}/") };
        jobs.push(DownloadJob {
            url: format!("{base}{rel}"),
            dest: libs_dir.join(&rel),
            sha1: None,
        });
    }
    download_all(app, client, jobs, "fabric", "Downloading Fabric", true).await?;

    Ok(profile_id)
}

/// Downloads a Temurin JRE into our own folder, never touching the system Java.
#[tauri::command]
pub async fn ensure_java(app: AppHandle, mc_version: String) -> Result<String, String> {
    let major = crate::health::required_java_major(&mc_version);
    let base = bonfirelan_dir().join("java").join(major.to_string());

    if let Some(java) = find_java_exe(&base) {
        return Ok(java.to_string_lossy().into_owned());
    }

    let client = http_client()?;

    emit(&app, "java", format!("Looking up Java {major}…"), 0, 0);
    let api = format!(
        "https://api.adoptium.net/v3/assets/latest/{major}/hotspot\
         ?architecture=x64&image_type=jre&os=windows&vendor=eclipse"
    );
    let assets: Vec<AdoptiumAsset> = client
        .get(&api)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Adoptium: {e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let pkg = assets
        .into_iter()
        .next()
        .map(|a| a.binary.package)
        .ok_or_else(|| format!("Adoptium has no Java {major} JRE for Windows x64."))?;

    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let tmp_zip = base.join("download.zip.part");
    download_with_progress(&app, &client, &pkg.link, &tmp_zip, pkg.size).await?;

    emit(&app, "java", "Extracting Java…", 0, 0);
    extract_zip(&tmp_zip, &base)?;
    let _ = std::fs::remove_file(&tmp_zip);

    let java = find_java_exe(&base)
        .ok_or_else(|| "java.exe not found in the extracted JRE.".to_string())?;
    Ok(java.to_string_lossy().into_owned())
}

async fn download_with_progress(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
) -> Result<(), String> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Error downloading Java: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Error downloading Java: {e}"))?;

    let total = expected_size.or_else(|| resp.content_length()).unwrap_or(0);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;

    let mut downloaded: u64 = 0;
    let mut last_emitted: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if downloaded - last_emitted >= 2 * 1024 * 1024 || downloaded == total {
            last_emitted = downloaded;
            let mb = downloaded / (1024 * 1024);
            let total_mb = total / (1024 * 1024);
            emit(
                app,
                "java",
                format!("Downloading Java ({mb}/{total_mb} MB)"),
                downloaded,
                total,
            );
        }
    }
    Ok(())
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        // Guards against zip-slip.
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_owned(),
            None => continue,
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The Temurin zip nests it one level down: `jdk-21.0.2+13-jre/bin/java.exe`.
fn find_java_exe(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("bin").join("java.exe");
    if direct.exists() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_java_exe(&p) {
                return Some(found);
            }
        }
    }
    None
}

fn bonfirelan_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(appdata).join("bonfireLAN")
}

struct DownloadJob {
    url: String,
    dest: PathBuf,
    sha1: Option<String>,
}

async fn download_all(
    app: &AppHandle,
    client: &reqwest::Client,
    jobs: Vec<DownloadJob>,
    phase: &str,
    label: &str,
    verify_existing: bool,
) -> Result<(), String> {
    let total = jobs.len() as u64;
    if total == 0 {
        return Ok(());
    }

    let sem = Arc::new(Semaphore::new(PARALLEL));
    let done = Arc::new(AtomicU64::new(0));
    let mut set = JoinSet::new();

    for job in jobs {
        let client = client.clone();
        let sem = sem.clone();
        let done = done.clone();
        let app = app.clone();
        let phase = phase.to_string();
        let label = label.to_string();
        set.spawn(async move {
            let _permit = sem.acquire().await.map_err(|e| e.to_string())?;
            let res =
                download_file(&client, &job.url, &job.dest, job.sha1.as_deref(), verify_existing)
                    .await;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 20 == 0 || n == total {
                emit(&app, &phase, format!("{label} ({n}/{total})"), n, total);
            }
            res
        });
    }

    let mut first_err: Option<String> = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(e) => {
                first_err.get_or_insert(e.to_string());
            }
        }
    }

    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub(crate) async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    sha1: Option<&str>,
    verify_existing: bool,
) -> Result<(), String> {
    if dest.exists() {
        match (verify_existing, sha1) {
            (false, _) => return Ok(()),
            (true, None) => return Ok(()),
            (true, Some(expected)) => {
                if file_sha1(dest).as_deref() == Some(expected) {
                    return Ok(());
                }
            }
        }
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Error downloading {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Error downloading {url}: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("Error reading {url}: {e}"))?;

    if let Some(expected) = sha1 {
        let actual = sha1_hex(&bytes);
        if actual != expected {
            return Err(format!("Hash mismatch for {url} (expected {expected}, got {actual})"));
        }
    }

    // Rename at the end so a cut download never looks complete.
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

fn minecraft_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA is not set".to_string())?;
    Ok(PathBuf::from(appdata).join(".minecraft"))
}

fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

pub(crate) fn file_sha1(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| sha1_hex(&b))
}

/// Mojang library rules: the last matching rule wins.
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

/// `group:artifact:version[:classifier]` -> `group/artifact/version/file.jar`
fn maven_to_path(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let file = match parts.get(3) {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!("{group}/{artifact}/{version}/{file}"))
}

#[derive(Debug, Deserialize)]
struct VersionManifest {
    versions: Vec<ManifestVersion>,
}

#[derive(Debug, Deserialize)]
struct ManifestVersion {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VanillaVersion {
    downloads: VanillaDownloads,
    #[serde(default)]
    libraries: Vec<Library>,
    asset_index: Option<AssetIndexRef>,
}

#[derive(Debug, Deserialize)]
struct VanillaDownloads {
    client: Option<DownloadRef>,
}

#[derive(Debug, Deserialize)]
struct DownloadRef {
    url: String,
    sha1: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetIndexRef {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct Library {
    name: String,
    #[serde(default)]
    downloads: Option<LibDownloads>,
    #[serde(default)]
    natives: Option<HashMap<String, String>>,
    #[serde(default)]
    rules: Option<Vec<Rule>>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LibDownloads {
    artifact: Option<Artifact>,
    #[serde(default)]
    classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    path: Option<String>,
    url: Option<String>,
    sha1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Rule {
    action: String,
    os: Option<RuleOs>,
}

#[derive(Debug, Deserialize)]
struct RuleOs {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssetIndexFile {
    objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Deserialize)]
struct AssetObject {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoader,
}

#[derive(Debug, Deserialize)]
struct FabricLoader {
    version: String,
    stable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FabricProfile {
    id: String,
    #[serde(default)]
    libraries: Vec<Library>,
}

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    link: String,
    #[serde(default)]
    size: Option<u64>,
}
