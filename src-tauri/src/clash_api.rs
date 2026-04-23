use serde_json::Value;

const CLASH_API_BASE: &str = "http://127.0.0.1:9090";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("Failed to build HTTP client")
}

pub async fn get_proxies() -> Result<Value, Box<dyn std::error::Error>> {
    let resp = client()
        .get(format!("{}/proxies", CLASH_API_BASE))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(format!("Clash API /proxies returned {}", resp.status()).into());
    }

    Ok(resp.json::<Value>().await?)
}

pub async fn select_proxy(group: &str, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client()
        .put(format!("{}/proxies/{}", CLASH_API_BASE, group))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(format!("Clash API select proxy returned {}", resp.status()).into());
    }

    Ok(())
}

/// GET /traffic — streaming endpoint. Read first chunk, parse first JSON line.
pub async fn get_traffic() -> Result<Value, Box<dyn std::error::Error>> {
    let mut resp = client()
        .get(format!("{}/traffic", CLASH_API_BASE))
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await?;

    // Read first chunk only (streaming endpoint)
    let chunk = resp.chunk().await?;
    if let Some(data) = chunk {
        let text = String::from_utf8_lossy(&data);
        if let Some(line) = text.lines().find(|l| l.starts_with('{')) {
            return Ok(serde_json::from_str(line)?);
        }
    }

    Ok(serde_json::json!({"up": 0, "down": 0}))
}

/// Hiddify-style latency test — TCP connect time only (no TLS, no HTTP).
/// Reads VPN server endpoint from cached config, does TcpStream::connect_timeout, returns ms.
///
/// Why not Clash API delay: that does full HTTPS GET (TCP+TLS+Reality+HTTP) which adds
/// ~150-200ms of handshake overhead per measurement. Hiddify shows TCP-only RTT for parity.
pub async fn test_delay(name: &str) -> Result<u32, Box<dyn std::error::Error>> {
    // Find server:port for this outbound tag
    let endpoint = find_outbound_endpoint(name)?;
    tcp_ping(&endpoint).await
}

/// Read both TUN and proxy configs to find outbound endpoint by tag.
fn find_outbound_endpoint(tag: &str) -> Result<String, Box<dyn std::error::Error>> {
    let candidates = [
        super::config::config_file_path(),
        super::config::tun_config_file_path(),
    ];
    for path in &candidates {
        if !path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(path)?;
        let cfg: Value = serde_json::from_str(&body)?;
        if let Some(arr) = cfg.get("outbounds").and_then(|o| o.as_array()) {
            for o in arr {
                if o.get("tag").and_then(|t| t.as_str()) == Some(tag) {
                    let server = o
                        .get("server")
                        .and_then(|s| s.as_str())
                        .ok_or("outbound has no server field (group?)")?;
                    let port = o
                        .get("server_port")
                        .and_then(|p| p.as_u64())
                        .ok_or("outbound has no server_port")?;
                    return Ok(format!("{}:{}", server, port));
                }
            }
        }
    }
    Err(format!("Outbound '{}' not found in config", tag).into())
}

/// TCP connect timing — pure RTT to server, like Hiddify.
/// On macOS, binds socket to physical interface (en0) to bypass TUN —
/// otherwise sing-box would intercept the test packet and report a fake low value.
async fn tcp_ping(addr: &str) -> Result<u32, Box<dyn std::error::Error>> {
    use std::net::ToSocketAddrs;
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(5);
    let socket_addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or("Cannot resolve address")?;
    let addr_str = addr.to_string();

    let start = Instant::now();
    let result =
        tokio::task::spawn_blocking(move || connect_bypass_tun(socket_addr, timeout)).await?;

    match result {
        Ok(_stream) => Ok(start.elapsed().as_millis() as u32),
        Err(e) => Err(format!("TCP connect to {} failed: {}", addr_str, e).into()),
    }
}

/// macOS: bind socket to en0/en1/etc to bypass TUN, then TCP connect.
/// Other platforms: plain connect.
#[cfg(target_os = "macos")]
fn connect_bypass_tun(
    addr: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    use std::os::unix::io::AsRawFd;

    // Find first existing physical interface (en0, en1, ...). 0 = none.
    let if_idx = find_physical_interface_index();

    // Create raw socket
    let domain = match addr {
        std::net::SocketAddr::V4(_) => libc::AF_INET,
        std::net::SocketAddr::V6(_) => libc::AF_INET6,
    };
    let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Bind to physical interface via IP_BOUND_IF (macOS-specific, value 25)
    if if_idx > 0 {
        const IP_BOUND_IF: libc::c_int = 25;
        let if_idx_u32: u32 = if_idx;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                IP_BOUND_IF,
                &if_idx_u32 as *const u32 as *const libc::c_void,
                std::mem::size_of::<u32>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error());
        }
    }

    // Wrap fd in std TcpStream and use its connect_timeout via socket2-like trick.
    // Easier: convert fd to TcpStream after manual connect with timeout.
    // We'll use blocking connect with socket-level timeout via SO_SNDTIMEO + SO_RCVTIMEO.
    set_socket_timeout(fd, timeout)?;

    let (sockaddr, sockaddr_len) = sockaddr_from(addr);
    let connect_result =
        unsafe { libc::connect(fd, sockaddr.as_ptr() as *const libc::sockaddr, sockaddr_len) };
    if connect_result < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // Wrap fd back into std TcpStream
    use std::os::unix::io::FromRawFd;
    let stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    Ok(stream)
}

#[cfg(not(target_os = "macos"))]
fn connect_bypass_tun(
    addr: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    std::net::TcpStream::connect_timeout(&addr, timeout)
}

#[cfg(target_os = "macos")]
fn find_physical_interface_index() -> u32 {
    // Try common physical interface names; first one that exists is our default.
    for name in &["en0", "en1", "en2", "en3"] {
        let cstr = std::ffi::CString::new(*name).unwrap();
        let idx = unsafe { libc::if_nametoindex(cstr.as_ptr()) };
        if idx != 0 {
            return idx;
        }
    }
    0
}

#[cfg(target_os = "macos")]
fn set_socket_timeout(fd: libc::c_int, timeout: std::time::Duration) -> std::io::Result<()> {
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: 0,
    };
    let tv_ptr = &tv as *const libc::timeval as *const libc::c_void;
    let tv_len = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
    let r1 = unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO, tv_ptr, tv_len) };
    let r2 = unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, tv_ptr, tv_len) };
    if r1 < 0 || r2 < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn sockaddr_from(addr: std::net::SocketAddr) -> (Vec<u8>, libc::socklen_t) {
    match addr {
        std::net::SocketAddr::V4(v4) => {
            let raw = libc::sockaddr_in {
                sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as u8,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &raw as *const _ as *const u8,
                    std::mem::size_of::<libc::sockaddr_in>(),
                )
            }
            .to_vec();
            (
                bytes,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        std::net::SocketAddr::V6(v6) => {
            let raw = libc::sockaddr_in6 {
                sin6_len: std::mem::size_of::<libc::sockaddr_in6>() as u8,
                sin6_family: libc::AF_INET6 as u8,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &raw as *const _ as *const u8,
                    std::mem::size_of::<libc::sockaddr_in6>(),
                )
            }
            .to_vec();
            (
                bytes,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}
