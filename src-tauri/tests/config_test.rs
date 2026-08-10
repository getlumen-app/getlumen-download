#[test]
fn config_source_keeps_relay_eu_443_out_of_auto_and_geo_pins() {
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
        "relay-eu-443 must stay excluded from Auto urltests"
    );

    let geo_pos = src
        .find("const GEO_SELECTOR_TAGS")
        .expect("geo selector list must exist");
    let geo_end = src[geo_pos..]
        .find("];")
        .expect("geo selector list must be closed");
    let geo = &src[geo_pos..geo_pos + geo_end];
    assert!(
        !geo.contains("\"relay-eu-443\""),
        "relay-eu-443 must not be the user-facing Germany pin"
    );
    assert!(
        geo.contains("\"relay-eu-grpc\""),
        "Germany pin should use the non-443 gRPC relay"
    );
}
