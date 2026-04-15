use std::process::Command;

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn active_network_service() -> Result<String, Box<dyn std::error::Error>> {
    let candidates = ["Wi-Fi", "Ethernet", "USB 10/100/1000 LAN"];

    for name in &candidates {
        let output = Command::new("networksetup")
            .args(["-getinfo", name])
            .output();

        if let Ok(out) = output {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("IP address") && !text.contains("none") {
                return Ok(name.to_string());
            }
        }
    }

    let output = Command::new("networksetup")
        .args(["-listallnetworkservices"])
        .output()?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let service = line.trim().trim_start_matches('*');
        if !service.is_empty() && !service.contains("Bluetooth") && !service.contains("Thunderbolt") {
            return Ok(service.to_string());
        }
    }

    Err("No active network service found".into())
}

#[cfg(target_os = "macos")]
pub fn enable_system_proxy(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let service = active_network_service()?;
    let port_str = port.to_string();

    log::info!("Setting system proxy on '{}' -> 127.0.0.1:{}", service, port);

    Command::new("networksetup").args(["-setwebproxy", &service, "127.0.0.1", &port_str]).output()?;
    Command::new("networksetup").args(["-setwebproxystate", &service, "on"]).output()?;
    Command::new("networksetup").args(["-setsecurewebproxy", &service, "127.0.0.1", &port_str]).output()?;
    Command::new("networksetup").args(["-setsecurewebproxystate", &service, "on"]).output()?;
    Command::new("networksetup").args(["-setsocksfirewallproxy", &service, "127.0.0.1", &port_str]).output()?;
    Command::new("networksetup").args(["-setsocksfirewallproxystate", &service, "on"]).output()?;

    log::info!("System proxy enabled: {}:{}", service, port);
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn disable_system_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let service = active_network_service()?;

    log::info!("Disabling system proxy on '{}'", service);

    Command::new("networksetup").args(["-setwebproxystate", &service, "off"]).output()?;
    Command::new("networksetup").args(["-setsecurewebproxystate", &service, "off"]).output()?;
    Command::new("networksetup").args(["-setsocksfirewallproxystate", &service, "off"]).output()?;

    log::info!("System proxy disabled on '{}'", service);
    Ok(())
}

// ── Windows ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn enable_system_proxy(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")?;

    let proxy_server = format!("127.0.0.1:{}", port);
    key.set_value("ProxyEnable", &1u32)?;
    key.set_value("ProxyServer", &proxy_server)?;

    log::info!("Windows proxy registry set to {}", proxy_server);

    // Notify system of proxy change via InternetSetOption
    notify_proxy_change();

    log::info!("System proxy enabled: {}", proxy_server);
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn disable_system_proxy() -> Result<(), Box<dyn std::error::Error>> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")?;

    key.set_value("ProxyEnable", &0u32)?;

    log::info!("Disabling Windows system proxy");

    notify_proxy_change();

    log::info!("System proxy disabled");
    Ok(())
}

#[cfg(target_os = "windows")]
fn notify_proxy_change() {
    use std::ptr;
    #[link(name = "wininet")]
    extern "system" {
        fn InternetSetOptionW(h: *mut std::ffi::c_void, opt: u32, buf: *mut std::ffi::c_void, len: u32) -> i32;
    }
    unsafe {
        // INTERNET_OPTION_SETTINGS_CHANGED = 39
        InternetSetOptionW(ptr::null_mut(), 39, ptr::null_mut(), 0);
        // INTERNET_OPTION_REFRESH = 37
        InternetSetOptionW(ptr::null_mut(), 37, ptr::null_mut(), 0);
    }
}
