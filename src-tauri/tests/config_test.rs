#[test]
fn config_source_keeps_relay_eu_443_in_auto_exclusions() {
    let src = include_str!("../src/config.rs");
    let exclusion_pos = src
        .find("const AUTO_EXCLUDED_GEO_TAGS")
        .expect("auto exclusion list must exist");
    let relay_pos = src[exclusion_pos..]
        .find("\"relay-eu-443\"")
        .expect("relay-eu-443 must be excluded from Auto urltests");
    let list_end = src[exclusion_pos..]
        .find("];")
        .expect("auto exclusion list must be closed");

    assert!(
        relay_pos < list_end,
        "relay-eu-443 should remain a manual selector pin, not an Auto member"
    );
}
