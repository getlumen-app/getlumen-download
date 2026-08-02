//! Regression: System Proxy process detection must never treat helper TUN
//! sing-box as "proxy running" (TUN disconnect thrash, 2026-08-02).
//!
//! Before the fix, `pgrep -f "sing-box run"` matched BOTH:
//!   - proxy:  sing-box run -c …/config.json
//!   - TUN:    sing-box run -c …/config-tun.json
//! so get_effective_status reported connected-proxy while TUN was up → UI
//! thought preference mismatched → power tap hot-swapped (Stop+Start) forever.

use app_lib::cmdline_is_proxy_singbox;

#[test]
fn proxy_cmdline_classifier_accepts_mixed_config_json() {
    assert!(cmdline_is_proxy_singbox(
        "/Applications/Lumen.app/Contents/Resources/_up_/bin/sing-box run -c /Users/user/Library/Caches/io.getlumen.app/config.json"
    ));
}

#[test]
fn proxy_cmdline_classifier_rejects_tun_config() {
    assert!(
        !cmdline_is_proxy_singbox(
            "/Applications/Lumen.app/Contents/Resources/_up_/bin/sing-box run -c /Users/user/Library/Caches/io.getlumen.app/config-tun.json"
        ),
        "TUN helper cmdline must NOT count as System Proxy running"
    );
}

#[test]
fn proxy_cmdline_classifier_rejects_tun_lastgood() {
    assert!(!cmdline_is_proxy_singbox(
        "sing-box run -c /Users/user/Library/Caches/io.getlumen.app/config-tun-lastgood.json"
    ));
}

#[test]
fn proxy_cmdline_classifier_rejects_unrelated() {
    assert!(!cmdline_is_proxy_singbox("nginx: worker process"));
    assert!(!cmdline_is_proxy_singbox(""));
}

#[test]
fn source_must_not_use_bare_sing_box_run_pgrep() {
    let src = include_str!("../src/singbox.rs");
    // Assemble needles so this test file itself does not trip naive greps.
    let bare_pgrep = ["\"-f\", ", "\"sing-box run\""].concat();
    let bare_pkill9 = ["\"-9\", ", "\"-f\", ", "\"sing-box run\""].concat();
    let killall_arg = [".arg(", "\"sing-box\"", ")"].concat();
    assert!(
        !src.contains(&bare_pgrep) && !src.contains(&bare_pkill9),
        "bare pgrep/pkill for sing-box run matches TUN; scope to proxy config.json"
    );
    assert!(
        src.contains("cmdline_is_proxy_singbox"),
        "proxy detection must go through the cmdline classifier"
    );
    assert!(
        !src.contains(&killall_arg),
        "killall sing-box is forbidden; it stops helper TUN too"
    );
}
