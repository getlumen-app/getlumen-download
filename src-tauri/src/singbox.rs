use std::process::Command;

pub struct SingboxManager {
    running: bool,
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
            Command::new("xattr").args(["-d", "com.apple.quarantine", &singbox_bin]).output().ok();
        }

        // Validate config (env var required for legacy DNS server format support)
        let check = Command::new(&singbox_bin)
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

        // Kill any old sing-box
        Self::kill_all();
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Start sing-box with log output to file
        let log_path = super::config::data_dir().join("singbox.log");
        let log_file = std::fs::File::create(&log_path)
            .map_err(|e| format!("Cannot create log file: {}", e))?;
        let log_err = log_file.try_clone()
            .map_err(|e| format!("Cannot clone log file: {}", e))?;

        log::info!("Starting sing-box: {} (log: {})", singbox_bin, log_path.display());

        #[cfg(unix)]
        {
            Command::new(&singbox_bin)
                .args(["run", "-c", config_path])
                .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(log_err))
                .spawn()
                .map_err(|e| format!("Failed to start sing-box: {}", e))?;
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            Command::new(&singbox_bin)
                .args(["run", "-c", config_path])
                .env("ENABLE_DEPRECATED_LEGACY_DNS_SERVERS", "true")
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(log_err))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| format!("Failed to start sing-box: {}", e))?;
        }

        // Wait for startup
        std::thread::sleep(std::time::Duration::from_secs(3));

        if !Self::check_running_static() {
            return Err("sing-box exited immediately. Check config.".into());
        }

        self.running = true;
        log::info!("sing-box started in proxy mode");
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Stopping sing-box");
        Self::kill_all();
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Force kill if still alive
        if Self::check_running_static() {
            Self::force_kill();
        }
        self.running = false;
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        self.running = Self::check_running_static();
        self.running
    }

    fn check_running_static() -> bool {
        #[cfg(unix)]
        {
            Command::new("pgrep")
                .args(["-f", "sing-box run"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(windows)]
        {
            Command::new("tasklist")
                .args(["/FI", "IMAGENAME eq sing-box.exe", "/NH"])
                .output()
                .map(|o| {
                    let text = String::from_utf8_lossy(&o.stdout);
                    text.contains("sing-box.exe")
                })
                .unwrap_or(false)
        }
    }

    fn kill_all() {
        #[cfg(unix)]
        {
            Command::new("killall").arg("sing-box").output().ok();
        }
        #[cfg(windows)]
        {
            Command::new("taskkill").args(["/F", "/IM", "sing-box.exe"]).output().ok();
        }
    }

    fn force_kill() {
        #[cfg(unix)]
        {
            Command::new("pkill").args(["-9", "-f", "sing-box run"]).output().ok();
        }
        #[cfg(windows)]
        {
            Command::new("taskkill").args(["/F", "/IM", "sing-box.exe"]).output().ok();
        }
    }

    fn find_binary() -> Result<String, Box<dyn std::error::Error>> {
        #[cfg(unix)]
        let bin_name = "sing-box";
        #[cfg(windows)]
        let bin_name = "sing-box.exe";

        // 1. Check next to the app executable (Tauri Resources)
        if let Ok(exe) = std::env::current_exe() {
            // Tauri bundles resources next to the exe on Windows
            #[cfg(windows)]
            {
                if let Some(dir) = exe.parent() {
                    let candidate = dir.join(bin_name);
                    if candidate.exists() {
                        return Ok(candidate.to_string_lossy().to_string());
                    }
                }
            }
            // macOS: Resources directory inside .app bundle
            #[cfg(target_os = "macos")]
            {
                if let Some(res) = exe.parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.join("Resources").join(bin_name))
                {
                    if res.exists() {
                        return Ok(res.to_string_lossy().to_string());
                    }
                }
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
}

impl Drop for SingboxManager {
    fn drop(&mut self) {
        self.stop().ok();
    }
}
