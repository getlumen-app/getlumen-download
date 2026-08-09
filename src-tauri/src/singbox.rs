use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const LOCAL_PROXY_PORT: u16 = 10808;

/// Build a `Command` that never flashes a console window on Windows.
/// A child process spawned without `CREATE_NO_WINDOW` briefly pops a console
/// window. `get_effective_status` polls `is_running()` every 5s (plus the
/// connect/disconnect kill paths), so a missing flag made the whole screen
/// blink every 5 seconds (Polina, Windows, 2026-06-10). Every process spawn in
/// this module goes through this one builder so the flag can never be missed.
fn silent_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
        return cmd;
    }
    #[cfg(not(windows))]
    cmd
}

pub struct SingboxManager {
    running: bool,
}

/// True when a process command line is the System Proxy sing-box (`config.json`),
/// not the privileged helper TUN instance (`config-tun.json`).
///
/// Tun cmdline example (must return false):
///   sing-box run -c …/io.getlumen.app/config-tun.json
/// Proxy cmdline example (must return true):
///   sing-box run -c …/io.getlumen.app/config.json
///
/// Note: `"config.json"` is NOT a substring of `"config-tun.json"`.
pub fn cmdline_is_proxy_singbox(cmdline: &str) -> bool {
    let c = cmdline.to_ascii_lowercase();
    if !c.contains("sing-box") {
        return false;
    }
    // Helper TUN / last-good must never count as System Proxy.
    if c.contains("config-tun") {
        return false;
    }
    c.contains("config.json")
}

/// Scan process table for a System Proxy sing-box (never helper TUN).
fn any_proxy_singbox_process() -> bool {
    #[cfg(unix)]
    {
        let output = silent_command("ps")
            .args(["-ax", "-o", "command="])
            .output();
        match output {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(cmdline_is_proxy_singbox),
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        // tasklist has no cmdline — use WMIC; fall back to port check via name
        // only when CommandLine is unavailable (still reject if we cannot tell).
        let output = silent_command("wmic")
            .args([
                "process",
                "where",
                "name='sing-box.exe'",
                "get",
                "CommandLine",
            ])
            .output();
        match output {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                // WMIC prints a header line "CommandLine" then rows.
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("CommandLine"))
                    .any(cmdline_is_proxy_singbox)
            }
            Err(_) => false,
        }
    }
}

/// Stop only System Proxy sing-box processes. Never `killall sing-box` — that
/// also murders the helper TUN instance and causes Stop→Start thrash.
fn kill_proxy_singbox_processes(force: bool) {
    #[cfg(unix)]
    {
        // BRE: config\.json does not match config-tun.json.
        let signal = if force { "-9" } else { "-15" };
        silent_command("pkill")
            .args([signal, "-f", "sing-box.*config\\.json"])
            .output()
            .ok();
    }
    #[cfg(windows)]
    {
        // Enumerate and taskkill by PID only when CommandLine is proxy config.
        let output = silent_command("wmic")
            .args([
                "process",
                "where",
                "name='sing-box.exe'",
                "get",
                "ProcessId,CommandLine",
                "/FORMAT:LIST",
            ])
            .output();
        if let Ok(o) = output {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut cmdline = String::new();
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line
                    .strip_prefix("CommandLine=")
                    .or_else(|| line.strip_prefix("Commandline="))
                {
                    cmdline = rest.to_string();
                } else if let Some(pid) = line
                    .strip_prefix("ProcessId=")
                    .or_else(|| line.strip_prefix("Processid="))
                {
                    if cmdline_is_proxy_singbox(&cmdline) {
                        silent_command("taskkill")
                            .args(["/F", "/PID", pid.trim()])
                            .output()
                            .ok();
                    }
                    cmdline.clear();
                }
            }
        }
    }
}

impl SingboxManager {
    pub fn new() -> Self {
        let running = Self::check_running_static();
        Self { running }
    }

    pub fn start(&mut self, config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        if self.running {
            self.stop()?;
        }

        let singbox_bin = Self::find_binary()?;

        // Ensure executable (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&singbox_bin, std::fs::Permissions::from_mode(0o755)).ok();
            silent_command("xattr")
                .args(["-d", "com.apple.quarantine", &singbox_bin])
                .output()
                .ok();
        }

        // Validate config (env var required for legacy DNS server format support)
        let check = silent_command(&singbox_bin)
            .args(["check", "-c", config_path])
            .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
            .output()
            .map_err(|e| format!("Cannot run sing-box: {}", e))?;

        if !check.status.success() {
            let stderr = String::from_utf8_lossy(&check.stderr);
            let fatal: Vec<&str> = stderr.lines().filter(|l| l.contains("FATAL")).collect();
            if !fatal.is_empty() {
                return Err(format!("Config error: {}", fatal.join("; ")).into());
            }
        }

        // Kill any old System Proxy sing-box (never helper TUN).
        Self::kill_all();
        if !Self::wait_until_stopped(Duration::from_secs(2), Duration::from_millis(50)) {
            Self::force_kill();
            if !Self::wait_until_stopped(Duration::from_millis(500), Duration::from_millis(50)) {
                return Err("old System Proxy sing-box did not stop".into());
            }
        }

        // Start sing-box with log output to file
        let log_path = super::config::data_dir().join("singbox.log");
        let log_file = std::fs::File::create(&log_path)
            .map_err(|e| format!("Cannot create log file: {}", e))?;
        let log_err = log_file
            .try_clone()
            .map_err(|e| format!("Cannot clone log file: {}", e))?;

        log::info!(
            "Starting sing-box: {} (log: {})",
            singbox_bin,
            log_path.display()
        );

        // One spawn for both platforms; silent_command() adds CREATE_NO_WINDOW
        // on Windows so sing-box never flashes a console window.
        let mut child = silent_command(&singbox_bin)
            .args(["run", "-c", config_path])
            .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("Failed to start sing-box: {}", e))?;

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if let Some(status) = child.try_wait()? {
                return Err(
                    format!("sing-box exited immediately ({status}). Check config.").into(),
                );
            }
            if Self::check_running_static() {
                self.running = true;
                log::info!("sing-box started in proxy mode");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Err("sing-box did not appear in process table after startup".into())
    }

    pub fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Stopping System Proxy sing-box");
        Self::kill_all();
        if !Self::wait_until_stopped(Duration::from_millis(500), Duration::from_millis(50)) {
            Self::force_kill();
            Self::wait_until_stopped(Duration::from_millis(500), Duration::from_millis(50));
        }
        self.running = false;
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        self.running = Self::check_running_static();
        self.running
    }

    pub fn is_ready(&mut self) -> bool {
        self.running = Self::check_running_static();
        self.running && Self::local_proxy_listener_ready()
    }

    pub fn local_proxy_listener_ready() -> bool {
        let addr = SocketAddr::from(([127, 0, 0, 1], LOCAL_PROXY_PORT));
        TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
    }

    fn check_running_static() -> bool {
        any_proxy_singbox_process()
    }

    fn kill_all() {
        kill_proxy_singbox_processes(false);
    }

    fn force_kill() {
        kill_proxy_singbox_processes(true);
    }

    fn wait_until_stopped(timeout: Duration, poll_interval: Duration) -> bool {
        let started = Instant::now();
        loop {
            if !Self::check_running_static() {
                return true;
            }
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(poll_interval);
        }
    }

    fn find_binary() -> Result<String, Box<dyn std::error::Error>> {
        #[cfg(unix)]
        let bin_name = "sing-box";
        #[cfg(windows)]
        let bin_name = "sing-box.exe";

        // 1. Check next to the app executable (Tauri Resources)
        if let Ok(exe) = std::env::current_exe() {
            if let Some(candidate) = Self::find_binary_near_exe(&exe, bin_name) {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }

        // 2. Check relative to CWD (dev mode)
        let cwd_bin = format!("bin/{}", bin_name);
        if std::path::Path::new(&cwd_bin).exists() {
            return Ok(cwd_bin);
        }

        // 3. Platform-specific fallback paths
        #[cfg(unix)]
        {
            for path in &["/usr/local/bin/sing-box", "/opt/homebrew/bin/sing-box"] {
                if std::path::Path::new(path).exists() {
                    return Ok(path.to_string());
                }
            }
        }
        #[cfg(windows)]
        {
            if let Some(home) = dirs::home_dir() {
                let candidate = home.join("sing-box.exe");
                if candidate.exists() {
                    return Ok(candidate.to_string_lossy().to_string());
                }
            }
        }

        Err(format!("{} not found", bin_name).into())
    }

    fn find_binary_near_exe(exe: &Path, bin_name: &str) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            for candidate in Self::windows_resource_candidates(&exe, bin_name) {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            for candidate in Self::macos_resource_candidates(exe, bin_name) {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    fn macos_resource_candidates(exe: &Path, bin_name: &str) -> [PathBuf; 2] {
        let resources_dir = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("Resources"))
            .unwrap_or_default();

        [
            resources_dir.join("_up_").join("bin").join(bin_name),
            resources_dir.join(bin_name),
        ]
    }

    fn windows_resource_candidates(exe: &Path, bin_name: &str) -> [PathBuf; 2] {
        let app_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();

        [
            app_dir.join("_up_").join("bin").join(bin_name),
            app_dir.join(bin_name),
        ]
    }
}

impl Drop for SingboxManager {
    fn drop(&mut self) {
        self.stop().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::{cmdline_is_proxy_singbox, SingboxManager};
    use std::path::{Path, PathBuf};

    #[test]
    fn cmdline_classifier_distinguishes_proxy_from_tun() {
        assert!(cmdline_is_proxy_singbox(
            "sing-box run -c /Users/user/Library/Caches/io.getlumen.app/config.json"
        ));
        assert!(!cmdline_is_proxy_singbox(
            "sing-box run -c /Users/user/Library/Caches/io.getlumen.app/config-tun.json"
        ));
        assert!(!cmdline_is_proxy_singbox(
            "sing-box run -c /tmp/config-tun-lastgood.json"
        ));
        assert!(!cmdline_is_proxy_singbox("nginx"));
    }

    /// Regression for the Windows "screen blinks every ~5s" bug (Polina, 2026-06-10).
    /// Root cause: process-status spawns (`tasklist`/`taskkill`) on Windows were
    /// created WITHOUT `CREATE_NO_WINDOW`, so a console window flashed every time
    /// `is_running()` ran — which the UI polls every 5s via `get_effective_status`.
    /// The sing-box `run` spawn already had the flag; the status/kill spawns were
    /// missed. Durable invariant: EVERY process spawn in this module goes through
    /// the single `silent_command()` builder (which applies `CREATE_NO_WINDOW` on
    /// Windows). So the raw constructor must appear exactly once — inside
    /// `silent_command` itself. Any new bare spawn re-introduces the console-flash
    /// bug and fails this test on the macOS host (no Windows box needed). The
    /// needle is assembled at runtime so this test's own source does not match it.
    #[test]
    fn all_process_spawns_go_through_silent_command_no_window() {
        let src = include_str!("singbox.rs");
        let raw_ctor = ["Command", "::new("].concat();
        let raw_count = src.matches(raw_ctor.as_str()).count();
        assert_eq!(
            raw_count, 1,
            "every process spawn must go through silent_command() so Windows gets \
             CREATE_NO_WINDOW (no 5s console-window flash); found {} raw constructors \
             (expected exactly 1, inside the helper)",
            raw_count
        );
        assert!(
            src.contains(&["fn silent_", "command"].concat()),
            "silent_command() helper must exist"
        );
        assert!(
            src.contains("CREATE_NO_WINDOW"),
            "CREATE_NO_WINDOW flag must be applied for Windows spawns"
        );
    }

    #[test]
    fn macos_prefers_tauri_up_bin_before_legacy_resource_root() {
        let exe = Path::new("/Applications/Lumen.app/Contents/MacOS/lumen");
        let candidates = SingboxManager::macos_resource_candidates(exe, "sing-box");

        assert_eq!(
            candidates[0],
            PathBuf::from("/Applications/Lumen.app/Contents/Resources/_up_/bin/sing-box")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from("/Applications/Lumen.app/Contents/Resources/sing-box")
        );
    }

    #[test]
    fn windows_prefers_tauri_up_bin_before_app_root() {
        let exe = Path::new("/Users/kiril/AppData/Local/Lumen/lumen.exe");
        let candidates = SingboxManager::windows_resource_candidates(exe, "sing-box.exe");

        assert_eq!(
            candidates[0],
            PathBuf::from("/Users/kiril/AppData/Local/Lumen/_up_/bin/sing-box.exe")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from("/Users/kiril/AppData/Local/Lumen/sing-box.exe")
        );
    }
}
