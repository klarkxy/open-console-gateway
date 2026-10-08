use super::{
    debug_managed_key_verify_target, install_managed_key_verify_target_for_tests,
    parse_loopback_http_url,
};

fn unique_generation() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

#[test]
fn parse_loopback_http_url_requires_exact_host_without_userinfo_query_or_fragment() {
    assert_eq!(
        parse_loopback_http_url("http://127.0.0.1:9/").as_deref(),
        Some("http://127.0.0.1:9/")
    );
    assert_eq!(
        parse_loopback_http_url("http://localhost:9/").as_deref(),
        Some("http://localhost:9/")
    );
    assert_eq!(
        parse_loopback_http_url("http://[::1]:9/").as_deref(),
        Some("http://[::1]:9/")
    );
    assert!(parse_loopback_http_url("http://127.0.0.1:9/?x=1").is_none());
    assert!(parse_loopback_http_url("http://127.0.0.1:9/#frag").is_none());
    assert!(parse_loopback_http_url("http://user@127.0.0.1:9/").is_none());
    assert!(parse_loopback_http_url("https://opencode.ai/zen/go").is_none());
    assert!(parse_loopback_http_url("http://127.0.0.2:9/").is_none());
}

#[test]
fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
    let first = unique_generation();
    let second = unique_generation();
    let _guard_a = install_managed_key_verify_target_for_tests(first, "http://127.0.0.1:11/");
    let _guard_b = install_managed_key_verify_target_for_tests(second, "http://127.0.0.1:12/");
    assert_eq!(
        debug_managed_key_verify_target(first).as_deref(),
        Some("http://127.0.0.1:11/")
    );
    assert_eq!(
        debug_managed_key_verify_target(second).as_deref(),
        Some("http://127.0.0.1:12/")
    );

    drop(_guard_a);
    let _cleared =
        install_managed_key_verify_target_for_tests(first, "http://127.0.0.1:11@example.com/");
    assert!(debug_managed_key_verify_target(first).is_none());
    assert_eq!(
        debug_managed_key_verify_target(second).as_deref(),
        Some("http://127.0.0.1:12/")
    );
}
