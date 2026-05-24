use crate::config;
use crate::wbstream_balancer;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::net::TcpStream;

static SIDECARS: OnceLock<Mutex<Vec<Child>>> = OnceLock::new();
const DEFAULT_VP8_FPS: u16 = 60;
const DEFAULT_VP8_BATCH: u16 = 120;

#[derive(Debug, Serialize)]
pub struct WbstreamFallbackStatus {
    pub manifest_cached: bool,
    pub room_url_available: bool,
    pub sidecar_running: bool,
    pub sidecar_count: usize,
    pub balancer_running: bool,
    pub balancer_upstream_count: usize,
    pub local_socks_port: u16,
    pub joiner_path: Option<String>,
    pub multipath_client_path: Option<String>,
}

fn sidecars_slot() -> &'static Mutex<Vec<Child>> {
    SIDECARS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn fallback_status(app: &tauri::AppHandle) -> WbstreamFallbackStatus {
    let manifest = config::load_cached_wbstream_manifest().ok();
    let room_url_available = manifest
        .as_ref()
        .and_then(config::select_wbstream_room_url)
        .is_some();
    let sidecar_count = sidecar_count();
    let balancer = wbstream_balancer::runtime_status();
    WbstreamFallbackStatus {
        manifest_cached: manifest.is_some(),
        room_url_available,
        sidecar_running: sidecar_count > 0,
        sidecar_count,
        balancer_running: balancer.running,
        balancer_upstream_count: balancer.upstream_count,
        local_socks_port: balancer
            .listen_port
            .unwrap_or_else(|| {
                if sidecar_count > config::WBSTREAM_MAX_ROOMS {
                    config::WBSTREAM_LOCAL_MULTIPATH_PORT
                } else {
                    config::WBSTREAM_LOCAL_SOCKS_PORT
                }
            }),
        joiner_path: find_joiner(app)
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
        multipath_client_path: find_multipath_client(app)
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
    }
}

pub async fn start_sidecar_from_cached_manifest(app: &tauri::AppHandle) -> Result<u16, String> {
    config::ensure_wbstream_manifest_cached()
        .await
        .map_err(|e| format!("WB Stream manifest unavailable: {}", e))?;
    let manifest = config::load_cached_wbstream_manifest()
        .map_err(|e| format!("WB Stream manifest unavailable: {}", e))?;
    let room_urls = config::select_wbstream_room_urls(&manifest, config::WBSTREAM_MAX_ROOMS);
    if room_urls.is_empty() {
        return Err("WB Stream manifest has no usable room".to_string());
    }
    start_sidecars(app, &room_urls).await
}

pub async fn start_sidecars(app: &tauri::AppHandle, room_urls: &[String]) -> Result<u16, String> {
    if sidecar_is_running() {
        if port_open(config::WBSTREAM_LOCAL_MULTIPATH_PORT).await {
            return Ok(config::WBSTREAM_LOCAL_MULTIPATH_PORT);
        }
        if port_open(config::WBSTREAM_LOCAL_BALANCER_PORT).await {
            return Ok(config::WBSTREAM_LOCAL_BALANCER_PORT);
        }
    }
    stop_sidecar();

    let joiner = find_joiner(app)?;
    let mut ports = Vec::new();
    for (index, room_url) in room_urls.iter().enumerate() {
        let port = config::WBSTREAM_LOCAL_SOCKS_PORT + index as u16;
        if let Err(e) = start_one_sidecar(&joiner, room_url, port, index).await {
            stop_sidecar();
            return Err(e);
        }
        ports.push(port);
    }

    if ports.len() >= 2 {
        match start_multipath_client(app, &ports).await {
            Ok(port) => return Ok(port),
            Err(e) => {
                log::warn!(
                    "WB Stream multipath client unavailable, falling back to round-robin balancer: {}",
                    e
                );
            }
        }
    }

    match ports.len() {
        1 => Ok(ports[0]),
        _ => wbstream_balancer::start_balancer(config::WBSTREAM_LOCAL_BALANCER_PORT, ports).await,
    }
}

async fn start_one_sidecar(
    joiner: &Path,
    room_url: &str,
    socks_port: u16,
    index: usize,
) -> Result<(), String> {
    let log_path = config::data_dir().join(format!("wbstream-joiner-{}.log", index));
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("open WB Stream log: {}", e))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("clone WB Stream log: {}", e))?;

    let mut command = Command::new(&joiner);
    let (vp8_fps, vp8_batch) = vp8_pacing_from_env();
    command.args([
        "--room",
        room_url,
        "--socks-port",
        &socks_port.to_string(),
        "--vp8-fps",
        &vp8_fps.to_string(),
        "--vp8-batch",
        &vp8_batch.to_string(),
    ]);
    if let Ok(mode) = std::env::var("LUMEN_WBSTREAM_TUNNEL_MODE") {
        let mode = mode.trim();
        if !mode.is_empty() {
            command.args(["--tunnel-mode", mode]);
        }
    }

    let child = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("start WB Stream joiner {}: {}", joiner.display(), e))?;

    {
        let mut slot = sidecars_slot().lock().unwrap();
        slot.push(child);
    }

    wait_for_sidecar_ready(&log_path, socks_port, Duration::from_secs(30)).await
}

async fn start_multipath_client(app: &tauri::AppHandle, upstream_ports: &[u16]) -> Result<u16, String> {
    let client = find_multipath_client(app)?;
    let log_path = config::data_dir().join("wbstream-multipath-client.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("open WB Stream multipath log: {}", e))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("clone WB Stream multipath log: {}", e))?;

    let args = multipath_client_args(upstream_ports);
    let child = Command::new(&client)
        .args(&args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("start WB Stream multipath client {}: {}", client.display(), e))?;

    {
        let mut slot = sidecars_slot().lock().unwrap();
        slot.push(child);
    }

    wait_for_port_ready(config::WBSTREAM_LOCAL_MULTIPATH_PORT, Duration::from_secs(5)).await?;
    Ok(config::WBSTREAM_LOCAL_MULTIPATH_PORT)
}

fn multipath_client_args(upstream_ports: &[u16]) -> Vec<String> {
    let upstreams = upstream_ports
        .iter()
        .map(|port| format!("127.0.0.1:{}", port))
        .collect::<Vec<_>>()
        .join(",");
    vec![
        "--socks-duplex-serve".to_string(),
        format!("127.0.0.1:{}", config::WBSTREAM_LOCAL_MULTIPATH_PORT),
        "--socks-aggregators".to_string(),
        upstreams,
        "--aggregator-target".to_string(),
        format!("127.0.0.1:{}", config::WBSTREAM_REMOTE_MULTIPATH_PORT),
    ]
}

fn vp8_pacing_from_env() -> (u16, u16) {
    (
        parse_positive_u16_env("LUMEN_WBSTREAM_VP8_FPS").unwrap_or(DEFAULT_VP8_FPS),
        parse_positive_u16_env("LUMEN_WBSTREAM_VP8_BATCH").unwrap_or(DEFAULT_VP8_BATCH),
    )
}

fn parse_positive_u16_env(name: &str) -> Option<u16> {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_positive_u16(value.trim()))
}

fn parse_positive_u16(value: &str) -> Option<u16> {
    value.parse::<u16>().ok().filter(|n| *n > 0)
}

pub fn stop_sidecar() {
    wbstream_balancer::stop_balancer();
    let mut slot = sidecars_slot().lock().unwrap();
    for mut child in slot.drain(..) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn sidecar_is_running() -> bool {
    sidecar_count() > 0
}

fn sidecar_count() -> usize {
    let mut slot = sidecars_slot().lock().unwrap();
    slot.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    slot.len()
}

async fn wait_for_sidecar_ready(
    log_path: &Path,
    socks_port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if port_open(socks_port).await && tunnel_connected(log_path) {
            tokio::time::sleep(Duration::from_secs(3)).await;
            return Ok(());
        }
        if !sidecar_is_running() {
            return Err("WB Stream joiner exited before the tunnel became ready".to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("WB Stream joiner did not finish tunnel setup in time".to_string())
}

async fn wait_for_port_ready(port: u16, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if port_open(port).await {
            return Ok(());
        }
        if !sidecar_is_running() {
            return Err("WB Stream multipath client exited before becoming ready".to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "WB Stream multipath client did not listen on {} in time",
        port
    ))
}

fn tunnel_connected(log_path: &Path) -> bool {
    std::fs::read_to_string(log_path)
        .map(|log| log.contains("TUNNEL CONNECTED"))
        .unwrap_or(false)
}

async fn port_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(350),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

fn find_joiner(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    find_bundled_or_lab_binary(
        app,
        "headless-wbstream-joiner",
        "/opt/getlumen/wbstream/headless-wbstream-joiner",
    )
        .ok_or_else(|| "headless-wbstream-joiner is not bundled".to_string())
}

fn find_multipath_client(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    find_bundled_or_lab_binary(
        app,
        "wbstream_multipath_client",
        "/opt/getlumen/wbstream/wbstream_multipath_client",
    )
        .ok_or_else(|| "wbstream_multipath_client is not bundled".to_string())
}

fn find_bundled_or_lab_binary(
    app: &tauri::AppHandle,
    bin_name: &str,
    fallback_path: &str,
) -> Option<PathBuf> {
    let resource_dir = app
        .path()
        .resource_dir()
        .ok()?;
    let candidates = [
        resource_dir
            .join("_up_")
            .join("bin")
            .join(bin_name),
        resource_dir.join(bin_name),
        PathBuf::from(fallback_path),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vp8_pacing_parser_accepts_positive_u16() {
        assert_eq!(parse_positive_u16("60"), Some(60));
        assert_eq!(parse_positive_u16("120"), Some(120));
    }

    #[test]
    fn vp8_pacing_parser_rejects_zero_and_invalid_values() {
        assert_eq!(parse_positive_u16("0"), None);
        assert_eq!(parse_positive_u16(""), None);
        assert_eq!(parse_positive_u16("fast"), None);
        assert_eq!(parse_positive_u16("70000"), None);
    }

    #[test]
    fn multipath_client_args_point_to_joiners_and_remote_loopback() {
        let args = multipath_client_args(&[11080, 11081, 11082]);
        assert_eq!(
            args,
            vec![
                "--socks-duplex-serve",
                "127.0.0.1:11078",
                "--socks-aggregators",
                "127.0.0.1:11080,127.0.0.1:11081,127.0.0.1:11082",
                "--aggregator-target",
                "127.0.0.1:19095"
            ]
        );
    }
}
