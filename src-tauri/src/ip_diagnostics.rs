//! Exit-IP diagnostics for Settings → Refresh.
//!
//! Bug class (2026-08-02): `reqwest` with `.no_proxy()` bypasses System Proxy, so
//! Refresh showed the underlay ISP (UAE IPv6) while the browser (honouring the
//! proxy) correctly showed the Hostodo USA exit. TUN mode was fine because the
//! kernel path still captures sockets.
//!
//! Strategy:
//! - connected-proxy → probe via local mixed inbound `127.0.0.1:10808`
//! - connected-tun / disconnected → direct socket (TUN still captures when up)
//! - prefer IPv4-literal / IPv4-first endpoints so Happy Eyeballs cannot prefer
//!   a leaked native IPv6 underlay when the proxy path is IPv4-only
//! - cascade several JSON/trace APIs; RF often blocks fancy HTML checkers
//!   (whoer/showmyip) but still allows Cloudflare trace + ipify + ipwho.is

use serde_json::Value;
use std::net::Ipv4Addr;
use std::time::Duration;

pub const LOCAL_MIXED_PROXY: &str = "http://127.0.0.1:10808";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsRoute {
    /// Honour Lumen's local mixed/HTTP proxy (System Proxy mode).
    ThroughLocalProxy,
    /// No HTTP proxy env — used for TUN (kernel captures) and disconnected.
    DirectSocket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpSnapshot {
    pub ip: String,
    pub region: Option<String>,
    pub country: Option<String>,
    pub asn_org: Option<String>,
    pub source: &'static str,
}

impl DiagnosticsRoute {
    pub fn from_effective_status(status: &str) -> Self {
        if status == "connected-proxy" {
            Self::ThroughLocalProxy
        } else {
            // connected-tun / connected-wbstream / disconnected
            Self::DirectSocket
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ThroughLocalProxy => "local-proxy",
            Self::DirectSocket => "direct-socket",
        }
    }
}

pub fn parse_cloudflare_trace(body: &str) -> Option<IpSnapshot> {
    let mut ip = None;
    let mut loc = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("ip=") {
            ip = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("loc=") {
            loc = Some(rest.trim().to_string());
        }
    }
    let ip = ip.filter(|s| !s.is_empty())?;
    let country = loc
        .as_deref()
        .filter(|c| *c != "XX" && !c.is_empty())
        .map(country_name_from_iso);
    Some(IpSnapshot {
        ip,
        region: None,
        country,
        asn_org: None,
        source: "cloudflare-trace",
    })
}

pub fn parse_ipwho_json(value: &Value) -> Option<IpSnapshot> {
    if value.get("success").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    let ip = value.get("ip")?.as_str()?.trim().to_string();
    if ip.is_empty() {
        return None;
    }
    let country = value
        .get("country")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let region = value
        .get("region")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let asn_org = value
        .get("connection")
        .and_then(|c| c.get("org"))
        .and_then(|v| v.as_str())
        .or_else(|| value.get("org").and_then(|v| v.as_str()))
        .or_else(|| value.get("isp").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Some(IpSnapshot {
        ip,
        region,
        country,
        asn_org,
        source: "ipwho.is",
    })
}

pub fn parse_ipify_json(value: &Value) -> Option<String> {
    value
        .get("ip")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

pub fn parse_myip_json(value: &Value) -> Option<IpSnapshot> {
    let ip = value.get("ip")?.as_str()?.trim().to_string();
    if ip.is_empty() {
        return None;
    }
    let country = value
        .get("country")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Some(IpSnapshot {
        ip,
        region: None,
        country,
        asn_org: None,
        source: "api.myip.com",
    })
}

pub fn parse_ifconfig_co_json(value: &Value) -> Option<IpSnapshot> {
    let ip = value.get("ip")?.as_str()?.trim().to_string();
    if ip.is_empty() {
        return None;
    }
    Some(IpSnapshot {
        ip,
        region: value
            .get("region_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        country: value
            .get("country")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        asn_org: value
            .get("asn_org")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        source: "ifconfig.co",
    })
}

/// Small ISO-3166-1 alpha-2 → English name map for Cloudflare `loc=`.
pub fn country_name_from_iso(code: &str) -> String {
    match code.to_ascii_uppercase().as_str() {
        "US" => "United States".into(),
        "DE" => "Germany".into(),
        "AE" => "United Arab Emirates".into(),
        "RU" => "Russia".into(),
        "NL" => "Netherlands".into(),
        "GB" | "UK" => "United Kingdom".into(),
        "FR" => "France".into(),
        "FI" => "Finland".into(),
        "SE" => "Sweden".into(),
        "PL" => "Poland".into(),
        "TR" => "Turkey".into(),
        "KZ" => "Kazakhstan".into(),
        other => other.to_string(),
    }
}

fn build_client(route: DiagnosticsRoute) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION"), " diagnostics"))
        // Prefer IPv4 so we do not Happy-Eyeball onto native IPv6 underlay
        // while the VPN exit is IPv4-only (Hostodo / Reality relays).
        .local_address(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    builder = match route {
        DiagnosticsRoute::ThroughLocalProxy => builder.proxy(
            reqwest::Proxy::all(LOCAL_MIXED_PROXY)
                .map_err(|e| format!("diagnostics proxy: {e}"))?,
        ),
        DiagnosticsRoute::DirectSocket => builder.no_proxy(),
    };

    builder
        .build()
        .map_err(|e| format!("diagnostics client: {e}"))
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url}: status {}", response.status()));
    }
    response
        .text()
        .await
        .map_err(|e| format!("{url}: body {e}"))
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url}: status {}", response.status()));
    }
    response
        .json::<Value>()
        .await
        .map_err(|e| format!("{url}: json {e}"))
}

/// Ordered probes: RF-resilient first (CF IPv4 literal + ipify), then richer geo.
pub async fn fetch_external_ip_snapshot(route: DiagnosticsRoute) -> Result<IpSnapshot, String> {
    let client = build_client(route)?;
    let mut errors: Vec<String> = Vec::new();

    // 1) Cloudflare over IPv4 literal — works in RF more often than named hosts,
    //    and cannot Dual-Stack onto native IPv6.
    match get_text(&client, "https://1.1.1.1/cdn-cgi/trace").await {
        Ok(body) => {
            if let Some(snap) = parse_cloudflare_trace(&body) {
                return Ok(enrich_if_needed(client, snap).await);
            }
            errors.push("cloudflare-trace: missing ip=".into());
        }
        Err(e) => errors.push(e),
    }

    // 2) ipwho.is — IP + region/country/org in one JSON response
    match get_json(&client, "https://ipwho.is/").await {
        Ok(value) => {
            if let Some(snap) = parse_ipwho_json(&value) {
                return Ok(snap);
            }
            errors.push("ipwho.is: unusable payload".into());
        }
        Err(e) => errors.push(e),
    }

    // 3) ipify IPv4 + optional enrich
    match get_json(&client, "https://api.ipify.org?format=json").await {
        Ok(value) => {
            if let Some(ip) = parse_ipify_json(&value) {
                let snap = IpSnapshot {
                    ip,
                    region: None,
                    country: None,
                    asn_org: None,
                    source: "ipify",
                };
                return Ok(enrich_if_needed(client, snap).await);
            }
            errors.push("ipify: missing ip".into());
        }
        Err(e) => errors.push(e),
    }

    // 4) api.myip.com
    match get_json(&client, "https://api.myip.com").await {
        Ok(value) => {
            if let Some(snap) = parse_myip_json(&value) {
                return Ok(snap);
            }
            errors.push("api.myip.com: unusable payload".into());
        }
        Err(e) => errors.push(e),
    }

    // 5) Named Cloudflare host (non-literal) + legacy ifconfig.co
    match get_text(&client, "https://www.cloudflare.com/cdn-cgi/trace").await {
        Ok(body) => {
            if let Some(snap) = parse_cloudflare_trace(&body) {
                return Ok(enrich_if_needed(client, snap).await);
            }
            errors.push("cloudflare-named: missing ip=".into());
        }
        Err(e) => errors.push(e),
    }

    match get_json(&client, "https://ifconfig.co/json").await {
        Ok(value) => {
            if let Some(snap) = parse_ifconfig_co_json(&value) {
                return Ok(snap);
            }
            errors.push("ifconfig.co: unusable payload".into());
        }
        Err(e) => errors.push(e),
    }

    Err(format!(
        "external ip: all probes failed via {} ({})",
        route.label(),
        errors.join("; ")
    ))
}

async fn enrich_if_needed(client: reqwest::Client, mut snap: IpSnapshot) -> IpSnapshot {
    if snap.country.is_some() && snap.region.is_some() && snap.asn_org.is_some() {
        return snap;
    }
    let url = format!("https://ipwho.is/{}", snap.ip);
    if let Ok(value) = get_json(&client, &url).await {
        if let Some(rich) = parse_ipwho_json(&value) {
            if snap.country.is_none() {
                snap.country = rich.country;
            }
            if snap.region.is_none() {
                snap.region = rich.region;
            }
            if snap.asn_org.is_none() {
                snap.asn_org = rich.asn_org;
            }
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn route_uses_local_proxy_only_for_system_proxy_mode() {
        assert_eq!(
            DiagnosticsRoute::from_effective_status("connected-proxy"),
            DiagnosticsRoute::ThroughLocalProxy
        );
        assert_eq!(
            DiagnosticsRoute::from_effective_status("connected-tun"),
            DiagnosticsRoute::DirectSocket
        );
        assert_eq!(
            DiagnosticsRoute::from_effective_status("disconnected"),
            DiagnosticsRoute::DirectSocket
        );
    }

    #[test]
    fn parse_cloudflare_trace_reads_ip_and_country() {
        let body = "fl=1\nh=1.1.1.1\nip=203.0.113.77\nloc=US\n";
        let snap = parse_cloudflare_trace(body).expect("snap");
        assert_eq!(snap.ip, "203.0.113.77");
        assert_eq!(snap.country.as_deref(), Some("United States"));
        assert_eq!(snap.source, "cloudflare-trace");
    }

    #[test]
    fn parse_cloudflare_trace_rejects_empty() {
        assert!(parse_cloudflare_trace("loc=US\n").is_none());
    }

    #[test]
    fn parse_ipwho_json_reads_geo_and_org() {
        let value = json!({
            "ip": "203.0.113.77",
            "success": true,
            "country": "United States",
            "region": "Michigan",
            "connection": { "org": "Hostodo" }
        });
        let snap = parse_ipwho_json(&value).expect("snap");
        assert_eq!(snap.ip, "203.0.113.77");
        assert_eq!(snap.region.as_deref(), Some("Michigan"));
        assert_eq!(snap.country.as_deref(), Some("United States"));
        assert_eq!(snap.asn_org.as_deref(), Some("Hostodo"));
    }

    #[test]
    fn parse_ifconfig_co_keeps_legacy_shape() {
        let value = json!({
            "ip": "2a00:f2a::1",
            "country": "United Arab Emirates",
            "region_name": "Dubai",
            "asn_org": "Emirates Integrated Telecommunications Company PJSC"
        });
        let snap = parse_ifconfig_co_json(&value).expect("snap");
        assert_eq!(snap.country.as_deref(), Some("United Arab Emirates"));
        assert_eq!(snap.region.as_deref(), Some("Dubai"));
    }

    #[test]
    fn parse_ipify_and_myip() {
        assert_eq!(
            parse_ipify_json(&json!({"ip": "1.2.3.4"})).as_deref(),
            Some("1.2.3.4")
        );
        let snap =
            parse_myip_json(&json!({"ip":"1.2.3.4","country":"Germany","cc":"DE"})).expect("myip");
        assert_eq!(snap.country.as_deref(), Some("Germany"));
    }
}
