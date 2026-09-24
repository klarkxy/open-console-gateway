use super::*;
use crate::custom_http::validate_custom_endpoint_url;
use crate::provider::ProviderBindingError;

fn assert_blocked(url: &str) {
    assert!(
        matches!(
            validate_custom_endpoint_url(url),
            Err(ProviderBindingError::InvalidCustomBaseUrl(_))
        ),
        "{url} must be rejected before outbound"
    );
}

fn assert_allowed(url: &str) {
    assert!(
        validate_custom_endpoint_url(url).is_ok(),
        "{url} must remain a documented Custom destination"
    );
}

#[test]
fn s01_cross_origin_override_does_not_inherit_the_key() {
    let granted = vec!["https://lab.example/v1".to_string()];
    assert!(
        ensure_secret_origin_granted("https://lab.example/v1/chat/completions", &granted).is_ok()
    );
    assert!(
        ensure_secret_origin_granted("https://lab.example:443/other/v1/messages", &granted).is_ok(),
        "same origin with default https port stays granted"
    );
    assert!(
        ensure_secret_origin_granted("https://evil.example/v1/chat/completions", &granted).is_err(),
        "a different Origin must not inherit the Key"
    );
    assert!(
        ensure_secret_origin_granted(
            "http://127.0.0.1:8080/v1/chat/completions",
            &["http://127.0.0.1:9/v1".to_string()],
        )
        .is_err(),
        "a different port is a different Origin and must not inherit the Key"
    );
}

#[test]
fn origins_match_equates_default_ports_and_ipv6_text_only() {
    assert!(origins_match(
        "https://service.example:443",
        "https://service.example/v1/messages"
    ));
    assert!(origins_match(
        "http://[2001:DB8:0:0:0:0:0:1]:8080",
        "http://[2001:db8::1]:8080/v1/messages"
    ));
    assert!(!origins_match(
        "http://service.example",
        "https://service.example"
    ));
    assert!(!origins_match(
        "https://service.example:8443",
        "https://service.example"
    ));
    assert!(!origins_match("https://a.example", "https://b.example"));
}

#[test]
fn s01_extra_allowed_origin_is_an_explicit_grant() {
    let granted = vec![
        "https://lab.example/v1".to_string(),
        "https://alt.example/v1".to_string(),
    ];
    assert!(ensure_secret_origin_granted("https://alt.example/v1/messages", &granted).is_ok());
}

#[test]
fn s01_sealed_adapter_origin_stays_sealed() {
    assert!(
        ensure_sealed_secret_origin(
            "https://opencode.ai/zen/go/v1/chat/completions",
            OPENCODE_GO_BASE_URL,
        )
        .is_ok()
    );
    assert!(
        ensure_sealed_secret_origin(
            "https://evil.example/v1/chat/completions",
            "https://evil.example/v1",
        )
        .is_err()
    );
    assert!(
        ensure_sealed_secret_origin(
            "http://127.0.0.1:9/zen/go/v1/chat/completions",
            "http://127.0.0.1:9/zen/go",
        )
        .is_ok(),
        "documented loopback test seam remains a sealed exception"
    );
}

#[test]
fn s03_metadata_and_link_local_custom_urls_are_rejected() {
    for url in [
        "https://169.254.169.254/latest",
        "http://169.254.169.254/latest",
        "https://[::ffff:169.254.169.254]/responses",
        "http://[fe80::1]/v1/messages",
        "http://metadata.google.internal/messages",
        "https://METADATA.GOOGLE.INTERNAL/latest",
        "http://compute.metadata.google.internal/v1",
        "http://metadata/latest",
        "http://0.0.0.0/v1/messages",
        "http://[::]/v1/messages",
        "https://fd00:ec2::254/latest",
        "https://[fd00:ec2::254]/latest",
    ] {
        assert_blocked(url);
    }
}

#[test]
fn s03_opaque_ipv4_tricks_are_rejected() {
    for url in [
        "http://0xa9fea9fe/latest",
        "http://0xA9.0xFE.0xA9.0xFE/latest",
        "http://2852039166/latest",
        "http://0251.0376.0251.0376/latest",
    ] {
        assert_blocked(url);
    }
}

#[test]
fn s03_documented_local_model_destinations_stay_allowed() {
    for url in [
        "http://127.0.0.1:8080/v1/messages",
        "http://localhost:3000/chat/completions",
        "http://[::1]/v1/responses",
        "http://[::ffff:127.0.0.1]/v1/responses",
        "http://app.localhost/v1/responses",
        "https://192.168.1.8/v1/responses",
        "http://10.0.0.1:9000/v1/messages",
        "https://api.example.com/v1/responses",
    ] {
        assert_allowed(url);
    }
}

#[test]
fn secret_bearing_requests_never_follow_redirects() {
    assert!(!follows_redirects_with_secret(true, true));
    assert!(follows_redirects_with_secret(true, false));
    assert!(!follows_redirects_with_secret(false, false));
    assert!(!follows_redirects_with_secret(false, true));
}

#[test]
fn enforce_attempt_secret_origin_distinguishes_user_and_sealed_grants() {
    assert!(
        enforce_attempt_secret_origin(
            "https://lab.example/v1/chat/completions",
            Some("https://lab.example/v1"),
            "https://lab.example/v1",
        )
        .is_ok()
    );
    assert!(
        enforce_attempt_secret_origin(
            "https://evil.example/v1/chat/completions",
            Some("https://lab.example/v1"),
            "https://evil.example/v1",
        )
        .is_err()
    );
    assert!(
        enforce_attempt_secret_origin(
            "https://opencode.ai/zen/go/v1/chat/completions",
            None,
            OPENCODE_GO_BASE_URL,
        )
        .is_ok()
    );
}
