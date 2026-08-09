//! Startup readiness must poll quickly. Fixed multi-second sleeps made System
//! Proxy connect look stuck even when sing-box and Clash API were ready.

#[test]
fn clash_api_readiness_uses_fast_bounded_polling() {
    let src = include_str!("../src/lib.rs");
    let start = src
        .find("async fn wait_for_clash_api()")
        .expect("wait_for_clash_api exists");
    let end = src[start..]
        .find("fn set_lumen_proxy_env")
        .map(|offset| start + offset)
        .expect("next helper after wait_for_clash_api exists");
    let body = &src[start..end];

    assert!(
        body.contains("Duration::from_millis(500)"),
        "local Clash API probes should not spend seconds on one attempt"
    );
    assert!(
        body.contains("Duration::from_millis(100)"),
        "readiness should poll frequently while sing-box starts"
    );
    assert!(
        !body.contains("Duration::from_secs(3)") && !body.contains("Duration::from_secs(1)"),
        "fixed second-scale sleeps make connect feel hung"
    );
}
