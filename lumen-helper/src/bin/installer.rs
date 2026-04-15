/// Lumen helper installer — runs as ROOT (via osascript admin privileges from main app).
///
/// Usage:
///   lumen-installer install <source_helper_path>
///   lumen-installer uninstall
///
/// Install steps:
///   1. Copy <source_helper_path> -> /Library/PrivilegedHelperTools/io.getlumen.helper
///   2. chown root:wheel + chmod 544
///   3. Write /Library/LaunchDaemons/io.getlumen.helper.plist (root:wheel 644)
///   4. launchctl bootout system <plist>  (ignore errors — handles re-install)
///   5. launchctl bootstrap system <plist>
///   6. launchctl enable system/io.getlumen.helper
///   7. launchctl kickstart -k system/io.getlumen.helper
///
/// Uninstall: reverse order.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const HELPER_LABEL: &str = "io.getlumen.helper";
const HELPER_INSTALL_PATH: &str = "/Library/PrivilegedHelperTools/io.getlumen.helper";
const PLIST_PATH: &str = "/Library/LaunchDaemons/io.getlumen.helper.plist";
const LOG_PATH: &str = "/var/log/io.getlumen.helper.log";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <install|uninstall> [source_helper_path]", args[0]);
        std::process::exit(1);
    }

    let result = match args[1].as_str() {
        "install" => {
            if args.len() < 3 {
                eprintln!("install requires <source_helper_path>");
                std::process::exit(1);
            }
            install(&args[2])
        }
        "uninstall" => uninstall(),
        cmd => {
            eprintln!("Unknown command: {}", cmd);
            std::process::exit(1);
        }
    };

    match result {
        Ok(msg) => {
            println!("OK: {}", msg);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("ERROR: {}", e);
            std::process::exit(1);
        }
    }
}

fn install(source_helper: &str) -> Result<String, String> {
    let src = Path::new(source_helper);
    if !src.exists() {
        return Err(format!("source helper not found: {}", source_helper));
    }

    // 1. Ensure /Library/PrivilegedHelperTools exists
    std::fs::create_dir_all("/Library/PrivilegedHelperTools")
        .map_err(|e| format!("mkdir PrivilegedHelperTools: {}", e))?;

    // 2. Copy helper binary
    std::fs::copy(src, HELPER_INSTALL_PATH)
        .map_err(|e| format!("copy helper: {}", e))?;

    // 3. chown root:wheel + chmod 544 (read+exec for owner+group, read for others)
    set_owner_root_wheel(HELPER_INSTALL_PATH)?;
    std::fs::set_permissions(HELPER_INSTALL_PATH, std::fs::Permissions::from_mode(0o544))
        .map_err(|e| format!("chmod helper: {}", e))?;

    // 4. Write LaunchDaemon plist
    let plist = build_plist();
    std::fs::write(PLIST_PATH, plist).map_err(|e| format!("write plist: {}", e))?;
    set_owner_root_wheel(PLIST_PATH)?;
    std::fs::set_permissions(PLIST_PATH, std::fs::Permissions::from_mode(0o644))
        .map_err(|e| format!("chmod plist: {}", e))?;

    // 5. Bootout existing (idempotent re-install)
    let _ = run_launchctl(&["bootout", "system", PLIST_PATH]);

    // 6. Bootstrap
    run_launchctl(&["bootstrap", "system", PLIST_PATH])
        .map_err(|e| format!("launchctl bootstrap: {}", e))?;

    // 7. Enable
    let label_target = format!("system/{}", HELPER_LABEL);
    run_launchctl(&["enable", &label_target])
        .map_err(|e| format!("launchctl enable: {}", e))?;

    // 8. Kickstart (start now)
    run_launchctl(&["kickstart", "-k", &label_target])
        .map_err(|e| format!("launchctl kickstart: {}", e))?;

    Ok(format!("helper installed at {}", HELPER_INSTALL_PATH))
}

fn uninstall() -> Result<String, String> {
    // 1. Bootout (stop and remove)
    let _ = run_launchctl(&["bootout", "system", PLIST_PATH]);

    // 2. Remove plist
    if Path::new(PLIST_PATH).exists() {
        std::fs::remove_file(PLIST_PATH).map_err(|e| format!("remove plist: {}", e))?;
    }

    // 3. Remove helper binary
    if Path::new(HELPER_INSTALL_PATH).exists() {
        std::fs::remove_file(HELPER_INSTALL_PATH).map_err(|e| format!("remove helper: {}", e))?;
    }

    // 4. Remove socket if exists
    let _ = std::fs::remove_file("/var/run/io.getlumen.helper.sock");

    Ok("helper uninstalled".to_string())
}

fn build_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>Program</key>
    <string>{program}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#,
        label = HELPER_LABEL,
        program = HELPER_INSTALL_PATH,
        log = LOG_PATH
    )
}

fn set_owner_root_wheel(path: &str) -> Result<(), String> {
    let status = Command::new("/usr/sbin/chown")
        .args(["root:wheel", path])
        .status()
        .map_err(|e| format!("chown spawn: {}", e))?;
    if !status.success() {
        return Err(format!("chown failed for {}", path));
    }
    Ok(())
}

fn run_launchctl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl spawn: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("launchctl {:?} failed: {}", args, stderr));
    }
    Ok(())
}
