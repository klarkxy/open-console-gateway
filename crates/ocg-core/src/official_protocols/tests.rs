use super::{
    OfficialProtocolBaseline, parse_command_code_official_protocols, parse_go_official_protocols,
    protocol_from_endpoint_url,
};
use crate::kernel::ids::{COMMAND_CODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID};
use crate::provider::UpstreamProtocolKind;

#[test]
fn go_endpoint_table_maps_documented_paths() {
    let html = r#"
<table><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th></tr>
<tr><td>Grok 4.6</td><td>grok-4.6</td><td>https://opencode.ai/zen/go/v1/responses</td><td>@ai-sdk/openai</td></tr>
<tr><td>GLM-5.3</td><td>glm-5.3</td><td>https://opencode.ai/zen/go/v1/chat/completions</td><td>@ai-sdk/openai-compatible</td></tr>
<tr><td>MiniMax M3</td><td>minimax-m3</td><td>https://opencode.ai/zen/go/v1/messages</td><td>@ai-sdk/anthropic</td></tr>
</table>
"#;
    let map = parse_go_official_protocols(html).unwrap();
    assert_eq!(map.get("grok-4.6"), Some(&UpstreamProtocolKind::Responses));
    assert_eq!(
        map.get("glm-5.3"),
        Some(&UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(map.get("minimax-m3"), Some(&UpstreamProtocolKind::Messages));
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
fn go_placeholder_endpoints_are_skipped_so_apply_defaults_to_chat() {
    let html = include_str!("../../tests/fixtures/opencode-go.html");
    let map = parse_go_official_protocols(html).unwrap();
    assert!(map.is_empty());
}

#[test]
fn command_code_provider_docs_without_model_table_use_family_rule() {
    let html = r#"
<p>Use /chat/completions for OpenAI and open-source. /messages for Anthropic.
Send a Claude model to /chat/completions and you get a 400.</p>
<table><tr><th>Endpoint</th><th>Method</th><th>Format</th></tr>
<tr><td>https://api.commandcode.ai/provider/v1/chat/completions</td><td>POST</td><td>OpenAI Chat Completions</td></tr>
<tr><td>https://api.commandcode.ai/provider/v1/messages</td><td>POST</td><td>Anthropic Messages</td></tr>
</table>
"#;
    assert_eq!(
        parse_command_code_official_protocols(html).unwrap(),
        OfficialProtocolBaseline::FamilyRule
    );
}

#[test]
fn unrecognized_command_code_html_fails_closed() {
    assert!(parse_command_code_official_protocols("<p>hello</p>").is_err());
}

#[test]
fn mapped_miss_and_failed_fetch_default_to_chat() {
    let mapped = OfficialProtocolBaseline::mapped([("grok-4.6", UpstreamProtocolKind::Responses)]);
    assert_eq!(
        mapped.protocol_for("opencode", "grok-4.6"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        mapped.protocol_for("opencode", "future-go-model"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        OfficialProtocolBaseline::FallbackChat.protocol_for("opencode", "grok-4.6"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        OfficialProtocolBaseline::FamilyRule
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"),
        Some(UpstreamProtocolKind::Messages)
    );
    assert_eq!(
        OfficialProtocolBaseline::FamilyRule
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "deepseek/deepseek-v4-flash"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        OfficialProtocolBaseline::FallbackChat
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"),
        Some(UpstreamProtocolKind::ChatCompletions)
    );
    assert_eq!(
        OfficialProtocolBaseline::FallbackChat
            .protocol_for(COMMAND_CODE_PROVIDER_ID, "stealth/ox-alpha"),
        None
    );
    assert_eq!(
        mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "grok-4.6-free"),
        Some(UpstreamProtocolKind::Responses)
    );
    assert_eq!(
        mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "future-free"),
        Some(UpstreamProtocolKind::ChatCompletions)
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
    assert_eq!(protocol_from_endpoint_url("x"), None);
}
