use super::{parse_endpoint, parse_route, route_allowed};

#[test]
fn default_loopback_origin_has_no_userinfo() {
    let endpoint = parse_endpoint(super::DEFAULT_ORIGIN).unwrap();
    assert_eq!(endpoint.origin, "http://127.0.0.1:9042");
    assert!(endpoint.loopback);
}

#[test]
fn endpoint_rejects_credentials_fragment_and_escapes() {
    for raw in [
        "http://user:pass@127.0.0.1:9042",
        "http://127.0.0.1:9042#frag",
        "http://127.0.0.1:9042/dashboard/api/v4",
        "http://127.0.0.1:9042/%2e%2e",
        "http://127.0.0.1:9042\\@evil.example",
        "http://evil.example#@127.0.0.1:9042",
        "ftp://127.0.0.1:9042",
        "http://127.0.0.1:9042?x=1",
    ] {
        assert!(parse_endpoint(raw).is_err(), "{raw}");
    }
}

#[test]
fn route_allowlist_keeps_v4_auth_inference_and_transfer() {
    assert!(route_allowed("/dashboard/api/v4/contract"));
    assert!(route_allowed("/dashboard/api/v4/accounts/transfer/export"));
    assert!(route_allowed("/dashboard/api/auth/login"));
    assert!(route_allowed("/v1/chat/completions"));
    assert!(route_allowed("/v1/messages/count_tokens"));
    assert!(super::is_inference("/v1/messages/count_tokens"));
    assert!(!route_allowed("/v1/messages/count_tokens/extra"));
    assert!(route_allowed("/v1/models/gemini:generateContent"));
    assert!(route_allowed("/v1beta/models/gemini:countTokens"));
    assert!(!route_allowed("/dashboard/api/v3/accounts"));
    assert!(!route_allowed("/dashboard/api/settings"));
    assert!(!route_allowed("/dashboard/api/v2/accounts"));
    assert!(!route_allowed("/dashboard/api/browser/sessions/tok/ws"));
    assert!(!route_allowed("/v1/chat/completions/extra"));
}

#[test]
fn path_rejects_encoded_and_literal_traversal() {
    for raw in [
        "/dashboard/api/v4/../v3/accounts",
        "/dashboard/api/v4/%2e%2e/accounts",
        "/dashboard/api/v4/%252e%252e/accounts",
        "/v1/models/%2fetc",
        "/dashboard/api/v3/accounts",
        "http://127.0.0.1/v1/models",
        "/dashboard/api/v4/accounts/#frag",
    ] {
        assert!(parse_route(raw).is_err(), "{raw}");
    }
    assert!(parse_route("/dashboard/api/v4/contract?unused=1").is_ok());
    assert!(parse_route("/dashboard/api/v4/accounts/@local").is_err());
}

#[test]
fn ipv6_loopback_keeps_one_pair_of_brackets() {
    let endpoint = parse_endpoint("http://[::1]:9042").unwrap();
    assert_eq!(endpoint.origin, "http://[::1]:9042");
    assert!(endpoint.loopback);
    assert!(parse_endpoint("http://[::1]:9042").unwrap().origin != "http://[[::1]]:9042");
}

#[test]
fn query_keeps_native_paths_and_encoded_models_on_the_same_origin() {
    let target = "/dashboard/api/v4/apps/native?targetPath=C:\\isolated\\config.toml&profilePath=C:\\isolated\\profile&runtimeUrl=http://127.0.0.1:9/runtime&owner=user@host";
    let parsed = parse_route(target).unwrap();
    assert_eq!(parsed, target);
    let origin = parse_endpoint("http://127.0.0.1:9042").unwrap().origin;
    let url = format!("{origin}{parsed}");
    assert!(url.starts_with("http://127.0.0.1:9042/"));
    assert!(url.contains("targetPath=C:\\isolated\\config.toml"));
    assert!(url.contains("owner=user@host"));
    assert!(!url.contains("http://user@"));

    let model = "/dashboard/api/v4/usage/requests?model=vendor%2Fmodel&path=%5cpreserve";
    let parsed = parse_route(model).unwrap();
    assert_eq!(parsed, model);
    let url = format!("{origin}{parsed}");
    assert!(url.starts_with("http://127.0.0.1:9042/dashboard/api/v4/"));
    assert!(url.contains("vendor%2Fmodel"));
    assert!(parse_route("/dashboard/api/v4/usage/requests?q=%00").is_err());
    assert!(parse_route("/v1/models/%2fetc").is_err());
}
