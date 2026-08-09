//! Regression: the launchd proxy variables Lumen writes outlive the app.
//!
//! `launchctl setenv` stores values in the launchd domain. Apps launched
//! afterwards from Dock/Spotlight inherit that environment for their whole
//! lifetime, so a stale value pointing at a dead local proxy can break desktop
//! Electron/CLI clients even after Lumen is turned off.

use app_lib::{lumen_proxy_env_pairs, stale_lumen_proxy_env_keys};

fn ours(key: &str) -> String {
    lumen_proxy_env_pairs()
        .into_iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| value)
        .expect("key is one Lumen writes")
}

#[test]
fn clears_our_own_variables_when_the_local_listener_is_dead() {
    let https = ours("https_proxy");
    let http = ours("http_proxy");
    let all = ours("all_proxy");
    let observed = [
        ("https_proxy", Some(https.as_str())),
        ("http_proxy", Some(http.as_str())),
        ("all_proxy", Some(all.as_str())),
    ];

    let mut keys = stale_lumen_proxy_env_keys(&observed, false);
    keys.sort_unstable();
    assert_eq!(keys, vec!["all_proxy", "http_proxy", "https_proxy"]);
}

#[test]
fn never_touches_a_proxy_lumen_did_not_write() {
    let observed = [
        ("https_proxy", Some("http://proxy.corp.example:3128")),
        ("http_proxy", Some("http://proxy.corp.example:3128")),
        ("all_proxy", Some("socks5://127.0.0.1:1080")),
    ];

    assert!(stale_lumen_proxy_env_keys(&observed, false).is_empty());
}

#[test]
fn keeps_our_variables_while_the_listener_is_alive() {
    let https = ours("https_proxy");
    let observed = [("https_proxy", Some(https.as_str()))];

    assert!(stale_lumen_proxy_env_keys(&observed, true).is_empty());
}

#[test]
fn unset_and_empty_variables_are_not_stale() {
    let observed = [
        ("https_proxy", None),
        ("http_proxy", Some("")),
        ("all_proxy", None),
    ];

    assert!(stale_lumen_proxy_env_keys(&observed, false).is_empty());
}

#[test]
fn a_partial_residue_clears_only_the_matching_keys() {
    let all = ours("all_proxy");
    let observed = [
        ("https_proxy", Some("http://proxy.corp.example:3128")),
        ("http_proxy", None),
        ("all_proxy", Some(all.as_str())),
    ];

    assert_eq!(
        stale_lumen_proxy_env_keys(&observed, false),
        vec!["all_proxy"]
    );
}

#[test]
fn disconnect_clears_env_even_when_the_system_proxy_flip_fails() {
    let src = include_str!("../src/lib.rs");
    let start = src
        .find("async fn disconnect(")
        .expect("disconnect command exists");
    let end = src[start..]
        .find("\n#[tauri::command]")
        .map(|offset| start + offset)
        .expect("another command follows disconnect");
    let body = &src[start..end];

    let clear_at = body
        .find("clear_lumen_proxy_env()")
        .expect("disconnect clears the launchd proxy env");
    let preamble = &body[..clear_at];

    assert!(
        !preamble.contains("?;"),
        "nothing may early-return before env cleanup in disconnect"
    );
    assert!(
        !preamble.contains("return Err"),
        "nothing may early-return before env cleanup in disconnect"
    );
}

#[test]
fn disconnect_reports_an_inherited_proxy_env_to_the_ui() {
    // Removing the variables fixes the NEXT app launch, never the one already
    // running, so disconnect has to tell the UI that something was inherited and
    // the user may need to restart those apps.
    let src = include_str!("../src/lib.rs");
    let start = src
        .find("async fn disconnect(")
        .expect("disconnect command exists");
    let end = src[start..]
        .find("\n#[tauri::command]")
        .map(|offset| start + offset)
        .expect("another command follows disconnect");
    let body = &src[start..end];

    assert!(
        body.contains("DisconnectOutcome"),
        "disconnect must return a structured outcome, not a bare unit"
    );
    assert!(
        body.contains("proxy_env_cleared"),
        "the outcome must say whether a launchd proxy was actually inherited"
    );
}

#[test]
fn startup_self_heals_a_launchd_residue_left_by_a_crash() {
    let src = include_str!("../src/lib.rs");
    let start = src.find(".setup(").expect("tauri setup hook exists");
    let end = src[start..]
        .find(".invoke_handler(")
        .map(|offset| start + offset)
        .expect("invoke_handler follows setup");
    let body = &src[start..end];

    assert!(
        body.contains("heal_stale_lumen_proxy_env()"),
        "startup must clear a stale launchd proxy left by a previous run"
    );
}

#[test]
fn env_cleanup_verifies_the_variable_is_actually_gone() {
    let src = include_str!("../src/lib.rs");
    let start = src
        .find("fn clear_lumen_proxy_env(")
        .expect("clear_lumen_proxy_env exists");
    let end = src[start..]
        .find("\n#[cfg(target_os = \"macos\")]")
        .map(|offset| start + offset)
        .expect("readback helper follows cleanup");
    let body = &src[start..end];

    assert!(
        body.contains("read_launchd_env("),
        "clear_lumen_proxy_env must re-read each key instead of trusting exit code"
    );
}
