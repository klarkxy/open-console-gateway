use super::*;

#[test]
fn official_support_matches_each_model_preference() {
    for profile in MODEL_PROTOCOLS {
        if profile.supported.is_empty() {
            continue;
        }
        assert!(
            profile.supported.contains(&profile.preferred),
            "{} preferred must be in supported",
            profile.id
        );
    }
    // 2026-08-27 Go live_supported matrix: extra Chat/Messages/Responses
    // paths stay available alongside the official preferred endpoint.
    assert!(opencode_supports_upstream(
        "deepseek-v4-flash",
        ApiFormat::ChatCompletions
    ));
    assert!(opencode_supports_upstream(
        "deepseek-v4-flash",
        ApiFormat::Responses
    ));
    assert!(opencode_supports_upstream(
        "deepseek-v4-flash",
        ApiFormat::Messages
    ));
    assert!(opencode_supports_upstream("kimi-k3", ApiFormat::Messages));
    assert!(!opencode_supports_upstream("kimi-k3", ApiFormat::Responses));
    assert!(opencode_supports_upstream(
        "minimax-m3",
        ApiFormat::ChatCompletions
    ));
    assert!(!opencode_supports_upstream(
        "grok-4.6",
        ApiFormat::ChatCompletions
    ));
}

#[test]
fn command_code_family_rules_split_anthropic_from_chat() {
    assert!(command_code_is_anthropic_model("claude-sonnet-4-6"));
    assert!(command_code_is_anthropic_model("anthropic/claude-opus-4-6"));
    assert!(command_code_is_anthropic_model("Claude-Haiku-4-5"));
    assert!(!command_code_is_anthropic_model(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    ));
    assert!(!command_code_is_anthropic_model("gpt-5.4"));
    assert_eq!(
        command_code_preferred_format("claude-sonnet-4-6"),
        Some(ApiFormat::Messages)
    );
    assert_eq!(
        command_code_preferred_format(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
        Some(ApiFormat::ChatCompletions)
    );
    assert!(command_code_supports_upstream(
        "claude-sonnet-4-6",
        ApiFormat::Messages
    ));
    assert!(!command_code_supports_upstream(
        "claude-sonnet-4-6",
        ApiFormat::ChatCompletions
    ));
    assert!(command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::ChatCompletions
    ));
    assert!(command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::Responses
    ));
    assert!(!command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::Messages
    ));
    assert_eq!(
        command_code_constructable_formats(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
        CHAT_AND_RESPONSES
    );
    assert_eq!(
        command_code_constructable_formats("claude-sonnet-4-6"),
        MESSAGES_ONLY
    );
    assert_eq!(
        command_code_constructable_formats("xiaomi/mimo-v2.6-flash"),
        CHAT_AND_RESPONSES
    );
    assert!(command_code_constructable_formats("minimax-m2.7").contains(&ApiFormat::Responses));
    assert!(command_code_supported_formats("minimax-m2.7").is_empty());
    assert!(command_code_supported_formats("xiaomi/mimo-v2.6-flash").is_empty());
    assert!(!command_code_supports_upstream(
        "xiaomi/mimo-v2.6-flash",
        ApiFormat::ChatCompletions
    ));
    assert!(!command_code_supports_upstream(
        "xiaomi/mimo-v2.6-flash",
        ApiFormat::Responses
    ));
    assert_eq!(
        command_code_preferred_format("xiaomi/mimo-v2.6-flash"),
        Some(ApiFormat::ChatCompletions)
    );
    assert_eq!(
        command_code_upstream_path(ApiFormat::ChatCompletions),
        Some("/chat/completions")
    );
    assert_eq!(
        command_code_upstream_path(ApiFormat::Responses),
        Some("/responses")
    );
    assert_eq!(
        command_code_upstream_path(ApiFormat::Messages),
        Some("/messages")
    );
    assert_eq!(command_code_upstream_path(ApiFormat::Gemini), None);
    assert!(!command_code_supports_upstream(
        "",
        ApiFormat::ChatCompletions
    ));
    assert!(
        command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).is_none(),
        "kebab Go aliases must not resolve through the Command Code seed table"
    );
}

#[test]
fn ollama_cloud_seed_is_locked_and_never_enters_model_protocols() {
    for profile in OLLAMA_CLOUD_PROTOCOL_SEED {
        assert!(!profile.id.contains(':') || profile.id.starts_with("gpt-oss:"));
    }
    assert!(ollama_cloud_model_protocol("deepseek-v4-flash").is_some());
    assert!(ollama_cloud_model_protocol("DeepSeek-V4-Pro").is_some());
    assert!(ollama_cloud_model_protocol("gpt-oss:120b").is_some());
    assert!(!ollama_cloud_includes_model("deepseek-v4-flash:0731"));
    assert!(ollama_cloud_includes_model("deepseek-v4-flash"));
    assert_eq!(ollama_cloud_supported_formats("brand-new:0915"), CHAT_ONLY);
    assert!(ollama_cloud_supported_formats("").is_empty());
    assert!(ollama_cloud_supports_upstream(
        "gpt-oss:20b",
        ApiFormat::ChatCompletions
    ));
    assert!(!ollama_cloud_supports_upstream(
        "gpt-oss:20b",
        ApiFormat::Responses
    ));
    assert!(!ollama_cloud_supports_upstream(
        "deepseek-v4-flash",
        ApiFormat::Messages
    ));
    assert_eq!(
        ollama_cloud_shared_alias_stems(),
        vec!["deepseek-v4-flash", "deepseek-v4-pro"]
    );
    for ollama_only in ["gpt-oss:20b", "gpt-oss:120b"] {
        assert!(
            !supported_model_ids().any(|id| id == ollama_only),
            "MODEL_PROTOCOLS must stay free of Ollama-only ids ({ollama_only})"
        );
    }
}
