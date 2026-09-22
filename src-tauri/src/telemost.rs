use crate::config;
use crate::wbstream;
use crate::wbstream_balancer;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;

static SIDECARS: OnceLock<Mutex<Vec<Child>>> = OnceLock::new();
const DEFAULT_VP8_FPS: u16 = 60;
const DEFAULT_VP8_BATCH: u16 = 120;

#[derive(Debug, Serialize)]
pub struct TelemostFallbackStatus {
    pub manifest_cached: bool,
    pub room_link_available: bool,
    pub sidecar_running: bool,
    pub sidecar_count: usize,
    pub balancer_running: bool,
    pub balancer_upstream_count: usize,
    pub local_socks_port: u16,
    pub joiner_path: Option<String>,
}

fn sidecars_slot() -> &'static Mutex<Vec<Child>> {
    SIDECARS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn fallback_status(app: &tauri::AppHandle) -> TelemostFallbackStatus {
    let manifest = config::load_cached_telemost_manifest().ok();
    let room_link_available = manifest
        .as_ref()
        .and_then(|m| config::select_telemost_room_links(m, 1).into_iter().next())
        .is_some();
    let sidecar_count = sidecar_count();
    let balancer = wbstream_balancer::runtime_status();
    TelemostFallbackStatus {
        manifest_cached: manifest.is_some(),
        room_link_available,
        sidecar_running: sidecar_count > 0,
        sidecar_count,
        balancer_running: balancer.running,
        balancer_upstream_count: balancer.upstream_count,
        local_socks_port: config::TELEMOST_LOCAL_BALANCER_PORT,
        joiner_path: find_joiner(app).ok().map(|p| p.display().to_string()),
    }
}

/// Fallback entry point: cache-only manifest (never live-prefetch at fallback
/// time — the control plane may be blocked or forged while whitelist mode is
/// active), verified, then joiners are spawned per room link and fronted by
/// the shared SOCKS balancer on TELEMOST_LOCAL_BALANCER_PORT. sing-box reaches
/// the tunnel through the `telemost-local` outbound injected at config build.
pub async fn start_sidecars_from_cached_manifest(
    app: &tauri::AppHandle,
) -> Result<u16, String> {
    let manifest = config::load_cached_telemost_manifest()
        .map_err(|e| format!("Telemost manifest unavailable: {}", e))?;
    config::verify_telemost_manifest_for_fallback(&manifest)?;
    let links = config::select_telemost_room_links(&manifest, config::TELEMOST_MAX_ROOMS);
    if links.is_empty() {
        return Err("Telemost manifest has no usable room link".to_string());
    }
    start_sidecars(app, &links).await
}

pub async fn start_sidecars(
    app: &tauri::AppHandle,
    room_links: &[String],
) -> Result<u16, String> {
    if sidecar_is_running() && port_open(config::TELEMOST_LOCAL_BALANCER_PORT).await {
        return Ok(config::TELEMOST_LOCAL_BALANCER_PORT);
    }
    stop_sidecars();

    let joiner = find_joiner(app)?;
    let mut ports = Vec::new();
    for (index, link) in room_links.iter().enumerate() {
        let port = config::TELEMOST_LOCAL_SOCKS_PORT + index as u16;
        if let Err(e) = start_one_sidecar(&joiner, link, port, index).await {
            stop_sidecars();
            return Err(e);
        }
        ports.push(port);
    }

    // Always front joiners with the shared round-robin balancer so the
    // `telemost-local` outbound has a deterministic port regardless of how
    // many room links the manifest carried.
    wbstream_balancer::start_balancer(config::TELEMOST_LOCAL_BALANCER_PORT, ports).await
}

async fn start_one_sidecar(
    joiner: &Path,
    room_link: &str,
    socks_port: u16,
    index: usize,
) -> Result<(), String> {
    let log_path = config::data_dir().join(format!("telemost-joiner-{}.log", index));
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("open Telemost log: {}", e))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("clone Telemost log: {}", e))?;

    let mut command = wbstream::silent_command(&joiner);
    let (vp8_fps, vp8_batch) = vp8_pacing_from_env();
    command.args([
        "--tm-link",
        room_link,
        "--name",
        &format!("lumen-{}", index),
        "--socks-host",
        "127.0.0.1",
        "--socks-port",
        &socks_port.to_string(),
        "--resources",
        "default",
        "--vp8-fps",
        &vp8_fps.to_string(),
        "--vp8-batch",
        &vp8_batch.to_string(),
    ]);

    let child = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("start Telemost joiner {}: {}", joiner.display(), e))?;

    {
        let mut slot = sidecars_slot().lock().unwrap();
        slot.push(child);
    }

    wait_for_sidecar_ready(&log_path, socks_port, Duration::from_secs(30)).await
}

pub fn stop_sidecars() {
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
        // The joiner prints "=== VP8 TUNNEL CONNECTED ===" once the WebRTC
        // carrier is up and the SOCKS listener accepts traffic.
        if port_open(socks_port).await && tunnel_connected(log_path) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
        if !sidecar_is_running() {
            return Err("Telemost joiner exited before the tunnel became ready".to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("Telemost joiner did not finish tunnel setup in time".to_string())
}

fn tunnel_connected(log_path: &Path) -> bool {
    std::fs::read_to_string(log_path)
        .map(|log| log.contains("TUNNEL CONNECTED"))
        .unwrap_or(false)
}

async fn port_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(350),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

fn vp8_pacing_from_env() -> (u16, u16) {
    (
        parse_positive_u16_env("LUMEN_TELEMOST_VP8_FPS").unwrap_or(DEFAULT_VP8_FPS),
        parse_positive_u16_env("LUMEN_TELEMOST_VP8_BATCH").unwrap_or(DEFAULT_VP8_BATCH),
    )
}

fn parse_positive_u16_env(name: &str) -> Option<u16> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|n| *n > 0)
}

fn find_joiner(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("resource dir unavailable: {}", e))?;
    let candidates = [
        resource_dir
            .join("_up_")
            .join("bin")
            .join("headless-telemost-joiner"),
        resource_dir.join("headless-telemost-joiner"),
        PathBuf::from("/opt/getlumen/telemost/headless-telemost-joiner"),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| "headless-telemost-joiner is not bundled".to_string())
}
