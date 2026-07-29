/// Tauri commands exposed to React for TUN mode (via privileged helper).
use crate::config;
#[cfg(target_os = "macos")]
use crate::tun_helper as tun_runtime;
#[cfg(target_os = "windows")]
use crate::tun_windows as tun_runtime;
#[cfg(target_os = "macos")]
use crate::wbstream::{self, WbstreamFallbackStatus};
use serde::Serialize;
use std::path::PathBuf;
use tauri::Manager;
use tun_runtime::{Request, Response};

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
    let installed = tun_runtime::is_helper_installed();
    if !installed {
        return Ok(TunStatus {
            helper_installed: false,
            helper_running: false,
            singbox_running: false,
            singbox_pid: None,
            uptime_secs: None,
        });
    }

    let running = tun_runtime::is_helper_running().await;
    if !running {
        return Ok(TunStatus {
            helper_installed: true,
            helper_running: false,
            singbox_running: false,
            singbox_pid: None,
            uptime_secs: None,
        });
    }

    match tun_runtime::send(Request::Status).await {
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

/// Install/validate the platform TUN runtime.
#[tauri::command]
pub fn tun_install_helper(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let (installer, source_helper) = bundled_paths(&app)?;
        tun_runtime::install_helper(
            &installer.to_string_lossy(),
            &source_helper.to_string_lossy(),
        )
    }
    #[cfg(target_os = "windows")]
    {
        let singbox = bundled_singbox_path(&app)?;
        tun_runtime::install_helper("", &singbox.to_string_lossy())
    }
}

/// Uninstall helper.
#[tauri::command]
pub fn tun_uninstall_helper(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let (installer, _) = bundled_paths(&app)?;
        tun_runtime::uninstall_helper(&installer.to_string_lossy())
    }
    #[cfg(target_os = "windows")]
    {
        let _ = app;
        tun_runtime::uninstall_helper("")
    }
}

/// Start sing-box via helper. Helper runs sing-box as root → TUN mode works.
#[tauri::command]
pub async fn tun_start(config_path: String, app: tauri::AppHandle) -> Result<u32, String> {
    let singbox_path = bundled_singbox_path(&app)?;
    match tun_runtime::send(Request::Start {
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
pub async fn tun_connect(
    key: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> Result<u32, String> {
    // TUN and System Proxy are mutually exclusive. Stop the per-user proxy
    // runtime before asking Windows/macOS for privileged TUN routing.
    crate::proxy::disable_system_proxy()
        .map_err(|e| format!("Could not disable System Proxy before TUN: {e}"))?;
    if state.singbox.lock().unwrap().is_running() {
        state
            .singbox
            .lock()
            .unwrap()
            .stop()
            .map_err(|e| format!("Could not stop System Proxy runtime before TUN: {e}"))?;
    }

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
        let urls = if s.starts_with("https://") || s.starts_with("http://") {
            vec![s.to_string()]
        } else {
            config::proteus_config_urls(s)
        };
        match config::fetch_and_cache_first_available_with_mode(&urls, config::InboundMode::Tun)
            .await
        {
            Ok(_) => {
                // Server fetch = full multi-exit config. Preserve an immutable
                // last-good copy so a later single-`vless://` connect (which
                // overwrites config-tun.json) cannot clobber the fallback.
                if let Ok(good) = std::fs::read_to_string(config::tun_config_file_path()) {
                    let _ = std::fs::write(config::tun_config_lastgood_path(), good);
                }
            }
            Err(fetch_err) => {
                // Control-plane (config endpoint) may be censored / unreachable
                // (e.g. SNI-blocked on a hostile network). The data-plane still
                // works, so fall back to the last-good cached config instead of
                // failing (which would eagerly drop to the wbstream carrier). No
                // secrets: this only reuses the user own previously-fetched config.
                match config::load_cached_tun_config() {
                    Ok(cached) => {
                        std::fs::write(config::tun_config_file_path(), &cached).map_err(|e| {
                            format!(
                                "Config fetch failed: {}; cache write failed: {}",
                                fetch_err, e
                            )
                        })?;
                        log::warn!(
                            "Config fetch failed ({}); connecting from last-good cached config",
                            fetch_err
                        );
                    }
                    Err(cache_err) => {
                        return Err(format!(
                            "Config fetch failed: {} (no usable cached config: {})",
                            fetch_err, cache_err
                        ));
                    }
                }
            }
        }
    }
    let config_path = config::tun_config_file_path().to_string_lossy().to_string();

    // 2. Ask helper to start
    let singbox_path = bundled_singbox_path(&app)?;
    match tun_runtime::send(Request::Start {
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

/// Hard-whitelist fallback: join a cached WB Stream room, expose a local SOCKS
/// bridge, then start TUN sing-box with route.final -> that local SOCKS.
/// The React shell calls this automatically after normal TUN health checks fail
/// repeatedly, and the command remains separate so diagnostics can invoke it
/// directly without duplicating fallback setup.
#[tauri::command]
#[cfg(target_os = "macos")]
pub async fn tun_connect_wbstream_fallback(app: tauri::AppHandle) -> Result<u32, String> {
    let socks_port = wbstream::start_sidecar_from_cached_manifest(&app).await?;
    config::save_wbstream_fallback_config(config::InboundMode::Tun, socks_port)
        .map_err(|e| format!("WB Stream fallback config failed: {}", e))?;

    let config_path = config::tun_config_file_path().to_string_lossy().to_string();
    let singbox_path = bundled_singbox_path(&app)?;
    match tun_runtime::send(Request::Start {
        config_path,
        singbox_path: singbox_path.to_string_lossy().to_string(),
    })
    .await?
    {
        Response::Started { pid } => Ok(pid),
        Response::Error { message } => {
            wbstream::stop_sidecar();
            Err(message)
        }
        other => {
            wbstream::stop_sidecar();
            Err(format!("Unexpected response: {:?}", other))
        }
    }
}

/// Disconnect TUN: stop sing-box via helper.
#[tauri::command]
pub async fn tun_disconnect() -> Result<(), String> {
    stop_wbstream_sidecar();
    match tun_runtime::send(Request::Stop).await? {
        Response::Stopped => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Stop sing-box.
#[tauri::command]
pub async fn tun_stop() -> Result<(), String> {
    stop_wbstream_sidecar();
    match tun_runtime::send(Request::Stop).await? {
        Response::Stopped => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

#[tauri::command]
#[cfg(target_os = "macos")]
pub fn wbstream_fallback_status(app: tauri::AppHandle) -> Result<WbstreamFallbackStatus, String> {
    Ok(wbstream::fallback_status(&app))
}

#[tauri::command]
#[cfg(target_os = "macos")]
pub fn wbstream_stop_sidecar() -> Result<(), String> {
    wbstream::stop_sidecar();
    Ok(())
}

/// Resolve bundled installer + helper paths.
/// Tauri puts resources at Lumen.app/Contents/Resources/_up_/bin/<name>
/// when source path was "../bin/...".
#[cfg(target_os = "macos")]
fn bundled_paths(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), String> {
    let bin = bin_dir(app)?;
    Ok((bin.join("lumen-installer"), bin.join("lumen-helper")))
}

fn bundled_singbox_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let filename = "sing-box";
    #[cfg(target_os = "windows")]
    let filename = "sing-box.exe";
    Ok(bin_dir(app)?.join(filename))
}

#[cfg(target_os = "macos")]
fn stop_wbstream_sidecar() {
    wbstream::stop_sidecar();
}

#[cfg(target_os = "windows")]
fn stop_wbstream_sidecar() {}

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
