/// Manages the sing-box subprocess (started/stopped on request from Lumen app).
/// Since the helper runs as root, sing-box inherits root privileges, enabling TUN mode.
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct SingboxManager {
    state: Arc<Mutex<Option<RunningProcess>>>,
}

struct RunningProcess {
    child: Child,
    pid: u32,
    started_at: Instant,
}

impl SingboxManager {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(None)) }
    }

    pub async fn start(&self, singbox_path: &str, config_path: &str) -> Result<u32, String> {
        let mut state = self.state.lock().await;
        // If already running, stop first
        if let Some(mut proc) = state.take() {
            let _ = proc.child.kill().await;
            let _ = proc.child.wait().await;
            log::info!("Stopped previous sing-box (pid {})", proc.pid);
        }

        log::info!("Starting sing-box: {} run -c {}", singbox_path, config_path);
        let child = Command::new(singbox_path)
            .args(["run", "-c", config_path])
            .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn sing-box: {}", e))?;

        let pid = child.id().ok_or("No PID returned from spawn")?;
        log::info!("sing-box started with pid {}", pid);

        *state = Some(RunningProcess {
            child,
            pid,
            started_at: Instant::now(),
        });
        Ok(pid)
    }

    pub async fn stop(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if let Some(mut proc) = state.take() {
            log::info!("Stopping sing-box (pid {})", proc.pid);
            proc.child.kill().await.map_err(|e| format!("Kill failed: {}", e))?;
            let _ = proc.child.wait().await;
            log::info!("sing-box stopped");
        } else {
            log::info!("stop() called but no sing-box running");
        }
        Ok(())
    }

    pub async fn status(&self) -> (bool, Option<u32>, Option<u64>) {
        let state = self.state.lock().await;
        match state.as_ref() {
            Some(proc) => {
                let uptime = proc.started_at.elapsed().as_secs();
                (true, Some(proc.pid), Some(uptime))
            }
            None => (false, None, None),
        }
    }
}

impl Drop for SingboxManager {
    fn drop(&mut self) {
        // Best-effort cleanup on helper shutdown
        if let Ok(mut state) = self.state.try_lock() {
            if let Some(mut proc) = state.take() {
                // Synchronous kill since we're dropping
                let _ = proc.child.start_kill();
                log::info!("Helper dropped, killed sing-box pid {}", proc.pid);
            }
        }
    }
}
