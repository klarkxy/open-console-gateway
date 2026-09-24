use super::{
    OfficialProtocolBaseline, parse_catalog_supported_endpoints_baseline,
    parse_command_code_official_protocols, parse_go_official_protocols, protocol_from_endpoint_url,
};
use crate::kernel::ids::{COMMAND_CODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID};
use crate::provider::UpstreamProtocolKind;

#[test]
fn go_endpoint_table_maps_documented_paths() {
    let html = r#"
<table><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th></tr>
<tr><td>Grok 4.6</td><td>grok-4.6</td><td>https://opencode.ai/zen/go/v1/responses</td><td>@ai-sdk/openai</td></tr>
<tr><td>GLM-5.3</td><td>glm-5.3</td><td>https://opencode.ai/zen/go/v1/chat/completions</td><td>@ai-sdk/openai-compatible</td></tr>
<tr><td>MiMo-V2.6-Flash</td><td>mimo-v2.6-flash</td><td>https://opencode.ai/zen/go/v1/chat/completions</td><td>@ai-sdk/openai-compatible</td></tr>
<tr><td>MiniMax M3</td><td>minimax-m3</td><td>https://opencode.ai/zen/go/v1/messages</td><td>@ai-sdk/anthropic</td></tr>
</table>
"#;
    let map = parse_go_official_protocols(html).unwrap();
    assert_eq!(map.get("grok-4.6"), Some(&UpstreamProtocolKind::Responses));
    assert_eq!(
        map.get("glm-5.3"),
        Some(&UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        map.get("mimo-v2.6-flash"),
        Some(&UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(map.get("minimax-m3"), Some(&UpstreamProtocolKind::Messages));
    let baseline = OfficialProtocolBaseline::mapped(map);
    assert_eq!(
        baseline.protocols_for("opencode", "mimo-v2.6-flash"),
        Some(vec![UpstreamProtocolKind::ChatCompletions])
    );
    assert_ne!(
        baseline.protocols_for("opencode", "mimo-v2.6-flash"),
        Some(vec![
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::Messages
        ])
    );
}

#[test]
fn go_endpoint_table_accepts_extra_header_columns() {
    let html = r#"
<table><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th><th>Notes</th></tr>
<tr><td>Grok 4.6</td><td>grok-4.6</td><td>https://opencode.ai/zen/go/v1/responses</td><td>@ai-sdk/openai</td><td></td></tr>
</table>
"#;
    let map = parse_go_official_protocols(html).unwrap();
    assert_eq!(map.get("grok-4.6"), Some(&UpstreamProtocolKind::Responses));
}

#[test]
fn go_placeholder_endpoints_are_not_protocol_evidence() {
    let html = include_str!("../../tests/fixtures/opencode-go.html");
    assert!(parse_go_official_protocols(html).is_err());
}

#[test]
fn command_code_provider_docs_without_model_table_use_family_rule() {
    let html = r#"
<p>Use /chat/completions for OpenAI and open-source, and most of them on /v1/responses as well.
/messages for Anthropic. Claude models stay on /v1/messages.
Send a Claude model to /chat/completions or /responses and you get a 400.</p>
<table><tr><th>Endpoint</th><th>Method</th><th>Format</th></tr>
<tr><td>https://api.commandcode.ai/provider/v1/chat/completions</td><td>POST</td><td>OpenAI Chat Completions</td></tr>
<tr><td>https://api.commandcode.ai/provider/v1/responses</td><td>POST</td><td>OpenAI Responses</td></tr>
<tr><td>https://api.commandcode.ai/provider/v1/messages</td><td>POST</td><td>Anthropic Messages</td></tr>
</table>
"#;
    let baseline = parse_command_code_official_protocols(html).unwrap();
    assert_eq!(baseline, OfficialProtocolBaseline::FamilyRule);
    assert_eq!(
        baseline.protocol_for(COMMAND_CODE_PROVIDER_ID, "deepseek/deepseek-v4-flash"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "deepseek/deepseek-v4-flash"),
        Some(vec![UpstreamProtocolKind::ChatCompletions])
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "xiaomi/mimo-v2.6-flash"),
        Some(vec![UpstreamProtocolKind::ChatCompletions])
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "claude-sonnet-4-6"),
        Some(vec![UpstreamProtocolKind::Messages])
    );
}

#[test]
fn unrecognized_command_code_html_fails_closed() {
    assert!(parse_command_code_official_protocols("<p>hello</p>").is_err());
}

#[test]
fn omitted_models_and_failed_fetch_supply_no_protocol_evidence() {
    let mapped = OfficialProtocolBaseline::mapped([("grok-4.6", UpstreamProtocolKind::Responses)]);
    assert_eq!(
        mapped.protocol_for("opencode", "grok-4.6"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(mapped.protocol_for("opencode", "future-go-model"), None);
    assert_eq!(
        mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "grok-4.6-free"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "future-free"),
        None
    );
    assert_eq!(
        OfficialProtocolBaseline::Unavailable.protocol_for("opencode", "grok-4.6"),
        None
    );
    assert_eq!(
        OfficialProtocolBaseline::Unavailable
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"),
        None
    );
    assert_eq!(
        OfficialProtocolBaseline::FamilyRule
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"),
        Some(UpstreamProtocolKind::Messages)
    );
    assert_eq!(
        OfficialProtocolBaseline::FamilyRule
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "stealth/ox-alpha"),
        None
    );
    assert_eq!(
        super::known_opencode_default("grok-4.6", false),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        super::known_opencode_default("future-go-model", false),
        None
    );
}

#[test]
fn endpoint_url_parser_accepts_official_go_and_command_paths() {
    assert_eq!(
        protocol_from_endpoint_url("https://opencode.ai/zen/go/v1/responses"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        protocol_from_endpoint_url("https://api.commandcode.ai/provider/v1/chat/completions"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        protocol_from_endpoint_url("https://api.commandcode.ai/provider/v1/messages"),
        Some(UpstreamProtocolKind::Messages)
    );
    assert_eq!(
        protocol_from_endpoint_url("/chat/completions"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        protocol_from_endpoint_url("/responses"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        protocol_from_endpoint_url("/messages"),
        Some(UpstreamProtocolKind::Messages)
    );
    assert_eq!(protocol_from_endpoint_url("x"), None);
    assert_eq!(protocol_from_endpoint_url("/systemone"), None);
}

#[test]
fn goat_catalog_supported_endpoints_keep_per_model_lists() {
    let bytes = br#"{
        "object":"list",
        "data":[
            {"id":"xiaomi/mimo-v2.6-flash","supported_endpoints":["/chat/completions","/responses"]},
            {"id":"claude-sonnet-4-6","supported_endpoints":["/messages"]},
            {"id":"deepseek/deepseek-v4-flash-fast","supported_endpoints":["/chat/completions"]},
            {"id":"typesafe/jev","supported_endpoints":["/systemone"]}
        ]
    }"#;
    let baseline = parse_catalog_supported_endpoints_baseline(bytes);
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "xiaomi/mimo-v2.6-flash"),
        Some(vec![
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses
        ])
    );
    assert_eq!(
        baseline.protocol_for(COMMAND_CODE_PROVIDER_ID, "xiaomi/mimo-v2.6-flash"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "claude-sonnet-4-6"),
        Some(vec![UpstreamProtocolKind::Messages])
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "deepseek/deepseek-v4-flash-fast"),
        Some(vec![UpstreamProtocolKind::ChatCompletions])
    );
    assert_eq!(
        baseline.protocols_for(COMMAND_CODE_PROVIDER_ID, "typesafe/jev"),
        None
    );
}

#[test]
fn goat_catalog_missing_or_unknown_endpoints_do_not_invent_three_protocols() {
    let missing = parse_catalog_supported_endpoints_baseline(
        br#"{"object":"list","data":[{"id":"xiaomi/mimo-v2.6-flash"},{"id":"gpt-5.6-sol"}]}"#,
    );
    assert_eq!(missing, OfficialProtocolBaseline::Unavailable);
    assert_eq!(
        missing.protocols_for(COMMAND_CODE_PROVIDER_ID, "xiaomi/mimo-v2.6-flash"),
        None
    );

    let unknown = parse_catalog_supported_endpoints_baseline(
        br#"{"object":"list","data":[{"id":"typesafe/jev","supported_endpoints":["/systemone"]}]}"#,
    );
    assert_eq!(unknown, OfficialProtocolBaseline::Unavailable);

    let mixed = parse_catalog_supported_endpoints_baseline(
        br#"{
            "object":"list",
            "data":[
                {"id":"gpt-5.6-sol","supported_endpoints":["/chat/completions","/responses"]},
                {"id":"future-open-model"},
                {"id":"broken","supported_endpoints":["/not-a-protocol"]}
            ]
        }"#,
    );
    assert_eq!(
        mixed.protocols_for(COMMAND_CODE_PROVIDER_ID, "gpt-5.6-sol"),
        Some(vec![
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses
        ])
    );
    assert_eq!(
        mixed.protocols_for(COMMAND_CODE_PROVIDER_ID, "future-open-model"),
        None
    );
    assert_eq!(
        mixed.protocols_for(COMMAND_CODE_PROVIDER_ID, "broken"),
        None
    );
    assert_ne!(
        mixed.protocols_for(COMMAND_CODE_PROVIDER_ID, "future-open-model"),
        Some(vec![
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::Messages
        ])
    );
}

#[test]
fn catalog_supported_endpoints_win_over_family_rule_docs() {
    let catalog = parse_catalog_supported_endpoints_baseline(
        br#"{"data":[{"id":"xiaomi/mimo-v2.6-flash","supported_endpoints":["/chat/completions","/responses"]}]}"#,
    );
    let merged = catalog.prefer_catalog(OfficialProtocolBaseline::FamilyRule);
    assert_eq!(
        merged.protocols_for(COMMAND_CODE_PROVIDER_ID, "xiaomi/mimo-v2.6-flash"),
        Some(vec![
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses
        ])
    );
    assert_eq!(
        OfficialProtocolBaseline::Unavailable.prefer_catalog(OfficialProtocolBaseline::FamilyRule),
        OfficialProtocolBaseline::FamilyRule
    );
}
