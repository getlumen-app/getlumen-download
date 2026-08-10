//! Windows privileged TUN runtime.
//!
//! Windows can only create a Wintun adapter from an elevated process. Lumen
//! follows the model proven by the working Windows clients (v2rayN, NekoRay,
//! Hiddify): the *app* is relaunched once with administrator rights when the
//! user enables TUN, and `sing-box.exe` then runs as an ordinary child process
//! of Lumen.
//!
//! That keeps the whole lifecycle inside Lumen:
//!   - stdout/stderr land in `singbox-tun.log`, so a failed TUN is diagnosable;
//!   - readiness is verified with a real request before we report "connected";
//!   - disconnect terminates our own child — no second UAC prompt.
//!
//! The previous design elevated `sing-box.exe` per connect through
//! `ShellExecuteEx`, which produced an unreadable orphan process (no captured
//! log, one UAC prompt to start and another to `taskkill`). Elevated orphans
//! from that era are still handled: `stop` falls back to an elevated taskkill
//! when it cannot terminate the recorded process directly.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;

/// How long the TUN route may take to carry a real request before we call the
/// attempt failed and give the machine its normal routing back.
const READINESS_TIMEOUT: Duration = Duration::from_secs(25);

/// How long a pinned exit gets to prove itself before we fall back to Auto.
/// Long enough for a slow-but-working exit, short enough to still leave most of
/// the readiness budget for the retry.
const PIN_RECOVERY_AFTER: Duration = Duration::from_secs(9);

/// Set when readiness only succeeded after dropping a dead manual exit pin, so
/// the command layer can tell the UI to stop re-applying that pin.
static PIN_WAS_RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Consume the "we dropped the pin" flag from the last successful start.
pub fn take_pin_reset() -> bool {
    PIN_WAS_RESET.swap(false, std::sync::atomic::Ordering::SeqCst)
}

pub const ELEVATION_REQUIRED_MESSAGE: &str =
    "TUN mode needs Lumen to run as administrator. Open Settings → VPN Mode → Enable TUN Mode to restart Lumen with administrator rights.";

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

/// sing-box output for the TUN session. Kept separate from the System Proxy
/// log so a TUN failure is never overwritten by the proxy runtime.
pub fn tun_log_path() -> PathBuf {
    crate::config::data_dir().join("singbox-tun.log")
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
    let mut candidates = Vec::new();
    if let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        candidates.push(parent.join("_up_").join("bin").join("sing-box.exe"));
        candidates.push(parent.join("bin").join("sing-box.exe"));
        candidates.push(parent.join("sing-box.exe"));
    }
    // Dev runs (`npm run tauri dev`) execute from the repo root.
    candidates.push(PathBuf::from("bin").join("sing-box.exe"));
    candidates
}

/// True when this process holds an elevated (administrator) token.
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = OwnedHandle(token);

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size,
            &mut size,
        )
    };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Relaunch Lumen itself with administrator rights, mirroring how v2rayN and
/// NekoRay unlock TUN on Windows. The caller is responsible for shutting the
/// current (unelevated) instance down once this returns `Ok`.
pub fn relaunch_self_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not locate Lumen: {e}"))?;
    launch_elevated_with_show(&exe, "", false, SW_SHOWNORMAL).map(|_| ())
}

pub fn is_helper_installed() -> bool {
    candidate_singbox_paths().iter().any(|path| path.is_file())
}

/// On Windows there is no separate helper daemon: the privileged runtime is
/// "available" exactly when Lumen itself can create a Wintun adapter.
pub fn tun_runtime_available() -> bool {
    is_helper_installed() && is_elevated()
}

pub async fn is_helper_running() -> bool {
    tun_runtime_available()
}

/// Validate the bundled runtime. Elevation itself is requested by the caller
/// (`tun_commands::tun_install_helper`), which owns the app handle needed to
/// restart Lumen.
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
        } => start(&config_path, &singbox_path)
            .await
            .map(|pid| Response::Started { pid }),
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

async fn start(config_path: &str, singbox_path: &str) -> Result<u32, String> {
    if !is_elevated() {
        return Err(ELEVATION_REQUIRED_MESSAGE.to_string());
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

    let log_path = tun_log_path();
    let log_file = std::fs::File::create(&log_path)
        .map_err(|e| format!("cannot create TUN log {}: {e}", log_path.display()))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("cannot clone TUN log handle: {e}"))?;

    let mut child = silent_command(&requested_singbox)
        .args(["run", "-c"])
        .arg(&requested_config)
        .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .map_err(|e| format!("could not start sing-box: {e}"))?;
    let pid = child.id();

    let record = TunProcessRecord {
        pid,
        singbox_path: requested_singbox.to_string_lossy().to_string(),
        started_unix_secs: now_unix_secs(),
    };
    write_record(&record)?;

    match wait_until_tun_carries_traffic(&mut child).await {
        Ok(()) => {
            log::info!("Windows TUN ready (sing-box pid {})", pid);
            Ok(pid)
        }
        Err(reason) => {
            let _ = child.kill();
            let _ = child.wait();
            clear_record();
            Err(format!("{reason}{}", log_tail_suffix()))
        }
    }
}

/// Readiness gate: the TUN is only "connected" once a real request survives it.
///
/// The 2.5.8 canary reported success on process liveness alone and left the
/// machine without internet when routing was up but the tunnel was not.
///
/// Half-way through the budget a still-silent tunnel gets one self-repair
/// attempt: drop any manual exit pin back to Auto. A pin restored from
/// sing-box's `cache.db` is the one failure that TUN cannot survive on its own,
/// because DNS detours through the same selector.
async fn wait_until_tun_carries_traffic(child: &mut std::process::Child) -> Result<(), String> {
    PIN_WAS_RESET.store(false, std::sync::atomic::Ordering::SeqCst);

    let probe = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION"), " tun-readiness"))
        .build()
        .map_err(|e| format!("TUN readiness probe unavailable: {e}"))?;

    let started = std::time::Instant::now();
    let deadline = started + READINESS_TIMEOUT;
    let mut last_error = "TUN did not become ready".to_string();
    let mut pin_dropped = false;

    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("sing-box exited during TUN startup ({status})"));
        }

        for url in [
            "https://www.cloudflare.com/cdn-cgi/trace",
            "https://config.getlumen.download/health",
        ] {
            match probe.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if pin_dropped {
                        PIN_WAS_RESET.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    return Ok(());
                }
                Ok(resp) => last_error = format!("{url} returned {}", resp.status()),
                Err(e) => last_error = format!("{url} failed: {e}"),
            }
        }

        if !pin_dropped && started.elapsed() >= PIN_RECOVERY_AFTER {
            pin_dropped = true;
            match crate::clash_api::reset_selector_to_auto().await {
                Ok(()) => log::warn!(
                    "TUN carried no traffic in {}s; dropped the manual exit pin back to {}",
                    PIN_RECOVERY_AFTER.as_secs(),
                    crate::clash_api::AUTO_MEMBER
                ),
                Err(e) => log::warn!("could not drop the manual exit pin: {e}"),
            }
        }

        if std::time::Instant::now() >= deadline {
            let hint = if pin_dropped {
                " (also tried the auto-selected exit)"
            } else {
                ""
            };
            return Err(format!(
                "TUN interface came up but no traffic passed through it{hint}: {last_error}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

/// Last lines of the sing-box TUN log, appended to user-visible errors so a
/// failure is actionable instead of anonymous.
fn log_tail_suffix() -> String {
    let Ok(body) = std::fs::read_to_string(tun_log_path()) else {
        return String::new();
    };
    let tail: Vec<&str> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(3)
        .collect();
    if tail.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    format!(" (sing-box: {})", tail.join(" | "))
}

fn stop_recorded_process() -> Result<(), String> {
    let Some(record) = read_record() else {
        return Ok(());
    };
    if !record_is_running(&record) {
        clear_record();
        return Ok(());
    }

    // Same-integrity child: terminate directly, no UAC prompt.
    if terminate_directly(record.pid) && wait_until_gone(&record) {
        clear_record();
        return Ok(());
    }

    // Orphan from an elevated ShellExecuteEx session (or a foreign integrity
    // level): fall back to an elevated taskkill of that exact PID.
    let taskkill = system32_path("taskkill.exe");
    if !taskkill.is_file() {
        return Err(format!(
            "Windows taskkill not found: {}",
            taskkill.display()
        ));
    }
    let params = format!("/PID {} /T /F", record.pid);
    let _ = launch_elevated(&taskkill, &params, true)?;

    if wait_until_gone(&record) {
        clear_record();
        return Ok(());
    }
    Err(format!("TUN process {} did not stop", record.pid))
}

fn terminate_directly(pid: u32) -> bool {
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let handle = OwnedHandle(handle);
    unsafe { TerminateProcess(handle.0, 1) != 0 }
}

fn wait_until_gone(record: &TunProcessRecord) -> bool {
    for _ in 0..20 {
        if !record_is_running(record) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn system32_path(program: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(program)
}

fn launch_elevated(program: &Path, parameters: &str, wait: bool) -> Result<u32, String> {
    launch_elevated_with_show(program, parameters, wait, SW_HIDE)
}

fn launch_elevated_with_show(
    program: &Path,
    parameters: &str,
    wait: bool,
    show: i32,
) -> Result<u32, String> {
    let verb = to_wide(OsStr::new("runas"));
    let file = to_wide(program.as_os_str());
    let params = to_wide(OsStr::new(parameters));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = show;

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

    /// The runtime must never claim TUN is available without administrator
    /// rights — that was the shape of the "connected but no internet" canary.
    #[test]
    fn tun_runtime_requires_elevation() {
        if !super::is_elevated() {
            assert!(
                !super::tun_runtime_available(),
                "unelevated Lumen must report the Windows TUN runtime as unavailable"
            );
        }
    }
}
