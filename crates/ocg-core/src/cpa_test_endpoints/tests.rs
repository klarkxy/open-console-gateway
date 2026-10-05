use super::{
    EndpointAuthority, EndpointFixtureSlot, OFFICIAL_ENDPOINTS, TEST_ENDPOINTS_ENV,
    complete_instance_map_json, rewrite_url, rewrite_url_with_mapping, slot_from_fallback,
};
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};
use ocg_domain::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_USAGE_URL, OPENCODE_GO_BASE_URL,
    OPENCODE_ZEN_BASE_URL,
};
use std::sync::Arc;

fn mapped(opencode: &str, command_code: &str) -> String {
    format!(r#"{{"opencode":"{opencode}","command-code":"{command_code}"}}"#)
}

fn go_chat() -> String {
    format!("{OPENCODE_GO_BASE_URL}/v1/chat/completions?beta=1")
}

#[cfg(not(feature = "ollama-cloud-loopback-test"))]
#[test]
fn feature_off_ignores_mapping_without_reading_env() {
    let _ = TEST_ENDPOINTS_ENV;
    let mapping = mapped("http://127.0.0.1:18080", "http://127.0.0.1:18081");
    assert_eq!(
        rewrite_url(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL).unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, &go_chat(), Some(&mapping)).unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_USAGE_URL,
            Some("{")
        )
        .unwrap(),
        None
    );
    let native = r#"{"codex":"http://127.0.0.1:9","anthropic":"http://127.0.0.1:9","kimi.com":"http://127.0.0.1:9","kimi.ai":"http://127.0.0.1:9","xai.cli":"http://127.0.0.1:9","xai.api":"http://127.0.0.1:9","antigravity":"http://127.0.0.1:9","cpa":"http://127.0.0.1:9"}"#;
    assert_eq!(
        rewrite_url_with_mapping(
            "codex",
            "https://chatgpt.com/backend-api/codex/responses",
            Some(native),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "xai.cli",
            "https://api.x.ai/v1/responses/compact",
            Some("{\"codex\""),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url(
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent"
        )
        .unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn absent_mapping_defaults_to_none() {
    assert_eq!(
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, None).unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping("minimax", OPENCODE_GO_BASE_URL, Some("   ")).unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, Some("{}")).unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn rewrites_scheme_host_and_port_and_keeps_path_and_query() {
    let mapping = mapped("http://127.0.0.1:18080", "http://[::1]:18081");
    let go = reqwest::Url::parse(
        &rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, &go_chat(), Some(&mapping))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(go.scheme(), "http");
    assert_eq!(go.host_str(), Some("127.0.0.1"));
    assert_eq!(go.port(), Some(18080));
    assert_eq!(go.path(), "/zen/go/v1/chat/completions");
    assert_eq!(go.query(), Some("beta=1"));
    assert!(go.username().is_empty());
    assert!(go.fragment().is_none());

    let usage = reqwest::Url::parse(
        &rewrite_url_with_mapping(
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_USAGE_URL,
            Some(&mapping),
        )
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(usage.host_str(), Some("[::1]"));
    assert_eq!(usage.port(), Some(18081));
    assert_eq!(usage.path(), "/alpha/billing/credits");

    let models = reqwest::Url::parse(
        &rewrite_url_with_mapping(
            COMMAND_CODE_PROVIDER_ID,
            &format!("{COMMAND_CODE_GOAT_BASE_URL}/models"),
            Some(&mapping),
        )
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(models.path(), "/provider/v1/models");
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn rejects_wrong_provider_origin_unknown_key_unapproved_host_and_malformed() {
    let mapping = mapped("http://127.0.0.1:18080", "http://127.0.0.1:18081");
    let wrong = rewrite_url_with_mapping(
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_GOAT_BASE_URL,
        Some(&mapping),
    )
    .unwrap_err();
    assert!(wrong.contains("official origin"), "{wrong}");
    let zen = rewrite_url_with_mapping(
        OPENCODE_PROVIDER_ID,
        &format!("{OPENCODE_ZEN_BASE_URL}/v1/models"),
        Some(&mapping),
    )
    .unwrap_err();
    assert!(zen.contains("official origin"), "{zen}");
    let crossed = rewrite_url_with_mapping(
        COMMAND_CODE_PROVIDER_ID,
        OPENCODE_GO_BASE_URL,
        Some(&mapping),
    )
    .unwrap_err();
    assert!(crossed.contains("official origin"), "{crossed}");

    let unknown_key = rewrite_url_with_mapping(
        OPENCODE_PROVIDER_ID,
        OPENCODE_GO_BASE_URL,
        Some(r#"{"ollama":"http://127.0.0.1:9"}"#),
    )
    .unwrap_err();
    assert!(unknown_key.contains("unknown provider"), "{unknown_key}");

    let unknown_provider =
        rewrite_url_with_mapping("minimax", OPENCODE_GO_BASE_URL, Some(&mapping)).unwrap_err();
    assert!(
        unknown_provider.contains("unknown provider"),
        "{unknown_provider}"
    );

    let unapproved = rewrite_url_with_mapping(
        OPENCODE_PROVIDER_ID,
        OPENCODE_GO_BASE_URL,
        Some(r#"{"opencode":"http://8.8.8.8:9"}"#),
    )
    .unwrap_err();
    assert!(unapproved.contains("HTTP loopback"), "{unapproved}");

    for bad in [
        "{",
        "[]",
        r#"{"opencode":9}"#,
        r#"{"opencode":"http://127.0.0.1"}"#,
        r#"{"opencode":"http://127.0.0.1:0"}"#,
        r#"{"opencode":"https://127.0.0.1:9"}"#,
        r#"{"opencode":"http://user:secret@127.0.0.1:9"}"#,
        r#"{"opencode":"http://127.0.0.1:9/zen"}"#,
        r#"{"opencode":"http://127.0.0.1:9?q=1"}"#,
        r#"{"opencode":"http://127.0.0.1:9#frag"}"#,
        "http://127.0.0.1:9",
    ] {
        let error = rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, Some(bad))
            .unwrap_err();
        assert!(
            error.contains("malformed")
                || error.contains("HTTP loopback")
                || error.contains("unknown provider"),
            "{bad} -> {error}"
        );
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn one_provider_mapping_leaves_the_other_canonical() {
    let only_go = r#"{"opencode":"http://localhost:18080"}"#;
    assert!(
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, Some(only_go))
            .unwrap()
            .unwrap()
            .starts_with("http://localhost:18080/zen/go")
    );
    assert_eq!(
        rewrite_url_with_mapping(
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_BASE_URL,
            Some(only_go)
        )
        .unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn native_map(origin: &str) -> String {
    format!(
        r#"{{"codex":"{origin}","anthropic":"{origin}","kimi.com":"{origin}","kimi.ai":"{origin}","xai.cli":"{origin}","xai.api":"{origin}","antigravity":"{origin}"}}"#
    )
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn rewritten(provider: &str, url: &str, mapping: &str) -> String {
    rewrite_url_with_mapping(provider, url, Some(mapping))
        .unwrap()
        .unwrap()
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn rewrites_one_exact_native_url_per_family() {
    let mapping = native_map("http://127.0.0.1:18080");
    let cases = [
        (
            "codex",
            "https://chatgpt.com/backend-api/codex/responses",
            "/backend-api/codex/responses",
            None,
        ),
        (
            "anthropic",
            "https://api.anthropic.com/v1/messages?beta=true",
            "/v1/messages",
            Some("beta=true"),
        ),
        (
            "kimi.com",
            "https://api.kimi.com/coding/v1/responses",
            "/coding/v1/responses",
            None,
        ),
        (
            "kimi.ai",
            "https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true",
            "/coding/v1/messages/count_tokens",
            Some("beta=true"),
        ),
        (
            "xai.cli",
            "https://cli-chat-proxy.grok.com/v1/responses",
            "/v1/responses",
            None,
        ),
        (
            "xai.api",
            "https://api.x.ai/v1/responses",
            "/v1/responses",
            None,
        ),
        (
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens",
            "/v1internal:countTokens",
            None,
        ),
    ];
    for (provider, url, path, query) in cases {
        let parsed = reqwest::Url::parse(&rewritten(provider, url, &mapping)).unwrap();
        assert_eq!(parsed.scheme(), "http", "{provider}");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"), "{provider}");
        assert_eq!(parsed.port(), Some(18080), "{provider}");
        assert_eq!(parsed.path(), path, "{provider}");
        assert_eq!(parsed.query(), query, "{provider}");
        assert!(parsed.fragment().is_none(), "{provider}");
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn preserves_native_query_and_path_escapes() {
    let mapping = native_map("http://localhost:9");
    assert_eq!(
        rewritten(
            "anthropic",
            "https://api.anthropic.com/v1/messages/count_tokens?beta=true",
            &mapping
        ),
        "http://localhost:9/v1/messages/count_tokens?beta=true"
    );
    assert_eq!(
        rewritten(
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent",
            &mapping
        ),
        "http://localhost:9/v1internal:generateContent"
    );
    assert_eq!(
        rewritten(
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse",
            &mapping
        ),
        "http://localhost:9/v1internal:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "codex",
            "https://chatgpt.com/backend-api/codex/responses%2Fcompact",
            Some(&mapping),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal%3AgenerateContent",
            Some(&mapping),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "anthropic",
            "https://api.anthropic.com/v1/messages?beta=%74rue",
            Some(&mapping),
        )
        .unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn explicit_port_80_is_accepted_and_implicit_zero_and_bad_ports_are_refused() {
    let mapping = r#"{"opencode":"http://127.0.0.1:80","command-code":"http://[::1]:80","codex":"http://localhost:80"}"#;
    let go = reqwest::Url::parse(&rewritten(OPENCODE_PROVIDER_ID, &go_chat(), mapping)).unwrap();
    assert_eq!(go.scheme(), "http");
    assert_eq!(go.host_str(), Some("127.0.0.1"));
    assert_eq!(go.port(), None);
    assert_eq!(go.path(), "/zen/go/v1/chat/completions");
    assert_eq!(go.query(), Some("beta=1"));

    let usage = reqwest::Url::parse(&rewritten(
        COMMAND_CODE_PROVIDER_ID,
        COMMAND_CODE_GOAT_USAGE_URL,
        mapping,
    ))
    .unwrap();
    assert_eq!(usage.host_str(), Some("[::1]"));
    assert_eq!(usage.port(), None);
    assert_eq!(usage.path(), "/alpha/billing/credits");

    let codex = reqwest::Url::parse(&rewritten(
        "codex",
        "https://chatgpt.com/backend-api/codex/responses",
        mapping,
    ))
    .unwrap();
    assert_eq!(codex.host_str(), Some("localhost"));
    assert_eq!(codex.port(), None);
    assert_eq!(codex.path(), "/backend-api/codex/responses");

    for bad in [
        "http://127.0.0.1",
        "http://127.0.0.1/",
        "http://localhost",
        "http://localhost/",
        "http://[::1]",
        "http://[::1]/",
        "http://127.0.0.1:0",
        "http://localhost:0",
        "http://[::1]:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:99999",
        "http://[::1]:99999",
        "http://127.0.0.1:80a",
        "http://127.0.0.1:",
        "http://[::1]:",
        "http://8.8.8.8:80",
        "http://user:secret@[::1]:80",
        "http://[::1]:80/codex",
        "http://[::1]:80?q=1",
        "http://127.0.0.1:80#frag",
    ] {
        let mapped = format!(r#"{{"codex":"{bad}"}}"#);
        let error = rewrite_url_with_mapping(
            "codex",
            "https://chatgpt.com/backend-api/codex/responses",
            Some(&mapped),
        )
        .unwrap_err();
        assert!(error.contains("HTTP loopback"), "{bad} -> {error}");
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn rewrites_native_ipv6_origin() {
    let mapping = native_map("http://[::1]:18081");
    assert_eq!(
        rewritten(
            "codex",
            "https://chatgpt.com/backend-api/codex/responses/compact",
            &mapping
        ),
        "http://[::1]:18081/backend-api/codex/responses/compact"
    );
    assert_eq!(
        rewritten(
            "kimi.com",
            "https://api.kimi.com/coding/v1/messages?beta=true",
            &mapping
        ),
        "http://[::1]:18081/coding/v1/messages?beta=true"
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn partial_native_map_leaves_other_families_sealed() {
    let only_kimi = r#"{"kimi.ai":"http://127.0.0.1:9"}"#;
    assert_eq!(
        rewritten(
            "kimi.ai",
            "https://api.kimi.ai/coding/v1/chat/completions",
            only_kimi
        ),
        "http://127.0.0.1:9/coding/v1/chat/completions"
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "kimi.com",
            "https://api.kimi.com/coding/v1/chat/completions",
            Some(only_kimi),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "codex",
            "https://chatgpt.com/backend-api/codex/responses",
            Some(only_kimi),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, Some(only_kimi))
            .unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn combined_map_rewrites_native_go_and_command_code() {
    let mapping = r#"{"opencode":"http://127.0.0.1:18080","command-code":"http://127.0.0.1:18081","xai.cli":"http://localhost:18082","xai.api":"http://127.0.0.1:18083"}"#;
    assert!(
        rewritten(OPENCODE_PROVIDER_ID, &go_chat(), mapping)
            .starts_with("http://127.0.0.1:18080/zen/go/")
    );
    assert_eq!(
        reqwest::Url::parse(&rewritten(
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_USAGE_URL,
            mapping
        ))
        .unwrap()
        .port(),
        Some(18081)
    );
    assert_eq!(
        rewritten("xai.cli", "https://api.x.ai/v1/responses/compact", mapping),
        "http://localhost:18082/v1/responses/compact"
    );
    assert_eq!(
        rewritten("xai.api", "https://api.x.ai/v1/responses/compact", mapping),
        "http://127.0.0.1:18083/v1/responses/compact"
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "xai.api",
            "https://cli-chat-proxy.grok.com/v1/responses",
            Some(mapping)
        )
        .unwrap(),
        None
    );
    assert_eq!(
        rewrite_url_with_mapping(
            "xai.cli",
            "https://api.x.ai/v1/responses/compact",
            Some(r#"{"xai.api":"http://127.0.0.1:9"}"#),
        )
        .unwrap(),
        None
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn refuses_malformed_unknown_duplicate_and_invalid_native_origin() {
    let codex = "https://chatgpt.com/backend-api/codex/responses";
    for bad in [
        "{",
        "[]",
        r#"{"codex":9}"#,
        r#"{"cpa":"http://127.0.0.1:9"}"#,
        r#"{"base_url":"http://127.0.0.1:9"}"#,
        r#"{"codex":"http://127.0.0.1:9","cpa":"http://127.0.0.1:8"}"#,
        r#"{"codex":"http://8.8.8.8:9"}"#,
        r#"{"codex":"https://127.0.0.1:9"}"#,
        r#"{"codex":"http://127.0.0.1:0"}"#,
        r#"{"codex":"http://127.0.0.1:9/codex"}"#,
        r#"{"codex":"http://127.0.0.1:9?q=1"}"#,
    ] {
        let error = rewrite_url_with_mapping("codex", codex, Some(bad)).unwrap_err();
        assert!(
            error.contains("malformed")
                || error.contains("HTTP loopback")
                || error.contains("unknown provider"),
            "{bad} -> {error}"
        );
        let crossed =
            rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL, Some(bad))
                .unwrap_err();
        assert!(
            crossed.contains("malformed")
                || crossed.contains("HTTP loopback")
                || crossed.contains("unknown provider"),
            "{bad} -> {crossed}"
        );
    }
    let duplicate = r#"{"codex":"http://127.0.0.1:9","codex":"http://127.0.0.1:10"}"#;
    let error = rewrite_url_with_mapping("codex", codex, Some(duplicate)).unwrap_err();
    assert!(error.contains("duplicate key"), "{error}");
    let opencode_duplicate =
        r#"{"opencode":"http://127.0.0.1:9","opencode":"http://127.0.0.1:10"}"#;
    let error = rewrite_url_with_mapping(
        OPENCODE_PROVIDER_ID,
        OPENCODE_GO_BASE_URL,
        Some(opencode_duplicate),
    )
    .unwrap_err();
    assert!(error.contains("duplicate key"), "{error}");
    let unknown = rewrite_url_with_mapping("cpa", codex, Some(&native_map("http://127.0.0.1:9")))
        .unwrap_err();
    assert!(unknown.contains("unknown provider"), "{unknown}");
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn same_host_wrong_path_or_query_stays_sealed() {
    let mapping = native_map("http://127.0.0.1:9");
    for (provider, url) in [
        (
            "codex",
            "https://chatgpt.com/backend-api/codex/responses/extra",
        ),
        ("codex", "https://chatgpt.com/backend-api/codex"),
        (
            "codex",
            "https://chatgpt.com:443/backend-api/codex/responses",
        ),
        ("anthropic", "https://api.anthropic.com/v1/messages"),
        (
            "anthropic",
            "https://api.anthropic.com/v1/messages?beta=false",
        ),
        (
            "anthropic",
            "https://api.anthropic.com/v1/messages?beta=true&x=1",
        ),
        (
            "kimi.com",
            "https://api.kimi.com/coding/v1/chat/completions?beta=true",
        ),
        (
            "kimi.ai",
            "https://api.kimi.ai/coding/v1/messages?beta=true&alt=sse",
        ),
        (
            "xai.cli",
            "https://cli-chat-proxy.grok.com/v1/responses?beta=true",
        ),
        ("xai.api", "https://api.x.ai/v1/responses/compact?alt=sse"),
        (
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent",
        ),
        (
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=json",
        ),
        (
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent/extra",
        ),
    ] {
        assert_eq!(
            rewrite_url_with_mapping(provider, url, Some(&mapping)).unwrap(),
            None,
            "{provider} {url}"
        );
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn zen_chat() -> String {
    format!("{OPENCODE_ZEN_BASE_URL}/v1/chat/completions?beta=1")
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn rewrites_zen_without_accepting_go_prefix() {
    let mapping = complete_instance_map_json(
        "http://127.0.0.1:18080",
        "http://127.0.0.1:18081",
        "http://127.0.0.1:18082",
    );
    let zen = reqwest::Url::parse(
        &rewrite_url_with_mapping(OPENCODE_ZEN_FREE_PROVIDER_ID, &zen_chat(), Some(&mapping))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(zen.scheme(), "http");
    assert_eq!(zen.host_str(), Some("127.0.0.1"));
    assert_eq!(zen.port(), Some(18082));
    assert_eq!(zen.path(), "/zen/v1/chat/completions");
    assert_eq!(zen.query(), Some("beta=1"));

    let go_as_zen =
        rewrite_url_with_mapping(OPENCODE_ZEN_FREE_PROVIDER_ID, &go_chat(), Some(&mapping))
            .unwrap_err();
    assert!(go_as_zen.contains("official origin"), "{go_as_zen}");

    let zen_as_go =
        rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, &zen_chat(), Some(&mapping)).unwrap_err();
    assert!(zen_as_go.contains("official origin"), "{zen_as_go}");

    let go = reqwest::Url::parse(
        &rewrite_url_with_mapping(OPENCODE_PROVIDER_ID, &go_chat(), Some(&mapping))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(go.path(), "/zen/go/v1/chat/completions");
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn instance_fixture_requires_go_goat_and_zen() {
    let slot = EndpointFixtureSlot::capture_child_environment();
    let missing = slot
        .install_instance(
            r#"{"opencode":"http://127.0.0.1:9","command-code":"http://127.0.0.1:9"}"#,
        )
        .unwrap_err();
    assert!(missing.contains("missing a required provider"), "{missing}");
    assert!(slot.authority().is_err());
    assert!(
        slot.install_instance(&complete_instance_map_for_test())
            .unwrap_err()
            .contains("already sealed")
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn complete_instance_map_for_test() -> String {
    complete_instance_map_json(
        "http://127.0.0.1:18080",
        "http://127.0.0.1:18081",
        "http://127.0.0.1:18082",
    )
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn authority_before_render_does_not_seal_absence() {
    let slot = EndpointFixtureSlot::capture_child_environment();
    let before = slot.authority().unwrap();
    assert!(matches!(*before, EndpointAuthority::Official));
    slot.install_instance(&complete_instance_map_for_test())
        .unwrap();
    let installed = slot.authority().unwrap();
    assert!(matches!(*installed, EndpointAuthority::InstanceFixture(_)));
    let rewritten = installed
        .rewrite_url(OPENCODE_ZEN_FREE_PROVIDER_ID, &zen_chat())
        .unwrap()
        .unwrap();
    assert!(rewritten.starts_with("http://127.0.0.1:18082/zen/"));
    assert!(!rewritten.contains("/zen/go/"));
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn first_render_seals_fallback_and_rejects_late_install() {
    let slot = EndpointFixtureSlot::capture_child_environment();
    let sealed = slot.seal_for_render().unwrap();
    assert!(matches!(*sealed, EndpointAuthority::Official));
    assert!(
        slot.install_instance(&complete_instance_map_for_test())
            .unwrap_err()
            .contains("already sealed")
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn poisoned_fallback_cannot_be_hidden_by_instance_map() {
    let slot = slot_from_fallback(Err("poisoned child environment".into()));
    let error = slot
        .install_instance(&complete_instance_map_for_test())
        .unwrap_err();
    assert!(error.contains("poisoned"), "{error}");
    assert_eq!(slot.authority().unwrap_err(), error);
    assert_eq!(slot.seal_for_render().unwrap_err(), error);
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn racing_installs_share_one_once_lock_winner() {
    let slot = Arc::new(EndpointFixtureSlot::capture_child_environment());
    let map = complete_instance_map_for_test();
    let mut joins = Vec::new();
    for _ in 0..8 {
        let slot = Arc::clone(&slot);
        let map = map.clone();
        joins.push(std::thread::spawn(move || slot.install_instance(&map)));
    }
    let mut ok = 0;
    let mut rejected = 0;
    for join in joins {
        match join.join().unwrap() {
            Ok(()) => ok += 1,
            Err(error) => {
                assert!(error.contains("already sealed"), "{error}");
                rejected += 1;
            }
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(rejected, 7);
    assert!(matches!(
        *slot.seal_for_render().unwrap(),
        EndpointAuthority::InstanceFixture(_)
    ));
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn instance_rewrite_fails_closed_when_a_participant_is_absent_from_the_map() {
    let child = super::parse_child_authority(r#"{"opencode":"http://127.0.0.1:9"}"#).unwrap();
    assert_eq!(
        child
            .rewrite_url(OPENCODE_ZEN_FREE_PROVIDER_ID, &zen_chat())
            .unwrap(),
        None
    );
    let slot = EndpointFixtureSlot::capture_child_environment();
    slot.install_instance(&complete_instance_map_for_test())
        .unwrap();
    let instance = slot.authority().unwrap();
    let missing = instance
        .rewrite_url("minimax", OPENCODE_GO_BASE_URL)
        .unwrap_err();
    assert!(missing.contains("unknown provider"), "{missing}");
    let _ = &OFFICIAL_ENDPOINTS;
}

#[cfg(not(feature = "ollama-cloud-loopback-test"))]
#[test]
fn feature_off_installer_is_ineffective() {
    let slot = EndpointFixtureSlot::capture_child_environment();
    assert!(
        slot.install_instance(&complete_instance_map_json(
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
        ))
        .unwrap_err()
        .contains("not enabled")
    );
    assert!(matches!(
        *slot.seal_for_render().unwrap(),
        EndpointAuthority::Official
    ));
    assert_eq!(
        OFFICIAL_ENDPOINTS
            .rewrite_url(OPENCODE_PROVIDER_ID, OPENCODE_GO_BASE_URL)
            .unwrap(),
        None
    );
}
