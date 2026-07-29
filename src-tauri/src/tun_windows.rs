//! Windows privileged TUN runtime.
//!
//! The Tauri UI remains unelevated. Only the bundled sing-box process is
//! started with the Windows `runas` verb, and its exact PID + executable path
//! are persisted so disconnect never falls back to an unscoped name-based kill.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetProcessId, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
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
#[serde(tag = "status", rename_all = "snake_case")]
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

#[derive(Debug, Serialize, Deserialize)]
struct TunProcessRecord {
    pid: u32,
    singbox_path: String,
    started_unix_secs: u64,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn pid_record_path() -> PathBuf {
    crate::config::data_dir().join("tun-process.json")
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn candidate_singbox_paths() -> Vec<PathBuf> {
    let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    vec![
        parent.join("_up_").join("bin").join("sing-box.exe"),
        parent.join("bin").join("sing-box.exe"),
        parent.join("sing-box.exe"),
    ]
}

fn windows_tun_canary_enabled() -> bool {
    std::env::var("LUMEN_WINDOWS_TUN_CANARY").ok().as_deref() == Some("1")
}

pub fn is_helper_installed() -> bool {
    windows_tun_canary_enabled() && candidate_singbox_paths().iter().any(|path| path.is_file())
}

pub async fn is_helper_running() -> bool {
    is_helper_installed()
}

pub fn install_helper(_installer_path: &str, singbox_path: &str) -> Result<(), String> {
    let path = Path::new(singbox_path);
    if !path.is_file() {
        return Err(format!("bundled sing-box not found: {}", path.display()));
    }
    let output = silent_command(path)
        .arg("version")
        .output()
        .map_err(|e| format!("could not inspect bundled sing-box: {e}"))?;
    if !output.status.success() {
        return Err("bundled sing-box failed its version check".to_string());
    }
    Ok(())
}

pub fn uninstall_helper(_installer_path: &str) -> Result<(), String> {
    stop_recorded_process()
}

pub async fn send(req: Request) -> Result<Response, String> {
    match req {
        Request::Ping => Ok(Response::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        Request::Start {
            config_path,
            singbox_path,
        } => start(&config_path, &singbox_path).map(|pid| Response::Started { pid }),
        Request::Stop => {
            stop_recorded_process()?;
            Ok(Response::Stopped)
        }
        Request::Status => {
            let record = read_record();
            let running = record.as_ref().map(record_is_running).unwrap_or(false);
            if !running {
                clear_record();
            }
            Ok(Response::Status {
                running,
                pid: record.as_ref().filter(|_| running).map(|record| record.pid),
                uptime_secs: record
                    .as_ref()
                    .filter(|_| running)
                    .map(|record| now_unix_secs().saturating_sub(record.started_unix_secs)),
            })
        }
        Request::Uninstall => {
            stop_recorded_process()?;
            Ok(Response::Uninstalling)
        }
    }
}

fn start(config_path: &str, singbox_path: &str) -> Result<u32, String> {
    if !windows_tun_canary_enabled() {
        return Err(
            "Windows TUN canary is disabled; use System Proxy while validation is pending"
                .to_string(),
        );
    }
    if let Some(record) = read_record().filter(record_is_running) {
        return Ok(record.pid);
    }
    clear_record();

    let expected_config = crate::config::tun_config_file_path()
        .canonicalize()
        .map_err(|e| format!("TUN config unavailable: {e}"))?;
    let requested_config = Path::new(config_path)
        .canonicalize()
        .map_err(|e| format!("TUN config path invalid: {e}"))?;
    if requested_config != expected_config {
        return Err("refusing a TUN config outside Lumen's data directory".to_string());
    }

    let requested_singbox = Path::new(singbox_path)
        .canonicalize()
        .map_err(|e| format!("bundled sing-box path invalid: {e}"))?;
    if requested_singbox
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| !name.eq_ignore_ascii_case("sing-box.exe"))
        .unwrap_or(true)
    {
        return Err("refusing to elevate a non-sing-box executable".to_string());
    }

    let params = format!("run -c \"{}\"", requested_config.display());
    let pid = launch_elevated(&requested_singbox, &params, false)?;
    let record = TunProcessRecord {
        pid,
        singbox_path: requested_singbox.to_string_lossy().to_string(),
        started_unix_secs: now_unix_secs(),
    };
    write_record(&record)?;

    std::thread::sleep(std::time::Duration::from_millis(700));
    if !record_is_running(&record) {
        clear_record();
        return Err("elevated sing-box exited before the TUN became ready".to_string());
    }
    Ok(pid)
}

fn stop_recorded_process() -> Result<(), String> {
    let Some(record) = read_record() else {
        return Ok(());
    };
    if !record_is_running(&record) {
        clear_record();
        return Ok(());
    }

    let taskkill = system32_path("taskkill.exe");
    if !taskkill.is_file() {
        return Err(format!(
            "Windows taskkill not found: {}",
            taskkill.display()
        ));
    }
    let params = format!("/PID {} /T /F", record.pid);
    let _ = launch_elevated(&taskkill, &params, true)?;

    for _ in 0..20 {
        if !record_is_running(&record) {
            clear_record();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("elevated TUN process {} did not stop", record.pid))
}

fn system32_path(program: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(program)
}

fn launch_elevated(program: &Path, parameters: &str, wait: bool) -> Result<u32, String> {
    let verb = to_wide(OsStr::new("runas"));
    let file = to_wide(program.as_os_str());
    let params = to_wide(OsStr::new(parameters));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;

    let launched = unsafe { ShellExecuteExW(&mut info) };
    if launched == 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(1223) {
            Err("Windows administrator prompt was cancelled".to_string())
        } else {
            Err(format!("Windows elevation failed: {error}"))
        };
    }
    if info.hProcess.is_null() {
        return Err("Windows elevation returned no process handle".to_string());
    }

    let handle = OwnedHandle(info.hProcess);
    let pid = unsafe { GetProcessId(handle.0) };
    if pid == 0 {
        return Err(format!(
            "could not read elevated process id: {}",
            std::io::Error::last_os_error()
        ));
    }
    if wait {
        unsafe {
            WaitForSingleObject(handle.0, 15_000);
        }
    }
    Ok(pid)
}

fn record_is_running(record: &TunProcessRecord) -> bool {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_PROCESS,
            0,
            record.pid,
        )
    };
    if handle.is_null() {
        return false;
    }
    let handle = OwnedHandle(handle);
    if unsafe { WaitForSingleObject(handle.0, 0) } != WAIT_TIMEOUT {
        return false;
    }

    let mut buffer = vec![0u16; 32_768];
    let mut size = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut size) } == 0 {
        return false;
    }
    let actual = String::from_utf16_lossy(&buffer[..size as usize]);
    normalize_windows_path(&actual) == normalize_windows_path(&record.singbox_path)
}

fn normalize_windows_path(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn read_record() -> Option<TunProcessRecord> {
    let body = std::fs::read_to_string(pid_record_path()).ok()?;
    serde_json::from_str(&body).ok()
}

fn write_record(record: &TunProcessRecord) -> Result<(), String> {
    let body = serde_json::to_string(record).map_err(|e| format!("PID record encode: {e}"))?;
    std::fs::write(pid_record_path(), body).map_err(|e| format!("PID record write: {e}"))
}

fn clear_record() {
    let _ = std::fs::remove_file(pid_record_path());
}

fn silent_command(program: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use super::normalize_windows_path;

    #[test]
    fn process_path_comparison_ignores_extended_prefix_case_and_slashes() {
        assert_eq!(
            normalize_windows_path(r"\\?\C:\Program Files\Lumen\_up_\bin\sing-box.exe"),
            normalize_windows_path(r"c:/program files/lumen/_up_/bin/SING-BOX.EXE")
        );
    }
}
