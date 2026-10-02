use std::path::{Path, PathBuf};
use std::process::Stdio;

use regex::Regex;
use sysinfo::System;
use tokio::process::Command;

use crate::contracts::{Check, HealthReport, RamCheck};

pub async fn run(required_mc_version: Option<&str>) -> HealthReport {
    let java = detect_java(required_mc_version).await;
    let minecraft = detect_minecraft();
    let loader = detect_fabric(required_mc_version);
    let ram = detect_ram(required_mc_version);

    HealthReport { java, minecraft, loader, ram }
}

struct JavaInstall {
    path: PathBuf,
    version: String,
    major: u32,
}

async fn detect_java(required_mc_version: Option<&str>) -> Check {
    let mut installs: Vec<JavaInstall> = Vec::new();
    for path in find_java_binaries() {
        if let Some(version) = get_java_version(&path).await {
            if let Some(major) = java_major(&version) {
                installs.push(JavaInstall { path, version, major });
            }
        }
    }

    if installs.is_empty() {
        return Check { state: "missing".into(), version: None, path: None };
    }

    let required = required_mc_version.map(required_java_major);

    match required {
        Some(req) => {
            // Lowest compatible major: old MC versions can break on newer JVMs.
            let best = installs
                .iter()
                .filter(|j| j.major >= req)
                .min_by_key(|j| j.major);

            match best {
                Some(j) => Check {
                    state: "ok".into(),
                    version: Some(j.version.clone()),
                    path: Some(j.path.to_string_lossy().into_owned()),
                },
                None => {
                    let newest = installs.iter().max_by_key(|j| j.major).unwrap();
                    Check {
                        state: "incompatible".into(),
                        version: Some(format!("{} (needs {})", newest.major, req)),
                        path: Some(newest.path.to_string_lossy().into_owned()),
                    }
                }
            }
        }
        None => {
            let newest = installs.iter().max_by_key(|j| j.major).unwrap();
            Check {
                state: "ok".into(),
                version: Some(newest.version.clone()),
                path: Some(newest.path.to_string_lossy().into_owned()),
            }
        }
    }
}

/// Mojang bumped the bundled runtime to 17 in 1.18 and to 21 in 1.20.5.
pub(crate) fn required_java_major(mc_version: &str) -> u32 {
    let mut it = mc_version.split('.').filter_map(|p| p.parse::<u32>().ok());
    let major = it.next().unwrap_or(0);
    let minor = it.next().unwrap_or(0);
    let patch = it.next().unwrap_or(0);

    if major != 1 {
        return 21;
    }
    match minor {
        m if m >= 21 => 21,
        20 => {
            if patch >= 5 {
                21
            } else {
                17
            }
        }
        18 | 19 => 17,
        17 => 16,
        _ => 8,
    }
}

/// `1.8.0_412` → 8, `21.0.2` → 21.
fn java_major(version: &str) -> Option<u32> {
    let mut parts = version
        .split(|c| c == '.' || c == '_' || c == '-')
        .filter(|p| !p.is_empty());
    let first = parts.next()?.parse::<u32>().ok()?;
    if first == 1 {
        parts.next()?.parse::<u32>().ok()
    } else {
        Some(first)
    }
}

fn find_java_binaries() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, found: &mut Vec<PathBuf>| {
        if p.exists() && !found.iter().any(|e| e == &p) {
            found.push(p);
        }
    };

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        push(
            PathBuf::from(&java_home).join("bin").join("java.exe"),
            &mut found,
        );
    }

    let vendor_roots = [
        r"C:\Program Files\Java",
        r"C:\Program Files (x86)\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files\Zulu",
    ];
    for base in vendor_roots {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                push(entry.path().join("bin").join("java.exe"), &mut found);
            }
        }
    }

    // runtime/<component>/<os>/<component>/bin/java.exe
    for root in minecraft_runtime_roots() {
        find_java_recursive(&root, 6, &mut found);
    }

    if let Ok(output) = std::process::Command::new("where").arg("java").output() {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    push(PathBuf::from(trimmed), &mut found);
                }
            }
        }
    }

    found
}

/// Where the official Minecraft launcher keeps its bundled JREs.
fn minecraft_runtime_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = PathBuf::from(&local);
        // MS Store launcher.
        roots.push(
            local
                .join("Packages")
                .join("Microsoft.4297127D64EC6_8wekyb3d8bbwe")
                .join("LocalCache")
                .join("Local")
                .join("runtime"),
        );
        roots.push(local.join("Packages").join("runtime"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(&appdata).join(".minecraft").join("runtime"));
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Minecraft Launcher\runtime"));
    roots.push(PathBuf::from(r"C:\Program Files\Minecraft Launcher\runtime"));
    roots
}

fn find_java_recursive(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
    if depth == 0 || !dir.is_dir() {
        return;
    }
    let java_exe = dir.join("bin").join("java.exe");
    if java_exe.exists() {
        if !found.iter().any(|e| e == &java_exe) {
            found.push(java_exe);
        }
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                find_java_recursive(&path, depth - 1, found);
            }
        }
    }
}

async fn get_java_version(java_path: &Path) -> Option<String> {
    let output = Command::new(java_path)
        .arg("-version")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .await
        .ok()?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let re = Regex::new(r#"version "([^"]+)""#).ok()?;
    re.captures(&stderr)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn detect_minecraft() -> Check {
    let appdata = match std::env::var("APPDATA") {
        Ok(v) => v,
        Err(_) => return Check { state: "missing".into(), version: None, path: None },
    };
    let mc_dir = PathBuf::from(appdata).join(".minecraft");

    if !mc_dir.exists() {
        return Check { state: "missing".into(), version: None, path: None };
    }

    let versions_dir = mc_dir.join("versions");
    let libraries_dir = mc_dir.join("libraries");

    if !versions_dir.exists() || !libraries_dir.exists() {
        return Check {
            state: "warn".into(),
            version: None,
            path: Some(mc_dir.to_string_lossy().into_owned()),
        };
    }

    Check {
        state: "ok".into(),
        version: None,
        path: Some(mc_dir.to_string_lossy().into_owned()),
    }
}

fn detect_fabric(required_version: Option<&str>) -> Check {
    let appdata = match std::env::var("APPDATA") {
        Ok(v) => v,
        Err(_) => return Check { state: "missing".into(), version: None, path: None },
    };

    let versions_dir = PathBuf::from(appdata).join(".minecraft").join("versions");
    if !versions_dir.exists() {
        return Check { state: "missing".into(), version: None, path: None };
    }

    let mut fabric_versions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&versions_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.to_lowercase().contains("fabric") {
                fabric_versions.push(name);
            }
        }
    }

    if fabric_versions.is_empty() {
        return Check { state: "missing".into(), version: None, path: None };
    }

    if let Some(required) = required_version {
        let has_match = fabric_versions.iter().any(|v| v.contains(required));
        if has_match {
            return Check {
                state: "ok".into(),
                version: Some(format!("Fabric for {}", required)),
                path: Some(versions_dir.to_string_lossy().into_owned()),
            };
        } else {
            return Check {
                state: "incompatible".into(),
                version: Some(fabric_versions.join(", ")),
                path: Some(versions_dir.to_string_lossy().into_owned()),
            };
        }
    }

    Check {
        state: "ok".into(),
        version: Some(fabric_versions.join(", ")),
        path: Some(versions_dir.to_string_lossy().into_owned()),
    }
}

fn detect_ram(required_version: Option<&str>) -> RamCheck {
    let mut sys = System::new();
    sys.refresh_memory();
    let total_bytes = sys.total_memory();
    let total_mb = total_bytes / (1024 * 1024);

    let recommended_mb = match required_version {
        Some(v) if v.starts_with("1.21") || v.starts_with("1.20") => 6144,
        Some(v) if v.starts_with("1.19") || v.starts_with("1.18") => 4096,
        _ => 2048,
    };

    let assigned_mb = std::cmp::min(recommended_mb, total_mb / 2);

    let state = if total_mb >= recommended_mb * 2 {
        "ok"
    } else if total_mb >= recommended_mb {
        "warn"
    } else {
        "incompatible"
    };

    RamCheck {
        state: state.into(),
        total_mb,
        recommended_mb,
        assigned_mb,
    }
}
