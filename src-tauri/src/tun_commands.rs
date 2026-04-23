/// Tauri commands exposed to React for TUN mode (via privileged helper).
use crate::config;
use crate::tun_helper::{self, Request, Response};
use serde::Serialize;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Serialize)]
pub struct TunStatus {
    pub helper_installed: bool,
    pub helper_running: bool,
    pub singbox_running: bool,
    pub singbox_pid: Option<u32>,
    pub uptime_secs: Option<u64>,
}

/// Get current TUN helper + sing-box status.
#[tauri::command]
pub async fn tun_status() -> Result<TunStatus, String> {
    let installed = tun_helper::is_helper_installed();
    if !installed {
        return Ok(TunStatus {
            helper_installed: false,
            helper_running: false,
            singbox_running: false,
            singbox_pid: None,
            uptime_secs: None,
        });
    }

    let running = tun_helper::is_helper_running().await;
    if !running {
        return Ok(TunStatus {
            helper_installed: true,
            helper_running: false,
            singbox_running: false,
            singbox_pid: None,
            uptime_secs: None,
        });
    }

    match tun_helper::send(Request::Status).await {
        Ok(Response::Status {
            running,
            pid,
            uptime_secs,
        }) => Ok(TunStatus {
            helper_installed: true,
            helper_running: true,
            singbox_running: running,
            singbox_pid: pid,
            uptime_secs,
        }),
        Ok(other) => Err(format!("Unexpected response: {:?}", other)),
        Err(e) => Err(e),
    }
}

/// Install helper via osascript admin prompt. One-time setup.
#[tauri::command]
pub fn tun_install_helper(app: tauri::AppHandle) -> Result<(), String> {
    let (installer, source_helper) = bundled_paths(&app)?;
    tun_helper::install_helper(
        &installer.to_string_lossy(),
        &source_helper.to_string_lossy(),
    )
}

/// Uninstall helper.
#[tauri::command]
pub fn tun_uninstall_helper(app: tauri::AppHandle) -> Result<(), String> {
    let (installer, _) = bundled_paths(&app)?;
    tun_helper::uninstall_helper(&installer.to_string_lossy())
}

/// Start sing-box via helper. Helper runs sing-box as root → TUN mode works.
#[tauri::command]
pub async fn tun_start(config_path: String, app: tauri::AppHandle) -> Result<u32, String> {
    let singbox_path = bundled_singbox_path(&app)?;
    match tun_helper::send(Request::Start {
        config_path,
        singbox_path: singbox_path.to_string_lossy().to_string(),
    })
    .await?
    {
        Response::Started { pid } => Ok(pid),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Full TUN connect: build config (auto-detect input type), save to disk, ask helper to start sing-box.
#[tauri::command]
pub async fn tun_connect(key: String, app: tauri::AppHandle) -> Result<u32, String> {
    // 1. Build TUN config — auto-detect VLESS link / sub URL / Proteus key
    let raw = key.trim();
    // Normalize: full subscription URLs for our backends → bare key via CF Worker
    let s: std::borrow::Cow<str> = if let Some(k) = crate::extract_proteus_key(raw) {
        std::borrow::Cow::Owned(k)
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let s: &str = &s;
    if s.starts_with("vless://") {
        let v = crate::vless::parse_vless(s).map_err(|e| format!("VLESS parse failed: {}", e))?;
        config::save_vless_config(&v, config::InboundMode::Tun)
            .await
            .map_err(|e| format!("Config build failed: {}", e))?;
    } else {
        let url = if s.starts_with("https://") || s.starts_with("http://") {
            s.to_string()
        } else {
            format!(
                "{}/proteus-sub?sub={}&format=json-text",
                config::config_base_url(),
                s
            )
        };
        config::fetch_and_cache_with_mode(&url, config::InboundMode::Tun)
            .await
            .map_err(|e| format!("Config fetch failed: {}", e))?;
    }
    let config_path = config::tun_config_file_path().to_string_lossy().to_string();

    // 2. Ask helper to start
    let singbox_path = bundled_singbox_path(&app)?;
    match tun_helper::send(Request::Start {
        config_path,
        singbox_path: singbox_path.to_string_lossy().to_string(),
    })
    .await?
    {
        Response::Started { pid } => Ok(pid),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Disconnect TUN: stop sing-box via helper.
#[tauri::command]
pub async fn tun_disconnect() -> Result<(), String> {
    match tun_helper::send(Request::Stop).await? {
        Response::Stopped => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Stop sing-box.
#[tauri::command]
pub async fn tun_stop() -> Result<(), String> {
    match tun_helper::send(Request::Stop).await? {
        Response::Stopped => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Resolve bundled installer + helper paths.
/// Tauri puts resources at Lumen.app/Contents/Resources/_up_/bin/<name>
/// when source path was "../bin/...".
fn bundled_paths(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), String> {
    let bin = bin_dir(app)?;
    Ok((bin.join("lumen-installer"), bin.join("lumen-helper")))
}

fn bundled_singbox_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(bin_dir(app)?.join("sing-box"))
}

fn bin_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("resource_dir: {}", e))?;
    // Try _up_/bin first (tauri layout for ../bin/* resources)
    let up_bin = resource_dir.join("_up_").join("bin");
    if up_bin.exists() {
        return Ok(up_bin);
    }
    // Fallback: resource_dir root
    Ok(resource_dir)
}
