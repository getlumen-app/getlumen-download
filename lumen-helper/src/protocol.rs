/// JSON IPC protocol shared between Lumen app and helper daemon.
/// One JSON object per line over Unix socket.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Health check — returns `Pong` with helper version
    Ping,
    /// Start sing-box with given config path. sing-box runs as root (TUN mode possible).
    Start {
        config_path: String,
        singbox_path: String,
    },
    /// Stop running sing-box subprocess.
    Stop,
    /// Get current status (running, pid, uptime).
    Status,
    /// Uninstall the helper daemon (stops sing-box, removes plist, unloads launchd).
    Uninstall,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong { version: String },
    Started { pid: u32 },
    Stopped,
    Status { running: bool, pid: Option<u32>, uptime_secs: Option<u64> },
    Uninstalling,
    Error { message: String },
}

pub const HELPER_SOCKET_PATH: &str = "/var/run/io.getlumen.helper.sock";
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
