use std::{net::SocketAddr, path::PathBuf};

type ConfigError = Box<dyn std::error::Error + Send + Sync>;

pub fn config_file_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn bootstrap_full_config_url_path() -> PathBuf {
    data_dir().join("bootstrap-full-config-url.txt")
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

const PROTEUS_CONFIG_FALLBACK_BASE: &str =
    "https://primary-production-1d1cf.up.railway.app/webhook";

const CONFIG_DNS_PINS: &[(&str, &[&str])] = &[(
    "config.getlumen.download",
    &["104.21.75.98:443", "172.67.220.94:443"],
)];

pub fn proteus_config_urls(sub_key: &str) -> Vec<String> {
    let key = sub_key.trim();
    let primary = format!(
        "{}/proteus-sub?sub={}&format=json-text",
        config_base_url().trim_end_matches('/'),
        key
    );
    let fallback = format!(
        "{}/proteus-sub?sub={}&format=json-text",
        PROTEUS_CONFIG_FALLBACK_BASE, key
    );
    let mut urls = vec![primary, fallback];
    urls.dedup();
    urls
}

pub fn redact_config_url_for_error(url: &str) -> String {
    redact_sub_query_in_text(url)
}

pub fn redact_sub_query_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after_marker)) = rest.split_once("sub=") {
        out.push_str(before);
        out.push_str("sub=<redacted>");
        let end = after_marker
            .find(|c| c == '&' || c == '#' || c == ')' || c == ' ')
            .unwrap_or(after_marker.len());
        out.push_str(&after_marker[end..]);
        rest = "";
    }
    if !rest.is_empty() {
        out.push_str(rest);
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub enum InboundMode {
    /// HTTP/SOCKS proxy on 127.0.0.1:10808 — runs as user, no root needed
    Mixed,
    /// TUN interface — needs a privileged runtime, low latency
    Tun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TunPolicy {
    interface_name: &'static str,
    mtu: u16,
    strict_route: bool,
    /// sing-box netstack. Windows clients that work in the field (v2rayN,
    /// Clash Verge, NekoRay) ship `gvisor` as the default there: the system
    /// TCP stack behind `mixed` is the fragile one on Wintun adapters.
    stack: &'static str,
}

fn tun_policy_for_target(target_os: &str) -> TunPolicy {
    match target_os {
        "windows" => TunPolicy {
            interface_name: "Lumen",
            mtu: 1500,
            strict_route: true,
            stack: "gvisor",
        },
        _ => TunPolicy {
            interface_name: "utun777",
            mtu: 9000,
            strict_route: false,
            stack: "mixed",
        },
    }
}

fn tun_inbounds_for_target(target_os: &str) -> serde_json::Value {
    let policy = tun_policy_for_target(target_os);
    serde_json::json!([
        {
            "type": "tun",
            "tag": "tun-in",
            "interface_name": policy.interface_name,
            "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
            "mtu": policy.mtu,
            "auto_route": true,
            "strict_route": policy.strict_route,
            "stack": policy.stack,
            "endpoint_independent_nat": true
        }
    ])
}

fn tun_inbounds() -> serde_json::Value {
    tun_inbounds_for_target(std::env::consts::OS)
}

fn mixed_inbounds() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "mixed",
            "tag": "mixed-in",
            "listen": "127.0.0.1",
            "listen_port": 10808
        }
    ])
}

fn enforce_requested_inbound(config: &mut serde_json::Value, mode: InboundMode) {
    match mode {
        InboundMode::Mixed => {
            // Server full configs may carry a TUN/mobile inbound. System Proxy
            // mode must always expose the local mixed listener macOS points at.
            config["inbounds"] = mixed_inbounds();
        }
        InboundMode::Tun => {
            config["inbounds"] = tun_inbounds();
            // Without DNS hijack, browsers keep the ISP resolver under TUN and
            // fail with NXDOMAIN while System Proxy still works (Ekaterina 2026-07-31).
            ensure_dns_hijack_route_rule(config);
        }
    }
    // Inbound sniff fields were removed in sing-box 1.13 — sniffing is now a
    // route rule action. Insert AFTER the hijack insert so sniff lands first.
    ensure_sniff_route_rule(config);
}

/// Insert `protocol=dns` → `action=hijack-dns` as the first route rule.
///
/// Our bundled sing-box rejects inbound `dns_mode` (pre-1.14 field) but accepts
/// the modern rule action. Idempotent: skips when hijack (or legacy dns-out)
/// is already present.
fn ensure_dns_hijack_route_rule(config: &mut serde_json::Value) {
    let rules = match config
        .pointer_mut("/route/rules")
        .and_then(|r| r.as_array_mut())
    {
        Some(rules) => rules,
        None => {
            if !config.get("route").map(|r| r.is_object()).unwrap_or(false) {
                config["route"] = serde_json::json!({});
            }
            config["route"]["rules"] = serde_json::json!([]);
            config
                .pointer_mut("/route/rules")
                .and_then(|r| r.as_array_mut())
                .expect("route.rules just created")
        }
    };

    let already = rules.iter().any(|r| {
        r.get("action").and_then(|a| a.as_str()) == Some("hijack-dns")
            || (r.get("protocol").and_then(|p| p.as_str()) == Some("dns")
                && r.get("outbound").and_then(|o| o.as_str()) == Some("dns-out"))
    });
    if already {
        return;
    }
    rules.insert(
        0,
        serde_json::json!({
            "protocol": "dns",
            "action": "hijack-dns"
        }),
    );
}

/// Insert `{"action": "sniff"}` as the first route rule — the 1.14 replacement
/// for the inbound `sniff`/`sniff_override_destination` fields removed in
/// sing-box 1.13. Domain rules (RU-direct, Telegram, YouTube) depend on the
/// sniffed SNI/Host. Idempotent: skips when a sniff action is already present.
/// Call AFTER ensure_dns_hijack_route_rule so sniff lands before hijack-dns.
fn ensure_sniff_route_rule(config: &mut serde_json::Value) {
    if !config
        .get("route")
        .map(|r| r.is_object())
        .unwrap_or(false)
    {
        config["route"] = serde_json::json!({});
    }
    if !config
        .pointer("/route/rules")
        .map(|r| r.is_array())
        .unwrap_or(false)
    {
        config["route"]["rules"] = serde_json::json!([]);
    }
    let rules = config
        .pointer_mut("/route/rules")
        .and_then(|r| r.as_array_mut())
        .expect("route.rules just ensured");

    let already = rules
        .iter()
        .any(|r| r.get("action").and_then(|a| a.as_str()) == Some("sniff"));
    if !already {
        rules.insert(0, serde_json::json!({"action": "sniff"}));
    }
}

/// Translate legacy (pre-1.12) sing-box constructs that the Proteus config
/// server still emits into the formats sing-box 1.14 accepts:
/// - flat `address` DNS servers -> typed servers; `rcode://X` servers fold
///   into `action: predefined` DNS rules
/// - `address_resolver` -> `domain_resolver`
/// - bare `direct` outbounds + route dial fields get an explicit resolver
/// Idempotent — already-migrated configs pass through unchanged.
fn migrate_legacy_singbox_config(config: &mut serde_json::Value) {
    let rcode_tags = migrate_legacy_dns_servers(config);
    if !rcode_tags.is_empty() {
        rewrite_rcode_dns_rules(config, &rcode_tags);
    }
    let local_tag = ensure_local_dns_server(config);
    ensure_dial_resolvers(config, &local_tag);
    strip_legacy_inbound_fields(config);
    drop_conditionless_route_rules(config);
}

/// Drop route rules whose only "condition" is an empty list (e.g. a stripped
/// `domain_suffix: []`). In sing-box a rule with no conditions matches ALL
/// traffic — observed 2026-09-15: the Proteus template carried such a rule to
/// the RU exit, silently sending YouTube/Instagram to a Moscow residential IP.
fn drop_conditionless_route_rules(config: &mut serde_json::Value) {
    const CONDITION_KEYS: &[&str] = &[
        "domain",
        "domain_suffix",
        "domain_keyword",
        "domain_regex",
        "ip_cidr",
        "rule_set",
        "geoip",
        "geosite",
        "network",
        "port",
        "port_range",
        "protocol",
        "ip_version",
        "ip_is_private",
        "source_ip_cidr",
        "source_port",
        "user",
        "process_name",
        "package_name",
        "wifi_ssid",
        "wifi_bssid",
        "clash_mode",
        "client",
    ];
    let Some(rules) = config
        .get_mut("route")
        .and_then(|r| r.get_mut("rules"))
        .and_then(|r| r.as_array_mut())
    else {
        return;
    };
    rules.retain(|rule| {
        if rule.get("action").is_some() {
            return true; // sniff/hijack/predefined actions are valid without conditions
        }
        CONDITION_KEYS.iter().any(|k| match rule.get(*k) {
            Some(serde_json::Value::Array(a)) => !a.is_empty(),
            Some(v) => !v.is_null(),
            None => false,
        })
    });
}

/// Convert `dns.servers[*].address` entries to typed 1.14 servers. Returns the
/// map tag -> rcode-name for `rcode://X` servers (which no longer exist as
/// servers in 1.14 — their rules become `action: predefined`).
fn migrate_legacy_dns_servers(
    config: &mut serde_json::Value,
) -> std::collections::HashMap<String, String> {
    let mut rcode: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let Some(servers) = config
        .pointer_mut("/dns/servers")
        .and_then(|s| s.as_array_mut())
    else {
        return rcode;
    };

    let mut i = 0;
    while i < servers.len() {
        let Some(obj) = servers[i].as_object_mut() else {
            i += 1;
            continue;
        };
        // Fields removed in 1.14 regardless of format.
        for gone in ["strategy", "client_subnet"] {
            if obj.remove(gone).is_some() {
                log::warn!("dns.servers[{i}]: dropped removed field '{gone}'");
            }
        }
        if let Some(resolver) = obj.remove("address_resolver") {
            obj.entry("domain_resolver".to_string()).or_insert(resolver);
        }
        let Some(addr) = obj
            .get("address")
            .and_then(|a| a.as_str())
            .map(|s| s.to_string())
        else {
            i += 1; // already typed
            continue;
        };
        obj.remove("address");
        if let Some(rc) = addr.strip_prefix("rcode://") {
            if let Some(tag) = obj.get("tag").and_then(|t| t.as_str()) {
                // sing-box names rcode 0 NOERROR; legacy configs write "success".
                let name = match rc.to_uppercase().as_str() {
                    "SUCCESS" => "NOERROR".to_string(),
                    other => other.to_string(),
                };
                rcode.insert(tag.to_string(), name);
            }
            servers.remove(i);
            continue;
        }
        let (typ, server, path, port) = split_legacy_dns_address(&addr);
        obj.insert("type".into(), serde_json::json!(typ));
        if let Some(s) = server {
            obj.insert("server".into(), serde_json::json!(s));
        }
        if let Some(p) = path {
            obj.insert("path".into(), serde_json::json!(p));
        }
        if let Some(p) = port {
            obj.insert("server_port".into(), serde_json::json!(p));
        }
        i += 1;
    }
    rcode
}

/// `(type, server, path, server_port)` for a legacy `address` value.
/// `https://host[:port]/path` keeps an explicit path only when non-default.
fn split_legacy_dns_address(
    addr: &str,
) -> (&'static str, Option<String>, Option<String>, Option<u16>) {
    match addr {
        "local" => return ("local", None, None, None),
        "fakeip" => return ("fakeip", None, None, None),
        _ => {}
    }
    let (scheme, rest) = match addr.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("udp", addr), // bare IP / hostname
    };
    if scheme == "dhcp" {
        return ("dhcp", None, None, None);
    }
    let typ = match scheme {
        "https" => "https",
        "h3" => "h3",
        "tls" => "tls",
        "quic" => "quic",
        "tcp" => "tcp",
        "udp" => "udp",
        _ => {
            log::warn!("dns server address '{addr}': unknown scheme, assuming udp");
            "udp"
        }
    };
    let (hostport, path) = match rest.split_once('/') {
        Some((h, p)) => (h, Some(format!("/{p}"))),
        None => (rest, None),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        // Don't split IPv6 literals like [2001:db8::1].
        Some((h, p)) if !h.ends_with('[') => (h, p.parse().ok()),
        _ => (hostport, None),
    };
    (
        typ,
        Some(host.to_string()),
        path.filter(|p| p != "/dns-query"),
        port,
    )
}

/// `{"domain": ..., "server": "dns-block"}` where dns-block was a `rcode://X`
/// server becomes `{"domain": ..., "action": "predefined", "rcode": "X"}`.
fn rewrite_rcode_dns_rules(
    config: &mut serde_json::Value,
    rcode: &std::collections::HashMap<String, String>,
) {
    let Some(rules) = config
        .pointer_mut("/dns/rules")
        .and_then(|r| r.as_array_mut())
    else {
        return;
    };
    for rule in rules.iter_mut() {
        let tag = rule
            .get("server")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let Some(rc) = tag.as_ref().and_then(|t| rcode.get(t)) else {
            continue;
        };
        let Some(obj) = rule.as_object_mut() else {
            continue;
        };
        obj.remove("server");
        obj.insert("action".into(), serde_json::json!("predefined"));
        obj.insert("rcode".into(), serde_json::json!(rc));
    }
}

/// 1.14 needs a resolver for dial fields; ensure a `local` (system) DNS server
/// exists to back `direct` dials. Returns the tag to reference.
fn ensure_local_dns_server(config: &mut serde_json::Value) -> String {
    let has_local = config
        .pointer("/dns/servers")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("local"))
        })
        .unwrap_or(false);
    if has_local {
        // Prefer the existing tag, whatever it is named.
        if let Some(tag) = config
            .pointer("/dns/servers")
            .and_then(|s| s.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|s| s.get("type").and_then(|t| t.as_str()) == Some("local"))
                    .and_then(|s| s.get("tag"))
                    .and_then(|t| t.as_str())
            })
        {
            return tag.to_string();
        }
        return "local".to_string();
    }
    // Pick a non-colliding tag.
    let taken: std::collections::HashSet<String> = config
        .pointer("/dns/servers")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("tag").and_then(|t| t.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let tag = if !taken.contains("local") {
        "local".to_string()
    } else {
        "local-system".to_string()
    };
    if !config.pointer("/dns").map(|d| d.is_object()).unwrap_or(false) {
        config["dns"] = serde_json::json!({});
    }
    if !config
        .pointer("/dns/servers")
        .map(|s| s.is_array())
        .unwrap_or(false)
    {
        config["dns"]["servers"] = serde_json::json!([]);
    }
    config
        .pointer_mut("/dns/servers")
        .and_then(|s| s.as_array_mut())
        .expect("dns.servers just ensured")
        .push(serde_json::json!({"tag": tag, "type": "local"}));
    tag
}

/// `direct` outbounds dial real hostnames (RU-direct domains) — point them at
/// the local resolver. Everything else (urltest probes, rule-set downloads)
/// resolves via `route.default_domain_resolver`; prefer the proxied DoH server
/// (`dns-proxy` in Proteus configs) so RU DNS poisoning can't kill the probes.
fn ensure_dial_resolvers(config: &mut serde_json::Value, local_tag: &str) {
    if let Some(outbounds) = config.get_mut("outbounds").and_then(|o| o.as_array_mut()) {
        for ob in outbounds.iter_mut() {
            if ob.get("type").and_then(|t| t.as_str()) == Some("direct")
                && ob.get("domain_resolver").is_none()
            {
                ob["domain_resolver"] = serde_json::json!(local_tag);
            }
        }
    }
    if config.pointer("/route/default_domain_resolver").is_none() {
        let prefer = config
            .pointer("/dns/servers")
            .and_then(|s| s.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|s| s.get("tag").and_then(|t| t.as_str()) == Some("dns-proxy"))
                    .map(|_| "dns-proxy".to_string())
            })
            .unwrap_or_else(|| local_tag.to_string());
        if !config.get("route").map(|r| r.is_object()).unwrap_or(false) {
            config["route"] = serde_json::json!({});
        }
        config["route"]["default_domain_resolver"] = serde_json::json!(prefer);
    }
}

/// Inbound sniff fields were removed in sing-box 1.13 — strip them wherever a
/// server-shipped config still carries them (sniffing lives in route rules).
fn strip_legacy_inbound_fields(config: &mut serde_json::Value) {
    let Some(inbounds) = config.get_mut("inbounds").and_then(|i| i.as_array_mut()) else {
        return;
    };
    for inbound in inbounds.iter_mut() {
        if let Some(obj) = inbound.as_object_mut() {
            for f in ["sniff", "sniff_override_destination", "sniff_timeout"] {
                obj.remove(f);
            }
        }
    }
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
pub fn load_cached_tun_config() -> Result<String, ConfigError> {
    let lastgood = tun_config_lastgood_path();
    if lastgood.exists() {
        if let Ok(s) = load_cached_config_for_mode_from(&lastgood) {
            return Ok(s);
        }
    }
    load_cached_config_for_mode_from(&tun_config_file_path())
}

/// Load a previously fetched proxy-mode config for censored control-plane
/// fallback. This is intentionally only used after all Proteus endpoints fail.
pub fn load_cached_proxy_config() -> Result<String, ConfigError> {
    load_cached_config_for_mode_from(&config_file_path())
}

pub fn save_bootstrap_full_config_url(url: Option<&str>) -> Result<(), ConfigError> {
    let path = bootstrap_full_config_url_path();
    match url.map(str::trim).filter(|s| !s.is_empty()) {
        Some(url) => {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err("bootstrap full config URL must be http(s)".into());
            }
            std::fs::write(path, url)?;
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

pub fn load_bootstrap_full_config_url() -> Option<String> {
    let url = std::fs::read_to_string(bootstrap_full_config_url_path()).ok()?;
    let url = url.trim().to_string();
    if url.starts_with("https://") || url.starts_with("http://") {
        Some(url)
    } else {
        None
    }
}

fn load_cached_config_for_mode_from(path: &std::path::Path) -> Result<String, ConfigError> {
    let body = std::fs::read_to_string(path)?;
    let mut v: serde_json::Value = serde_json::from_str(&body)?;
    let has_outbounds = v
        .get("outbounds")
        .and_then(|o| o.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_outbounds {
        return Err("cached config has no outbounds".into());
    }
    // Caches written before the sing-box 1.14 upgrade are still in the legacy
    // format — migrate on load so the offline path survives the upgrade.
    migrate_legacy_singbox_config(&mut v);
    Ok(serde_json::to_string_pretty(&v)?)
}

pub fn wbstream_manifest_file_path() -> PathBuf {
    data_dir().join("wbstream-manifest.json")
}

pub fn telemost_manifest_file_path() -> PathBuf {
    data_dir().join("telemost-manifest.json")
}

pub const WBSTREAM_LOCAL_SOCKS_PORT: u16 = 11080;
pub const WBSTREAM_LOCAL_BALANCER_PORT: u16 = 11079;
pub const WBSTREAM_LOCAL_MULTIPATH_PORT: u16 = 11078;
pub const WBSTREAM_REMOTE_MULTIPATH_PORT: u16 = 19095;
pub const WBSTREAM_MAX_ROOMS: usize = 3;

pub const TELEMOST_LOCAL_SOCKS_PORT: u16 = 11090;
pub const TELEMOST_LOCAL_BALANCER_PORT: u16 = 11097;
pub const TELEMOST_MAX_ROOMS: usize = 3;

/// Fetch config from server, generate working config, cache to disk.
/// Server returns outbounds (proxies). Client wraps them with DNS, routing, inbounds.
pub async fn fetch_and_cache(url: &str) -> Result<String, ConfigError> {
    fetch_and_cache_with_mode(url, InboundMode::Mixed).await
}

pub async fn fetch_and_cache_with_mode(
    url: &str,
    mode: InboundMode,
) -> Result<String, ConfigError> {
    fetch_and_cache_first_available_with_mode(&[url.to_string()], mode).await
}

pub async fn fetch_and_cache_first_available_with_mode(
    urls: &[String],
    mode: InboundMode,
) -> Result<String, ConfigError> {
    let mut errors = Vec::new();
    for url in urls {
        match fetch_and_cache_single_with_mode(url, mode).await {
            Ok(config) => return Ok(config),
            Err(e) => {
                let redacted = redact_config_url_for_error(url);
                let redacted_error = redact_sub_query_in_text(&e.to_string());
                log::warn!("Config fetch failed from {}: {}", redacted, redacted_error);
                errors.push(format!("{} => {}", redacted, redacted_error));
            }
        }
    }
    Err(format!("All config endpoints failed: {}", errors.join("; ")).into())
}

pub async fn fetch_and_cache_first_available_with_mode_via_proxy(
    urls: &[String],
    mode: InboundMode,
    proxy_url: &str,
) -> Result<String, ConfigError> {
    let mut errors = Vec::new();
    for url in urls {
        match fetch_config_body_via_proxy(url, proxy_url).await {
            Ok(body) => return parse_and_cache_config_body(&body, mode).await,
            Err(e) => {
                let redacted = redact_config_url_for_error(url);
                let redacted_error = redact_sub_query_in_text(&e.to_string());
                log::warn!(
                    "Config fetch via proxy failed from {}: {}",
                    redacted,
                    redacted_error
                );
                errors.push(format!("{} => {}", redacted, redacted_error));
            }
        }
    }
    Err(format!("All proxied config endpoints failed: {}", errors.join("; ")).into())
}

async fn fetch_and_cache_single_with_mode(
    url: &str,
    mode: InboundMode,
) -> Result<String, ConfigError> {
    match fetch_config_body(url, false).await {
        Ok(body) => parse_and_cache_config_body(&body, mode).await,
        Err(first_err) => {
            let first_error = first_err.to_string();
            drop(first_err);
            if config_url_supports_dns_pins(url) {
                log::warn!(
                    "Config fetch failed before response; retrying with pinned DNS for {}: {}",
                    redact_config_url_for_error(url),
                    redact_sub_query_in_text(&first_error)
                );
                let body = fetch_config_body(url, true).await.map_err(|pinned_err| {
                    format!(
                        "{}; pinned DNS retry failed: {}",
                        first_error,
                        redact_sub_query_in_text(&pinned_err.to_string())
                    )
                })?;
                parse_and_cache_config_body(&body, mode).await
            } else {
                Err(first_error.into())
            }
        }
    }
}

async fn fetch_config_body(url: &str, pinned_dns: bool) -> Result<String, ConfigError> {
    let client = config_http_client(pinned_dns)?;
    fetch_config_body_with_client(url, client).await
}

async fn fetch_config_body_via_proxy(url: &str, proxy_url: &str) -> Result<String, ConfigError> {
    let client = config_http_client_via_proxy(proxy_url)?;
    fetch_config_body_with_client(url, client).await
}

async fn fetch_config_body_with_client(
    url: &str,
    client: reqwest::Client,
) -> Result<String, ConfigError> {
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

    Ok(resp.text().await?)
}

fn config_http_client(pinned_dns: bool) -> Result<reqwest::Client, ConfigError> {
    let mut builder = reqwest::Client::builder()
        // User-Agent reflects the installed binary's version automatically at
        // compile time. Must never be hard-coded — it drifts and logs become
        // useless. `env!("CARGO_PKG_VERSION")` pulls from Cargo.toml.
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION"), " sing-box"))
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy();

    if pinned_dns {
        for (host, addrs) in CONFIG_DNS_PINS {
            let parsed: Vec<SocketAddr> = addrs
                .iter()
                .filter_map(|addr| addr.parse::<SocketAddr>().ok())
                .collect();
            builder = builder.resolve_to_addrs(host, &parsed);
        }
    }

    Ok(builder.build()?)
}

fn config_http_client_via_proxy(proxy_url: &str) -> Result<reqwest::Client, ConfigError> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION"), " sing-box"))
        .timeout(std::time::Duration::from_secs(20))
        .proxy(reqwest::Proxy::all(proxy_url)?)
        .build()?)
}

fn config_url_supports_dns_pins(url: &str) -> bool {
    CONFIG_DNS_PINS.iter().any(|(host, _)| url.contains(host))
}

async fn parse_and_cache_config_body(body: &str, mode: InboundMode) -> Result<String, ConfigError> {
    let mut server_config: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid config JSON: {}", e))?;
    cache_wbstream_manifest_from_server(&server_config);
    cache_telemost_manifest_from_server(&server_config);
    let _ = prefetch_wbstream_manifest_sidecar().await;
    let _ = prefetch_telemost_manifest_sidecar().await;

    // If the server returns a full sing-box config (has dns + inbounds + route),
    // use it directly — no need to rebuild. This allows the Proteus config server
    // to ship complete configs (with routing rules, Samokat bypass, etc.).
    // Otherwise, treat the response as an outbounds-only payload and wrap it.
    let is_full_config = server_config.get("dns").is_some()
        && server_config.get("inbounds").is_some()
        && server_config.get("route").is_some();

    let mut config = if is_full_config {
        log::info!("Server returned full sing-box config");
        strip_client_side_metadata(&mut server_config);
        migrate_legacy_singbox_config(&mut server_config);
        ensure_location_selector(&mut server_config);
        ensure_absolute_cache_file(&mut server_config);
        enforce_requested_inbound(&mut server_config, mode);
        server_config
    } else {
        build_config_from_server(&server_config, mode)?
    };
    inject_telemost_fallback(&mut config);

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
            if let Err(e) =
                verify_wbstream_manifest_signature_with_pem(&manifest, WBSTREAM_MANIFEST_PUBLIC_PEM)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("manifest signature rejected: {e}"),
                )
                .into());
            }
            let path = wbstream_manifest_file_path();
            let body = serde_json::to_string_pretty(&manifest)?;
            std::fs::write(&path, body)?;
            Ok::<_, ConfigError>(())
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

/// Embedded RS256 public key for WB Stream room manifests (FirstByte signer).
const WBSTREAM_MANIFEST_PUBLIC_PEM: &str = include_str!("../keys/wbstream-manifest.pub.pem");

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

/// Verify RS256 over decoded `payload_b64` and that `payload` matches it.
pub fn verify_wbstream_manifest_signature_with_pem(
    manifest: &serde_json::Value,
    public_pem: &str,
) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rsa::pkcs8::DecodePublicKey;
    use rsa::{Pkcs1v15Sign, RsaPublicKey};
    use sha2::{Digest, Sha256};

    if manifest.get("signature_alg").and_then(|v| v.as_str()) != Some("RS256") {
        return Err("unsupported signature_alg".to_string());
    }
    let payload_b64 = manifest
        .get("payload_b64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "manifest missing payload_b64".to_string())?;
    let signature_b64 = manifest
        .get("signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "manifest missing signature".to_string())?;
    let payload_obj = manifest
        .get("payload")
        .ok_or_else(|| "manifest missing payload".to_string())?;

    let payload_bytes = STANDARD
        .decode(payload_b64)
        .map_err(|e| format!("payload_b64 decode failed: {e}"))?;
    let signature = STANDARD
        .decode(signature_b64)
        .map_err(|e| format!("signature decode failed: {e}"))?;

    let decoded_payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("payload_b64 JSON invalid: {e}"))?;
    if payload_obj != &decoded_payload {
        return Err("payload does not match payload_b64".to_string());
    }

    let public_key = RsaPublicKey::from_public_key_pem(public_pem.trim())
        .map_err(|e| format!("public key parse failed: {e}"))?;
    let digest = Sha256::digest(&payload_bytes);
    public_key
        .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature)
        .map_err(|_| "RS256 signature verification failed".to_string())?;
    Ok(())
}

/// Full fallback gate: shape + RS256 against embedded key + non-expired valid_until.
pub fn verify_wbstream_manifest_for_fallback(manifest: &serde_json::Value) -> Result<(), String> {
    verify_wbstream_manifest_for_fallback_with_pem(manifest, WBSTREAM_MANIFEST_PUBLIC_PEM)
}

fn verify_wbstream_manifest_for_fallback_with_pem(
    manifest: &serde_json::Value,
    public_pem: &str,
) -> Result<(), String> {
    if !is_usable_wbstream_manifest(manifest) {
        return Err("WB Stream manifest shape unusable".to_string());
    }
    verify_wbstream_manifest_signature_with_pem(manifest, public_pem)?;
    let valid_until = manifest
        .get("payload")
        .and_then(|p| p.get("valid_until"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "manifest missing valid_until".to_string())?;
    let until = chrono::DateTime::parse_from_rfc3339(valid_until)
        .map_err(|e| format!("valid_until parse failed: {e}"))?;
    let now = chrono::Utc::now();
    if until.with_timezone(&chrono::Utc) + chrono::Duration::seconds(60) < now {
        return Err(format!("WB Stream manifest expired at {valid_until}"));
    }
    Ok(())
}

fn strip_client_side_metadata(config: &mut serde_json::Value) {
    if let Some(obj) = config.as_object_mut() {
        obj.remove("wbstream_manifest");
        obj.remove("telemost_manifest");
    }
}

/// Server full configs put a urltest group in `route.final` — urltest members
/// cannot be pinned via the Clash API (PUT /proxies/<urltest> → 400). Wrap it
/// in a `proxy` selector so the Home/Proxies location picker works; picking the
/// inner group member stays "Auto". Specialized groups (messenger-auto,
/// telegram-cdn, whitelist-auto…) keep their own urltests and remain
/// pin-independent by design.
fn ensure_location_selector(config: &mut serde_json::Value) {
    let already = config
        .get("outbounds")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter().any(|o| {
                o.get("tag").and_then(|t| t.as_str()) == Some("proxy")
                    && o.get("type").and_then(|t| t.as_str()) == Some("selector")
            })
        })
        .unwrap_or(false);
    if already {
        return;
    }

    let Some(final_tag) = config
        .pointer("/route/final")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string())
    else {
        return;
    };

    // Members of the group route.final points at — only wrap group types.
    let members = config
        .get("outbounds")
        .and_then(|o| o.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|o| {
                    o.get("tag").and_then(|t| t.as_str()) == Some(final_tag.as_str())
                        && matches!(
                            o.get("type").and_then(|t| t.as_str()),
                            Some("urltest") | Some("selector")
                        )
                })
                .and_then(|g| g.get("outbounds").and_then(|m| m.as_array()))
                .cloned()
        });
    let Some(mut members) = members else {
        return; // final already points at a leaf outbound — nothing to wrap
    };
    members.insert(0, serde_json::json!(final_tag.clone()));

    // Whitelist fallback: when every foreign exit is unreachable (RF whitelist
    // mode blocks all non-whitelisted IPs), only the Telemost/Yandex path can
    // still be probed. Nesting `whitelist-auto` into the auto urltest makes
    // failover automatic in both directions — telemost wins only while all
    // foreign members are dead; once a foreign member probes again its lower
    // latency takes the route back. It also joins the `proxy` selector so the
    // user can pin it manually.
    let has_whitelist = config
        .get("outbounds")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter().any(|o| {
                o.get("tag").and_then(|t| t.as_str()) == Some("whitelist-auto")
                    && matches!(
                        o.get("type").and_then(|t| t.as_str()),
                        Some("urltest") | Some("selector")
                    )
            })
        })
        .unwrap_or(false);
    if has_whitelist {
        if let Some(arr) = config.get_mut("outbounds").and_then(|o| o.as_array_mut()) {
            if let Some(auto) = arr.iter_mut().find(|o| {
                o.get("tag").and_then(|t| t.as_str()) == Some(final_tag.as_str())
                    && o.get("type").and_then(|t| t.as_str()) == Some("urltest")
            }) {
                if let Some(list) = auto.get_mut("outbounds").and_then(|m| m.as_array_mut()) {
                    if !list.iter().any(|m| m.as_str() == Some("whitelist-auto")) {
                        list.push(serde_json::json!("whitelist-auto"));
                    }
                }
            }
        }
        members.push(serde_json::json!("whitelist-auto"));
    }

    // Pin-only exits: leaves that must stay OUT of the auto urltest pool (RU
    // residential minis, Telemost leaves, Moscow relay) but remain selectable
    // in the location sheet — e.g. reaching proteus-msk-01 / izhevsk from
    // abroad, or pinning a Telemost path by hand.
    const PIN_ONLY_TAGS: &[&str] = &[
        "msk-via-netcup",
        "msk-via-firstbyte",
        "izhevsk-telemost",
        "firstbyte-tm-telemost",
    ];
    if let Some(arr) = config.get("outbounds").and_then(|o| o.as_array()) {
        for tag in PIN_ONLY_TAGS {
            if arr.iter().any(|o| {
                o.get("tag").and_then(|t| t.as_str()) == Some(*tag)
                    && !matches!(
                        o.get("type").and_then(|t| t.as_str()),
                        Some("urltest") | Some("selector")
                    )
            }) && !members.iter().any(|m| m.as_str() == Some(*tag))
            {
                members.push(serde_json::json!(tag));
            }
        }
    }

    if let Some(arr) = config.get_mut("outbounds").and_then(|o| o.as_array_mut()) {
        arr.push(serde_json::json!({
            "type": "selector",
            "tag": "proxy",
            "outbounds": members,
            "default": final_tag
        }));
    }
    config["route"]["final"] = serde_json::json!("proxy");
}

/// Server configs may carry a relative `cache_file.path` ("cache.db") — the
/// selector pin persistence then depends on the spawned process cwd. Pin it to
/// the app data dir like our generated configs do.
fn ensure_absolute_cache_file(config: &mut serde_json::Value) {
    let cache_path = data_dir().join("cache.db");
    config["experimental"]["cache_file"]["enabled"] = serde_json::json!(true);
    config["experimental"]["cache_file"]["path"] =
        serde_json::json!(cache_path.to_string_lossy());
}

pub fn load_cached_wbstream_manifest() -> Result<serde_json::Value, ConfigError> {
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

const TELEMOST_LINK_PREFIX: &str = "https://telemost.yandex.ru/j/";

fn is_usable_telemost_manifest(manifest: &serde_json::Value) -> bool {
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
                    .map(|url| url.starts_with(TELEMOST_LINK_PREFIX))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn cache_telemost_manifest_from_server(server_config: &serde_json::Value) {
    let Some(manifest) = server_config.get("telemost_manifest") else {
        return;
    };
    if !is_usable_telemost_manifest(manifest) {
        log::warn!("Ignoring unusable Telemost manifest metadata from config server");
        return;
    }
    let path = telemost_manifest_file_path();
    match serde_json::to_string_pretty(manifest)
        .map_err(|e| e.to_string())
        .and_then(|body| std::fs::write(&path, body).map_err(|e| e.to_string()))
    {
        Ok(()) => log::info!("Telemost manifest cached to {}", path.display()),
        Err(e) => log::warn!("Could not cache Telemost manifest: {}", e),
    }
}

pub fn load_cached_telemost_manifest() -> Result<serde_json::Value, ConfigError> {
    let path = telemost_manifest_file_path();
    let body = std::fs::read_to_string(&path)?;
    let manifest: serde_json::Value = serde_json::from_str(&body)?;
    if !is_usable_telemost_manifest(&manifest) {
        return Err("cached Telemost manifest is unusable".into());
    }
    Ok(manifest)
}

/// Full fallback gate for Telemost: shape + RS256 (same embedded trust root as
/// the WB Stream manifest — one signing identity for Proteus whitelist
/// carriers) + non-expired valid_until.
pub fn verify_telemost_manifest_for_fallback(
    manifest: &serde_json::Value,
) -> Result<(), String> {
    if !is_usable_telemost_manifest(manifest) {
        return Err("Telemost manifest shape unusable".to_string());
    }
    verify_wbstream_manifest_signature_with_pem(manifest, WBSTREAM_MANIFEST_PUBLIC_PEM)?;
    let valid_until = manifest
        .get("payload")
        .and_then(|p| p.get("valid_until"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "manifest missing valid_until".to_string())?;
    let until = chrono::DateTime::parse_from_rfc3339(valid_until)
        .map_err(|e| format!("valid_until parse failed: {e}"))?;
    let now = chrono::Utc::now();
    if until.with_timezone(&chrono::Utc) + chrono::Duration::seconds(60) < now {
        return Err(format!("Telemost manifest expired at {valid_until}"));
    }
    Ok(())
}

/// True when a verified Telemost manifest is cached — i.e. the client-side
/// joiner fallback can actually be started right now.
pub fn telemost_fallback_available() -> bool {
    load_cached_telemost_manifest()
        .map(|m| verify_telemost_manifest_for_fallback(&m).is_ok())
        .unwrap_or(false)
}

/// Stable per-install seed used to spread clients across the shared room
/// pool. A Telemost conference carries exactly ONE active joiner — two
/// clients in the same room reset each other's relay bridge on every peer
/// epoch (verified live), so "everyone takes the first N links" degrades the
/// whole pool the moment a second user falls back. The seed is generated
/// once, persisted, and never sent anywhere.
fn telemost_client_seed() -> u64 {
    let path = data_dir().join("telemost-seed");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(seed) = raw.trim().parse::<u64>() {
            return seed;
        }
    }
    let seed: u64 = rand::random();
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(&path, seed.to_string());
    seed
}

fn room_hash(seed: u64, url: &str) -> u64 {
    // FNV-1a over seed || url — deterministic per client, no new deps.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.to_le_bytes().iter().chain(url.as_bytes().iter()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn select_telemost_room_links(
    manifest: &serde_json::Value,
    limit: usize,
) -> Vec<String> {
    let rooms = manifest
        .get("payload")
        .and_then(|payload| payload.get("rooms"))
        .and_then(|rooms| rooms.as_array());
    let Some(rooms) = rooms else {
        return Vec::new();
    };

    let seed = telemost_client_seed();
    let mut candidates: Vec<(u64, String)> = rooms
        .iter()
        .filter_map(|room| {
            let url = room.get("url").and_then(|url| url.as_str())?;
            if !url.starts_with(TELEMOST_LINK_PREFIX) {
                return None;
            }
            Some((room_hash(seed, url), url.to_string()))
        })
        .collect();
    candidates.sort_by_key(|(key, _)| *key);
    candidates
        .into_iter()
        .map(|(_, url)| url)
        .take(limit)
        .collect()
}

/// Inject the client-side Telemost joiner into the generated config: a local
/// SOCKS outbound (`telemost-local`) pointed at the sidecar balancer port is
/// added as a member of the server-provided `whitelist-auto` urltest, and the
/// joiner binary's own signalling/media traffic is pinned to `direct` so TUN
/// mode never loops it back through itself. No-op unless a verified manifest
/// is cached — a forged manifest must never be able to install a carrier.
/// Server-side Telemost relay leaves — they stay in `whitelist-auto` next to
/// the client-side `telemost-local` joiner. Under a hard whitelist their relay
/// IPs are unreachable and the joiner path wins; in degraded modes they are a
/// useful extra member.
const TELEMOST_RELAY_TAGS: &[&str] = &["izhevsk-telemost", "firstbyte-tm-telemost"];

fn inject_telemost_fallback(config: &mut serde_json::Value) {
    if !telemost_fallback_available() {
        return;
    }
    let Some(outbounds) = config.get_mut("outbounds").and_then(|o| o.as_array_mut()) else {
        return;
    };
    if !outbounds.iter().any(|o| {
        o.get("tag").and_then(|t| t.as_str()) == Some("telemost-local")
    }) {
        outbounds.push(serde_json::json!({
            "type": "socks",
            "tag": "telemost-local",
            "server": "127.0.0.1",
            "server_port": TELEMOST_LOCAL_BALANCER_PORT,
            "version": "5"
        }));
    }

    // Collect whitelist members: telemost-local + any server-provided
    // Telemost relay leaves present in the payload.
    let mut wl_members: Vec<String> = vec!["telemost-local".to_string()];
    for tag in TELEMOST_RELAY_TAGS {
        if outbounds.iter().any(|o| {
            o.get("tag").and_then(|t| t.as_str()) == Some(*tag)
                && !matches!(
                    o.get("type").and_then(|t| t.as_str()),
                    Some("urltest") | Some("selector")
                )
        }) {
            wl_members.push((*tag).to_string());
        }
    }

    let mut group_exists = false;
    for o in outbounds.iter_mut() {
        if o.get("tag").and_then(|t| t.as_str()) == Some("whitelist-auto")
            && matches!(
                o.get("type").and_then(|t| t.as_str()),
                Some("urltest") | Some("selector")
            )
        {
            group_exists = true;
            if let Some(list) = o.get_mut("outbounds").and_then(|m| m.as_array_mut()) {
                for member in &wl_members {
                    if !list.iter().any(|m| m.as_str() == Some(member.as_str())) {
                        list.push(serde_json::json!(member));
                    }
                }
            }
        }
    }
    if !group_exists {
        // Outbounds-only (Lumen subscription) payloads carry no groups — the
        // group must be synthesized here or the fallback can never engage.
        outbounds.push(serde_json::json!({
            "type": "urltest",
            "tag": "whitelist-auto",
            "outbounds": wl_members,
            "url": "https://www.cloudflare.com/cdn-cgi/trace",
            "interval": "15s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));
    }

    // Nest whitelist-auto into the Auto urltest (failover in both directions)
    // and into the `proxy` selector (manual pin). Full server configs already
    // do this inside ensure_location_selector; wrapped payloads need it here.
    for o in outbounds.iter_mut() {
        let tag = o.get("tag").and_then(|t| t.as_str()).unwrap_or("");
        let is_group = matches!(
            o.get("type").and_then(|t| t.as_str()),
            Some("urltest") | Some("selector")
        );
        if (tag == "proxy-auto" || tag == "proxy") && is_group {
            if let Some(list) = o.get_mut("outbounds").and_then(|m| m.as_array_mut()) {
                if !list.iter().any(|m| m.as_str() == Some("whitelist-auto")) {
                    list.push(serde_json::json!("whitelist-auto"));
                }
            }
        }
    }
    if let Some(rules) = config
        .get_mut("route")
        .and_then(|r| r.get_mut("rules"))
        .and_then(|r| r.as_array_mut())
    {
        rules.insert(
            0,
            serde_json::json!({"process_name": ["headless-telemost-joiner"], "outbound": "direct"}),
        );
        rules.insert(
            1,
            serde_json::json!({
                "domain_suffix": ["telemost.yandex.ru", ".strm.yandex.net", "yastatic.net"],
                "outbound": "direct"
            }),
        );
    }
}

pub fn save_wbstream_fallback_config(
    mode: InboundMode,
    local_socks_port: u16,
) -> Result<String, ConfigError> {
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

    let mut config = serde_json::json!({
        "log": {"level": "info", "timestamp": true},
        "dns": {
            "servers": [
                {
                    "tag": "dns-proxy",
                    "type": "https",
                    "server": "1.1.1.1",
                    "detour": "wbstream-local"
                },
                {
                    "tag": "dns-direct",
                    "type": "https",
                    "server": "77.88.8.8",
                    "detour": "direct"
                },
                // 1.14 requires an explicit domain_resolver on dial fields;
                // "local" is the system resolver — zero dependency on exits.
                {"tag": "local", "type": "local"}
            ],
            "rules": [
                {"domain_suffix": wb_domains.clone(), "server": "dns-direct"}
            ],
            "final": "dns-proxy",
            "strategy": "ipv4_only"
        },
        "inbounds": match mode {
            InboundMode::Mixed => mixed_inbounds(),
            InboundMode::Tun => tun_inbounds(),
        },
        "outbounds": [
            {
                "type": "socks",
                "tag": "wbstream-local",
                "server": "127.0.0.1",
                "server_port": local_socks_port,
                "version": "5"
            },
            {"type": "direct", "tag": "direct", "domain_resolver": "local"},
            {"type": "block", "tag": "block"}
        ],
        "route": {
            "rules": [
                {"domain_suffix": wb_domains.clone(), "outbound": "direct"},
                {"ip_cidr": wb_endpoint_cidrs.clone(), "outbound": "direct"}
            ],
            "final": "wbstream-local",
            "auto_detect_interface": true,
            "default_domain_resolver": "dns-proxy"
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
    if matches!(mode, InboundMode::Tun) {
        ensure_dns_hijack_route_rule(&mut config);
    }
    ensure_sniff_route_rule(&mut config);
    config
}

const WBSTREAM_MANIFEST_PREFETCH_URLS: &[&str] =
    &["https://config.getlumen.download/wbstream-manifest.json"];

/// Telemost manifest lives on Yandex Object Storage — reachable from RF under
/// whitelist mode, so prefetch stays possible even when the config gateway is
/// cut. Verified RS256 before it is ever written to cache.
const TELEMOST_MANIFEST_PREFETCH_URLS: &[&str] = &[
    "https://storage.yandexcloud.net/proteus-sub-hwai-9f2/telemost-manifest.json",
];

/// Best-effort Telemost manifest refresh. Unlike the WB prefetch this one is
/// also safe to call while whitelist mode is active — the bucket is
/// whitelisted — but the fallback path itself still uses the cache only.
pub async fn prefetch_telemost_manifest_sidecar() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "Lumen/",
            env!("CARGO_PKG_VERSION"),
            " telemost-prefetch"
        ))
        .timeout(std::time::Duration::from_secs(6))
        .no_proxy()
        .build()
        .map_err(|e| format!("Could not build Telemost prefetch client: {}", e))?;

    let mut last_error = None;
    for url in TELEMOST_MANIFEST_PREFETCH_URLS {
        let result = async {
            let resp = client
                .get(*url)
                .send()
                .await
                .map_err(|e| format!("fetch: {}", e))?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status()));
            }
            let manifest: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("manifest JSON: {}", e))?;
            if !is_usable_telemost_manifest(&manifest) {
                return Err("unusable manifest shape".to_string());
            }
            verify_wbstream_manifest_signature_with_pem(
                &manifest,
                WBSTREAM_MANIFEST_PUBLIC_PEM,
            )
            .map_err(|e| format!("manifest signature rejected: {}", e))?;
            let path = telemost_manifest_file_path();
            let body = serde_json::to_string_pretty(&manifest)
                .map_err(|e| e.to_string())?;
            std::fs::write(&path, body).map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        }
        .await;

        match result {
            Ok(()) => {
                log::info!("Telemost manifest prefetched from {}", url);
                return Ok(());
            }
            Err(e) => {
                log::warn!("Telemost manifest prefetch from {} failed: {}", url, e);
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "no Telemost manifest URLs".to_string()))
}

/// Build a single-outbound sing-box config from a parsed VLESS link.
/// Used when user supplies a raw vless:// URI instead of a Proteus subscription.
///
/// IMPORTANT: the outbound tag must NOT collide with reserved tags used by the
/// wrapping config — specifically "proxy" (selector), "proxy-auto" (urltest),
/// "direct", "block", or route targets — otherwise sing-box rejects the config
/// with a duplicate-tag error and the Clash API returns empty, which surfaces
/// in the UI as an empty Proxies list.
pub fn build_config_from_vless(
    vless: &crate::vless::VlessConfig,
    mode: InboundMode,
) -> Result<serde_json::Value, ConfigError> {
    let tag = outbound_tag_from_name(&vless.name, "vless-out");
    let outbound = crate::vless::to_singbox_outbound(vless, &tag);
    // Wrap as a "server response" with single outbound and reuse the existing builder
    let pseudo_server = serde_json::json!({ "outbounds": [outbound] });
    build_config_from_server(&pseudo_server, mode)
}

/// Build a single-outbound sing-box config from a parsed Hysteria2 link.
/// Same wrapper as `build_config_from_vless` — reserved-tag rules apply.
pub fn build_config_from_hy2(
    hy2: &crate::hy2::Hy2Config,
    mode: InboundMode,
) -> Result<serde_json::Value, ConfigError> {
    let tag = outbound_tag_from_name(&hy2.name, "hy2-out");
    let outbound = crate::hy2::to_singbox_outbound(hy2, &tag);
    let pseudo_server = serde_json::json!({ "outbounds": [outbound] });
    build_config_from_server(&pseudo_server, mode)
}

/// Produce a sing-box outbound tag from a user-supplied link fragment/name.
///
/// Rules:
/// - ASCII alphanumerics, dash, underscore kept; everything else collapsed to `-`
/// - Leading/trailing `-` trimmed; multiple `-` collapsed to one
/// - Empty / reserved names fall back to the caller-provided `fallback`
/// - Result is lowercased to keep Clash API names consistent
fn outbound_tag_from_name(raw_name: &str, fallback: &str) -> String {
    const RESERVED: &[&str] = &[
        "proxy",
        "proxy-auto",
        "proxy-tg",
        "proxy-yt",
        "direct",
        "block",
        "dns-out",
        "dns-in",
        "tun-in",
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
        return fallback.to_string();
    }
    out
}

/// Save VLESS-derived config to disk (same path as Proteus configs).
pub async fn save_vless_config(
    vless: &crate::vless::VlessConfig,
    mode: InboundMode,
) -> Result<String, ConfigError> {
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

/// Save HY2-derived config to disk (same path as Proteus configs).
pub async fn save_hy2_config(
    hy2: &crate::hy2::Hy2Config,
    mode: InboundMode,
) -> Result<String, ConfigError> {
    let cfg = build_config_from_hy2(hy2, mode)?;
    let final_json = serde_json::to_string_pretty(&cfg)?;
    let path = match mode {
        InboundMode::Mixed => config_file_path(),
        InboundMode::Tun => tun_config_file_path(),
    };
    std::fs::write(&path, &final_json)?;
    log::info!("HY2 config ({:?}) saved to {}", mode, path.display());
    Ok(final_json)
}

/// Manual geo-picker leaves (Hiddify-like). Order is UI order.
/// USA pin is FirstByte-only — never hostodo-via-timeweb here.
/// Germany must avoid the :443 stream path: field reports showed that pin can
/// pass probes while carrying no traffic on RF networks.
// 2026-09-15: relay-eu-grpc / proxy-moscow / firstbyte-moscow-reality removed —
// their Timeweb :36743 :36745 and FirstByte :36743 inbounds are decommissioned
// (measured closed). relay-eu-httpupgrade (:36744) is the live Timeweb relay.
const GEO_SELECTOR_TAGS: &[&str] = &[
    "hostodo-via-firstbyte",
    "relay-eu-httpupgrade",
    "dubai-residential",
    "izhevsk-via-firstbyte",
    "izhevsk-via-netcup",
    "msk-via-netcup",
    "msk-via-firstbyte",
];

/// Leaves that may appear in the selector for explicit pin, but must never
/// join Auto urltest (true RF exits / residentials, or field-broken relays).
/// They remain available for manual diagnostics/pins without poisoning Auto.
const AUTO_EXCLUDED_GEO_TAGS: &[&str] = &[
    "relay-eu-443",
    "relay-eu-httpupgrade",
    "dubai-residential",
    "izhevsk-via-firstbyte",
    "izhevsk-via-netcup",
    "msk-via-netcup",
    "msk-via-firstbyte",
    "hostodo-via-timeweb",
];

/// Prefer Hostodo-via-FirstByte ahead of Hostodo-via-Timeweb inside Auto urltest.
/// Only swaps those two relative positions; other member order is unchanged.
fn prioritize_hostodo_firstbyte(members: &[String]) -> Vec<String> {
    let mut out = members.to_vec();
    let fb = out.iter().position(|m| m == "hostodo-via-firstbyte");
    let tw = out.iter().position(|m| m == "hostodo-via-timeweb");
    if let (Some(i_fb), Some(i_tw)) = (fb, tw) {
        if i_fb > i_tw {
            out.swap(i_fb, i_tw);
        }
    }
    out
}

fn geo_selector_members(available: &[String]) -> Vec<String> {
    GEO_SELECTOR_TAGS
        .iter()
        .filter(|tag| available.iter().any(|a| a == *tag))
        .map(|s| (*s).to_string())
        .collect()
}

/// Build sing-box config from server-provided outbounds.
/// Server is responsible for all proxy outbounds (IPs, keys, transport).
/// Client adds: DNS, inbounds, route rules, selector+urltest groups, direct/block.
///
/// Shape (Hiddify-safe autorouting):
///   selector `proxy` (default=`proxy-auto`) → [proxy-auto, …geo leaves]
///   urltest  `proxy-auto` → vision-safe Auto members (today's probe semantics)
///   urltest  `proxy-tg` / `proxy-yt` → independent service probes (unchanged)
fn build_config_from_server(
    server: &serde_json::Value,
    mode: InboundMode,
) -> Result<serde_json::Value, ConfigError> {
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
    let service_names_raw: Vec<String> = if service_proxy_names.is_empty() {
        proxy_names.clone()
    } else {
        service_proxy_names
    };
    // Drop manual-only geo exits from Auto (moscow / residential). Keep them
    // available on the selector for explicit pin.
    let service_names_filtered: Vec<String> = service_names_raw
        .into_iter()
        .filter(|tag| !AUTO_EXCLUDED_GEO_TAGS.contains(&tag.as_str()))
        .collect();
    let service_names_for_auto = if service_names_filtered.is_empty() {
        // Degenerate payload: only manual-only leaves — fall back so Auto is
        // non-empty (same spirit as the all-vision fallback).
        proxy_names.clone()
    } else {
        service_names_filtered
    };
    // FirstByte-before-Timeweb for Hostodo USA paths inside Auto.
    let service_names = prioritize_hostodo_firstbyte(&service_names_for_auto);
    let geo_members = geo_selector_members(&proxy_names);
    let mut selector_members = vec!["proxy-auto".to_string()];
    selector_members.extend(geo_members);

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
                // DoH to Cloudflare — direct, NOT tunnelled through the VPN.
                // The firstbyte-relay-httpupgrade exit cannot carry DoH to
                // 1.1.1.1/8.8.8.8 (log shows context deadline exceeded), so
                // detour via proxy breaks all DNS. Traffic still goes through
                // the VPN; only the lookup is direct. The local network sees
                // that Cloudflare DoH was queried, not which domains.
                {
                    "tag": "dns-proxy",
                    "type": "https",
                    "server": "1.1.1.1",
                    "detour": "direct"
                },
                // Direct fallback — never depends on a VPN exit. sing-box picks
                // dns-direct for RU domains (see rules) and falls back to it
                // when the proxied resolver is unreachable.
                {
                    "tag": "dns-direct",
                    "type": "https",
                    "server": "77.88.8.8",
                    "detour": "direct"
                },
                // 1.14 requires an explicit domain_resolver on dial fields;
                // "local" is the system resolver — zero dependency on exits.
                {"tag": "local", "type": "local"}
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
            InboundMode::Mixed => mixed_inbounds(),
            // Do NOT override destination with the sniffed domain. DNS
            // resolution already goes through dns-proxy, and override=true
            // breaks applications that connect to an IP with a decoy SNI.
            InboundMode::Tun => tun_inbounds(),
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
            "auto_detect_interface": true,
            "default_domain_resolver": "dns-proxy"
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
        // Outer selector — sticky manual pin via Clash API. Default stays
        // proxy-auto so unpinned behaviour matches today's urltest Auto.
        // route.final + dns-proxy.detour bind to this tag ("proxy").
        arr.push(serde_json::json!({
            "type": "selector",
            "tag": "proxy",
            "outbounds": selector_members,
            "default": "proxy-auto"
        }));

        // Auto URLTest — same probe semantics as the former tag=proxy urltest.
        // tolerance=200: don't flap between exits for small latency differences.
        // v2.6.3: interval 60s→15s — TUN mode depends on this group for DNS
        // (dns-proxy detours through proxy). A dead exit must be replaced fast,
        // not after 60 seconds of ERR_NAME_NOT_RESOLVED in the browser.
        // interrupt_exist_connections=false — probe switches must not tear down
        // active calls/streams (v2.5.2 / RF field reports).
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-auto",
            "outbounds": service_names.clone(),
            "url": "https://www.cloudflare.com/cdn-cgi/trace",
            "interval": "15s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));
        // Destination-specific URLTest groups — independent of the selector pin
        // so Telegram/YouTube keep their own health probes.
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-tg",
            "outbounds": service_names.clone(),
            "url": "https://web.telegram.org/",
            "interval": "60s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));
        arr.push(serde_json::json!({
            "type": "urltest",
            "tag": "proxy-yt",
            "outbounds": service_names.clone(),
            "url": "https://www.youtube.com/generate_204",
            "interval": "60s",
            "tolerance": 200,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false
        }));

        // Server-provided proxy outbounds.
        for o in outbounds {
            arr.push(o.clone());
        }

        // Standard outbounds. domain_resolver is required on the direct dial
        // field in 1.14 — a bare `direct` is rejected when a DNS server detours
        // to it ("detour to an empty direct outbound makes no sense").
        arr.push(serde_json::json!({"type": "direct", "tag": "direct", "domain_resolver": "local"}));
        arr.push(serde_json::json!({"type": "block", "tag": "block"}));
    }

    if matches!(mode, InboundMode::Tun) {
        ensure_dns_hijack_route_rule(&mut config);
    }
    ensure_sniff_route_rule(&mut config);

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

pub fn _load_cached() -> Result<String, ConfigError> {
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
    fn windows_tun_policy_uses_windows_safe_route_contract() {
        let policy = tun_policy_for_target("windows");
        assert_eq!(policy.interface_name, "Lumen");
        assert_eq!(policy.mtu, 1500);
        assert!(policy.strict_route, "Windows TUN must prevent DNS leaks");
        assert_eq!(
            policy.stack, "gvisor",
            "Windows must use the netstack the working Windows clients ship"
        );
    }

    #[test]
    fn macos_tun_policy_preserves_existing_route_contract() {
        let policy = tun_policy_for_target("macos");
        assert_eq!(policy.interface_name, "utun777");
        assert_eq!(policy.mtu, 9000);
        assert!(!policy.strict_route);
        assert_eq!(policy.stack, "mixed");
    }

    #[test]
    fn requested_tun_mode_replaces_full_server_config_inbound() {
        let mut config = serde_json::json!({
            "dns": {},
            "inbounds": [{"type": "mixed", "listen_port": 10808}],
            "route": {},
            "outbounds": [{"type": "direct", "tag": "direct"}]
        });
        enforce_requested_inbound(&mut config, InboundMode::Tun);
        let inbound = &config["inbounds"][0];
        assert_eq!(inbound["type"], "tun");
        assert_eq!(inbound["auto_route"], true);
    }

    #[test]
    fn requested_mixed_mode_replaces_full_server_config_inbound() {
        let mut config = serde_json::json!({
            "dns": {},
            "inbounds": [{"type": "tun", "auto_route": true}],
            "route": {"rules": [{"protocol": "dns", "action": "hijack-dns"}]},
            "outbounds": [{"type": "direct", "tag": "direct"}]
        });
        enforce_requested_inbound(&mut config, InboundMode::Mixed);
        let inbound = &config["inbounds"][0];
        assert_eq!(inbound["type"], "mixed");
        assert_eq!(inbound["tag"], "mixed-in");
        assert_eq!(inbound["listen"], "127.0.0.1");
        assert_eq!(inbound["listen_port"], 10808);
    }

    #[test]
    fn vless_tag_is_never_reserved() {
        for name in &["proxy", "direct", "block", "PROXY", "Direct", ""] {
            let t = outbound_tag_from_name(name, "vless-out");
            assert!(!t.is_empty(), "empty tag for {:?}", name);
            assert_ne!(t, "proxy", "tag clashes with urltest group for {:?}", name);
            assert_ne!(t, "direct");
            assert_ne!(t, "block");
        }
    }

    #[test]
    fn vless_tag_sanitizes_common_fragments() {
        assert_eq!(outbound_tag_from_name("user-1", "vless-out"), "user-1");
        assert_eq!(outbound_tag_from_name("Server-Name-01", "vless-out"), "server-name-01");
        assert_eq!(outbound_tag_from_name("Canada Toronto", "vless-out"), "canada-toronto");
        assert_eq!(outbound_tag_from_name("🚀 fast", "vless-out"), "fast");
        assert_eq!(outbound_tag_from_name("---", "vless-out"), "vless-out");
        assert_eq!(outbound_tag_from_name("", "vless-out"), "vless-out");
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
        // Required tags: selector proxy + three URLTest groups + direct/block + VLESS.
        for required in &[
            "proxy",
            "proxy-auto",
            "proxy-tg",
            "proxy-yt",
            "direct",
            "block",
        ] {
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

        let proxy = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy"))
            .expect("proxy selector");
        assert_eq!(
            proxy.get("type").and_then(|t| t.as_str()),
            Some("selector"),
            "proxy must be a selector wrapping proxy-auto"
        );
        assert_eq!(
            proxy.get("default").and_then(|t| t.as_str()),
            Some("proxy-auto")
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
                "proxy-auto".to_string(),
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

    /// TUN must hijack client DNS into sing-box's DNS module. Without this,
    /// macOS browsers keep using poisoned/local ISP resolvers
    /// (DNS_PROBE_FINISHED_NXDOMAIN on LinkedIn) while System Proxy works.
    /// See notes/lumen_ekaterina_macos_system_proxy_working_tun_dns_fail_2026-07-31.md.
    #[test]
    fn tun_route_hijacks_dns_to_singbox_module() {
        let raw = "vless://00000000-0000-4000-8000-000000000005@192.0.2.50:443?type=tcp&security=reality&pbk=EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE&fp=chrome&sni=google.com&sid=feedface&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        let cfg = build_config_from_vless(&v, InboundMode::Tun).expect("build");
        let rules = cfg["route"]["rules"].as_array().expect("route.rules");
        // Rule 0 is the sniff action (replaces inbound sniff fields removed in
        // sing-box 1.13); rule 1 must hijack DNS into the sing-box DNS module.
        assert_eq!(
            rules
                .first()
                .and_then(|r| r.get("action"))
                .and_then(|a| a.as_str()),
            Some("sniff"),
            "first route rule must sniff (replaces removed inbound sniff fields)"
        );
        let hijack = rules
            .iter()
            .find(|r| r.get("action").and_then(|a| a.as_str()) == Some("hijack-dns"))
            .expect("route rule must hijack DNS into sing-box DNS module");
        assert_eq!(
            hijack.get("protocol").and_then(|p| p.as_str()),
            Some("dns"),
            "hijack rule must match DNS protocol"
        );

        // Full-config path (server returns dns+inbounds+route) must still
        // receive the hijack when the client swaps in TUN inbound.
        let mut full = serde_json::json!({
            "dns": {"final": "dns-proxy"},
            "inbounds": [{"type": "mixed", "listen_port": 10808}],
            "route": {
                "rules": [
                    {"domain_suffix": [".example.com"], "outbound": "direct"}
                ],
                "final": "direct"
            },
            "outbounds": [{"type": "direct", "tag": "direct"}]
        });
        enforce_requested_inbound(&mut full, InboundMode::Tun);
        let full_rules = full["route"]["rules"].as_array().expect("full route.rules");
        assert!(
            full_rules
                .iter()
                .any(|r| r.get("action").and_then(|a| a.as_str()) == Some("sniff")),
            "full-config path must gain the sniff route rule"
        );
        assert!(
            full_rules
                .iter()
                .any(|r| r.get("action").and_then(|a| a.as_str()) == Some("hijack-dns")),
            "full-config path must gain the hijack-dns route rule"
        );
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
        // Auto group must re-probe frequently — TUN DNS goes through this group,
        // so a dead exit must be replaced quickly, not after a minute.
        let proxy_auto = outbounds
            .iter()
            .find(|o| {
                o.get("type").and_then(|t| t.as_str()) == Some("urltest")
                    && o.get("tag").and_then(|t| t.as_str()) == Some("proxy-auto")
            })
            .expect("proxy-auto urltest must exist");
        assert_eq!(
            proxy_auto.get("interval").and_then(|v| v.as_str()),
            Some("15s"),
            "proxy-auto interval must be 15s for TUN DNS recovery"
        );
        // and is what the v2.2.4 bug-probe happened to be using.
        let default_probe = by_tag.get("proxy-auto").cloned().unwrap_or_default();
        assert!(
            !default_probe.contains("gstatic.com"),
            "proxy-auto should not probe gstatic (regional-block risk), got {:?}",
            default_probe
        );
        assert!(
            !default_probe.is_empty(),
            "proxy-auto group has no probe URL"
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
        // Inbound sniff fields were removed in sing-box 1.13 — sniffing is a
        // route-rule action now, and must not override the destination.
        assert!(
            tun.get("sniff").is_none() && tun.get("sniff_override_destination").is_none(),
            "inbound sniff fields are removed in sing-box 1.13 — use route action"
        );
        let rules = cfg
            .pointer("/route/rules")
            .and_then(|r| r.as_array())
            .expect("route.rules");
        let sniff = rules
            .iter()
            .find(|r| r.get("action").and_then(|a| a.as_str()) == Some("sniff"))
            .expect("sniff route rule must exist so SNI/Host can be extracted");
        assert_ne!(
            sniff.get("override_destination").and_then(|v| v.as_bool()),
            Some(true),
            "sniff override_destination must be OFF — DNS goes through dns-proxy \
             (correct IPs), and override breaks Telegram anti-censorship (fake SNI)"
        );
    }

    /// sing-box 1.14 removed two legacy constructs our generated configs used:
    /// inbound `sniff` fields (removed 1.13) and flat `address`-style DNS
    /// servers (removed 1.14). Guard both formats so a regression fails the
    /// test suite before it reaches a user's `sing-box check`.
    #[test]
    fn generated_config_uses_singbox_114_formats() {
        let raw = "vless://00000000-0000-4000-8000-000000000007@192.0.2.70:443?type=tcp&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F&flow=xtls-rprx-vision#u";
        let v = crate::vless::parse_vless(raw).expect("parse");
        for mode in [InboundMode::Mixed, InboundMode::Tun] {
            let cfg = build_config_from_vless(&v, mode).expect("build");
            for server in cfg
                .pointer("/dns/servers")
                .and_then(|s| s.as_array())
                .expect("dns.servers")
            {
                assert!(
                    server.get("type").is_some(),
                    "{mode:?}: dns server must use typed 1.12+ format: {server:?}"
                );
                assert!(
                    server.get("address").is_none(),
                    "{mode:?}: legacy `address` DNS format removed in sing-box 1.14: {server:?}"
                );
            }
            for inbound in cfg
                .get("inbounds")
                .and_then(|i| i.as_array())
                .expect("inbounds")
            {
                assert!(
                    inbound.get("sniff").is_none(),
                    "{mode:?}: inbound sniff removed in sing-box 1.13: {inbound:?}"
                );
            }
            let rules = cfg
                .pointer("/route/rules")
                .and_then(|r| r.as_array())
                .expect("route.rules");
            assert!(
                rules
                    .iter()
                    .any(|r| r.get("action").and_then(|a| a.as_str()) == Some("sniff")),
                "{mode:?}: sniff must live as a route rule action"
            );
            // 1.14 runtime gate: a bare `direct` outbound is rejected when a
            // DNS server detours to it — dial fields need domain_resolver.
            for ob in cfg
                .get("outbounds")
                .and_then(|o| o.as_array())
                .expect("outbounds")
            {
                if ob.get("type").and_then(|t| t.as_str()) == Some("direct") {
                    assert!(
                        ob.get("domain_resolver").is_some(),
                        "{mode:?}: direct outbound needs domain_resolver in 1.14: {ob:?}"
                    );
                }
            }
            assert!(
                cfg.pointer("/route/default_domain_resolver").is_some(),
                "{mode:?}: route.default_domain_resolver required in 1.14"
            );
        }
    }

    /// The real gate: generated configs must be accepted by the bundled
    /// sing-box binary, not just have the right shape. Field removals land in
    /// the kernel faster than our tests can predict — this catches the drift.
    /// Skips silently when bin/sing-box is absent (CI checkout).
    #[test]
    fn generated_configs_pass_bundled_singbox_check() {
        let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .join("bin/sing-box");
        if !bin.exists() {
            eprintln!("bin/sing-box not present — skipping bundled kernel check");
            return;
        }

        let vless_raw = "vless://00000000-0000-4000-8000-000000000007@192.0.2.70:443?type=tcp&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F&flow=xtls-rprx-vision#u";
        let vless = crate::vless::parse_vless(vless_raw).expect("parse vless");
        let hy2 = crate::hy2::parse_hy2(
            "hy2://pw@192.0.2.9:36757?insecure=1&obfs=gecko&obfs-password=pw2#t",
        )
        .expect("parse hy2");

        for mode in [InboundMode::Mixed, InboundMode::Tun] {
            for (kind, cfg) in [
                ("vless", build_config_from_vless(&vless, mode).expect("vless cfg")),
                ("hy2", build_config_from_hy2(&hy2, mode).expect("hy2 cfg")),
            ] {
                let path = std::env::temp_dir().join(format!("lumen-check-{kind}-{mode:?}.json"));
                std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
                let out = std::process::Command::new(&bin)
                    .args(["check", "-c"])
                    .arg(&path)
                    .output()
                    .expect("run sing-box check");
                assert!(
                    out.status.success(),
                    "{kind} {mode:?} config rejected by bundled sing-box:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    /// sing-box 1.14 migration: the Proteus config server still ships the
    /// legacy flat-address DNS layout. `migrate_legacy_singbox_config` must
    /// translate it without losing routing semantics — this fixture mirrors
    /// the real server response shape (sanitized).
    #[test]
    fn migrate_legacy_full_config_translates_proteus_layout() {
        let mut cfg = serde_json::json!({
            "dns": {
                "servers": [
                    {"tag": "dns-proxy", "address": "https://dns.cloudflare.com/dns-query", "address_resolver": "dns-direct", "detour": "proxy-auto"},
                    {"tag": "dns-ru", "address": "77.88.8.8", "detour": "proxy-auto"},
                    {"tag": "dns-direct", "address": "8.8.8.8", "detour": "direct"},
                    {"tag": "dns-block", "address": "rcode://success"}
                ],
                "rules": [
                    {"domain": ["api.oneme.ru"], "server": "dns-block"},
                    {"domain_suffix": ["youtube.com"], "server": "dns-proxy"},
                    {"rule_set": ["geosite-ru"], "server": "dns-ru"}
                ],
                "strategy": "ipv4_only"
            },
            "inbounds": [
                {"type": "tun", "sniff": true, "sniff_override_destination": false}
            ],
            "outbounds": [
                {"type": "urltest", "tag": "proxy-auto", "outbounds": ["exit-a"]},
                {"type": "urltest", "tag": "whitelist-auto", "outbounds": ["tm-a"]},
                {"type": "vless", "tag": "exit-a", "server": "192.0.2.1", "server_port": 443},
                {"type": "vless", "tag": "tm-a", "server": "192.0.2.2", "server_port": 443},
                {"type": "vless", "tag": "msk-via-netcup", "server": "192.0.2.3", "server_port": 443},
                {"type": "direct", "tag": "direct"},
                {"type": "block", "tag": "block"}
            ],
            "route": {
                "rules": [
                    {"domain_suffix": [], "outbound": "tm-a"},
                    {"rule_set": ["geoip-ru"], "outbound": "direct"}
                ],
                "final": "proxy-auto"
            }
        });
        migrate_legacy_singbox_config(&mut cfg);
        ensure_location_selector(&mut cfg);
        ensure_absolute_cache_file(&mut cfg);
        enforce_requested_inbound(&mut cfg, InboundMode::Mixed);

        let servers = cfg.pointer("/dns/servers").unwrap().as_array().unwrap();
        for s in servers {
            assert!(s.get("address").is_none(), "legacy address left: {s:?}");
            assert!(s.get("address_resolver").is_none(), "address_resolver left: {s:?}");
            assert!(s.get("type").is_some(), "missing type: {s:?}");
        }
        // rcode://success server folded away.
        assert!(servers.iter().all(|s| s.get("tag") != Some(&serde_json::json!("dns-block"))));
        // Typed conversions.
        let by_tag = |t: &str| servers.iter().find(|s| s.get("tag") == Some(&serde_json::json!(t))).cloned().unwrap();
        assert_eq!(by_tag("dns-proxy")["type"], "https");
        assert_eq!(by_tag("dns-proxy")["server"], "dns.cloudflare.com");
        assert_eq!(by_tag("dns-proxy")["domain_resolver"], "dns-direct");
        assert_eq!(by_tag("dns-ru")["type"], "udp");
        assert_eq!(by_tag("dns-ru")["server"], "77.88.8.8");
        assert_eq!(by_tag("dns-direct")["server"], "8.8.8.8");
        assert_eq!(by_tag("local")["type"], "local");
        // dns-block rule became a predefined action.
        let rules = cfg.pointer("/dns/rules").unwrap().as_array().unwrap();
        assert_eq!(rules[0]["action"], "predefined");
        assert_eq!(rules[0]["rcode"], "NOERROR");
        assert!(rules[0].get("server").is_none());
        // Non-rcode rules keep `server` routing.
        assert_eq!(rules[1]["server"], "dns-proxy");
        // Dial resolvers: direct -> local, default -> dns-proxy (proxied DoH,
        // so RU DNS poisoning can't kill urltest probes).
        let direct = cfg.pointer("/outbounds").unwrap().as_array().unwrap().iter()
            .find(|o| o.get("tag") == Some(&serde_json::json!("direct"))).unwrap();
        assert_eq!(direct["domain_resolver"], "local");
        assert_eq!(cfg.pointer("/route/default_domain_resolver").unwrap(), "dns-proxy");
        // Condition-less rules (empty domain_suffix) are dropped — they would
        // match ALL traffic and silently misroute everything. The sniff
        // action rule (inbound sniff fields were removed in 1.13) survives.
        let route_rules = cfg.pointer("/route/rules").unwrap().as_array().unwrap();
        assert_eq!(route_rules.len(), 2, "empty-condition rule must be dropped");
        assert_eq!(route_rules[0]["action"], "sniff");
        assert_eq!(route_rules[1]["outbound"], "direct");
        // route.final wrapped in a `proxy` selector so Clash API can pin exits.
        assert_eq!(cfg.pointer("/route/final").unwrap(), "proxy");
        let sel = cfg.pointer("/outbounds").unwrap().as_array().unwrap().iter()
            .find(|o| o.get("tag") == Some(&serde_json::json!("proxy"))).unwrap();
        assert_eq!(sel["type"], "selector");
        assert_eq!(sel["default"], "proxy-auto");
        assert_eq!(
            sel["outbounds"].as_array().unwrap()[0],
            serde_json::json!("proxy-auto")
        );
        assert!(sel["outbounds"].as_array().unwrap()
            .iter().any(|m| m == &serde_json::json!("exit-a")));
        // Whitelist fallback: whitelist-auto is pinned into the selector for
        // manual choice AND nested into proxy-auto so urltest auto-fails over
        // to the Telemost path when all foreign exits are unreachable.
        assert!(sel["outbounds"].as_array().unwrap()
            .iter().any(|m| m == &serde_json::json!("whitelist-auto")));
        let auto = cfg.pointer("/outbounds").unwrap().as_array().unwrap().iter()
            .find(|o| o.get("tag") == Some(&serde_json::json!("proxy-auto"))).unwrap();
        assert_eq!(auto["outbounds"].as_array().unwrap().last().unwrap(),
            &serde_json::json!("whitelist-auto"));
        // Pin-only leaves (RU minis, Telemost leaves, Moscow relay) join the
        // selector for manual pinning but never the auto urltest pool.
        assert!(sel["outbounds"].as_array().unwrap()
            .iter().any(|m| m == &serde_json::json!("msk-via-netcup")));
        assert!(!auto["outbounds"].as_array().unwrap()
            .iter().any(|m| m == &serde_json::json!("msk-via-netcup")));
        // Idempotent — a second pass must not double-wrap.
        let once = cfg.clone();
        migrate_legacy_singbox_config(&mut cfg);
        ensure_location_selector(&mut cfg);
        assert_eq!(once, cfg);
    }

    /// `ensure_location_selector` leaves configs whose route.final already
    /// points at a leaf outbound untouched, and never double-wraps.
    #[test]
    fn location_selector_wraps_only_urltest_final() {
        let mut leaf = serde_json::json!({
            "outbounds": [{"type": "vless", "tag": "exit-a"}],
            "route": {"final": "exit-a"}
        });
        ensure_location_selector(&mut leaf);
        assert_eq!(leaf.pointer("/route/final").unwrap(), "exit-a");
        assert_eq!(leaf.pointer("/outbounds").unwrap().as_array().unwrap().len(), 1);
    }

    /// Dev harness: migrates a real fetched Proteus config placed at
    /// /tmp/proteus-full.json into /tmp/proteus-mig-{mixed,tun}.json for
    /// manual `sing-box check`/`run` validation. Skips when absent.
    #[test]
    fn migrate_live_proteus_fixture() {
        let Ok(body) = std::fs::read_to_string("/tmp/proteus-full.json") else {
            return;
        };
        let mut cfg: serde_json::Value = serde_json::from_str(&body).unwrap();
        strip_client_side_metadata(&mut cfg);
        migrate_legacy_singbox_config(&mut cfg);
        ensure_location_selector(&mut cfg);
        ensure_absolute_cache_file(&mut cfg);
        for mode in [InboundMode::Mixed, InboundMode::Tun] {
            let mut m = cfg.clone();
            enforce_requested_inbound(&mut m, mode);
            let name = format!("proteus-mig-{}", if matches!(mode, InboundMode::Tun) { "tun" } else { "mixed" });
            std::fs::write(
                format!("/tmp/{name}.json"),
                serde_json::to_string_pretty(&m).unwrap(),
            )
            .unwrap();
        }
    }

    /// v2.3.4 regression: Auto urltest `proxy-auto` must ALSO exclude
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
        // + one gRPC Reality exit (no xtls flow). Auto `proxy-auto` must
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
        let proxy_auto = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy-auto"))
            .expect("proxy-auto urltest group");
        let members: Vec<&str> = proxy_auto
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !members.contains(&"tcp-reality-1"),
            "Auto `proxy-auto` must NOT include TCP Reality (xtls-rprx-vision) \
             when safer exits exist. Got members: {:?}. This protects RU users from \
             TSPU bulk-block on long streams (persistent WebSocket sessions, \
             voice/media apps, large downloads).",
            members
        );
        assert!(
            members.contains(&"grpc-reality-1"),
            "Auto `proxy-auto` must include the safe gRPC Reality exit, \
             got members: {:?}",
            members
        );
    }

    #[test]
    fn general_proxy_group_excludes_field_broken_relay_members_when_alternatives_exist() {
        let server = serde_json::json!({
            "outbounds": [
                {
                    "type": "vless",
                    "tag": "relay-eu-httpupgrade",
                    "server": "192.0.2.10",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000001"
                },
                {
                    "type": "vless",
                    "tag": "hostodo-via-timeweb",
                    "server": "192.0.2.20",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000002"
                },
                {
                    "type": "vless",
                    "tag": "hostodo-via-firstbyte",
                    "server": "192.0.2.30",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000003"
                },
                {
                    "type": "vless",
                    "tag": "relay-eu-grpc",
                    "server": "192.0.2.40",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000004"
                }
            ]
        });
        let cfg = build_config_from_server(&server, InboundMode::Mixed).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        let proxy_auto = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy-auto"))
            .expect("proxy-auto urltest group");
        let members: Vec<&str> = proxy_auto
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !members.contains(&"relay-eu-httpupgrade"),
            "Auto must not choose relay-eu-httpupgrade when safer exits exist"
        );
        assert!(
            !members.contains(&"hostodo-via-timeweb"),
            "Auto must not choose hostodo-via-timeweb when safer exits exist"
        );
        assert!(members.contains(&"hostodo-via-firstbyte"));
        assert!(members.contains(&"relay-eu-grpc"));
    }

    /// v2.3.4: when ALL exits are TCP Reality (non-RF user, single Reality
    /// origin), Auto `proxy-auto` MUST fall back to including them.
    /// An empty urltest group is worse than a TSPU-vulnerable one — it breaks
    /// the UI entirely (App.tsx binds "Auto Select" to proxy / proxy-auto).
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
        let proxy_auto = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy-auto"))
            .expect("proxy-auto urltest group");
        let members: Vec<&str> = proxy_auto
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            members,
            vec!["only-tcp-reality"],
            "Auto `proxy-auto` must fall back to the TCP Reality exit when \
             no safer alternatives exist (non-RF user with single Reality origin)"
        );
    }

    /// Geo picker: outer `proxy` is a selector defaulting to `proxy-auto`.
    /// Manual USA pin is FirstByte-only; Vision residentials are selectable
    /// but never members of Auto urltest when safer exits exist.
    #[test]
    fn proxy_selector_wraps_auto_and_geo_leaves() {
        let server = serde_json::json!({
            "outbounds": [
                {
                    "type": "vless",
                    "tag": "hostodo-via-timeweb",
                    "server": "192.0.2.40",
                    "server_port": 36748,
                    "uuid": "00000000-0000-4000-8000-000000000010"
                },
                {
                    "type": "vless",
                    "tag": "hostodo-via-firstbyte",
                    "server": "192.0.2.41",
                    "server_port": 36748,
                    "uuid": "00000000-0000-4000-8000-000000000011"
                },
                {
                    "type": "vless",
                    "tag": "relay-eu-443",
                    "server": "192.0.2.42",
                    "server_port": 443,
                    "uuid": "00000000-0000-4000-8000-000000000012"
                },
                {
                    "type": "vless",
                    "tag": "relay-eu-httpupgrade",
                    "server": "192.0.2.43",
                    "server_port": 36744,
                    "uuid": "00000000-0000-4000-8000-000000000017"
                },
                {
                    "type": "vless",
                    "tag": "dubai-residential",
                    "server": "192.0.2.44",
                    "server_port": 36754,
                    "uuid": "00000000-0000-4000-8000-000000000013",
                    "flow": "xtls-rprx-vision"
                },
                {
                    "type": "vless",
                    "tag": "izhevsk-via-firstbyte",
                    "server": "192.0.2.45",
                    "server_port": 36755,
                    "uuid": "00000000-0000-4000-8000-000000000014",
                    "flow": "xtls-rprx-vision"
                },
                {
                    "type": "vless",
                    "tag": "msk-via-netcup",
                    "server": "192.0.2.46",
                    "server_port": 36758,
                    "uuid": "00000000-0000-4000-8000-000000000015",
                    "flow": "xtls-rprx-vision"
                },
                {
                    "type": "vless",
                    "tag": "msk-via-firstbyte",
                    "server": "192.0.2.47",
                    "server_port": 36758,
                    "uuid": "00000000-0000-4000-8000-000000000016"
                }
            ]
        });
        let cfg = build_config_from_server(&server, InboundMode::Tun).expect("build");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();

        let proxy = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy"))
            .expect("proxy selector");
        assert_eq!(proxy.get("type").and_then(|t| t.as_str()), Some("selector"));
        assert_eq!(
            proxy.get("default").and_then(|t| t.as_str()),
            Some("proxy-auto")
        );
        let sel: Vec<&str> = proxy
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            sel,
            vec![
                "proxy-auto",
                "hostodo-via-firstbyte",
                "relay-eu-httpupgrade",
                "dubai-residential",
                "izhevsk-via-firstbyte",
                "msk-via-netcup",
                "msk-via-firstbyte",
            ],
            "selector members must be Auto + geo pins; USA pin is FirstByte only \
             and Germany pin avoids relay-eu-443. Got {:?}",
            sel
        );

        let proxy_auto = outbounds
            .iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("proxy-auto"))
            .expect("proxy-auto");
        let auto_members: Vec<&str> = proxy_auto
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !auto_members.contains(&"relay-eu-443"),
            "Auto must not include relay-eu-443; field probes showed it can \
             pass selection but timeout general web/OpenAI traffic"
        );
        assert!(
            !auto_members.contains(&"dubai-residential"),
            "Auto must not include Vision residential dubai"
        );
        assert!(
            !auto_members.contains(&"izhevsk-via-firstbyte"),
            "Auto must not include Vision residential izhevsk"
        );
        assert!(
            !auto_members.contains(&"msk-via-netcup"),
            "Auto must not include Moscow residential (manual-only geo)"
        );
        assert!(
            !auto_members.contains(&"msk-via-firstbyte"),
            "Auto must not include Moscow residential (manual-only geo)"
        );
        assert!(
            auto_members.contains(&"hostodo-via-firstbyte"),
            "Auto must include Hostodo FirstByte when available, got {:?}",
            auto_members
        );
        assert!(
            !auto_members.contains(&"hostodo-via-timeweb"),
            "Auto must not include Hostodo Timeweb after live route health failed, got {:?}",
            auto_members
        );

        // TG/YT stay independent urltests (not redirected to selector pin).
        for tag in ["proxy-tg", "proxy-yt"] {
            let g = outbounds
                .iter()
                .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some(tag))
                .unwrap_or_else(|| panic!("missing {tag}"));
            assert_eq!(g.get("type").and_then(|t| t.as_str()), Some("urltest"));
        }
    }

    #[test]
    fn prioritize_hostodo_firstbyte_swaps_only_those_two() {
        let input = vec![
            "relay-eu-443".into(),
            "hostodo-via-timeweb".into(),
            "firstbyte-relay-httpupgrade".into(),
            "hostodo-via-firstbyte".into(),
        ];
        assert_eq!(
            prioritize_hostodo_firstbyte(&input),
            vec![
                "relay-eu-443".to_string(),
                "hostodo-via-firstbyte".to_string(),
                "firstbyte-relay-httpupgrade".to_string(),
                "hostodo-via-timeweb".to_string(),
            ]
        );
    }

    /// Opt-in live proof: `LUMEN_LIVE_OUTBOUNDS_PATH=/tmp/lumen-live.json cargo test …`
    /// where the JSON is `{ "outbounds": [ …vless leaves… ] }` from the config gateway.
    #[test]
    fn live_worker_payload_builds_safe_autoroute_shape() {
        let path = match std::env::var("LUMEN_LIVE_OUTBOUNDS_PATH") {
            Ok(p) if !p.is_empty() => p,
            _ => {
                eprintln!("skip: set LUMEN_LIVE_OUTBOUNDS_PATH for live worker proof");
                return;
            }
        };
        let raw = std::fs::read_to_string(&path).expect("read live outbounds");
        let server: serde_json::Value = serde_json::from_str(&raw).expect("parse live json");
        let cfg = build_config_from_server(&server, InboundMode::Tun).expect("build live");
        let outbounds = cfg.get("outbounds").and_then(|o| o.as_array()).unwrap();
        let by_tag = |tag: &str| {
            outbounds
                .iter()
                .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some(tag))
                .unwrap_or_else(|| panic!("missing {tag}"))
        };

        let proxy = by_tag("proxy");
        assert_eq!(proxy.get("type").and_then(|t| t.as_str()), Some("selector"));
        assert_eq!(
            proxy.get("default").and_then(|t| t.as_str()),
            Some("proxy-auto")
        );
        let sel: Vec<&str> = proxy
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(sel.first().copied(), Some("proxy-auto"));
        assert!(
            sel.contains(&"hostodo-via-firstbyte"),
            "USA FirstByte pin must be selectable: {:?}",
            sel
        );
        assert!(
            !sel.contains(&"hostodo-via-timeweb"),
            "USA Timeweb must NOT be a Home pin member: {:?}",
            sel
        );

        let auto = by_tag("proxy-auto");
        assert_eq!(auto.get("type").and_then(|t| t.as_str()), Some("urltest"));
        assert_eq!(
            auto.get("interrupt_exist_connections")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        let auto_members: Vec<&str> = auto
            .get("outbounds")
            .and_then(|o| o.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for banned in [
            "relay-eu-httpupgrade",
            "dubai-residential",
            "izhevsk-via-firstbyte",
            "firstbyte-moscow-reality",
            "hostodo-via-timeweb",
            "proxy-moscow",
        ] {
            assert!(
                !auto_members.contains(&banned),
                "Auto leaked manual-only geo {banned}: {:?}",
                auto_members
            );
        }
        for tag in ["proxy-tg", "proxy-yt"] {
            let g = by_tag(tag);
            assert_eq!(g.get("type").and_then(|t| t.as_str()), Some("urltest"));
            assert_eq!(
                g.get("interrupt_exist_connections")
                    .and_then(|v| v.as_bool()),
                Some(false)
            );
        }
    }

    /// v2.5.2: URLTest groups must not force-teardown active flows on probe
    /// switches. Field reports from RF users map to the 15-30s/60s probe
    /// cadence: sessions work briefly, then calls/streams reconnect. Stale
    /// exit recovery must be handled by bounded probes/idle recovery, not by
    /// killing every active flow on each selected-outbound change.
    #[test]
    fn urltest_groups_do_not_interrupt_existing_connections_on_switch() {
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
                Some(false),
                "urltest group {:?} must not interrupt active connections on \
                 probe-driven switches; this can kill calls/streams at the \
                 same cadence as URLTest probes.",
                tag
            );
        }
    }

    #[test]
    fn proteus_config_urls_include_backend_fallback() {
        let urls = proteus_config_urls("test-sub-key");
        assert_eq!(urls.len(), 2);
        assert_eq!(
            urls[0],
            "https://config.getlumen.download/proteus-sub?sub=test-sub-key&format=json-text"
        );
        assert_eq!(
            urls[1],
            "https://primary-production-1d1cf.up.railway.app/webhook/proteus-sub?sub=test-sub-key&format=json-text"
        );
    }

    #[test]
    fn config_url_errors_redact_sub_key() {
        let url = "https://config.getlumen.download/proteus-sub?sub=test-sub-key&format=json-text";
        assert_eq!(
            redact_config_url_for_error(url),
            "https://config.getlumen.download/proteus-sub?sub=<redacted>&format=json-text"
        );
        let err = "error sending request for url (https://config.getlumen.download/proteus-sub?sub=test-sub-key&format=json-text)";
        assert_eq!(
            redact_sub_query_in_text(err),
            "error sending request for url (https://config.getlumen.download/proteus-sub?sub=<redacted>&format=json-text)"
        );
    }

    #[test]
    fn config_dns_pins_cover_cf_control_plane_hosts() {
        assert!(config_url_supports_dns_pins(
            "https://config.getlumen.download/proteus-sub?sub=x"
        ));
        assert!(!config_url_supports_dns_pins(
            "https://primary-production-1d1cf.up.railway.app/webhook/proteus-sub?sub=x"
        ));
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

    fn sign_wbstream_manifest_for_test(
        payload: serde_json::Value,
        private_key: &rsa::RsaPrivateKey,
    ) -> serde_json::Value {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use rsa::Pkcs1v15Sign;
        use sha2::{Digest, Sha256};

        let payload_bytes = serde_json::to_vec(&payload).expect("payload json");
        let digest = Sha256::digest(&payload_bytes);
        let signature = private_key
            .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
            .expect("sign");
        serde_json::json!({
            "payload": payload,
            "payload_b64": STANDARD.encode(&payload_bytes),
            "signature_alg": "RS256",
            "signature": STANDARD.encode(signature),
        })
    }

    #[test]
    fn wbstream_manifest_rejects_forged_shape_only_signature() {
        let forged = serde_json::json!({
            "payload": {
                "kind": "proteus-wbstream-room-manifest",
                "valid_until": "2099-01-01T00:00:00Z",
                "rooms": [
                    { "role": "active", "priority": 0, "url": "wbstream://forged" }
                ]
            },
            "payload_b64": "e30=",
            "signature_alg": "RS256",
            "signature": "sig"
        });
        assert!(is_usable_wbstream_manifest(&forged));
        let err = verify_wbstream_manifest_for_fallback(&forged).unwrap_err();
        assert!(
            err.contains("signature")
                || err.contains("decode")
                || err.contains("match")
                || err.contains("payload"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn wbstream_manifest_accepts_valid_rs256_and_rejects_wrong_key() {
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{RsaPrivateKey, RsaPublicKey};

        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("keygen");
        let public_pem = RsaPublicKey::from(&private_key)
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .expect("pem");

        let until = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let payload = serde_json::json!({
            "kind": "proteus-wbstream-room-manifest",
            "key_id": "test",
            "valid_until": until,
            "rooms": [
                { "role": "active", "priority": 0, "url": "wbstream://test-room" }
            ]
        });
        let manifest = sign_wbstream_manifest_for_test(payload, &private_key);
        assert!(is_usable_wbstream_manifest(&manifest));
        verify_wbstream_manifest_signature_with_pem(&manifest, &public_pem).expect("verify ok");
        assert!(
            verify_wbstream_manifest_signature_with_pem(&manifest, WBSTREAM_MANIFEST_PUBLIC_PEM)
                .is_err(),
            "ephemeral-signed manifest must fail against production public key"
        );
    }

    #[test]
    fn wbstream_manifest_fallback_rejects_expired_valid_until() {
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{RsaPrivateKey, RsaPublicKey};

        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("keygen");
        let public_pem = RsaPublicKey::from(&private_key)
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .expect("pem");

        let payload = serde_json::json!({
            "kind": "proteus-wbstream-room-manifest",
            "key_id": "test",
            "valid_until": "2020-01-01T00:00:00Z",
            "rooms": [
                { "role": "active", "priority": 0, "url": "wbstream://expired" }
            ]
        });
        let manifest = sign_wbstream_manifest_for_test(payload, &private_key);
        let err =
            verify_wbstream_manifest_for_fallback_with_pem(&manifest, &public_pem).unwrap_err();
        assert!(
            err.contains("expired"),
            "expected expiry rejection, got: {err}"
        );
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
        assert!(
            load_cached_config_for_mode_from(&good).is_ok(),
            "valid config with outbounds must load"
        );

        let empty = dir.join("empty.json");
        std::fs::write(&empty, r#"{"outbounds":[]}"#).unwrap();
        assert!(
            load_cached_config_for_mode_from(&empty).is_err(),
            "empty outbounds must be rejected"
        );

        let garbage = dir.join("garbage.json");
        std::fs::write(&garbage, "not json").unwrap();
        assert!(
            load_cached_config_for_mode_from(&garbage).is_err(),
            "invalid json must be rejected"
        );

        let missing = dir.join("missing.json");
        assert!(
            load_cached_config_for_mode_from(&missing).is_err(),
            "missing file must be rejected"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_cached_config_for_mode_from_validates_proxy_cache() {
        let dir = std::env::temp_dir().join(format!("lumen-proxy-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let good = dir.join("proxy-good.json");
        std::fs::write(
            &good,
            r#"{"outbounds":[{"type":"vless","tag":"proxy-auto"}]}"#,
        )
        .unwrap();
        assert!(
            load_cached_config_for_mode_from(&good).is_ok(),
            "proxy mode must be able to reuse a valid cached config when control-plane endpoints are blocked"
        );

        let empty = dir.join("proxy-empty.json");
        std::fs::write(&empty, r#"{"outbounds":[]}"#).unwrap();
        assert!(
            load_cached_config_for_mode_from(&empty).is_err(),
            "empty proxy cache must be rejected"
        );

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
        "dns": { "servers": [
            { "type": "udp", "server": "1.1.1.1" },
            { "tag": "local", "type": "local" }
        ] },
        "inbounds": [{
            "type": "mixed",
            "tag": "bootstrap-socks",
            "listen": "127.0.0.1",
            "listen_port": port
        }],
        "outbounds": [ exit, { "type": "direct", "tag": "direct", "domain_resolver": "local" } ],
        // No dns-proxy here — bootstrap dials only IP-based exits.
        "route": { "final": tag, "default_domain_resolver": "local" }
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
        assert_eq!(
            e.get("tag").and_then(|t| t.as_str()),
            Some("netcup-tcp-reality")
        );
    }

    #[test]
    fn extract_bootstrap_exit_falls_back_to_any_reality() {
        let cfg = serde_json::json!({"outbounds": [
            {"type":"vless","tag":"relay-eu-1","tls":{"reality":{}},"transport":{"type":"grpc"}}
        ]});
        assert!(
            extract_bootstrap_exit(&cfg).is_some(),
            "any reality exit is acceptable fallback"
        );
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
