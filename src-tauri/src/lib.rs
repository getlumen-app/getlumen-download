mod clash_api;
mod config;
mod proxy;
mod singbox;
#[cfg(target_os = "macos")]
mod tun_commands;
#[cfg(target_os = "macos")]
mod tun_helper;
mod vless;

use std::sync::Mutex;
use tauri::State;

pub struct AppState {
    singbox: Mutex<singbox::SingboxManager>,
    config_path: Mutex<Option<String>>,
}

/// Detect what kind of input the user provided.
fn detect_input_type(raw: &str) -> &'static str {
    let s = raw.trim();
    if s.starts_with("vless://") {
        return "vless";
    }
    // Known internal subscription URLs — extract the ?sub= key and treat as proteus_key
    // so they always route through the CF Worker (not the raw backend).
    if (s.starts_with("https://") || s.starts_with("http://")) && extract_proteus_key(s).is_some() {
        return "proteus_key";
    }
    if s.starts_with("https://") || s.starts_with("http://") {
        "subscription_url"
    } else {
        "proteus_key"
    }
}

/// Extract a bare subscription key from any URL variant.
///
/// Handles:
///   - `https://<host>/proteus-sub?sub=KEY[&format=...]`
///   - `https://<host>/sub/KEY`
///
/// Returns `None` for URLs not matching these patterns (e.g. third-party subs
/// that do not follow either shape).
pub(crate) fn extract_proteus_key(url: &str) -> Option<String> {
    // ?sub=KEY query param
    if let Some(pos) = url.find("?sub=").or_else(|| url.find("&sub=")) {
        let rest = &url[pos + 5..];
        let key: String = rest
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        if key.len() >= 8
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some(key);
        }
    }
    // /sub/KEY path
    if let Some(idx) = url.find("/sub/") {
        let rest = &url[idx + 5..];
        let key: String = rest
            .chars()
            .take_while(|c| *c != '?' && *c != '#' && *c != '/')
            .collect();
        if key.len() >= 8
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some(key);
        }
    }
    None
}

/// Build config (proxy mode) from any input type — VLESS link or Proteus sub key/URL.
async fn prepare_proxy_config(key: &str) -> Result<(), String> {
    let raw = key.trim();
    // Normalize: if user pasted a full subscription URL for our backends,
    // extract the bare key so it routes through the CF Worker.
    let key = if let Some(extracted) = extract_proteus_key(raw) {
        std::borrow::Cow::Owned(extracted)
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let key: &str = &key;
    match detect_input_type(key) {
        "vless" => {
            let v = vless::parse_vless(key).map_err(|e| format!("VLESS parse failed: {}", e))?;
            config::save_vless_config(&v, config::InboundMode::Mixed)
                .await
                .map_err(|e| format!("Config build failed: {}", e))?;
        }
        "subscription_url" => {
            config::fetch_and_cache(key)
                .await
                .map_err(|e| format!("Config fetch failed: {}", e))?;
        }
        _ => {
            let url = format!(
                "{}/proteus-sub?sub={}&format=json-text",
                config::config_base_url(),
                key
            );
            config::fetch_and_cache(&url)
                .await
                .map_err(|e| format!("Config fetch failed: {}", e))?;
        }
    }
    Ok(())
}

/// Inspect input — used by UI for auto-detect feedback.
#[tauri::command]
fn detect_key(input: String) -> serde_json::Value {
    let kind = detect_input_type(&input);
    match kind {
        "vless" => match vless::parse_vless(&input) {
            Ok(v) => serde_json::json!({
                "type": "vless",
                "valid": true,
                "name": v.name,
                "host": v.host,
            }),
            Err(e) => serde_json::json!({
                "type": "vless",
                "valid": false,
                "error": e,
            }),
        },
        "subscription_url" => serde_json::json!({
            "type": "subscription_url",
            "valid": true,
        }),
        _ => serde_json::json!({
            "type": "proteus_key",
            "valid": input.trim().len() >= 4,
        }),
    }
}

#[tauri::command]
async fn fetch_config(key: String, state: State<'_, AppState>) -> Result<String, String> {
    prepare_proxy_config(&key).await?;
    let path = config::config_file_path();
    *state.config_path.lock().unwrap() = Some(path.to_string_lossy().to_string());
    Ok(std::fs::read_to_string(&path).unwrap_or_default())
}

#[tauri::command]
async fn connect(key: String, state: State<'_, AppState>) -> Result<(), String> {
    // 1. Build config (auto-detects vless / sub URL / Proteus key)
    prepare_proxy_config(&key).await?;

    let path = config::config_file_path();
    let path_str = path.to_string_lossy().to_string();
    *state.config_path.lock().unwrap() = Some(path_str.clone());

    // 2. Start sing-box (proxy mode, no root needed)
    state
        .singbox
        .lock()
        .unwrap()
        .start(&path_str)
        .map_err(|e| format!("sing-box failed: {}", e))?;

    // 3. Verify Clash API is responding
    let probe = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();

    let mut api_ok = false;
    for _ in 0..3 {
        if let Ok(resp) = probe.get("http://127.0.0.1:9090/version").send().await {
            if resp.status().is_success() {
                api_ok = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    if !api_ok {
        state.singbox.lock().unwrap().stop().ok();
        return Err("sing-box started but Clash API not responding".to_string());
    }

    // 4. Enable system proxy (covers browser + native apps)
    proxy::enable_system_proxy(10808).map_err(|e| format!("Proxy setup failed: {}", e))?;

    // 5. Set env vars for Electron apps (macOS only)
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("launchctl")
            .args(["setenv", "https_proxy", "http://127.0.0.1:10808"])
            .output()
            .ok();
        std::process::Command::new("launchctl")
            .args(["setenv", "http_proxy", "http://127.0.0.1:10808"])
            .output()
            .ok();
        std::process::Command::new("launchctl")
            .args(["setenv", "all_proxy", "socks5://127.0.0.1:10808"])
            .output()
            .ok();
    }

    Ok(())
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    // 1. Disable system proxy
    proxy::disable_system_proxy().map_err(|e| e.to_string())?;

    // 2. Clear env vars (macOS only)
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("launchctl")
            .args(["setenv", "https_proxy", ""])
            .output()
            .ok();
        std::process::Command::new("launchctl")
            .args(["setenv", "http_proxy", ""])
            .output()
            .ok();
        std::process::Command::new("launchctl")
            .args(["setenv", "all_proxy", ""])
            .output()
            .ok();
    }

    // 3. Stop sing-box
    state
        .singbox
        .lock()
        .unwrap()
        .stop()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<String, String> {
    let running = state.singbox.lock().unwrap().is_running();
    Ok(if running { "connected" } else { "disconnected" }.to_string())
}

#[tauri::command]
async fn get_proxies() -> Result<serde_json::Value, String> {
    clash_api::get_proxies().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn select_proxy(group: String, name: String) -> Result<(), String> {
    clash_api::select_proxy(&group, &name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_traffic() -> Result<serde_json::Value, String> {
    clash_api::get_traffic().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_logs() -> Result<Vec<String>, String> {
    let log_path = config::data_dir().join("singbox.log");
    let fallback = if cfg!(windows) {
        std::env::temp_dir().join("lumen.log")
    } else {
        std::path::PathBuf::from("/tmp/lumen.log")
    };
    let paths = [log_path.clone(), fallback];

    for p in &paths {
        if p.exists() {
            if let Ok(content) = std::fs::read_to_string(p) {
                let lines: Vec<String> = content
                    .lines()
                    .rev()
                    .take(200)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|s| s.to_string())
                    .collect();
                return Ok(lines);
            }
        }
    }
    Ok(vec![
        "No log file found. Connect to start logging.".to_string()
    ])
}

#[tauri::command]
async fn test_delay(name: String) -> Result<u32, String> {
    clash_api::test_delay(&name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    // Only allow http(s) to avoid local command injection via schemes
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only http(s) urls are allowed".into());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("unsupported platform".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState {
            singbox: Mutex::new(singbox::SingboxManager::new()),
            config_path: Mutex::new(None),
        })
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_key,
            fetch_config,
            connect,
            disconnect,
            get_status,
            get_proxies,
            select_proxy,
            get_traffic,
            test_delay,
            open_url,
            get_logs,
            #[cfg(target_os = "macos")]
            tun_commands::tun_status,
            #[cfg(target_os = "macos")]
            tun_commands::tun_install_helper,
            #[cfg(target_os = "macos")]
            tun_commands::tun_uninstall_helper,
            #[cfg(target_os = "macos")]
            tun_commands::tun_start,
            #[cfg(target_os = "macos")]
            tun_commands::tun_stop,
            #[cfg(target_os = "macos")]
            tun_commands::tun_connect,
            #[cfg(target_os = "macos")]
            tun_commands::tun_disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
