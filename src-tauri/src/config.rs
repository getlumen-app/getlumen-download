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
    const RESERVED: &[&str] = &["proxy", "direct", "block", "dns-out", "dns-in", "tun-in", "mixed-in"];
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

    let mut config = serde_json::json!({
        "log": {"level": "info", "timestamp": true},
        "dns": {
            "servers": [
                {
                    "tag": "dns-proxy",
                    "address": "https://1.1.1.1/dns-query",
                    "detour": "proxy"
                },
                {
                    "tag": "dns-direct",
                    "address": "77.88.8.1",
                    "detour": "direct"
                }
            ],
            "rules": [
                {"outbound": "any", "server": "dns-direct"}
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
                    "listen_port": 10808
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
                    "sniff": true
                }
            ]),
        },
        "outbounds": [],
        "route": {
            "rules": [
                {"domain_suffix": [".ru", ".su", ".xn--p1ai"], "outbound": "direct"},
                {
                    "domain_suffix": [
                        ".yandex.net", ".yandex.ru", ".yandex.com",
                        ".yastatic.net", ".yastat.net", ".ya.ru", ".dzen.ru"
                    ],
                    "outbound": "direct"
                },
                {
                    "domain_suffix": [
                        ".vk.com", ".vk.me", ".mail.ru", ".ok.ru",
                        ".userapi.com", ".vkuservideo.net"
                    ],
                    "outbound": "direct"
                },
                {
                    "domain_suffix": [
                        ".sberbank.ru", ".tinkoff.ru", ".tbank.ru",
                        ".vtb.ru", ".alfabank.ru", ".gosuslugi.ru"
                    ],
                    "outbound": "direct"
                }
            ],
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
        // urltest group — auto-selects best proxy
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy",
            "outbounds": proxy_names,
            "url": "https://www.gstatic.com/generate_204",
            "interval": "60s",
            "tolerance": 100
        }));

        // Server-provided proxy outbounds
        for o in outbounds {
            arr.push(o.clone());
        }

        // Standard outbounds
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
        // Required tags present
        assert!(tags.contains(&"proxy".to_string()), "missing 'proxy' urltest group");
        assert!(tags.contains(&"direct".to_string()), "missing 'direct'");
        assert!(tags.contains(&"block".to_string()), "missing 'block'");
        assert!(tags.iter().any(|t| t == "user-1"), "missing vless outbound 'user-1', tags={:?}", tags);

        // The urltest group must reference the vless outbound, not itself.
        let urltest = outbounds
            .iter()
            .find(|o| o.get("type").and_then(|t| t.as_str()) == Some("urltest"))
            .expect("urltest group present");
        let inner = urltest
            .get("outbounds")
            .and_then(|o| o.as_array())
            .expect("urltest.outbounds is array");
        let inner_tags: Vec<&str> = inner.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(inner_tags, vec!["user-1"], "urltest must wrap vless tag, not 'proxy'");
    }
}
