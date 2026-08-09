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

#[test]
fn proxy_status_requires_local_listener_and_env_cleanup_unsets_vars() {
    let src = include_str!("../src/lib.rs");

    let status_start = src.find("async fn get_status").expect("get_status exists");
    let status_end = src[status_start..]
        .find("async fn get_effective_status")
        .map(|offset| status_start + offset)
        .expect("get_effective_status follows get_status");
    let status_body = &src[status_start..status_end];
    assert!(
        status_body.contains(".is_ready()"),
        "proxy status must require the 127.0.0.1:10808 listener, not only a process"
    );

    let effective_start = src
        .find("async fn get_effective_status")
        .expect("get_effective_status exists");
    let effective_end = src[effective_start..]
        .find("async fn network_diagnostics")
        .map(|offset| effective_start + offset)
        .expect("network_diagnostics follows get_effective_status");
    let effective_body = &src[effective_start..effective_end];
    assert!(
        effective_body.contains(".is_ready()"),
        "effective status must not report connected-proxy without the local listener"
    );

    let cleanup_start = src
        .find("fn clear_lumen_proxy_env")
        .expect("clear_lumen_proxy_env exists");
    let cleanup_end = src[cleanup_start..]
        .find("/// Inspect input")
        .map(|offset| cleanup_start + offset)
        .expect("next section follows env cleanup");
    let cleanup_body = &src[cleanup_start..cleanup_end];
    assert!(
        cleanup_body.contains("\"unsetenv\""),
        "disconnect/repair must remove launchctl proxy variables, not leave empty values"
    );
    assert!(
        !cleanup_body.contains("[\"setenv\", key, \"\"]"),
        "empty launchctl proxy variables can still confuse Electron apps"
    );
}

#[test]
fn system_proxy_connect_health_checks_outbound_before_enabling_macos_proxy() {
    let src = include_str!("../src/lib.rs");

    let health_start = src
        .find("async fn wait_for_local_proxy_route_health")
        .expect("local proxy route health helper exists");
    let health_end = src[health_start..]
        .find("fn set_lumen_proxy_env")
        .map(|offset| health_start + offset)
        .expect("env helper follows local proxy health helper");
    let health_body = &src[health_start..health_end];
    assert!(
        health_body.contains("reqwest::Proxy::all") && health_body.contains("127.0.0.1"),
        "proxy route health must probe through the local mixed listener"
    );
    assert!(
        health_body.contains("https://www.cloudflare.com/cdn-cgi/trace"),
        "proxy route health should validate a real HTTPS path, not only a local port"
    );

    let connect_start = src.find("async fn connect").expect("connect exists");
    let connect_end = src[connect_start..]
        .find("async fn disconnect")
        .map(|offset| connect_start + offset)
        .expect("disconnect follows connect");
    let connect_body = &src[connect_start..connect_end];
    let health_call = connect_body
        .find("wait_for_local_proxy_route_health")
        .expect("connect must call local proxy route health");
    let enable_call = connect_body
        .find("proxy::enable_system_proxy")
        .expect("connect must enable system proxy only after checks");
    assert!(
        health_call < enable_call,
        "connect must fail closed before enabling macOS System Proxy"
    );
}
