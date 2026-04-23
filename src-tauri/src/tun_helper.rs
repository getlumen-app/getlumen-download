/// Tauri-side IPC client for the Lumen privileged helper daemon.
///
/// Talks to /var/run/io.getlumen.helper.sock using line-delimited JSON.
/// The helper runs as root (installed via osascript admin prompt one-time)
/// and spawns sing-box in TUN mode.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

const SOCKET_PATH: &str = "/var/run/io.getlumen.helper.sock";
const HELPER_INSTALL_PATH: &str = "/Library/PrivilegedHelperTools/io.getlumen.helper";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Start {
        config_path: String,
        singbox_path: String,
    },
    Stop,
    Status,
    Uninstall,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
    },
    Started {
        pid: u32,
    },
    Stopped,
    Status {
        running: bool,
        pid: Option<u32>,
        uptime_secs: Option<u64>,
    },
    Uninstalling,
    Error {
        message: String,
    },
}

/// Check if helper is installed (binary + plist exist).
pub fn is_helper_installed() -> bool {
    Path::new(HELPER_INSTALL_PATH).exists()
        && Path::new("/Library/LaunchDaemons/io.getlumen.helper.plist").exists()
}

/// Check if helper is responding (socket exists + ping works).
pub async fn is_helper_running() -> bool {
    if !Path::new(SOCKET_PATH).exists() {
        return false;
    }
    matches!(send(Request::Ping).await, Ok(Response::Pong { .. }))
}

/// Install helper via osascript admin prompt.
/// Bundled paths inside Lumen.app:
///   - source helper: <Resources>/lumen-helper
///   - installer: <Resources>/lumen-installer
pub fn install_helper(installer_path: &str, source_helper_path: &str) -> Result<(), String> {
    if !Path::new(installer_path).exists() {
        return Err(format!("installer not found: {}", installer_path));
    }
    if !Path::new(source_helper_path).exists() {
        return Err(format!("source helper not found: {}", source_helper_path));
    }

    // Quote paths for shell, escape any single quotes by ending the quoted string and prepending escaped quote
    let shell_cmd = format!(
        "'{}' install '{}'",
        installer_path.replace('\'', "'\\''"),
        source_helper_path.replace('\'', "'\\''")
    );
    // Then wrap in osascript "with administrator privileges"
    // AppleScript needs double-quote escapes inside the do shell script string
    let script = format!(
        r#"do shell script "{}" with administrator privileges with prompt "Lumen needs to install a privileged helper to enable VPN tunnel mode. This is a one-time setup.""#,
        shell_cmd.replace('\\', "\\\\").replace('"', "\\\"")
    );

    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("osascript spawn: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // User cancelled = error -128
        if stderr.contains("-128") || stderr.contains("User canceled") {
            return Err("User cancelled the password prompt".to_string());
        }
        return Err(format!("Helper install failed: {}", stderr));
    }
    Ok(())
}

/// Uninstall helper (also via osascript admin prompt).
pub fn uninstall_helper(installer_path: &str) -> Result<(), String> {
    if !Path::new(installer_path).exists() {
        return Err(format!("installer not found: {}", installer_path));
    }
    let script = format!(
        r#"do shell script "'{}' uninstall" with administrator privileges with prompt "Lumen wants to remove its VPN helper.""#,
        installer_path
            .replace('\'', "'\\''")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    );
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("osascript spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "Helper uninstall failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Send one request, await one response (with 10s timeout).
pub async fn send(req: Request) -> Result<Response, String> {
    let connect = timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH))
        .await
        .map_err(|_| "Connect timeout".to_string())?
        .map_err(|e| format!("Connect failed: {}", e))?;

    let (read_half, mut write_half) = connect.into_split();
    let mut reader = BufReader::new(read_half);

    let mut json = serde_json::to_string(&req).map_err(|e| format!("encode: {}", e))?;
    json.push('\n');
    write_half
        .write_all(json.as_bytes())
        .await
        .map_err(|e| format!("write: {}", e))?;
    write_half
        .flush()
        .await
        .map_err(|e| format!("flush: {}", e))?;

    let mut line = String::new();
    timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .map_err(|_| "Read timeout".to_string())?
        .map_err(|e| format!("read: {}", e))?;

    serde_json::from_str(line.trim()).map_err(|e| format!("decode: {}", e))
}
