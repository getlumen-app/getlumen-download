use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::Emitter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Tun,
    Proxy,
}

impl TransportKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tun" => Some(Self::Tun),
            "proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeOutcome {
    Healthy,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorAction {
    Stay,
    ActivateFallback,
}

impl MonitorAction {
    pub fn as_str(self) -> &'static str {
        match self {
            MonitorAction::Stay => "stay",
            MonitorAction::ActivateFallback => "activate_fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthPolicy {
    pub consecutive_failures_to_switch: u8,
}

#[derive(Debug, Serialize)]
pub struct HealthDecision {
    pub consecutive_failures: u8,
    pub action: &'static str,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            consecutive_failures_to_switch: 2,
        }
    }
}

pub fn next_failure_count(previous: u8, outcome: ProbeOutcome) -> u8 {
    match outcome {
        ProbeOutcome::Healthy => 0,
        ProbeOutcome::Failed => previous.saturating_add(1),
    }
}

/// Telemost fallback (2026-09-22): when a verified Telemost manifest is cached
/// and the TUN transport keeps failing, start the local joiner sidecars. The
/// `whitelist-auto` urltest then picks up `telemost-local` and hands the route
/// back to a foreign exit once one probes healthy again.
pub fn decide_action(
    transport: TransportKind,
    consecutive_failures: u8,
    policy: HealthPolicy,
    fallback_available: bool,
) -> MonitorAction {
    if transport == TransportKind::Tun
        && fallback_available
        && consecutive_failures >= policy.consecutive_failures_to_switch
    {
        return MonitorAction::ActivateFallback;
    }
    MonitorAction::Stay
}

/// Backend-owned health probe loop (2026-09-22). The probe used to live in a
/// WebView `setInterval`, but WKWebView throttles timers for occluded windows —
/// a menubar app spends most of its life with the window hidden, so the
/// fallback never fired in real use. The loop now runs on the Rust side for
/// the whole TUN session regardless of window visibility.
pub const PROBE_WARMUP: Duration = Duration::from_secs(5);
pub const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// Emitted once per TUN session when the fallback sidecars come up, so the
/// shell can surface it later without polling.
pub const FALLBACK_ACTIVE_EVENT: &str = "lumen://telemost-fallback-active";

/// Slot on `AppState` holding the running monitor task; `None` = not running.
pub type MonitorSlot = Mutex<Option<JoinHandle<()>>>;

pub fn new_monitor_slot() -> MonitorSlot {
    Mutex::new(None)
}

/// (Re)start the monitor for a fresh TUN session. Any previous task is
/// aborted so a reconnect never leaves two loops probing.
pub fn start(app: &tauri::AppHandle, slot: &MonitorSlot) {
    spawn_into(slot, monitor_loop(app.clone()));
}

/// Abort the monitor if one is running. Idempotent.
pub fn stop(slot: &MonitorSlot) {
    if let Some(task) = slot.lock().unwrap().take() {
        task.abort();
    }
}

fn spawn_into<F>(slot: &MonitorSlot, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    stop(slot);
    *slot.lock().unwrap() = Some(tauri::async_runtime::spawn(fut));
}

async fn monitor_loop(app: tauri::AppHandle) {
    tokio::time::sleep(PROBE_WARMUP).await;
    let mut consecutive_failures: u8 = 0;
    let mut fallback_started = false;
    loop {
        let outcome = match crate::probe_internet().await {
            Ok(true) => ProbeOutcome::Healthy,
            _ => ProbeOutcome::Failed,
        };
        consecutive_failures = next_failure_count(consecutive_failures, outcome);
        let action = decide_action(
            TransportKind::Tun,
            consecutive_failures,
            HealthPolicy::default(),
            crate::telemost_fallback_available_flag(),
        );
        if action == MonitorAction::ActivateFallback && !fallback_started {
            log::warn!(
                "health monitor: {} consecutive probe failures under TUN, activating Telemost fallback",
                consecutive_failures
            );
            match activate_fallback(&app).await {
                Ok(port) => {
                    fallback_started = true;
                    log::warn!("Telemost fallback active: local SOCKS on {}", port);
                    if let Err(e) = app.emit(FALLBACK_ACTIVE_EVENT, port) {
                        log::warn!("fallback event emit failed: {}", e);
                    }
                }
                Err(e) => log::warn!("Telemost fallback start failed: {}", e),
            }
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

#[cfg(target_os = "macos")]
async fn activate_fallback(app: &tauri::AppHandle) -> Result<u16, String> {
    crate::telemost::start_sidecars_from_cached_manifest(app).await
}

#[cfg(not(target_os = "macos"))]
async fn activate_fallback(_app: &tauri::AppHandle) -> Result<u16, String> {
    Err("Telemost fallback is not supported on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_probe_resets_failure_count() {
        assert_eq!(next_failure_count(2, ProbeOutcome::Healthy), 0);
    }

    #[test]
    fn failed_probe_increments_failure_count() {
        assert_eq!(next_failure_count(1, ProbeOutcome::Failed), 2);
    }

    #[test]
    fn tun_switches_after_threshold_when_fallback_available() {
        assert_eq!(
            decide_action(TransportKind::Tun, 2, HealthPolicy::default(), true),
            MonitorAction::ActivateFallback
        );
    }

    #[test]
    fn tun_stays_after_threshold_when_no_fallback() {
        assert_eq!(
            decide_action(TransportKind::Tun, 2, HealthPolicy::default(), false),
            MonitorAction::Stay
        );
    }

    #[test]
    fn tun_does_not_switch_on_single_failure() {
        assert_eq!(
            decide_action(TransportKind::Tun, 1, HealthPolicy::default(), true),
            MonitorAction::Stay
        );
    }

    #[test]
    fn proxy_mode_does_not_switch() {
        assert_eq!(
            decide_action(TransportKind::Proxy, 3, HealthPolicy::default(), true),
            MonitorAction::Stay
        );
    }

    #[tokio::test]
    async fn monitor_slot_stop_aborts_task() {
        let slot = new_monitor_slot();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        spawn_into(&slot, async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        });
        assert!(slot.lock().unwrap().is_some());
        stop(&slot);
        assert!(slot.lock().unwrap().is_none());
        // The aborted task is dropped → the oneshot sender it owned closes.
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn monitor_slot_replaces_running_task() {
        let slot = new_monitor_slot();
        let (tx1, rx1) = tokio::sync::oneshot::channel::<()>();
        spawn_into(&slot, async move {
            let _tx = tx1;
            std::future::pending::<()>().await;
        });
        let (tx2, rx2) = tokio::sync::oneshot::channel::<()>();
        spawn_into(&slot, async move {
            let _tx = tx2;
            std::future::pending::<()>().await;
        });
        // First task was aborted by the respawn; second is still alive.
        assert!(rx1.await.is_err());
        assert!(slot.lock().unwrap().is_some());
        stop(&slot);
        assert!(rx2.await.is_err());
    }
}
