mod contracts;
mod discovery;
mod health;
mod instance;
mod launch;
mod manifest;
mod mods;
mod provision;

use contracts::{DiscoveredServer, HealthReport};
use launch::{LaunchConfig, LaunchResult};

#[tauri::command]
async fn run_health_check(server: DiscoveredServer) -> Result<HealthReport, String> {
    let mc_version = if server.mc_version.is_empty() {
        None
    } else {
        Some(server.mc_version.as_str())
    };
    Ok(health::run(mc_version).await)
}

#[tauri::command]
async fn launch_minecraft(
    app: tauri::AppHandle,
    config: LaunchConfig,
) -> Result<LaunchResult, String> {
    launch::launch(app, config).await
}

#[tauri::command]
fn default_username() -> String {
    launch::default_username()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            discovery::scan_lan,
            discovery::ping_server,
            manifest::get_manifest,
            mods::sync_mods,
            provision::prepare_instance,
            provision::ensure_java,
            run_health_check,
            launch_minecraft,
            default_username
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
