use super::{
    AppConfig, CLAUDE_DESKTOP_HAIKU_ALIAS, CLAUDE_DESKTOP_OPUS_ALIAS, CLAUDE_DESKTOP_SONNET_ALIAS,
    ClaudeDesktopModels, DEFAULT_OPENCODE_INVITE_URL, MAX_ACCOUNT_NOTES_CHARS, ProxyListDirection,
    ProxyMode, RoutingMode, normalize_account_notes, normalize_opencode_invite_url,
    normalize_proxy_url, normalize_purchase_date, purchase_expires_on,
};

#[test]
fn claude_desktop_models_map_aliases_and_inherit_by_role_priority() {
    let models = ClaudeDesktopModels {
        sonnet: String::new(),
        opus: "glm-5.2".to_string(),
        haiku: "mimo-v2.5".to_string(),
    };

    assert_eq!(
        models.model_for_alias(CLAUDE_DESKTOP_SONNET_ALIAS),
        Some("glm-5.2")
    );
    assert_eq!(
        models.model_for_alias(CLAUDE_DESKTOP_OPUS_ALIAS),
        Some("glm-5.2")
    );
    assert_eq!(
        models.model_for_alias(CLAUDE_DESKTOP_HAIKU_ALIAS),
        Some("mimo-v2.5")
    );
    assert_eq!(models.model_for_alias("claude-unknown"), None);
}

#[test]
fn claude_desktop_models_reject_unknown_and_all_empty_values() {
    let empty = ClaudeDesktopModels {
        sonnet: String::new(),
        opus: String::new(),
        haiku: String::new(),
    };
    assert!(empty.validate().is_err());

    let unknown = ClaudeDesktopModels {
        sonnet: "not-a-supported-model".to_string(),
        ..ClaudeDesktopModels::default()
    };
    assert!(unknown.validate().is_err());
    assert!(ClaudeDesktopModels::default().validate().is_ok());
}

#[test]
fn account_notes_trim_empty_and_reject_overlong() {
    assert_eq!(normalize_account_notes("").unwrap(), None);
    assert_eq!(normalize_account_notes("   ").unwrap(), None);
    assert_eq!(
        normalize_account_notes("  keep this  ").unwrap().as_deref(),
        Some("keep this")
    );
    let overlong = "n".repeat(MAX_ACCOUNT_NOTES_CHARS + 1);
    assert!(normalize_account_notes(&overlong).is_err());
    let max = "你".repeat(MAX_ACCOUNT_NOTES_CHARS);
    assert_eq!(
        normalize_account_notes(&max).unwrap().as_deref(),
        Some(max.as_str())
    );
}

#[test]
fn purchase_dates_require_canonical_calendar_dates() {
    assert_eq!(
        normalize_purchase_date("2026-07-15").expect("valid date should normalize"),
        "2026-07-15"
    );
    for invalid in ["2026-7-15", " 2026-07-15", "2026-07-15 ", "2026-02-29", ""] {
        assert!(
            normalize_purchase_date(invalid).is_err(),
            "{invalid:?} should be rejected"
        );
    }
}

#[test]
fn purchase_expiry_uses_the_next_natural_month() {
    for (purchase, expected) in [
        ("2026-01-15", "2026-02-15"),
        ("2026-01-31", "2026-02-28"),
        ("2024-01-31", "2024-02-29"),
        ("2024-02-29", "2024-03-29"),
        ("2026-12-31", "2027-01-31"),
    ] {
        assert_eq!(
            purchase_expires_on(purchase).expect("valid date should have an expiry"),
            expected
        );
    }
}

#[test]
fn routing_mode_defaults_and_rejects_unknown_values() {
    let missing: AppConfig = serde_json::from_value(serde_json::json!({
        "gateway_key": "k"
    }))
    .expect("missing routing fields should default");
    assert_eq!(missing.routing_mode, RoutingMode::StrictPriority);
    assert!(!missing.conversation_sticky);
    assert_eq!(missing.proxy_mode, ProxyMode::Auto);
    assert!(missing.proxy_url.is_empty());

    for mode in [
        RoutingMode::StrictPriority,
        RoutingMode::StickyGlobal,
        RoutingMode::RoundRobin,
    ] {
        let config = AppConfig {
            routing_mode: mode,
            conversation_sticky: true,
            gateway_key: "k".into(),
            ..AppConfig::default()
        };
        config.validate().expect("valid routing config");
        let encoded = serde_json::to_value(&config).expect("serialize");
        let decoded: AppConfig =
            serde_json::from_value(encoded).expect("round-trip routing config");
        assert_eq!(decoded.routing_mode, mode);
        assert!(decoded.conversation_sticky);
    }

    assert!(
        serde_json::from_value::<AppConfig>(serde_json::json!({
            "gateway_key": "k",
            "routing_mode": "weighted"
        }))
        .is_err()
    );
}

#[test]
fn legacy_config_json_with_gateway_keys_list_keeps_the_scalar_key() {
    // Config JSON written by the never-released PR #43 form embeds a
    // `gateway_keys` list; current builds ignore it and keep the legacy
    // scalar, so downgraded databases stay readable either way.
    let legacy: AppConfig = serde_json::from_value(serde_json::json!({
        "gateway_key": "ocg-legacy-key",
        "gateway_keys": [
            {
                "id": "key-1",
                "name": "Primary",
                "key": "ocg-legacy-key",
                "enabled": true,
                "created_at": "2026-08-16T00:00:00Z"
            }
        ],
        "upstream_base_url": "https://opencode.ai/zen/go"
    }))
    .expect("legacy config with an embedded key list should deserialize");
    assert_eq!(legacy.gateway_key, "ocg-legacy-key");
    legacy
        .validate()
        .expect("the scalar key satisfies validation");

    let encoded = serde_json::to_value(&AppConfig {
        gateway_key: "ocg-keep".into(),
        ..AppConfig::default()
    })
    .expect("config should serialize");
    assert!(encoded.get("gateway_keys").is_none());
}

#[test]
fn blank_primary_key_is_rejected_by_validate() {
    for blank in ["", "   ", "\t"] {
        let config = AppConfig {
            gateway_key: blank.to_string(),
            ..AppConfig::default()
        };
        assert_eq!(
            config.validate().unwrap_err(),
            "key is required",
            "{blank:?} must be rejected"
        );
    }
    AppConfig {
        gateway_key: "  padded  ".into(),
        ..AppConfig::default()
    }
    .validate()
    .expect("a non-blank key passes");
}

#[test]
fn proxy_url_requires_a_supported_origin_without_credentials() {
    assert_eq!(
        normalize_proxy_url(ProxyMode::Manual, " http://127.0.0.1:7890/ ").unwrap(),
        "http://127.0.0.1:7890"
    );
    assert_eq!(
        normalize_proxy_url(ProxyMode::Auto, "").unwrap(),
        String::new()
    );
    assert_eq!(
        normalize_proxy_url(ProxyMode::Auto, " http://127.0.0.1:7890/ ").unwrap(),
        "http://127.0.0.1:7890"
    );
    assert_eq!(
        normalize_proxy_url(ProxyMode::Direct, "socks5://127.0.0.1:1080").unwrap(),
        "socks5://127.0.0.1:1080"
    );
    assert!(normalize_proxy_url(ProxyMode::Manual, "").is_err());
    for invalid in [
        "socks5://127.0.0.1:1080",
        "http://user:secret@127.0.0.1:7890",
        "http://127.0.0.1:7890/proxy",
        "http://127.0.0.1:7890?x=1",
    ] {
        assert!(
            normalize_proxy_url(ProxyMode::Manual, invalid).is_err(),
            "{invalid:?} should be rejected"
        );
    }

    AppConfig {
        gateway_key: "k".to_string(),
        proxy_mode: ProxyMode::Auto,
        proxy_url: "not-a-proxy".to_string(),
        ..AppConfig::default()
    }
    .validate()
    .expect("auto mode must not reject leftover invalid proxy URLs");
}

#[test]
fn list_proxy_mode_requires_a_valid_proxy_url_but_not_a_valid_list() {
    let mut config = AppConfig {
        gateway_key: "k".to_string(),
        proxy_mode: ProxyMode::List,
        proxy_url: String::new(),
        ..AppConfig::default()
    };
    assert_eq!(
        config.validate().unwrap_err(),
        "list proxy mode requires a proxy URL"
    );
    config.proxy_url = "http://127.0.0.1:7890".to_string();
    // validate() must stay self-contained: an empty list or unknown ids are
    // write-gate concerns and must never block the load path.
    config.proxy_list_models = Vec::new();
    config
        .validate()
        .expect("empty list must not block validate");
    config.proxy_list_models = vec!["not-a-known-model".to_string()];
    config
        .validate()
        .expect("unknown list ids must not block validate");

    assert!(normalize_proxy_url(ProxyMode::List, "socks5://127.0.0.1:1080").is_err());
}

#[test]
fn non_list_modes_keep_list_fields_untouched() {
    let config = AppConfig {
        gateway_key: "k".to_string(),
        proxy_list_direction: ProxyListDirection::Blacklist,
        proxy_list_models: vec!["gpt-5.6-luna".to_string(), "grok-4.5".to_string()],
        ..AppConfig::default()
    };
    config
        .validate()
        .expect("auto mode with list leftovers passes");
    assert_eq!(config.proxy_list_direction, ProxyListDirection::Blacklist);
    assert_eq!(config.proxy_list_models.len(), 2);
}

#[test]
fn proxy_mode_and_direction_serde_round_trip() {
    assert_eq!(
        serde_json::to_value(ProxyMode::List).unwrap(),
        serde_json::json!("list")
    );
    assert_eq!(
        serde_json::to_value(ProxyListDirection::Blacklist).unwrap(),
        serde_json::json!("blacklist")
    );
    assert_eq!(
        serde_json::from_value::<ProxyListDirection>(serde_json::json!("whitelist")).unwrap(),
        ProxyListDirection::Whitelist
    );
}

#[test]
fn legacy_config_without_list_fields_loads_with_defaults() {
    let legacy = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": "ocg-keep",
        "upstream_base_url": "https://opencode.ai/zen/go",
        "proxy_mode": "manual",
        "proxy_url": "http://127.0.0.1:7890",
        "opencode_invite_url": DEFAULT_OPENCODE_INVITE_URL,
        "client_root_url": "",
        "auto_start": false,
        "show_dock_icon": true,
        "connect_timeout_secs": 30,
        "non_stream_timeout_secs": 900,
        "stream_idle_timeout_secs": 300,
        "routing_mode": "strict-priority",
        "conversation_sticky": false,
        "free_model_routing": "explicit",
        "claude_desktop_models": {
            "sonnet": "minimax-m3",
            "opus": "",
            "haiku": ""
        }
    });
    let config: AppConfig = serde_json::from_value(legacy).expect("legacy config loads");
    assert_eq!(config.proxy_list_direction, ProxyListDirection::Whitelist);
    assert!(config.proxy_list_models.is_empty());
    for mode in [ProxyMode::Auto, ProxyMode::Manual, ProxyMode::Direct] {
        let mut legacy_config = config.clone();
        legacy_config.proxy_mode = mode;
        legacy_config
            .validate()
            .expect("legacy three-mode behavior is unchanged");
    }
}

#[test]
fn list_mode_deserialization_fails_loudly_without_serde_other() {
    // D8: no #[serde(other)] fallback — an older binary must fail to start
    // on a "list" config instead of silently routing restricted models
    // directly.
    let encoded = serde_json::json!("list");
    // This build knows the variant, so it decodes; the fail-loud contract
    // is about older builds lacking it, asserted via raw JSON round trip.
    assert_eq!(
        serde_json::from_value::<ProxyMode>(encoded).unwrap(),
        ProxyMode::List
    );
    assert!(serde_json::from_value::<ProxyMode>(serde_json::json!("unknown-mode")).is_err());
}

#[test]
fn persisted_list_with_stale_ids_loads_and_never_matches() {
    // Registry-shrink tolerance: the load path only needs a URL; stale ids
    // and empty lists resolve to "no match" inside the route set (covered
    // by http_client tests), never to a startup failure.
    let config: AppConfig = serde_json::from_value(serde_json::json!({
        "gateway_key": "k",
        "proxy_mode": "list",
        "proxy_url": "http://127.0.0.1:7890",
        "proxy_list_direction": "whitelist",
        "proxy_list_models": ["gpt-5.6-luna", "removed-model"],
        "claude_desktop_models": { "sonnet": "minimax-m3", "opus": "", "haiku": "" }
    }))
    .expect("stale list entries must load");
    config
        .validate()
        .expect("validate only checks the self-contained URL invariant");
    assert!(
        config
            .proxy_list_models
            .contains(&"removed-model".to_string())
    );
}

#[test]
fn default_opencode_invite_url_is_allowlisted() {
    assert_eq!(
        normalize_opencode_invite_url(DEFAULT_OPENCODE_INVITE_URL).unwrap(),
        DEFAULT_OPENCODE_INVITE_URL
    );
    assert_eq!(
        AppConfig::default().opencode_invite_url,
        DEFAULT_OPENCODE_INVITE_URL
    );
}

#[test]
fn opencode_invite_url_is_https_and_host_allowlisted() {
    assert_eq!(normalize_opencode_invite_url("  ").unwrap(), "");
    assert_eq!(
        normalize_opencode_invite_url("https://opencode.ai/invite/test").unwrap(),
        "https://opencode.ai/invite/test"
    );
    assert!(normalize_opencode_invite_url("https://console.opencode.ai/invite?id=1").is_ok());
    for invalid in [
        "http://opencode.ai/invite/test",
        "https://opencode.ai.evil.test/invite",
        "https://user:pass@opencode.ai/invite",
        "https://example.com/invite",
        "not-a-url",
    ] {
        assert!(
            normalize_opencode_invite_url(invalid).is_err(),
            "accepted unsafe invite URL {invalid:?}"
        );
    }
    assert!(
        normalize_opencode_invite_url(&format!("https://opencode.ai/{}", "x".repeat(2049)))
            .is_err()
    );
}
