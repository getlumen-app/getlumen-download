use crate::config;
use crate::wbstream;
use crate::wbstream_balancer;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static SIDECARS: OnceLock<Mutex<Vec<Child>>> = OnceLock::new();
const DEFAULT_VP8_FPS: u16 = 60;
const DEFAULT_VP8_BATCH: u16 = 120;
const DEFAULT_PROBE_TARGET: &str = "www.gstatic.com:80";
const DATA_PROBE_ATTEMPTS: u32 = 2;
const DATA_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

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
    // Order the whole manifest by the per-install seed so a dead room can be
    // rotated past without re-selecting: candidates[0..MAX] are the preferred
    // set, the rest are fallbacks for the same client only.
    let links = config::select_telemost_room_links(&manifest, usize::MAX);
    if links.is_empty() {
        return Err("Telemost manifest has no usable room link".to_string());
    }
    start_sidecars(app, &links).await
}

/// `room_links` is an ordered candidate list, not a fixed assignment: each
/// SOCKS port walks the list until a joiner passes the data-plane probe
/// (SOCKS CONNECT + a real response byte through the tunnel). The probe is
/// what catches a dead downlink — the tunnel grants CONNECT on control
/// frames even while MsgData responses never come back — and a room already
/// occupied by another client, which flaps on peer epochs.
pub async fn start_sidecars(
    app: &tauri::AppHandle,
    room_links: &[String],
) -> Result<u16, String> {
    if sidecar_is_running() && port_open(config::TELEMOST_LOCAL_BALANCER_PORT).await {
        return Ok(config::TELEMOST_LOCAL_BALANCER_PORT);
    }
    stop_sidecars();

    let joiner = find_joiner(app)?;
    let wanted = room_links.len().min(config::TELEMOST_MAX_ROOMS);
    let mut candidates = room_links.iter();
    let mut ports = Vec::new();
    for index in 0..wanted {
        let port = config::TELEMOST_LOCAL_SOCKS_PORT + index as u16;
        let mut healthy = false;
        for link in candidates.by_ref() {
            match start_one_sidecar(&joiner, link, port, index).await {
                Ok(mut child) => {
                    if data_plane_probe(port).await {
                        let mut slot = sidecars_slot().lock().unwrap();
                        slot.push(child);
                        healthy = true;
                        break;
                    }
                    log::warn!(
                        "telemost: room {} failed data-plane probe on :{}, rotating",
                        link, port
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(e) => {
                    log::warn!(
                        "telemost: room {} did not become ready on :{}: {}, rotating",
                        link, port, e
                    );
                }
            }
        }
        if !healthy {
            break;
        }
        ports.push(port);
    }
    if ports.is_empty() {
        stop_sidecars();
        return Err(format!(
            "no Telemost room passed the data-plane probe ({} candidates tried)",
            room_links.len() - candidates.len()
        ));
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
) -> Result<Child, String> {
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

    let mut child = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("start Telemost joiner {}: {}", joiner.display(), e))?;

    match wait_for_sidecar_ready(&mut child, &log_path, socks_port, Duration::from_secs(30)).await {
        Ok(()) => Ok(child),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(e)
        }
    }
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
    child: &mut Child,
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
        if let Ok(Some(_)) = child.try_wait() {
            return Err("Telemost joiner exited before the tunnel became ready".to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("Telemost joiner did not finish tunnel setup in time".to_string())
}

/// Full-duplex check through a joiner's SOCKS port: greeting, CONNECT to the
/// probe target, then a minimal HTTP request that must return at least one
/// response byte. A granted CONNECT alone is not enough — under a dead
/// downlink the tunnel still ACKs control frames while response data is
/// silently dropped, which is exactly the failure this probe exists to catch.
async fn data_plane_probe(socks_port: u16) -> bool {
    for _ in 0..DATA_PROBE_ATTEMPTS {
        if probe_once(socks_port).await {
            return true;
        }
    }
    false
}

fn probe_target() -> String {
    std::env::var("LUMEN_TELEMOST_PROBE_TARGET")
        .ok()
        .filter(|t| t.contains(':'))
        .unwrap_or_else(|| DEFAULT_PROBE_TARGET.to_string())
}

async fn probe_once(socks_port: u16) -> bool {
    let target = probe_target();
    tokio::time::timeout(DATA_PROBE_TIMEOUT, probe_inner(socks_port, &target))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

async fn probe_inner(socks_port: u16, target: &str) -> Result<(), String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", socks_port))
        .await
        .map_err(|e| format!("probe connect: {e}"))?;

    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| format!("probe greeting: {e}"))?;
    let mut greeting = [0u8; 2];
    stream
        .read_exact(&mut greeting)
        .await
        .map_err(|e| format!("probe greeting reply: {e}"))?;
    if greeting != [0x05, 0x00] {
        return Err(format!("probe greeting refused: {greeting:02x?}"));
    }

    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| format!("probe target malformed: {target}"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("probe target port malformed: {target}"))?;
    if host.is_empty() || host.len() > 255 {
        return Err(format!("probe target host malformed: {target}"));
    }
    let mut request = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|e| format!("probe connect request: {e}"))?;

    let mut reply_head = [0u8; 4];
    stream
        .read_exact(&mut reply_head)
        .await
        .map_err(|e| format!("probe connect reply: {e}"))?;
    if reply_head[1] != 0x00 {
        return Err(format!("probe connect refused: rep={:#04x}", reply_head[1]));
    }
    // Drain BND.ADDR per its ATYP so the HTTP request is not misaligned.
    let drain = match reply_head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .map_err(|e| format!("probe bnd len: {e}"))?;
            len[0] as usize
        }
        other => return Err(format!("probe bnd atyp unknown: {other:#04x}")),
    };
    let mut discard = vec![0u8; drain + 2];
    stream
        .read_exact(&mut discard)
        .await
        .map_err(|e| format!("probe bnd drain: {e}"))?;

    let http = format!("GET /generate_204 HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(http.as_bytes())
        .await
        .map_err(|e| format!("probe http send: {e}"))?;
    let mut byte = [0u8; 1];
    stream
        .read_exact(&mut byte)
        .await
        .map_err(|e| format!("probe response: {e}"))?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Mock SOCKS5 server: answers the greeting, grants CONNECT, then either
    /// serves one response byte or goes silent — mirroring a live vs dead
    /// tunnel downlink.
    async fn mock_socks(serve_response: bool) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            socket.write_all(&[0x05, 0x00]).await.unwrap();
            let mut head = [0u8; 5];
            socket.read_exact(&mut head).await.unwrap();
            let dlen = head[4] as usize;
            let mut rest = vec![0u8; dlen + 2];
            socket.read_exact(&mut rest).await.unwrap();
            socket
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            if serve_response {
                let mut req = vec![0u8; 512];
                let _ = socket.read(&mut req).await;
                socket
                    .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                    .await
                    .unwrap();
            } else {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn data_plane_probe_passes_when_tunnel_returns_data() {
        let port = mock_socks(true).await;
        std::env::set_var("LUMEN_TELEMOST_PROBE_TARGET", "probe.local:80");
        assert!(data_plane_probe(port).await);
    }

    #[tokio::test]
    async fn data_plane_probe_fails_when_downlink_is_silent() {
        let port = mock_socks(false).await;
        std::env::set_var("LUMEN_TELEMOST_PROBE_TARGET", "probe.local:80");
        assert!(!data_plane_probe(port).await);
    }

    #[tokio::test]
    async fn probe_inner_reports_refused_connect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            socket.write_all(&[0x05, 0x00]).await.unwrap();
            let mut head = [0u8; 5];
            socket.read_exact(&mut head).await.unwrap();
            let dlen = head[4] as usize;
            let mut rest = vec![0u8; dlen + 2];
            socket.read_exact(&mut rest).await.unwrap();
            socket
                .write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        std::env::set_var("LUMEN_TELEMOST_PROBE_TARGET", "probe.local:80");
        let err = probe_inner(port, "probe.local:80").await.unwrap_err();
        assert!(err.contains("refused"), "unexpected error: {err}");
    }
}
