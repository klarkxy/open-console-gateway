use super::*;

#[test]
fn zen_free_identity_fills_missing_headers_and_keeps_official_user_agent() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        "opencode/1.17.7".parse().unwrap(),
    );
    let session = reqwest::header::HeaderValue::from_static("ses_1");
    apply_zen_free_identity_headers(&mut headers, &session, "req_1");
    assert_eq!(headers.get("x-opencode-client").unwrap(), "cli");
    assert_eq!(headers.get("x-opencode-request").unwrap(), "req_1");
    assert_eq!(headers.get("x-opencode-project").unwrap(), "ses_1");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "opencode/1.17.7"
    );
}

#[test]
fn zen_free_identity_replaces_foreign_user_agent_and_preserves_client_fields() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::USER_AGENT, "reqwest/0.12".parse().unwrap());
    headers.insert("x-opencode-client", "cli".parse().unwrap());
    headers.insert("x-opencode-request", "keep-me".parse().unwrap());
    headers.insert("x-opencode-project", "proj_keep".parse().unwrap());
    let session = reqwest::header::HeaderValue::from_static("ses_1");
    apply_zen_free_identity_headers(&mut headers, &session, "req_1");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "opencode"
    );
    assert_eq!(headers.get("x-opencode-client").unwrap(), "cli");
    assert_eq!(headers.get("x-opencode-request").unwrap(), "keep-me");
    assert_eq!(headers.get("x-opencode-project").unwrap(), "proj_keep");
}

#[test]
fn identity_headers_follow_adapter_capability() {
    assert!(identity_headers_enabled(ProviderAdapterKind::OpenCodeGo));
    assert!(identity_headers_enabled(ProviderAdapterKind::ZenFree));
    assert!(!identity_headers_enabled(
        ProviderAdapterKind::CommandCodeGoat
    ));
    assert!(!identity_headers_enabled(
        ProviderAdapterKind::ConfigurableHttp
    ));

    let empty = HeaderMap::new();
    let mut zen = reqwest::header::HeaderMap::new();
    apply_provider_identity_headers(
        &mut zen,
        &empty,
        ProviderAdapterKind::ZenFree,
        ApiFormat::ChatCompletions,
        "m-free",
        b"{}",
        "req_1",
    );
    assert!(zen.get("x-opencode-session").is_some());
    assert_eq!(zen.get("x-opencode-client").unwrap(), "cli");

    let mut go = reqwest::header::HeaderMap::new();
    apply_provider_identity_headers(
        &mut go,
        &empty,
        ProviderAdapterKind::OpenCodeGo,
        ApiFormat::ChatCompletions,
        "glm-5.2",
        b"{}",
        "req_1",
    );
    assert!(go.get("x-opencode-session").is_some());
    assert!(go.get("x-opencode-client").is_none());

    let mut goat = reqwest::header::HeaderMap::new();
    apply_provider_identity_headers(
        &mut goat,
        &empty,
        ProviderAdapterKind::CommandCodeGoat,
        ApiFormat::ChatCompletions,
        "goat",
        b"{}",
        "req_1",
    );
    assert!(goat.get("x-opencode-session").is_none());
}
