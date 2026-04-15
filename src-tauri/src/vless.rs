/// VLESS link parser.
///
/// Format:
///   vless://UUID@HOST:PORT?type=tcp&security=reality&pbk=KEY&fp=chrome&sni=google.com&sid=ID&spx=%2F#NAME
///
/// `vless://` is not a registered URL scheme — we rewrite to `https://` for `url` crate parsing,
/// then extract original semantic fields.
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone)]
pub struct VlessConfig {
    pub uuid: String,
    pub host: String,
    pub port: u16,
    /// Display name (URL-decoded fragment, or hostname fallback)
    pub name: String,
    /// "reality" | "tls" | "none"
    pub security: String,
    /// Reality public key (required when security=reality)
    pub pbk: Option<String>,
    /// Server name indication (required when security=reality)
    pub sni: Option<String>,
    /// TLS fingerprint, default "chrome"
    pub fp: String,
    /// Reality short ID, default ""
    pub sid: String,
    /// Spider X path, default "/"
    pub spx: String,
    /// Transport: "tcp" | "ws" | "grpc" | "httpupgrade"
    pub transport: String,
    /// XTLS flow, default "" (server may set to "xtls-rprx-vision")
    pub flow: String,
    /// Path for ws/grpc/httpupgrade
    pub path: String,
    /// Host header for ws
    pub host_header: String,
}

pub fn parse_vless(raw: &str) -> Result<VlessConfig, String> {
    let s = raw.trim();
    if !s.starts_with("vless://") {
        return Err("Not a vless:// link".to_string());
    }

    // Rewrite scheme so url crate can parse the structure. Use placeholder host
    // so we can detect if real host was missing.
    let normalized = s.replacen("vless://", "https://", 1);
    let url = Url::parse(&normalized).map_err(|e| format!("Bad VLESS URI: {}", e))?;

    let uuid = url.username().to_string();
    if uuid.is_empty() {
        return Err("Missing UUID in VLESS link".to_string());
    }

    let host = url
        .host_str()
        .ok_or("Missing host in VLESS link")?
        .to_string();

    // NOTE: the `url` crate's `.port()` returns None when the port equals the scheme's
    // default. Since we rewrite `vless://` → `https://` for parsing, an explicit `:443`
    // in the VLESS link collapses to "default" and .port() reports None — which broke
    // Reality links bound to 443. Use `port_or_known_default()` which returns the
    // explicit port if present and falls back to the https default (443) otherwise.
    let port = url
        .port_or_known_default()
        .ok_or("Missing port in VLESS link")?;

    let params: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let security = params
        .get("security")
        .map(String::from)
        .unwrap_or_else(|| "none".to_string());

    let pbk = params.get("pbk").cloned();
    let sni = params.get("sni").cloned();

    if security == "reality" {
        if pbk.is_none() {
            return Err("Reality requires 'pbk' parameter".to_string());
        }
        if sni.is_none() {
            return Err("Reality requires 'sni' parameter".to_string());
        }
    }

    let name = url
        .fragment()
        .and_then(|f| urlencoding::decode(f).ok().map(|c| c.into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| host.clone());

    let fp = params
        .get("fp")
        .cloned()
        .unwrap_or_else(|| "chrome".to_string());

    let sid = params.get("sid").cloned().unwrap_or_default();

    let spx = params
        .get("spx")
        .and_then(|s| urlencoding::decode(s).ok().map(|c| c.into_owned()))
        .unwrap_or_else(|| "/".to_string());

    let transport = params
        .get("type")
        .cloned()
        .unwrap_or_else(|| "tcp".to_string());

    // Default flow: empty (sing-box uses xtls-rprx-vision automatically with reality+tcp,
    // but we explicitly set it for tcp+reality).
    let flow = params.get("flow").cloned().unwrap_or_else(|| {
        if security == "reality" && transport == "tcp" {
            "xtls-rprx-vision".to_string()
        } else {
            String::new()
        }
    });

    let path = params
        .get("path")
        .and_then(|s| urlencoding::decode(s).ok().map(|c| c.into_owned()))
        .unwrap_or_else(|| "/".to_string());

    let host_header = params.get("host").cloned().unwrap_or_default();

    Ok(VlessConfig {
        uuid,
        host,
        port,
        name,
        security,
        pbk,
        sni,
        fp,
        sid,
        spx,
        transport,
        flow,
        path,
        host_header,
    })
}

/// Convert parsed VlessConfig to a sing-box outbound JSON object.
pub fn to_singbox_outbound(v: &VlessConfig, tag: &str) -> serde_json::Value {
    let mut outbound = serde_json::json!({
        "type": "vless",
        "tag": tag,
        "server": v.host,
        "server_port": v.port,
        "uuid": v.uuid,
    });

    if !v.flow.is_empty() {
        outbound["flow"] = serde_json::Value::String(v.flow.clone());
    }

    // TLS layer
    if v.security == "reality" || v.security == "tls" {
        let mut tls = serde_json::json!({
            "enabled": true,
            "server_name": v.sni.clone().unwrap_or_else(|| v.host.clone()),
            "utls": {
                "enabled": true,
                "fingerprint": v.fp,
            },
        });
        if v.security == "reality" {
            tls["reality"] = serde_json::json!({
                "enabled": true,
                "public_key": v.pbk.clone().unwrap_or_default(),
                "short_id": v.sid,
            });
        }
        outbound["tls"] = tls;
    }

    // Transport
    match v.transport.as_str() {
        "tcp" => { /* no transport block needed */ }
        "ws" => {
            let mut transport = serde_json::json!({
                "type": "ws",
                "path": v.path,
            });
            if !v.host_header.is_empty() {
                transport["headers"] = serde_json::json!({"Host": v.host_header});
            }
            outbound["transport"] = transport;
        }
        "grpc" => {
            outbound["transport"] = serde_json::json!({
                "type": "grpc",
                "service_name": v.path.trim_start_matches('/'),
            });
        }
        "httpupgrade" => {
            outbound["transport"] = serde_json::json!({
                "type": "httpupgrade",
                "host": v.sni.clone().unwrap_or_else(|| v.host.clone()),
                "path": v.path,
            });
        }
        _ => {}
    }

    outbound
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test fixtures use synthetic UUIDs, RFC 5737 documentation IP ranges,
    // and neutral fragment names. They never touch real infrastructure so
    // source readers cannot identify which server list this client talks to.
    #[test]
    fn parse_nonstandard_port_link() {
        let raw = "vless://00000000-0000-4000-8000-000000000001@192.0.2.10:18241?type=tcp&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=google.com&sid=deadbeef&spx=%2F#test-reality";
        let v = parse_vless(raw).expect("parse should succeed");
        assert_eq!(v.uuid, "00000000-0000-4000-8000-000000000001");
        assert_eq!(v.host, "192.0.2.10");
        assert_eq!(v.port, 18241);
        assert_eq!(v.name, "test-reality");
        assert_eq!(v.security, "reality");
        assert_eq!(v.pbk.as_deref(), Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert_eq!(v.sni.as_deref(), Some("google.com"));
        assert_eq!(v.fp, "chrome");
        assert_eq!(v.sid, "deadbeef");
        assert_eq!(v.spx, "/");
        assert_eq!(v.transport, "tcp");
        assert_eq!(v.flow, "xtls-rprx-vision");
    }

    #[test]
    fn parse_port_443_does_not_collapse() {
        // Regression: url crate collapses default ports — explicit `:443` on an https-
        // rewritten URL returned None. Must be 443.
        let raw = "vless://00000000-0000-4000-8000-000000000002@192.0.2.20:443?type=tcp&security=reality&pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB&fp=chrome&sni=google.com&sid=cafebabe&spx=%2F&flow=xtls-rprx-vision#test-443";
        let v = parse_vless(raw).expect("parse should succeed for port 443");
        assert_eq!(v.host, "192.0.2.20");
        assert_eq!(v.port, 443);
        assert_eq!(v.name, "test-443");
        assert_eq!(v.security, "reality");
        assert_eq!(v.flow, "xtls-rprx-vision");
    }
}
