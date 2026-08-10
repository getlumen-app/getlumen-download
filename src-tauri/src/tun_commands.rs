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
    /// `"macos"` | `"windows"` — lets the UI name the right unlock action.
    pub platform: &'static str,
    /// Windows only: the runtime exists but Lumen is not running elevated, so
    /// enabling TUN means restarting Lumen as administrator.
    pub needs_elevation: bool,
}

fn empty_status(installed: bool, needs_elevation: bool) -> TunStatus {
    TunStatus {
        helper_installed: installed,
        helper_running: false,
        singbox_running: false,
        singbox_pid: None,
        uptime_secs: None,
        platform: PLATFORM,
        needs_elevation,
    }
}

#[cfg(target_os = "macos")]
const PLATFORM: &str = "macos";
#[cfg(target_os = "windows")]
const PLATFORM: &str = "windows";

/// True when the platform runtime is present but blocked on a privilege the
/// user still has to grant. macOS grants it once at helper install time.
fn needs_elevation() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(target_os = "windows")]
    {
        tun_runtime::is_helper_installed() && !tun_runtime::is_elevated()
    }
}

/// Get current TUN helper + sing-box status.
#[tauri::command]
pub async fn tun_status() -> Result<TunStatus, String> {
    let installed = tun_runtime::is_helper_installed();
    if !installed {
        return Ok(empty_status(false, false));
    }

    let running = tun_runtime::is_helper_running().await;
    if !running {
        return Ok(empty_status(true, needs_elevation()));
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
            platform: PLATFORM,
            needs_elevation: false,
        }),
        Ok(other) => Err(format!("Unexpected response: {:?}", other)),
        Err(e) => Err(e),
    }
}

/// Install/validate the platform TUN runtime.
///
/// macOS installs the privileged helper daemon. Windows has no daemon: the
/// bundled sing-box is validated and, when Lumen is not elevated, Lumen
/// restarts itself with administrator rights — the same unlock v2rayN and
/// NekoRay use, and the only way to create a Wintun adapter.
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
        tun_runtime::install_helper("", &singbox.to_string_lossy())?;
        if tun_runtime::is_elevated() {
            return Ok(());
        }

        // Ask for the UAC prompt first: a cancelled prompt must leave the
        // current session exactly as it was.
        tun_runtime::relaunch_self_elevated()?;

        // Hand the machine back to its normal routing before this instance
        // goes away; the elevated instance starts from a clean state.
        crate::shutdown_network_runtime(&app);
        app.exit(0);
        Ok(())
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
        Response::Started { pid } => {
            announce_dropped_exit_pin(&app);
            Ok(pid)
        }
        Response::Error { message } => Err(message),
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Tell the shell that readiness only succeeded after a dead manual exit pin
/// was dropped. Without this the shell re-applies its stored location a few
/// seconds later and puts the tunnel straight back on the dead exit.
fn announce_dropped_exit_pin(app: &tauri::AppHandle) {
    #[cfg(target_os = "windows")]
    {
        use tauri::Emitter;
        if tun_runtime::take_pin_reset() {
            if let Err(e) = app.emit(crate::EXIT_PIN_RESET_EVENT, crate::clash_api::AUTO_MEMBER) {
                log::warn!("exit pin reset event: {}", e);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
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
    let bundled = bin_dir(app)?.join(filename);
    if bundled.is_file() {
        return Ok(bundled);
    }
    // Dev runs resolve the sidecar the same way the System Proxy runtime does,
    // so `npm run tauri dev` exercises the real TUN path instead of failing on
    // a bundle-only resource layout.
    crate::singbox::find_singbox_binary().or(Ok(bundled))
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
