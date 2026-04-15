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
                .join("AppData").join("Local")
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

/// Config endpoint URL — injected at build time, fallback to default.
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

    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(format!("Config server returned {}", resp.status()).into());
    }

    let body = resp.text().await?;
    let server_config: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("Invalid config JSON: {}", e))?;

    let config = build_config_from_server(&server_config, mode)?;

    let final_json = serde_json::to_string_pretty(&config)?;
    let path = match mode {
        InboundMode::Mixed => config_file_path(),
        InboundMode::Tun => tun_config_file_path(),
    };
    std::fs::write(&path, &final_json)?;
    log::info!("Config ({:?}) saved to {} ({} bytes)", mode, path.display(), final_json.len());

    Ok(final_json)
}

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
        "proxy", "proxy-tg", "proxy-yt",
        "direct", "block",
        "dns-out", "dns-in", "tun-in", "mixed-in",
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
fn build_config_from_server(server: &serde_json::Value, mode: InboundMode) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let outbounds = server.get("outbounds")
        .and_then(|o| o.as_array())
        .ok_or("No outbounds in server config")?;

    // Collect proxy outbound names for urltest group
    let proxy_names: Vec<String> = outbounds.iter()
        .filter_map(|o| {
            let tag = o.get("tag").and_then(|t| t.as_str()).unwrap_or("");
            let otype = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
            // Skip non-proxy types
            if tag == "direct" || tag == "block" || otype == "direct" || otype == "block" {
                return None;
            }
            if tag.is_empty() { return None; }
            Some(tag.to_string())
        })
        .collect();

    if proxy_names.is_empty() {
        return Err("No proxy outbounds in server config".into());
    }

    let cache_path = data_dir().join("cache.db");

    // Domains the user's real ISP must resolve & reach directly (RKN-compliant
    // resolvers, Russian banks, Russian-only services). Keep DNS and routing
    // rules mirrored — a domain that resolves via direct DNS must also route
    // via direct, and vice-versa.
    let russia_direct_domains = serde_json::json!([
        ".ru", ".su", ".xn--p1ai",
        ".yandex.net", ".yandex.ru", ".yandex.com",
        ".yastatic.net", ".yastat.net", ".ya.ru", ".dzen.ru",
        ".vk.com", ".vk.me", ".mail.ru", ".ok.ru",
        ".userapi.com", ".vkuservideo.net",
        ".sberbank.ru", ".tinkoff.ru", ".tbank.ru",
        ".vtb.ru", ".alfabank.ru", ".gosuslugi.ru"
    ]);

    // Domains that get their own health-probed proxy group. sing-box runs
    // a real HTTP probe against the target, not a generic /generate_204 —
    // an exit that cannot actually reach YouTube is excluded from proxy-yt,
    // even if it passes a generic latency check.
    let telegram_domains = serde_json::json!([
        ".telegram.org", ".t.me", ".telegram.me", ".telesco.pe",
        ".telegram-cdn.org", ".tdesktop.com"
    ]);
    // Known Telegram IP ranges — matched when SNI sniffing fails (e.g. QUIC).
    let telegram_ip_cidr = serde_json::json!([
        "91.108.4.0/22", "91.108.8.0/21", "91.108.12.0/22",
        "91.108.16.0/21", "91.108.56.0/22", "109.239.140.0/24",
        "149.154.160.0/20", "95.161.64.0/20", "2001:b28:f23d::/48",
        "2001:b28:f23f::/48", "2001:67c:4e8::/48"
    ]);
    let youtube_domains = serde_json::json!([
        ".youtube.com", ".youtu.be", ".googlevideo.com",
        ".ytimg.com", ".youtube-nocookie.com"
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
                    "sniff_override_destination": true
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
                    // Rewrite connection destination from the DNS-resolved IP
                    // to the SNI/Host domain after sniffing. Outbound proxies
                    // then re-resolve at the exit, bypassing locally-poisoned
                    // DNS for any protocol that exposes a domain (TLS, HTTP).
                    "sniff_override_destination": true
                }
            ]),
        },
        "outbounds": [],
        "route": {
            "rules": [
                // Russia-direct traffic never touches the VPN.
                {"domain_suffix": russia_direct_domains.clone(), "outbound": "direct"},
                // Telegram: match by domain AND by IP range (for QUIC / UDP
                // cases where sniffing yields no domain).
                {"domain_suffix": telegram_domains.clone(), "outbound": "proxy-tg"},
                {"ip_cidr": telegram_ip_cidr.clone(), "outbound": "proxy-tg"},
                // YouTube and Google video.
                {"domain_suffix": youtube_domains.clone(), "outbound": "proxy-yt"}
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
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy",
            "outbounds": proxy_names.clone(),
            // Small, cache-free payload served by Cloudflare; not regional-blocked.
            "url": "https://www.cloudflare.com/cdn-cgi/trace",
            "interval": "30s",
            "tolerance": 100,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));
        // Destination-specific URLTest groups — the probe URL is the actual
        // service, so an exit that can't reach it is dropped from the group.
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-tg",
            "outbounds": proxy_names.clone(),
            "url": "https://web.telegram.org/",
            "interval": "30s",
            "tolerance": 100,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-yt",
            "outbounds": proxy_names.clone(),
            "url": "https://www.youtube.com/generate_204",
            "interval": "30s",
            "tolerance": 100,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));

        // Server-provided proxy outbounds.
        for o in outbounds {
            arr.push(o.clone());
        }

        // Standard outbounds.
        arr.push(serde_json::json!({"type": "direct", "tag": "direct"}));
        arr.push(serde_json::json!({"type": "block", "tag": "block"}));
    }

    log::info!("Built config: {} proxy outbounds",
        config.get("outbounds").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0));

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

        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).expect("outbounds array");
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
        assert!(tags.iter().any(|t| t == "user-1"), "missing vless outbound 'user-1', tags={:?}", tags);

        // Each urltest group must reference the vless outbound, not itself.
        let urltest_tags: Vec<String> = outbounds
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
            .map(|o| o.get("tag").and_then(|t| t.as_str()).unwrap_or("").to_string())
            .collect();
        assert_eq!(
            urltest_tags,
            vec!["proxy".to_string(), "proxy-tg".to_string(), "proxy-yt".to_string()],
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
        let rules = dns.get("rules").and_then(|r| r.as_array()).expect("dns.rules array");
        for r in rules {
            if r.get("outbound").and_then(|v| v.as_str()) == Some("any") {
                panic!("dns.rules contains a catch-all `outbound: any` rule: {:?}", r);
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

        fn direct_domains(section: &serde_json::Value, outbound_field: &str, target: &str) -> Vec<String> {
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
            direct_domains(cfg.get("dns").unwrap(), "server", "dns-direct").into_iter().collect();
        let route_direct: HashSet<String> =
            direct_domains(cfg.get("route").unwrap(), "outbound", "direct").into_iter().collect();

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
        let rules = route.get("rules").and_then(|r| r.as_array()).expect("route.rules");

        fn rule_outbound(
            rules: &[serde_json::Value],
            field: &str,
            value: &str,
        ) -> Option<String> {
            for r in rules {
                let list = r.get(field).and_then(|v| v.as_array());
                if let Some(arr) = list {
                    for item in arr {
                        if item.as_str() == Some(value) {
                            return r.get("outbound").and_then(|v| v.as_str()).map(str::to_string);
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
        for o in outbounds.iter().filter(|o| {
            o.get("type").and_then(|t| t.as_str()) == Some("urltest")
        }) {
            let tag = o.get("tag").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let url = o.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            by_tag.insert(tag, url);
        }
        assert!(
            by_tag.get("proxy-tg").map(|u| u.contains("telegram.org")).unwrap_or(false),
            "proxy-tg probe must target telegram.org, got {:?}",
            by_tag.get("proxy-tg")
        );
        assert!(
            by_tag.get("proxy-yt").map(|u| u.contains("youtube.com")).unwrap_or(false),
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

    /// TUN inbound must rewrite the destination to the sniffed domain so the
    /// exit-side resolver can pick the correct IP, bypassing any local DNS
    /// poisoning. Without this, sing-box carries the DNS-resolved IP — which
    /// is the exact failure mode we saw in v2.2.4.
    #[test]
    fn tun_inbound_overrides_destination_on_sniff() {
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
            tun.get("sniff_override_destination").and_then(|v| v.as_bool()),
            Some(true),
            "sniff_override_destination must be on so poisoned-IP traffic gets\
             rewritten to the sniffed domain before reaching the outbound"
        );
    }
}

