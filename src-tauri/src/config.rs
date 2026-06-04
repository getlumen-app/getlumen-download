use std::path::PathBuf;

pub fn config_file_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn data_dir() -> PathBuf {
    let dir = platform_data_dir().join("io.getlumen.app");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn platform_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join("Library").join("Caches")
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("C:\\Temp"))
                .join("AppData")
                .join("Local")
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

/// Config endpoint URL — injected at build time, fallback to Lumen config gateway.
pub fn config_base_url() -> String {
    option_env!("LUMEN_CONFIG_URL")
        .unwrap_or("https://config.getlumen.download")
        .to_string()
}

#[derive(Clone, Copy, Debug)]
pub enum InboundMode {
    /// HTTP/SOCKS proxy on 127.0.0.1:10808 — runs as user, no root needed
    Mixed,
    /// TUN interface (utun) — needs root via privileged helper, low latency
    Tun,
}

pub fn tun_config_file_path() -> PathBuf {
    data_dir().join("config-tun.json")
}

/// Immutable last-good full TUN config. Written ONLY on a successful server
/// fetch, so it survives a single pasted `vless://` overwriting config-tun.json.
pub fn tun_config_lastgood_path() -> PathBuf {
    data_dir().join("config-tun-lastgood.json")
}

/// Load the last-good cached TUN config for offline / censored-bootstrap
/// fallback (e.g. when the config endpoint is SNI-blocked on a hostile network).
/// Prefers the immutable last-good copy, then the active config. Validates that
/// outbounds exist so sing-box never starts on garbage. No secrets: this only
/// ever returns the user's own previously-fetched config.
pub fn load_cached_tun_config() -> Result<String, Box<dyn std::error::Error>> {
    let lastgood = tun_config_lastgood_path();
    if lastgood.exists() {
        if let Ok(s) = load_cached_tun_config_from(&lastgood) {
            return Ok(s);
        }
    }
    load_cached_tun_config_from(&tun_config_file_path())
}

fn load_cached_tun_config_from(
    path: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let has_outbounds = v
        .get("outbounds")
        .and_then(|o| o.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_outbounds {
        return Err("cached TUN config has no outbounds".into());
    }
    Ok(body)
}

pub fn wbstream_manifest_file_path() -> PathBuf {
    data_dir().join("wbstream-manifest.json")
}

pub const WBSTREAM_LOCAL_SOCKS_PORT: u16 = 11080;
pub const WBSTREAM_LOCAL_BALANCER_PORT: u16 = 11079;
pub const WBSTREAM_LOCAL_MULTIPATH_PORT: u16 = 11078;
pub const WBSTREAM_REMOTE_MULTIPATH_PORT: u16 = 19095;
pub const WBSTREAM_MAX_ROOMS: usize = 3;

/// Fetch config from server, generate working config, cache to disk.
/// Server returns outbounds (proxies). Client wraps them with DNS, routing, inbounds.
pub async fn fetch_and_cache(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    fetch_and_cache_with_mode(url, InboundMode::Mixed).await
}

pub async fn fetch_and_cache_with_mode(
    url: &str,
    mode: InboundMode,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        // User-Agent reflects the installed binary's version automatically at
        // compile time. Must never be hard-coded — it drifts and logs become
        // useless. `env!("CARGO_PKG_VERSION")` pulls from Cargo.toml.
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION"), " sing-box"))
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()?;

    log::info!("Fetching config from: {}", url);
    let mut resp = client.get(url).send().await?;
    log::info!("Config response: {} (url={})", resp.status(), url);
    // Retry up to 2 times on 403 (Cloudflare KV edge propagation delay) or 5xx.
    let mut retries = 0;
    while (resp.status() == reqwest::StatusCode::FORBIDDEN || resp.status().is_server_error())
        && retries < 2
    {
        retries += 1;
        log::warn!(
            "Config fetch got {} (attempt {}), retrying in 2s…",
            resp.status(),
            retries
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        resp = client.get(url).send().await?;
        log::info!("Retry {} response: {}", retries, resp.status());
    }
    if !resp.status().is_success() {
        return Err(format!("Config server returned {}", resp.status()).into());
    }

    let body = resp.text().await?;
    let mut server_config: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid config JSON: {}", e))?;
    cache_wbstream_manifest_from_server(&server_config);
    let _ = prefetch_wbstream_manifest_sidecar().await;

    // If the server returns a full sing-box config (has dns + inbounds + route),
    // use it directly — no need to rebuild. This allows the Proteus config server
    // to ship complete configs (with routing rules, Samokat bypass, etc.).
    // Otherwise, treat the response as an outbounds-only payload and wrap it.
    let is_full_config = server_config.get("dns").is_some()
        && server_config.get("inbounds").is_some()
        && server_config.get("route").is_some();

    let config = if is_full_config {
        log::info!("Server returned full sing-box config — using as-is (mode override ignored)");
        strip_client_side_metadata(&mut server_config);
        server_config
    } else {
        build_config_from_server(&server_config, mode)?
    };

    let final_json = serde_json::to_string_pretty(&config)?;
    let path = match mode {
        InboundMode::Mixed => config_file_path(),
        InboundMode::Tun => tun_config_file_path(),
    };
    std::fs::write(&path, &final_json)?;
    log::info!(
        "Config ({:?}) saved to {} ({} bytes)",
        mode,
        path.display(),
        final_json.len()
    );

    Ok(final_json)
}

fn cache_wbstream_manifest_from_server(server_config: &serde_json::Value) {
    let Some(manifest) = server_config.get("wbstream_manifest") else {
        return;
    };
    if !is_usable_wbstream_manifest(manifest) {
        log::warn!("Ignoring unusable WB Stream manifest metadata from config server");
        return;
    }
    let path = wbstream_manifest_file_path();
    match serde_json::to_string_pretty(manifest)
        .map_err(|e| e.to_string())
        .and_then(|body| std::fs::write(&path, body).map_err(|e| e.to_string()))
    {
        Ok(()) => log::info!("WB Stream manifest cached to {}", path.display()),
        Err(e) => log::warn!("Could not cache WB Stream manifest: {}", e),
    }
}

pub async fn ensure_wbstream_manifest_cached() -> Result<(), String> {
    if load_cached_wbstream_manifest().is_ok() {
        return Ok(());
    }
    prefetch_wbstream_manifest_sidecar().await
}

pub async fn prefetch_wbstream_manifest_sidecar() -> Result<(), String> {
    let client = match reqwest::Client::builder()
        .user_agent(concat!(
            "Lumen/",
            env!("CARGO_PKG_VERSION"),
            " wbstream-prefetch"
        ))
        .timeout(std::time::Duration::from_secs(4))
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            let msg = format!("Could not build WB Stream prefetch client: {}", e);
            log::warn!("{}", msg);
            return Err(msg);
        }
    };
    let mut last_error = None;
    for url in WBSTREAM_MANIFEST_PREFETCH_URLS {
        let result = async {
            let resp = client.get(*url).send().await?;
            if !resp.status().is_success() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("status {}", resp.status()),
                )
                .into());
            }
            let manifest: serde_json::Value = resp.json().await?;
            if !is_usable_wbstream_manifest(&manifest) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unusable manifest shape",
                )
                .into());
            }
            let path = wbstream_manifest_file_path();
            let body = serde_json::to_string_pretty(&manifest)?;
            std::fs::write(&path, body)?;
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;

        match result {
            Ok(()) => {
                log::info!("WB Stream manifest prefetched from {}", url);
                return Ok(());
            }
            Err(e) => {
                let msg = format!("WB Stream manifest prefetch failed from {}: {}", url, e);
                log::warn!("{}", msg);
                last_error = Some(msg);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "WB Stream manifest prefetch failed".to_string()))
}

fn is_usable_wbstream_manifest(manifest: &serde_json::Value) -> bool {
    if manifest.get("signature_alg").and_then(|v| v.as_str()) != Some("RS256") {
        return false;
    }
    if manifest
        .get("payload_b64")
        .and_then(|v| v.as_str())
        .is_none()
        || manifest.get("signature").and_then(|v| v.as_str()).is_none()
    {
        return false;
    }
    manifest
        .get("payload")
        .and_then(|payload| payload.get("rooms"))
        .and_then(|rooms| rooms.as_array())
        .map(|rooms| {
            rooms.iter().any(|room| {
                room.get("url")
                    .and_then(|url| url.as_str())
                    .map(|url| url.starts_with("wbstream://"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn strip_client_side_metadata(config: &mut serde_json::Value) {
    if let Some(obj) = config.as_object_mut() {
        obj.remove("wbstream_manifest");
    }
}

pub fn load_cached_wbstream_manifest() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let path = wbstream_manifest_file_path();
    let body = std::fs::read_to_string(&path)?;
    let manifest: serde_json::Value = serde_json::from_str(&body)?;
    if !is_usable_wbstream_manifest(&manifest) {
        return Err("cached WB Stream manifest is unusable".into());
    }
    Ok(manifest)
}

pub fn select_wbstream_room_url(manifest: &serde_json::Value) -> Option<String> {
    select_wbstream_room_urls(manifest, 1).into_iter().next()
}

pub fn select_wbstream_room_urls(manifest: &serde_json::Value, limit: usize) -> Vec<String> {
    let rooms = manifest
        .get("payload")
        .and_then(|payload| payload.get("rooms"))
        .and_then(|rooms| rooms.as_array());
    let Some(rooms) = rooms else {
        return Vec::new();
    };

    let mut candidates: Vec<(u64, String)> = rooms
        .iter()
        .filter_map(|room| {
            let url = room.get("url").and_then(|url| url.as_str())?;
            if !url.starts_with("wbstream://") {
                return None;
            }
            let priority = room
                .get("priority")
                .and_then(|priority| priority.as_u64())
                .unwrap_or(u64::MAX);
            Some((priority, url.to_string()))
        })
        .collect();
    candidates.sort_by_key(|(priority, _)| *priority);
    candidates
        .into_iter()
        .map(|(_, url)| url)
        .take(limit)
        .collect()
}

pub fn save_wbstream_fallback_config(
    mode: InboundMode,
    local_socks_port: u16,
) -> Result<String, Box<dyn std::error::Error>> {
    let cfg = build_wbstream_fallback_config(mode, local_socks_port);
    let final_json = serde_json::to_string_pretty(&cfg)?;
    let path = match mode {
        InboundMode::Mixed => config_file_path(),
        InboundMode::Tun => tun_config_file_path(),
    };
    std::fs::write(&path, &final_json)?;
    log::info!(
        "WB Stream fallback config ({:?}) saved to {}",
        mode,
        path.display()
    );
    Ok(final_json)
}

fn build_wbstream_fallback_config(mode: InboundMode, local_socks_port: u16) -> serde_json::Value {
    let cache_path = data_dir().join("cache-wbstream.db");
    let wb_domains = serde_json::json!([".wb.ru", ".wildberries.ru", ".wbbasket.ru"]);
    let wb_endpoint_cidrs = serde_json::json!([
        // Observed WB Stream API / LiveKit endpoints.
        "185.62.202.8/32",
        "194.1.214.97/32",
    ]);

    serde_json::json!({
        "log": {"level": "info", "timestamp": true},
        "dns": {
            "servers": [
                {
                    "tag": "dns-proxy",
                    "address": "https://1.1.1.1/dns-query",
                    "detour": "wbstream-local"
                },
                {
                    "tag": "dns-direct",
                    "address": "https://77.88.8.8/dns-query",
                    "detour": "direct"
                }
            ],
            "rules": [
                {"domain_suffix": wb_domains.clone(), "server": "dns-direct"}
            ],
            "final": "dns-proxy",
            "strategy": "ipv4_only"
        },
        "inbounds": match mode {
            InboundMode::Mixed => serde_json::json!([
                {
                    "type": "mixed",
                    "tag": "mixed-in",
                    "listen": "127.0.0.1",
                    "listen_port": 10808,
                    "sniff": true,
                    "sniff_override_destination": false
                }
            ]),
            InboundMode::Tun => serde_json::json!([
                {
                    "type": "tun",
                    "tag": "tun-in",
                    "interface_name": "utun777",
                    "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
                    "mtu": 9000,
                    "auto_route": true,
                    "strict_route": false,
                    "stack": "mixed",
                    "endpoint_independent_nat": true,
                    "sniff": true,
                    "sniff_override_destination": false
                }
            ]),
        },
        "outbounds": [
            {
                "type": "socks",
                "tag": "wbstream-local",
                "server": "127.0.0.1",
                "server_port": local_socks_port,
                "version": "5"
            },
            {"type": "direct", "tag": "direct"},
            {"type": "block", "tag": "block"}
        ],
        "route": {
            "rules": [
                {"domain_suffix": wb_domains.clone(), "outbound": "direct"},
                {"ip_cidr": wb_endpoint_cidrs.clone(), "outbound": "direct"}
            ],
            "final": "wbstream-local",
            "auto_detect_interface": true
        },
        "experimental": {
            "clash_api": {
                "external_controller": "127.0.0.1:9090",
                "default_mode": "rule"
            },
            "cache_file": {
                "enabled": true,
                "path": cache_path.to_string_lossy()
            }
        }
    })
}

const WBSTREAM_MANIFEST_PREFETCH_URLS: &[&str] = &[
    "https://config.getlumen.download/wbstream-manifest.json",
];

/// Build a single-outbound sing-box config from a parsed VLESS link.
/// Used when user supplies a raw vless:// URI instead of a Proteus subscription.
///
/// IMPORTANT: the outbound tag must NOT collide with reserved tags used by the
/// wrapping config — specifically "proxy" (the urltest group name), "direct",
/// "block", or route targets — otherwise sing-box rejects the config with a
/// duplicate-tag error and the Clash API returns empty, which surfaces in the UI
/// as an empty Proxies list.
pub fn build_config_from_vless(
    vless: &crate::vless::VlessConfig,
    mode: InboundMode,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let tag = vless_outbound_tag(&vless.name);
    let outbound = crate::vless::to_singbox_outbound(vless, &tag);
    // Wrap as a "server response" with single outbound and reuse the existing builder
    let pseudo_server = serde_json::json!({ "outbounds": [outbound] });
    build_config_from_server(&pseudo_server, mode)
}

/// Produce a sing-box outbound tag from a user-supplied VLESS fragment/name.
///
/// Rules:
/// - ASCII alphanumerics, dash, underscore kept; everything else collapsed to `-`
/// - Leading/trailing `-` trimmed; multiple `-` collapsed to one
/// - Empty / reserved names fall back to `vless-out`
/// - Result is lowercased to keep Clash API names consistent
fn vless_outbound_tag(raw_name: &str) -> String {
    const RESERVED: &[&str] = &[
        "proxy", "proxy-tg", "proxy-yt", "direct", "block", "dns-out", "dns-in", "tun-in",
        "mixed-in",
    ];
    let sanitized: String = raw_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of '-' and trim
    let mut out = String::with_capacity(sanitized.len());
    let mut prev_dash = false;
    for ch in sanitized.chars() {
        if ch == '-' {
            if !prev_dash && !out.is_empty() {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() || RESERVED.contains(&out.as_str()) {
        return "vless-out".to_string();
    }
    out
}

/// Save VLESS-derived config to disk (same path as Proteus configs).
pub async fn save_vless_config(
    vless: &crate::vless::VlessConfig,
    mode: InboundMode,
) -> Result<String, Box<dyn std::error::Error>> {
    let cfg = build_config_from_vless(vless, mode)?;
    let final_json = serde_json::to_string_pretty(&cfg)?;
    let path = match mode {
        InboundMode::Mixed => config_file_path(),
        InboundMode::Tun => tun_config_file_path(),
    };
    std::fs::write(&path, &final_json)?;
    log::info!("VLESS config ({:?}) saved to {}", mode, path.display());
    Ok(final_json)
}

/// Build sing-box config from server-provided outbounds.
/// Server is responsible for all proxy outbounds (IPs, keys, transport).
/// Client only adds: DNS, inbounds, route rules, urltest group, direct/block.
fn build_config_from_server(
    server: &serde_json::Value,
    mode: InboundMode,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let outbounds = server
        .get("outbounds")
        .and_then(|o| o.as_array())
        .ok_or("No outbounds in server config")?;

    // Collect proxy outbound names for urltest group
    let proxy_names: Vec<String> = outbounds
        .iter()
        .filter_map(|o| {
            let tag = o.get("tag").and_then(|t| t.as_str()).unwrap_or("");
            let otype = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
            // Skip non-proxy types
            if tag == "direct" || tag == "block" || otype == "direct" || otype == "block" {
                return None;
            }
            if tag.is_empty() {
                return None;
            }
            Some(tag.to_string())
        })
        .collect();

    if proxy_names.is_empty() {
        return Err("No proxy outbounds in server config".into());
    }

    // Service-specific URLTest groups (proxy-tg, proxy-yt) must exclude
    // TCP Reality exits (flow: xtls-rprx-vision). TSPU blocks sustained
    // traffic on TCP Reality after 15-20KB while tiny URLTest probes pass,
    // causing the group to select a broken exit. Verified 2026-04-16 on
    // a Moscow test network: proxy-tg picked netcup-tcp-reality (216ms probe)
    // but Telegram MTProto was frozen. General proxy group keeps all exits
    // for non-RF users where TCP Reality works fine.
    let service_proxy_names: Vec<String> = outbounds
        .iter()
        .filter_map(|o| {
            let tag = o.get("tag").and_then(|t| t.as_str()).unwrap_or("");
            let otype = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let flow = o.get("flow").and_then(|f| f.as_str()).unwrap_or("");
            if tag == "direct" || tag == "block" || otype == "direct" || otype == "block" {
                return None;
            }
            if tag.is_empty() {
                return None;
            }
            // Exclude TCP Reality (TSPU-blocked for sustained traffic in RF)
            if flow.contains("xtls-rprx-vision") {
                return None;
            }
            Some(tag.to_string())
        })
        .collect();
    // Fallback: if ALL exits are TCP Reality, use them anyway (better than empty group)
    let service_names = if service_proxy_names.is_empty() {
        &proxy_names
    } else {
        &service_proxy_names
    };

    let cache_path = data_dir().join("cache.db");

    // Extract unique server IPs from outbounds → direct route prevents circular
    // routing and keeps SSH remote-support tunnels alive when TUN is active.
    let server_ips: std::collections::HashSet<String> = outbounds
        .iter()
        .filter_map(|o| {
            o.get("server")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .filter(|ip| {
            // Only IPs, not hostnames (hostnames handled by DNS rules)
            ip.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .collect();
    let server_ip_cidrs: Vec<String> = server_ips.iter().map(|ip| format!("{}/32", ip)).collect();
    let server_ip_cidrs = serde_json::json!(server_ip_cidrs);

    // Domains the user's real ISP must resolve & reach directly (RKN-compliant
    // resolvers, Russian banks, Russian-only services). Keep DNS and routing
    // rules mirrored — a domain that resolves via direct DNS must also route
    // via direct, and vice-versa.
    let russia_direct_domains = serde_json::json!([
        ".ru",
        ".su",
        ".xn--p1ai",
        ".yandex.net",
        ".yandex.ru",
        ".yandex.com",
        ".yastatic.net",
        ".yastat.net",
        ".ya.ru",
        ".dzen.ru",
        ".vk.com",
        ".vk.me",
        ".mail.ru",
        ".ok.ru",
        ".userapi.com",
        ".vkuservideo.net",
        ".sberbank.ru",
        ".tinkoff.ru",
        ".tbank.ru",
        ".vtb.ru",
        ".alfabank.ru",
        ".gosuslugi.ru"
    ]);

    // Domains that get their own health-probed proxy group. sing-box runs
    // a real HTTP probe against the target, not a generic /generate_204 —
    // an exit that cannot actually reach YouTube is excluded from proxy-yt,
    // even if it passes a generic latency check.
    let telegram_domains = serde_json::json!([
        ".telegram.org",
        ".t.me",
        ".telegram.me",
        ".telesco.pe",
        ".telegram-cdn.org",
        ".tdesktop.com"
    ]);
    // Known Telegram IP ranges — matched when SNI sniffing fails (e.g. QUIC).
    let telegram_ip_cidr = serde_json::json!([
        "91.108.4.0/22",
        "91.108.8.0/21",
        "91.108.12.0/22",
        "91.108.16.0/21",
        "91.108.56.0/22",
        "109.239.140.0/24",
        "149.154.160.0/20",
        "95.161.64.0/20",
        "2001:b28:f23d::/48",
        "2001:b28:f23f::/48",
        "2001:67c:4e8::/48"
    ]);
    let youtube_domains = serde_json::json!([
        ".youtube.com",
        ".youtu.be",
        ".googlevideo.com",
        ".ytimg.com",
        ".youtube-nocookie.com"
    ]);
    // Google ASN15169 IP-CIDR fallback → proxy-yt when SNI sniff misses the
    // domain (QUIC/alt-svc case: Chrome caches `h3=:443` for googlevideo.com
    // after first HTTP/2 response, opens UDP 443 to Google CDN IPs without
    // sniffable SNI; rule by domain_suffix does not fire → connection falls
    // through to route.final. With IP-CIDR fallback, UDP 443 to Google hits
    // proxy-yt regardless of sniff result. Mirrors telegram_ip_cidr above.
    // @voksep 2026-04-22 repro: first YT video buffers, second hangs, page
    // reload fixes. Server-side mirror landed in template v81 (same CIDRs).
    let youtube_ip_cidr = serde_json::json!([
        "142.250.0.0/15",
        "172.217.0.0/16",
        "173.194.0.0/16",
        "216.58.192.0/19",
        "209.85.128.0/17",
        "74.125.0.0/16",
        "64.233.160.0/19",
        "35.190.0.0/16",
        "2607:f8b0::/32"
    ]);

    let mut config = serde_json::json!({
        "log": {"level": "info", "timestamp": true},
        "dns": {
            "servers": [
                // Default resolver for everything proxied: DoH to Cloudflare,
                // tunnelled through the VPN. Resistant to local DNS poisoning.
                {
                    "tag": "dns-proxy",
                    "address": "https://1.1.1.1/dns-query",
                    "detour": "proxy"
                },
                // Resolver for Russia-direct domains: DoH to Yandex over the
                // real ISP. DoH (not plain :53) because DPI tampers with
                // cleartext DNS even for allowed destinations in some RU networks.
                {
                    "tag": "dns-direct",
                    "address": "https://77.88.8.8/dns-query",
                    "detour": "direct"
                }
            ],
            "rules": [
                // Russia-direct domains resolve via the local resolver — they
                // must, since we'll route their traffic via the direct outbound.
                {"domain_suffix": russia_direct_domains.clone(), "server": "dns-direct"}
            ],
            "final": "dns-proxy",
            "strategy": "ipv4_only"
        },
        "inbounds": match mode {
            InboundMode::Mixed => serde_json::json!([
                {
                    "type": "mixed",
                    "tag": "mixed-in",
                    "listen": "127.0.0.1",
                    "listen_port": 10808,
                    "sniff": true,
                    "sniff_override_destination": false
                }
            ]),
            InboundMode::Tun => serde_json::json!([
                {
                    "type": "tun",
                    "tag": "tun-in",
                    "interface_name": "utun777",
                    "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
                    "mtu": 9000,
                    "auto_route": true,
                    "strict_route": false,
                    "stack": "mixed",
                    "endpoint_independent_nat": true,
                    "sniff": true,
                    // Do NOT override destination with sniffed domain.
                    // DNS resolution already goes through dns-proxy (Cloudflare
                    // DoH tunnelled via VPN), so resolved IPs are correct.
                    // override=true breaks Telegram's anti-censorship proxies:
                    // TG connects to DC IPs with fake SNI (e.g. "www.google.com")
                    // and override rewrites the destination to Google instead
                    // of the real DC. Verified 2026-04-16 on @STmarkml (Moscow).
                    "sniff_override_destination": false
                }
            ]),
        },
        "outbounds": [],
        "route": {
            "rules": [
                // Direct route for all proxy server IPs extracted from
                // outbounds. Prevents: (1) circular routing — traffic to a
                // proxy server going through itself; (2) SSH remote-support
                // tunnels being intercepted by TUN. IPs are dynamic — derived
                // from what the config server returns, not hardcoded.
                {"ip_cidr": server_ip_cidrs.clone(), "outbound": "direct"},
                // Russia-direct traffic never touches the VPN.
                {"domain_suffix": russia_direct_domains.clone(), "outbound": "direct"},
                // Telegram: match by domain AND by IP range (for QUIC / UDP
                // cases where sniffing yields no domain).
                {"domain_suffix": telegram_domains.clone(), "outbound": "proxy-tg"},
                {"ip_cidr": telegram_ip_cidr.clone(), "outbound": "proxy-tg"},
                // YouTube and Google video — by domain (TCP/HTTP-2 with sniff).
                {"domain_suffix": youtube_domains.clone(), "outbound": "proxy-yt"},
                // …and by IP for QUIC/UDP cases where sniffing yields no domain.
                {"ip_cidr": youtube_ip_cidr.clone(), "outbound": "proxy-yt"}
            ],
            // Everything else (web, messengers, file downloads, ...) goes
            // through the general-purpose URLTest group.
            "final": "proxy",
            "auto_detect_interface": true
        },
        "experimental": {
            "clash_api": {
                "external_controller": "127.0.0.1:9090",
                "default_mode": "rule"
            },
            "cache_file": {
                "enabled": true,
                "path": cache_path.to_string_lossy()
            }
        }
    });

    if let Some(arr) = config.get_mut("outbounds").and_then(|o| o.as_array_mut()) {
        // Default URLTest group — probe a reliably-unrestricted endpoint.
        // "proxy" is the name the UI looks for (App.tsx :141). All user-facing
        // "Auto Select" logic binds here. Other groups below are routing-only.
        // tolerance=200: don't flap between exits for small latency differences
        // (183ms proxy-moscow vs 233ms netcup-grpc = 50ms diff, both fine).
        // Higher tolerance → more stable connection, less TSPU attention from
        // repeated TLS handshakes to different servers.
        //
        // v2.3.4 defense-in-depth: use service_names (excludes TCP Reality flow
        // xtls-rprx-vision) for the GENERAL proxy group too, not just proxy-tg
        // /proxy-yt. TSPU bulk-blocks sustained traffic on xtls-rprx-vision
        // after 15-20KB while tiny URLTest probes pass, causing urltest to
        // pick a broken exit for long streams (persistent WebSocket sessions,
        // voice/media apps, large downloads). Same bug pattern as field tests
        // on 2026-04-16 and 2026-04-22. service_names falls back to proxy_names if all
        // exits are Reality (non-RF users with a single TCP Reality exit still
        // work as before). Server-side mirror landed in template v81.
        //
        // interrupt_exist_connections: true ensures new requests don't inherit
        // a stale-selected exit for 30m after urltest re-probes.
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy",
            "outbounds": service_names.clone(),
            "url": "https://www.cloudflare.com/cdn-cgi/trace",
            "interval": "60s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": true
        }));
        // Destination-specific URLTest groups — the probe URL is the actual
        // service, so an exit that can't reach it is dropped from the group.
        // Uses service_names (excludes TCP Reality) instead of proxy_names.
        // interval=60s (not 30s) — less probe traffic, less TSPU exposure.
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-tg",
            "outbounds": service_names.clone(),
            "url": "https://web.telegram.org/",
            "interval": "60s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": true
        }));
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-yt",
            "outbounds": service_names.clone(),
            "url": "https://www.youtube.com/generate_204",
            "interval": "60s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": true
        }));

        // Server-provided proxy outbounds.
        for o in outbounds {
            arr.push(o.clone());
        }

        // Standard outbounds.
        arr.push(serde_json::json!({"type": "direct", "tag": "direct"}));
        arr.push(serde_json::json!({"type": "block", "tag": "block"}));
    }

    log::info!(
        "Built config: {} proxy outbounds",
        config
            .get("outbounds")
            .and_then(|o| o.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    );

    Ok(config)
}

pub fn _load_cached() -> Result<String, Box<dyn std::error::Error>> {
    let path = config_file_path();
    if !path.exists() {
        return Err("No cached config found".into());
    }
    Ok(std::fs::read_to_string(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn vless_tag_is_never_reserved() {
        for name in &["proxy", "direct", "block", "PROXY", "Direct", ""] {
            let t = vless_outbound_tag(name);
            assert!(!t.is_empty(), "empty tag for {:?}", name);
            assert_ne!(t, "proxy", "tag clashes with urltest group for {:?}", name);
            assert_ne!(t, "direct");
            assert_ne!(t, "block");
        }
    }

    #[test]
    fn vless_tag_sanitizes_common_fragments() {
        assert_eq!(vless_outbound_tag("user-1"), "user-1");
        assert_eq!(vless_outbound_tag("Server-Name-01"), "server-name-01");
        assert_eq!(vless_outbound_tag("Canada Toronto"), "canada-toronto");
        assert_eq!(vless_outbound_tag("🚀 fast"), "fast");
        assert_eq!(vless_outbound_tag("---"), "vless-out");
        assert_eq!(vless_outbound_tag(""), "vless-out");
    }

    #[test]
    fn vless_config_has_no_duplicate_tags() {
        // Regression: on :443 with fragment "user-1" the outbound tag used to
        // collide with the synthetic "proxy" urltest group. Synthetic fixture,
        // RFC 5737 doc-range IP, null UUID.
        let raw = "vless://00000000-0000-4000-8000-000000000002@192.0.2.20:443?type=tcp&security=reality&pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB&fp=chrome&sni=google.com&sid=cafebabe&spx=%2F&flow=xtls-rprx-vision#user-1";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");

        let outbounds = cfg
            .get("outbounds")
            .and_then(|o| o.as_array())
            .expect("outbounds array");
        let mut seen: HashSet<String> = HashSet::new();
        let mut tags: Vec<String> = Vec::new();
        for o in outbounds {
            let tag = o
                .get("tag")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            assert!(!tag.is_empty(), "outbound has empty tag");
            tags.push(tag.clone());
            assert!(
                seen.insert(tag.clone()),
                "DUPLICATE tag {:?} in outbounds: {:?}",
                tag,
                tags
            );
        }
        // Required tags present — three URLTest groups + direct/block + the VLESS outbound.
        for required in &["proxy", "proxy-tg", "proxy-yt", "direct", "block"] {
            assert!(
                tags.contains(&required.to_string()),
                "missing required tag {:?}, got={:?}",
                required,
                tags
            );
        }
        assert!(
            tags.iter().any(|t| t == "user-1"),
            "missing vless outbound 'user-1', tags={:?}",
            tags
        );

        // Each urltest group must reference the vless outbound, not itself.
        let urltest_tags: Vec<String> = outbounds
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
            .map(|o| {
                o.get("tag")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(
            urltest_tags,
            vec![
                "proxy".to_string(),
                "proxy-tg".to_string(),
                "proxy-yt".to_string()
            ],
            "expected three URLTest groups in order"
        );
        for ut in outbounds
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
        {
            let inner = ut
                .get("outbounds")
                .and_then(|o| o.as_array())
                .expect("urltest.outbounds is array");
            let inner_tags: Vec<&str> = inner.iter().filter_map(|v| v.as_str()).collect();
            assert_eq!(
                inner_tags,
                vec!["user-1"],
                "urltest {:?} must wrap vless tag, not any group name",
                ut.get("tag")
            );
        }
    }

    /// A 2.2.4 regression: `{"outbound": "any", "server": "dns-direct"}`
    /// routed every DNS query through the Russian resolver, which returned
    /// poisoned IPs for YouTube / Telegram. The fix is a domain-scoped rule
    /// plus a `final: dns-proxy` fallback. Guard it forever.
    #[test]
    fn dns_does_not_catch_all_to_direct() {
        let raw = "vless://00000000-0000-4000-8000-000000000003@192.0.2.30:443?type=tcp&security=reality&pbk=CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");

        let dns = cfg.get("dns").expect("dns section present");
        let rules = dns
            .get("rules")
            .and_then(|r| r.as_array())
            .expect("dns.rules array");
        for r in rules {
            if r.get("outbound").and_then(|v| v.as_str()) == Some("any") {
                panic!(
                    "dns.rules contains a catch-all `outbound: any` rule: {:?}",
                    r
                );
            }
        }
        assert_eq!(
            dns.get("final").and_then(|v| v.as_str()),
            Some("dns-proxy"),
            "dns.final must be dns-proxy so non-Russian domains resolve through the VPN"
        );
    }

    /// Russia-direct destinations must be mirrored in BOTH `dns.rules` and
    /// `route.rules`. A mismatch would cause DNS-over-proxy to leak through
    /// the VPN or vice-versa — either way breaks split-tunnelling.
    #[test]
    fn russia_direct_domains_mirror_dns_and_route() {
        let raw = "vless://00000000-0000-4000-8000-000000000004@192.0.2.40:443?type=tcp&security=reality&pbk=DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD&fp=chrome&sni=google.com&sid=cafebabe&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");

        fn direct_domains(
            section: &serde_json::Value,
            outbound_field: &str,
            target: &str,
        ) -> Vec<String> {
            section
                .get("rules")
                .and_then(|r| r.as_array())
                .unwrap()
                .iter()
                .filter(|r| r.get(outbound_field).and_then(|v| v.as_str()) == Some(target))
                .filter_map(|r| r.get("domain_suffix").and_then(|v| v.as_array()))
                .flatten()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        }
        let dns_direct: HashSet<String> =
            direct_domains(cfg.get("dns").unwrap(), "server", "dns-direct")
                .into_iter()
                .collect();
        let route_direct: HashSet<String> =
            direct_domains(cfg.get("route").unwrap(), "outbound", "direct")
                .into_iter()
                .collect();

        assert_eq!(
            dns_direct, route_direct,
            "dns `server: dns-direct` and route `outbound: direct` domain lists must match exactly"
        );
        // Essential Russia-direct anchors.
        for domain in &[".ru", ".yandex.ru", ".sberbank.ru"] {
            assert!(
                dns_direct.contains(*domain),
                "Russia-direct set missing {:?}",
                domain
            );
        }
    }

    /// Smart-routing contract: Telegram and YouTube destinations must be
    /// routed to their dedicated health-probed groups, not the default.
    #[test]
    fn route_rules_steer_tg_and_yt_to_dedicated_groups() {
        let raw = "vless://00000000-0000-4000-8000-000000000005@192.0.2.50:443?type=tcp&security=reality&pbk=EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE&fp=chrome&sni=google.com&sid=feedface&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");

        let route = cfg.get("route").expect("route section");
        let rules = route
            .get("rules")
            .and_then(|r| r.as_array())
            .expect("route.rules");

        fn rule_outbound(rules: &[serde_json::Value], field: &str, value: &str) -> Option<String> {
            for r in rules {
                let list = r.get(field).and_then(|v| v.as_array());
                if let Some(arr) = list {
                    for item in arr {
                        if item.as_str() == Some(value) {
                            return r
                                .get("outbound")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                    }
                }
            }
            None
        }
        assert_eq!(
            rule_outbound(rules, "domain_suffix", ".telegram.org").as_deref(),
            Some("proxy-tg"),
            "Telegram domains must route to proxy-tg"
        );
        assert_eq!(
            rule_outbound(rules, "domain_suffix", ".youtube.com").as_deref(),
            Some("proxy-yt"),
            "YouTube domains must route to proxy-yt"
        );
        // IP-CIDR fallback for Telegram UDP/QUIC (no SNI to sniff).
        assert!(
            rules.iter().any(|r| {
                r.get("outbound").and_then(|v| v.as_str()) == Some("proxy-tg")
                    && r.get("ip_cidr").is_some()
            }),
            "Telegram IP-CIDR fallback rule missing (required for UDP/QUIC)"
        );
        assert_eq!(
            route.get("final").and_then(|v| v.as_str()),
            Some("proxy"),
            "route.final must be the generic proxy group"
        );
    }

    /// Each URLTest group probes its own target — generic `/generate_204`
    /// is not enough because an exit that passes latency probes can still
    /// fail bulk traffic to the real destination.
    #[test]
    fn urltest_groups_use_real_world_probes() {
        let raw = "vless://00000000-0000-4000-8000-000000000006@192.0.2.60:443?type=tcp&security=reality&pbk=FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF&fp=chrome&sni=google.com&sid=facefeed&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();

        let mut by_tag: std::collections::HashMap<String, String> = Default::default();
        for o in outbounds
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
        {
            let tag = o
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = o
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            by_tag.insert(tag, url);
        }
        assert!(
            by_tag
                .get("proxy-tg")
                .map(|u| u.contains("telegram.org"))
                .unwrap_or(false),
            "proxy-tg probe must target telegram.org, got {:?}",
            by_tag.get("proxy-tg")
        );
        assert!(
            by_tag
                .get("proxy-yt")
                .map(|u| u.contains("youtube.com"))
                .unwrap_or(false),
            "proxy-yt probe must target youtube.com, got {:?}",
            by_tag.get("proxy-yt")
        );
        // Default group — avoid gstatic which sometimes is regional-blocked
        // and is what the v2.2.4 bug-probe happened to be using.
        let default_probe = by_tag.get("proxy").cloned().unwrap_or_default();
        assert!(
            !default_probe.contains("gstatic.com"),
            "default proxy group should not probe gstatic (regional-block risk), got {:?}",
            default_probe
        );
        assert!(
            !default_probe.is_empty(),
            "default proxy group has no probe URL"
        );
    }

    /// TUN inbound must NOT override destination with the sniffed domain.
    /// DNS resolution goes through dns-proxy (Cloudflare DoH via VPN tunnel)
    /// so resolved IPs are already correct. override=true breaks Telegram's
    /// anti-censorship proxies: TG connects to DC IPs with fake SNI
    /// (e.g. "www.google.com") and override rewrites destination to Google.
    /// Verified 2026-04-16: @STmarkml Telegram frozen, sing-box connections
    /// showed host=www.google.com dest=5.28.195.2:5222 (Telegram DC with
    /// fake SNI being overridden).
    #[test]
    fn tun_inbound_does_not_override_destination() {
        let raw = "vless://00000000-0000-4000-8000-000000000007@192.0.2.70:443?type=tcp&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");
        let inbounds = cfg.get("inbounds").and_then(|o| o.as_array()).unwrap();
        let tun = inbounds
            .iter()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("tun"))
            .expect("tun inbound");
        assert_eq!(
            tun.get("sniff").and_then(|v| v.as_bool()),
            Some(true),
            "sniff must be on so the SNI/Host can be extracted"
        );
        assert_eq!(
            tun.get("sniff_override_destination")
                .and_then(|v| v.as_bool()),
            Some(false),
            "sniff_override_destination must be OFF — DNS goes through dns-proxy \
             (correct IPs), and override breaks Telegram anti-censorship (fake SNI)"
        );
    }

    /// v2.3.4 regression: the general `proxy` urltest group must ALSO exclude
    /// TCP Reality exits (flow=xtls-rprx-vision), not only proxy-tg/proxy-yt.
    /// TSPU bulk-blocks sustained traffic on xtls-rprx-vision after 15-20KB
    /// while tiny URLTest probes pass → urltest picks a broken exit for long
    /// streams (persistent WebSocket sessions, voice/media apps, large downloads).
    /// Same pattern as field tests on 2026-04-16 and 2026-04-22. Mixing a TCP Reality exit
    /// with safer exits (gRPC Reality / HTTPUpgrade / port-443 relay) is ONLY
    /// safe when the Reality exit is the last resort. Defense-in-depth
    /// alongside server-side template v81.
    #[test]
    fn general_proxy_group_excludes_tcp_reality_when_alternatives_exist() {
        // Synthetic server payload: one TCP Reality exit (flow=xtls-rprx-vision)
        // + one gRPC Reality exit (no xtls flow). General `proxy` group must
        // pick only the gRPC one.
        let server = serde_json::json!({
            "outbounds": [
                {
                    "type": "vless",
                    "tag": "tcp-reality-1",
                    "server": "192.0.2.10",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000001",
                    "flow": "xtls-rprx-vision"
                },
                {
                    "type": "vless",
                    "tag": "grpc-reality-1",
                    "server": "192.0.2.20",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000002"
                }
            ]
        });
        let cfg = build_config_from_server(&server, InboundMode::Tun).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        let proxy = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy"))
            .expect("proxy urltest group");
        let members: Vec<&str> = proxy
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !members.contains(&"tcp-reality-1"),
            "general `proxy` group must NOT include TCP Reality (xtls-rprx-vision) \
             when safer exits exist. Got members: {:?}. This protects RU users from \
             TSPU bulk-block on long streams (persistent WebSocket sessions, \
             voice/media apps, large downloads).",
            members
        );
        assert!(
            members.contains(&"grpc-reality-1"),
            "general `proxy` group must include the safe gRPC Reality exit, \
             got members: {:?}",
            members
        );
    }

    /// v2.3.4: when ALL exits are TCP Reality (non-RF user, single Reality
    /// origin), the general `proxy` group MUST fall back to including them.
    /// An empty urltest group is worse than a TSPU-vulnerable one — it breaks
    /// the UI entirely (App.tsx binds "Auto Select" to this group name).
    #[test]
    fn general_proxy_group_falls_back_to_tcp_reality_if_only_option() {
        let server = serde_json::json!({
            "outbounds": [
                {
                    "type": "vless",
                    "tag": "only-tcp-reality",
                    "server": "192.0.2.30",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000003",
                    "flow": "xtls-rprx-vision"
                }
            ]
        });
        let cfg = build_config_from_server(&server, InboundMode::Tun).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        let proxy = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy"))
            .expect("proxy urltest group");
        let members: Vec<&str> = proxy
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            members,
            vec!["only-tcp-reality"],
            "general `proxy` group must fall back to the TCP Reality exit when \
             no safer alternatives exist (non-RF user with single Reality origin)"
        );
    }

    /// v2.3.4: all three URLTest groups must have `interrupt_exist_connections
    /// = true` so a stale selection does not keep serving new requests after
    /// the urltest picks a different exit. Matches server-side template v81.
    /// Without this, @voksep's symptom reproduces on the client fallback path
    /// (build_config_from_server) even when server sends only outbounds.
    #[test]
    fn urltest_groups_interrupt_existing_connections_on_switch() {
        let raw = "vless://00000000-0000-4000-8000-000000000008@192.0.2.80:443?type=tcp&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        for ut in outbounds
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
        {
            let tag = ut.get("tag").and_then(|t| t.as_str()).unwrap_or("?");
            assert_eq!(
                ut.get("interrupt_exist_connections")
                    .and_then(|v| v.as_bool()),
                Some(true),
                "urltest group {:?} must have interrupt_exist_connections=true \
                 (stale exits otherwise serve new requests for 30m after probe \
                 switches selection). @voksep 2026-04-22, server mirror v81.",
                tag
            );
        }
    }

    /// v2.3.4: a Google/YouTube IP-CIDR rule must be present in route.rules
    /// to catch QUIC UDP 443 to googlevideo.com when sniff misses SNI. Chrome
    /// caches `alt-svc: h3=:443` after first HTTP/2 response and switches to
    /// QUIC; sing-box sniff on QUIC initial packet is unreliable → rule by
    /// domain_suffix does not fire → connection falls through to route.final.
    /// Without this rule, @voksep's "second video hangs, page reload fixes"
    /// symptom reproduces. Mirrors telegram_ip_cidr pattern and server-side
    /// template v81 Google ASN15169 rule.
    #[test]
    fn youtube_ip_cidr_fallback_rule_present_for_quic_leak() {
        let raw = "vless://00000000-0000-4000-8000-000000000009@192.0.2.90:443?type=tcp&security=reality&pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB&fp=chrome&sni=google.com&sid=cafebabe&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");
        let rules = cfg
            .get("route")
            .and_then(|r| r.get("rules"))
            .and_then(|r| r.as_array())
            .expect("route.rules array");
        // Find the IP-CIDR rule that steers Google to proxy-yt.
        let google_cidr_rule = rules.iter().find(|r| {
            r.get("outbound").and_then(|v| v.as_str()) == Some("proxy-yt")
                && r.get("ip_cidr")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().any(|v| v.as_str() == Some("142.250.0.0/15")))
                    .unwrap_or(false)
        });
        assert!(
            google_cidr_rule.is_some(),
            "route.rules must contain a Google/YouTube IP-CIDR rule → proxy-yt \
             (at minimum 142.250.0.0/15 from ASN15169). Without it, QUIC UDP 443 \
             to googlevideo leaks to route.final when sniff misses SNI. Got rules: {:?}",
            rules
        );
        let rule = google_cidr_rule.unwrap();
        let cidrs: Vec<&str> = rule
            .get("ip_cidr")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        // Expect the same coverage as server-side template v81 (9 prefixes).
        for required in &[
            "142.250.0.0/15",
            "172.217.0.0/16",
            "173.194.0.0/16",
            "216.58.192.0/19",
            "2607:f8b0::/32",
        ] {
            assert!(
                cidrs.contains(required),
                "Google IP-CIDR rule missing {:?}, got={:?}",
                required,
                cidrs
            );
        }
    }

    #[test]
    fn wbstream_manifest_metadata_is_shape_checked() {
        let manifest = serde_json::json!({
            "payload": {
                "kind": "proteus-wbstream-room-manifest",
                "rooms": [
                    { "role": "active", "priority": 0, "url": "wbstream://019e3d05-046e-7af4-8ca6-5d81631bf9aa" }
                ]
            },
            "payload_b64": "e30=",
            "signature_alg": "RS256",
            "signature": "sig"
        });
        assert!(is_usable_wbstream_manifest(&manifest));

        let bad_manifest = serde_json::json!({
            "payload": { "rooms": [{ "url": "https://example.com" }] },
            "payload_b64": "e30=",
            "signature_alg": "RS256",
            "signature": "sig"
        });
        assert!(!is_usable_wbstream_manifest(&bad_manifest));
    }

    #[test]
    fn client_side_metadata_is_removed_before_full_singbox_write() {
        let mut cfg = serde_json::json!({
            "dns": {},
            "inbounds": [],
            "route": {},
            "wbstream_manifest": {
                "payload": { "rooms": [{ "url": "wbstream://room" }] },
                "payload_b64": "e30=",
                "signature_alg": "RS256",
                "signature": "sig"
            }
        });
        strip_client_side_metadata(&mut cfg);
        assert!(cfg.get("wbstream_manifest").is_none());
        assert!(cfg.get("dns").is_some());
        assert!(cfg.get("inbounds").is_some());
        assert!(cfg.get("route").is_some());
    }

    #[test]
    fn wbstream_room_selection_prefers_lowest_priority() {
        let manifest = serde_json::json!({
            "payload": {
                "kind": "proteus-wbstream-room-manifest",
                "rooms": [
                    { "role": "standby", "priority": 1, "url": "wbstream://standby" },
                    { "role": "active", "priority": 0, "url": "wbstream://active" }
                ]
            },
            "payload_b64": "e30=",
            "signature_alg": "RS256",
            "signature": "sig"
        });
        assert_eq!(
            select_wbstream_room_url(&manifest).as_deref(),
            Some("wbstream://active")
        );
    }

    #[test]
    fn wbstream_room_selection_can_return_multiple_sorted_rooms() {
        let manifest = serde_json::json!({
            "payload": {
                "kind": "proteus-wbstream-room-manifest",
                "rooms": [
                    { "role": "next", "priority": 2, "url": "wbstream://next" },
                    { "role": "standby", "priority": 1, "url": "wbstream://standby" },
                    { "role": "active", "priority": 0, "url": "wbstream://active" },
                    { "role": "bad", "priority": 3, "url": "https://example.invalid" }
                ]
            },
            "payload_b64": "e30=",
            "signature_alg": "RS256",
            "signature": "sig"
        });

        assert_eq!(
            select_wbstream_room_urls(&manifest, 3),
            vec![
                "wbstream://active".to_string(),
                "wbstream://standby".to_string(),
                "wbstream://next".to_string()
            ]
        );
    }

    #[test]
    fn wbstream_fallback_config_routes_data_plane_and_keeps_wb_direct() {
        let cfg = build_wbstream_fallback_config(InboundMode::Tun, 11080);
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        assert!(outbounds.iter().any(|o| {
            o.get("tag").and_then(|v| v.as_str()) == Some("wbstream-local")
                && o.get("type").and_then(|v| v.as_str()) == Some("socks")
        }));
        assert_eq!(
            cfg.get("route")
                .and_then(|r| r.get("final"))
                .and_then(|v| v.as_str()),
            Some("wbstream-local")
        );
        let rules = cfg
            .get("route")
            .and_then(|r| r.get("rules"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert!(
            rules.iter().any(|r| {
                r.get("outbound").and_then(|v| v.as_str()) == Some("direct")
                    && r.get("domain_suffix")
                        .and_then(|v| v.as_array())
                        .map(|domains| domains.iter().any(|d| d.as_str() == Some(".wb.ru")))
                        .unwrap_or(false)
            }),
            "WB endpoints must stay direct so the sidecar is not captured by the TUN route"
        );
    }

    #[test]
    fn wbstream_fallback_config_can_route_to_balancer_port() {
        let cfg = build_wbstream_fallback_config(InboundMode::Tun, WBSTREAM_LOCAL_BALANCER_PORT);
        let outbound = cfg
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .find(|o| o.get("tag").and_then(|v| v.as_str()) == Some("wbstream-local"))
            .expect("wbstream-local outbound");

        assert_eq!(
            outbound.get("server_port").and_then(|v| v.as_u64()),
            Some(WBSTREAM_LOCAL_BALANCER_PORT as u64)
        );
        assert_eq!(
            cfg.get("route")
                .and_then(|r| r.get("final"))
                .and_then(|v| v.as_str()),
            Some("wbstream-local")
        );
    }
}

#[cfg(test)]
mod cache_fallback_tests {
    use super::*;

    #[test]
    fn load_cached_tun_config_from_validates_outbounds() {
        let dir = std::env::temp_dir().join(format!("lumen-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let good = dir.join("good.json");
        std::fs::write(&good, r#"{"outbounds":[{"type":"vless","tag":"x"}]}"#).unwrap();
        assert!(load_cached_tun_config_from(&good).is_ok(), "valid config with outbounds must load");

        let empty = dir.join("empty.json");
        std::fs::write(&empty, r#"{"outbounds":[]}"#).unwrap();
        assert!(load_cached_tun_config_from(&empty).is_err(), "empty outbounds must be rejected");

        let garbage = dir.join("garbage.json");
        std::fs::write(&garbage, "not json").unwrap();
        assert!(load_cached_tun_config_from(&garbage).is_err(), "invalid json must be rejected");

        let missing = dir.join("missing.json");
        assert!(load_cached_tun_config_from(&missing).is_err(), "missing file must be rejected");

        let _ = std::fs::remove_dir_all(&dir);
    }
}


// ---- P1a: bootstrap-over-own-cached-exit (L2) — pure helpers ----------------
// When the config endpoint is blocked, we fetch a fresh config THROUGH one of
// the user's OWN cached Reality exits. These helpers build that bootstrap tunnel
// config. No bundled secrets: they only ever read the user's cached config.

/// Pick a usable Reality outbound from a cached/server config to serve as the
/// bootstrap tunnel. Prefers a direct (non-relay) tcp/Vision exit; falls back to
/// any Reality outbound. Returns None if the config has no Reality exit.
pub fn extract_bootstrap_exit(config: &serde_json::Value) -> Option<serde_json::Value> {
    let obs = config.get("outbounds")?.as_array()?;
    let is_reality = |o: &serde_json::Value| -> bool {
        o.get("type").and_then(|t| t.as_str()) == Some("vless")
            && o.get("tls").and_then(|tls| tls.get("reality")).is_some()
    };
    let is_direct = |o: &serde_json::Value| -> bool {
        let tag = o
            .get("tag")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_lowercase();
        let relayish = ["relay", "moscow", "firstbyte", "timeweb", "via"]
            .iter()
            .any(|k| tag.contains(k));
        let tcp = o
            .get("transport")
            .and_then(|tr| tr.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("tcp")
            == "tcp";
        let vision = o.get("flow").and_then(|f| f.as_str()).unwrap_or("") == "xtls-rprx-vision";
        !relayish && (tcp || vision)
    };
    obs.iter()
        .find(|o| is_reality(o) && is_direct(o))
        .cloned()
        .or_else(|| obs.iter().find(|o| is_reality(o)).cloned())
}

/// Build a minimal sing-box config exposing a local SOCKS/mixed proxy that
/// routes through one Reality `exit` outbound. Used to fetch the config gateway
/// THROUGH a working tunnel when the endpoint is locally (SNI-)blocked.
pub fn build_bootstrap_proxy_config(exit: &serde_json::Value, port: u16) -> serde_json::Value {
    let tag = exit
        .get("tag")
        .and_then(|t| t.as_str())
        .unwrap_or("bootstrap-exit")
        .to_string();
    serde_json::json!({
        "log": { "level": "warn" },
        "dns": { "servers": [{ "address": "1.1.1.1" }] },
        "inbounds": [{
            "type": "mixed",
            "tag": "bootstrap-socks",
            "listen": "127.0.0.1",
            "listen_port": port
        }],
        "outbounds": [ exit, { "type": "direct", "tag": "direct" } ],
        "route": { "final": tag }
    })
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;

    #[test]
    fn extract_bootstrap_exit_prefers_direct_reality_over_relay() {
        let cfg = serde_json::json!({"outbounds": [
            {"type":"vless","tag":"relay-eu-1","tls":{"reality":{}},"transport":{"type":"httpupgrade"}},
            {"type":"vless","tag":"netcup-tcp-reality","flow":"xtls-rprx-vision","tls":{"reality":{}}},
            {"type":"direct","tag":"direct"}
        ]});
        let e = extract_bootstrap_exit(&cfg).expect("should find a direct reality exit");
        assert_eq!(e.get("tag").and_then(|t| t.as_str()), Some("netcup-tcp-reality"));
    }

    #[test]
    fn extract_bootstrap_exit_falls_back_to_any_reality() {
        let cfg = serde_json::json!({"outbounds": [
            {"type":"vless","tag":"relay-eu-1","tls":{"reality":{}},"transport":{"type":"grpc"}}
        ]});
        assert!(extract_bootstrap_exit(&cfg).is_some(), "any reality exit is acceptable fallback");
    }

    #[test]
    fn extract_bootstrap_exit_none_when_no_reality() {
        let cfg = serde_json::json!({"outbounds": [{"type":"direct","tag":"direct"}]});
        assert!(extract_bootstrap_exit(&cfg).is_none());
    }

    #[test]
    fn build_bootstrap_proxy_config_has_socks_inbound_and_routes_via_exit() {
        let exit = serde_json::json!({"type":"vless","tag":"x-exit","tls":{"reality":{}}});
        let c = build_bootstrap_proxy_config(&exit, 11991);
        assert_eq!(c["inbounds"][0]["type"], "mixed");
        assert_eq!(c["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(c["inbounds"][0]["listen_port"], 11991);
        assert_eq!(c["route"]["final"], "x-exit");
        assert_eq!(c["outbounds"][0]["tag"], "x-exit");
        assert_eq!(c["outbounds"][1]["type"], "direct");
    }
}
