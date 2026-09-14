/// Hysteria2 link parser.
///
/// Format:
///   hy2://PASSWORD@HOST:PORT?sni=NAME&insecure=1&obfs=gecko&obfs-password=PW#NAME
///   hysteria2://... (alias)
///
/// `hy2://` is not a registered URL scheme — we rewrite to `https://` for `url` crate
/// parsing, then extract original semantic fields.
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone)]
pub struct Hy2Config {
    /// Authentication password (userinfo part before `@`)
    pub password: String,
    pub host: String,
    pub port: u16,
    /// Display name (URL-decoded fragment, or hostname fallback)
    pub name: String,
    /// TLS server name (SNI)
    pub sni: Option<String>,
    /// Skip TLS certificate verification (self-signed server certs)
    pub insecure: bool,
    /// Obfuscation type: "salamander" | "gecko" (gecko needs sing-box >= 1.14)
    pub obfs: Option<String>,
    /// Obfuscation password (required when obfs is set)
    pub obfs_password: Option<String>,
    /// Optional bandwidth hints in Mbps (0 = unset)
    pub up_mbps: u32,
    pub down_mbps: u32,
}

pub fn parse_hy2(raw: &str) -> Result<Hy2Config, String> {
    let s = raw.trim();
    if !s.starts_with("hy2://") && !s.starts_with("hysteria2://") {
        return Err("Not a hy2:// link".to_string());
    }

    let normalized = s
        .replacen("hysteria2://", "https://", 1)
        .replacen("hy2://", "https://", 1);
    let url = Url::parse(&normalized).map_err(|e| format!("Bad HY2 URI: {}", e))?;

    // Password lives in the userinfo. Reconstruct `user:pass` if both parts are
    // present so share links that embed a colon keep working.
    let password = match url.password() {
        Some(p) => format!("{}:{}", url.username(), p),
        None => url.username().to_string(),
    };
    if password.is_empty() {
        return Err("Missing password in HY2 link".to_string());
    }

    let host = url
        .host_str()
        .ok_or("Missing host in HY2 link")?
        .to_string();

    // Same default-port collapse as the VLESS parser: explicit `:443` after the
    // https rewrite must still read as 443, and a missing port falls back to 443.
    let port = url
        .port_or_known_default()
        .ok_or("Missing port in HY2 link")?;

    let params: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let sni = params.get("sni").cloned().filter(|s| !s.is_empty());

    let insecure = matches!(
        params.get("insecure").map(String::as_str),
        Some("1") | Some("true")
    );

    let obfs = params.get("obfs").cloned().filter(|s| !s.is_empty());
    if let Some(o) = &obfs {
        if o != "salamander" && o != "gecko" {
            return Err(format!("Unsupported obfs type: {}", o));
        }
    }
    let obfs_password = params
        .get("obfs-password")
        .cloned()
        .filter(|s| !s.is_empty());
    if obfs.is_some() && obfs_password.is_none() {
        return Err("obfs requires 'obfs-password' parameter".to_string());
    }

    let parse_mbps = |key: &str| -> Result<u32, String> {
        match params.get(key) {
            Some(v) => v
                .parse::<u32>()
                .map_err(|_| format!("Invalid {} value: {}", key, v)),
            None => Ok(0),
        }
    };
    let up_mbps = parse_mbps("upmbps")?;
    let down_mbps = parse_mbps("downmbps")?;

    let name = url
        .fragment()
        .and_then(|f| urlencoding::decode(f).ok().map(|c| c.into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| host.clone());

    Ok(Hy2Config {
        password,
        host,
        port,
        name,
        sni,
        insecure,
        obfs,
        obfs_password,
        up_mbps,
        down_mbps,
    })
}

/// Convert parsed Hy2Config to a sing-box outbound JSON object.
pub fn to_singbox_outbound(h: &Hy2Config, tag: &str) -> serde_json::Value {
    let mut outbound = serde_json::json!({
        "type": "hysteria2",
        "tag": tag,
        "server": h.host,
        "server_port": h.port,
        "password": h.password,
    });

    if h.up_mbps > 0 {
        outbound["up_mbps"] = serde_json::Value::from(h.up_mbps);
    }
    if h.down_mbps > 0 {
        outbound["down_mbps"] = serde_json::Value::from(h.down_mbps);
    }

    if let Some(obfs) = &h.obfs {
        outbound["obfs"] = serde_json::json!({
            "type": obfs,
            "password": h.obfs_password.clone().unwrap_or_default(),
        });
    }

    let mut tls = serde_json::json!({ "enabled": true });
    if let Some(sni) = &h.sni {
        tls["server_name"] = serde_json::Value::String(sni.clone());
    }
    if h.insecure {
        tls["insecure"] = serde_json::Value::Bool(true);
    }
    outbound["tls"] = tls;

    outbound
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test fixtures use RFC 5737 documentation IPs and synthetic passwords —
    // never real infrastructure values.
    #[test]
    fn parse_basic_link() {
        let raw = "hy2://testpass123@192.0.2.10:36757?insecure=1#eu-hop";
        let h = parse_hy2(raw).expect("parse should succeed");
        assert_eq!(h.password, "testpass123");
        assert_eq!(h.host, "192.0.2.10");
        assert_eq!(h.port, 36757);
        assert_eq!(h.name, "eu-hop");
        assert!(h.insecure);
        assert!(h.sni.is_none());
        assert!(h.obfs.is_none());
    }

    #[test]
    fn parse_hysteria2_alias() {
        let raw = "hysteria2://pass@192.0.2.20:8443#alias-name";
        let h = parse_hy2(raw).expect("hysteria2:// alias parses");
        assert_eq!(h.password, "pass");
        assert_eq!(h.host, "192.0.2.20");
        assert_eq!(h.port, 8443);
    }

    #[test]
    fn parse_explicit_443_does_not_collapse() {
        let raw = "hy2://pass@192.0.2.30:443#p443";
        let h = parse_hy2(raw).expect("parse should succeed");
        assert_eq!(h.port, 443);
    }

    #[test]
    fn parse_missing_port_defaults_443() {
        let raw = "hy2://pass@192.0.2.40#noport";
        let h = parse_hy2(raw).expect("parse should succeed");
        assert_eq!(h.port, 443);
    }

    #[test]
    fn parse_sni_and_obfs() {
        let raw = "hy2://pass@198.51.100.5:36757?sni=example.com&obfs=gecko&obfs-password=obfspw&insecure=1#obfs-hop";
        let h = parse_hy2(raw).expect("parse should succeed");
        assert_eq!(h.sni.as_deref(), Some("example.com"));
        assert_eq!(h.obfs.as_deref(), Some("gecko"));
        assert_eq!(h.obfs_password.as_deref(), Some("obfspw"));
        assert!(h.insecure);
    }

    #[test]
    fn parse_salamander_obfs() {
        let raw = "hy2://pass@198.51.100.6:443?obfs=salamander&obfs-password=spw";
        let h = parse_hy2(raw).expect("parse should succeed");
        assert_eq!(h.obfs.as_deref(), Some("salamander"));
        assert_eq!(h.obfs_password.as_deref(), Some("spw"));
    }

    #[test]
    fn obfs_without_password_rejected() {
        let raw = "hy2://pass@198.51.100.7:443?obfs=gecko";
        let err = parse_hy2(raw).unwrap_err();
        assert!(err.contains("obfs-password"), "{err}");
    }

    #[test]
    fn unknown_obfs_rejected() {
        let raw = "hy2://pass@198.51.100.8:443?obfs=magic&obfs-password=x";
        let err = parse_hy2(raw).unwrap_err();
        assert!(err.contains("Unsupported obfs"), "{err}");
    }

    #[test]
    fn missing_password_rejected() {
        let err = parse_hy2("hy2://@198.51.100.9:443").unwrap_err();
        assert!(err.contains("password"), "{err}");
    }

    #[test]
    fn missing_host_rejected() {
        let err = parse_hy2("hy2://pass").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn non_hy2_rejected() {
        let err = parse_hy2("vless://uuid@192.0.2.1:443").unwrap_err();
        assert!(err.contains("hy2"), "{err}");
    }

    #[test]
    fn name_falls_back_to_host() {
        let h = parse_hy2("hy2://pass@192.0.2.50:8443").expect("parse");
        assert_eq!(h.name, "192.0.2.50");
    }

    #[test]
    fn colon_password_preserved() {
        let h = parse_hy2("hy2://user:pw@192.0.2.60:8443").expect("parse");
        assert_eq!(h.password, "user:pw");
    }

    #[test]
    fn bandwidth_params_parsed() {
        let h = parse_hy2("hy2://p@192.0.2.70:443?upmbps=50&downmbps=200").expect("parse");
        assert_eq!(h.up_mbps, 50);
        assert_eq!(h.down_mbps, 200);
    }

    #[test]
    fn outbound_shape_with_gecko() {
        let h = parse_hy2(
            "hy2://pw@203.0.113.5:36757?sni=cdn.example.net&insecure=1&obfs=gecko&obfs-password=op#geo",
        )
        .expect("parse");
        let out = to_singbox_outbound(&h, "geo");
        assert_eq!(out["type"], "hysteria2");
        assert_eq!(out["tag"], "geo");
        assert_eq!(out["server"], "203.0.113.5");
        assert_eq!(out["server_port"], 36757);
        assert_eq!(out["password"], "pw");
        assert_eq!(out["obfs"]["type"], "gecko");
        assert_eq!(out["obfs"]["password"], "op");
        assert_eq!(out["tls"]["enabled"], true);
        assert_eq!(out["tls"]["server_name"], "cdn.example.net");
        assert_eq!(out["tls"]["insecure"], true);
    }

    #[test]
    fn outbound_without_obfs_omits_block() {
        let h = parse_hy2("hy2://pw@203.0.113.6:443?sni=example.com").expect("parse");
        let out = to_singbox_outbound(&h, "plain");
        assert!(out.get("obfs").is_none());
        assert_eq!(out["tls"]["server_name"], "example.com");
        // insecure absent unless requested
        assert!(out["tls"].get("insecure").is_none());
    }

    #[test]
    fn outbound_bandwidth_fields() {
        let h = parse_hy2("hy2://pw@203.0.113.7:443?upmbps=30&downmbps=100").expect("parse");
        let out = to_singbox_outbound(&h, "bw");
        assert_eq!(out["up_mbps"], 30);
        assert_eq!(out["down_mbps"], 100);
    }
}
