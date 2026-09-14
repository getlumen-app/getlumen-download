use serde::{Deserialize, Serialize};

use crate::{config, hy2, vless};

type BootstrapError = Box<dyn std::error::Error + Send + Sync>;

const BOOTSTRAP_SCHEMA: &str = "lumen.bootstrap.v1";
const BOOTSTRAP_PREFIX: &str = "lumen-bootstrap-v1:";

#[derive(Debug, Deserialize)]
struct BootstrapPayload {
    schema_version: String,
    name: Option<String>,
    vless: String,
    preferred_mode: Option<String>,
    full_config_url: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct BootstrapImportResult {
    pub id: String,
    pub name: String,
    pub key_type: String,
    pub value: String,
    pub preferred_mode: String,
    pub full_config_url: Option<String>,
}

fn decode_payload(raw: &str) -> Result<String, BootstrapError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("bootstrap payload is empty".into());
    }
    if let Some(encoded) = s.strip_prefix(BOOTSTRAP_PREFIX) {
        return Ok(urlencoding::decode(encoded)?.into_owned());
    }
    Ok(s.to_string())
}

pub fn parse_bootstrap_payload(raw: &str) -> Result<BootstrapImportResult, BootstrapError> {
    let decoded = decode_payload(raw)?;
    let payload: BootstrapPayload = serde_json::from_str(&decoded)?;
    if payload.schema_version != BOOTSTRAP_SCHEMA {
        return Err(format!("unsupported bootstrap schema: {}", payload.schema_version).into());
    }

    let link = payload.vless.trim();
    let is_hy2 = link.starts_with("hy2://") || link.starts_with("hysteria2://");
    let (key_type, default_name) = if is_hy2 {
        let parsed =
            hy2::parse_hy2(link).map_err(|e| format!("HY2 parse failed: {}", e))?;
        ("hy2", parsed.name)
    } else {
        let parsed =
            vless::parse_vless(link).map_err(|e| format!("VLESS parse failed: {}", e))?;
        ("vless", parsed.name)
    };
    let name = payload
        .name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_name.clone());
    let preferred_mode = match payload.preferred_mode.as_deref().unwrap_or("proxy") {
        "proxy" => "proxy",
        "tun" => "tun",
        other => return Err(format!("unsupported preferred_mode: {}", other).into()),
    };
    let full_config_url = payload
        .full_config_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(url) = &full_config_url {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("full_config_url must be http(s)".into());
        }
    }

    Ok(BootstrapImportResult {
        id: "bootstrap-imported".to_string(),
        name,
        key_type: key_type.to_string(),
        value: link.to_string(),
        preferred_mode: preferred_mode.to_string(),
        full_config_url,
    })
}

pub async fn import_bootstrap_payload(raw: &str) -> Result<BootstrapImportResult, BootstrapError> {
    let result = parse_bootstrap_payload(raw)?;

    // Prebuild both configs so a clean install can connect without reaching the
    // control-plane endpoints. The payload is still per-user and revocable.
    if result.key_type == "hy2" {
        let parsed =
            hy2::parse_hy2(&result.value).map_err(|e| format!("HY2 parse failed: {}", e))?;
        config::save_hy2_config(&parsed, config::InboundMode::Mixed).await?;
        config::save_hy2_config(&parsed, config::InboundMode::Tun).await?;
    } else {
        let parsed =
            vless::parse_vless(&result.value).map_err(|e| format!("VLESS parse failed: {}", e))?;
        config::save_vless_config(&parsed, config::InboundMode::Mixed).await?;
        config::save_vless_config(&parsed, config::InboundMode::Tun).await?;
    }
    config::save_bootstrap_full_config_url(result.full_config_url.as_deref())?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_vless() -> &'static str {
        "vless://00000000-0000-4000-8000-000000000001@192.0.2.10:443?type=grpc&security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&sni=dl.google.com&sid=deadbeef&path=%2Fproteus-eu#Katya"
    }

    #[test]
    fn bootstrap_payload_imports_personal_vless_profile() {
        let raw = serde_json::json!({
            "schema_version": "lumen.bootstrap.v1",
            "name": "Katya bootstrap",
            "vless": valid_vless(),
            "preferred_mode": "proxy"
        })
        .to_string();

        let profile = parse_bootstrap_payload(&raw).expect("bootstrap payload parses");
        assert_eq!(profile.name, "Katya bootstrap");
        assert_eq!(profile.key_type, "vless");
        assert_eq!(profile.value, valid_vless());
        assert_eq!(profile.preferred_mode, "proxy");
    }

    #[test]
    fn bootstrap_payload_accepts_urlencoded_form_for_messenger_copying() {
        let raw_json = serde_json::json!({
            "schema_version": "lumen.bootstrap.v1",
            "vless": valid_vless()
        })
        .to_string();
        let payload = format!("{}{}", BOOTSTRAP_PREFIX, urlencoding::encode(&raw_json));

        let profile = parse_bootstrap_payload(&payload).expect("urlencoded payload parses");
        assert_eq!(profile.name, "Katya");
        assert_eq!(profile.value, valid_vless());
    }

    #[test]
    fn bootstrap_payload_can_carry_full_config_url_for_promotion() {
        let raw = serde_json::json!({
            "schema_version": "lumen.bootstrap.v1",
            "name": "Katya bootstrap",
            "vless": valid_vless(),
            "preferred_mode": "proxy",
            "full_config_url": "https://config.getlumen.download/proteus-sub?sub=iyp3VWoxkpnYNQO4"
        })
        .to_string();

        let profile = parse_bootstrap_payload(&raw).expect("bootstrap payload parses");
        assert_eq!(
            profile.full_config_url.as_deref(),
            Some("https://config.getlumen.download/proteus-sub?sub=iyp3VWoxkpnYNQO4")
        );
    }

    #[test]
    fn bootstrap_payload_accepts_hy2_link() {
        let raw = serde_json::json!({
            "schema_version": "lumen.bootstrap.v1",
            "name": "HY2 hop",
            "vless": "hy2://synthetic-pw@198.51.100.20:36757?insecure=1&obfs=gecko&obfs-password=obfs-pw#hop"
        })
        .to_string();

        let profile = parse_bootstrap_payload(&raw).expect("hy2 payload parses");
        assert_eq!(profile.name, "HY2 hop");
        assert_eq!(profile.key_type, "hy2");
        assert_eq!(profile.preferred_mode, "proxy");
    }

    #[test]
    fn bootstrap_payload_rejects_wrong_schema() {
        let raw = serde_json::json!({
            "schema_version": "lumen.bootstrap.v0",
            "vless": valid_vless()
        })
        .to_string();

        let err = parse_bootstrap_payload(&raw).unwrap_err().to_string();
        assert!(err.contains("unsupported bootstrap schema"), "{err}");
    }

    #[test]
    fn bootstrap_payload_rejects_non_vless_secret_material() {
        let raw = serde_json::json!({
            "schema_version": "lumen.bootstrap.v1",
            "vless": "iyp3VWoxkpnYNQO4"
        })
        .to_string();

        let err = parse_bootstrap_payload(&raw).unwrap_err().to_string();
        assert!(err.contains("VLESS parse failed"), "{err}");
    }
}
