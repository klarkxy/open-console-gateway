use super::*;

fn synthesis() -> ResponseSynthesis {
    ResponseSynthesis {
        created_at: 1_700_000_123,
        empty_response_id: "resp_fixedempty".to_string(),
    }
}

fn convert_req(client: ApiFormat, upstream: ApiFormat, body: Value) -> ConvertedRequestJson {
    convert_request_json(client, upstream, body).expect("request should convert")
}

fn convert_resp(
    upstream: ApiFormat,
    client: ApiFormat,
    body: &Value,
    custom_tools: &[String],
    namespace_tools: &[NamespaceToolMapping],
    model_hint: Option<&str>,
) -> Value {
    convert_response_json(
        upstream,
        client,
        body,
        custom_tools,
        namespace_tools,
        synthesis(),
        model_hint,
    )
    .expect("response should convert")
    .body
}

#[test]
fn convert_request_json_does_not_probe_unknown_models() {
    let converted = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "not-a-catalog-model",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        }),
    );
    assert_eq!(converted.body["model"], "not-a-catalog-model");
    assert_eq!(converted.body["messages"][0]["role"], "user");
    assert!(converted.namespace_tools.is_empty());
}

#[test]
fn convert_request_json_preserves_exact_validation_strings() {
    let store = convert_request_json(
        ApiFormat::Responses,
        ApiFormat::Responses,
        json!({"model": "any", "input": "hi"}),
    )
    .expect_err("store=false is required");
    assert_eq!(
        store.message,
        "this stateless gateway requires Responses store=false"
    );

    let previous = convert_request_json(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "previous_response_id": "resp_1"
        }),
    )
    .expect_err("stateful previous_response_id is rejected");
    assert_eq!(
        previous.message,
        "Responses previous_response_id is not supported by this stateless gateway"
    );

    let format = convert_request_json(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {"type": "json_object"}
        }),
    )
    .expect_err("structured output cannot convert");
    assert_eq!(
        format.message,
        "Chat Completions response_format cannot be preserved by protocol conversion"
    );

    let gemini_upstream = convert_request_json(
        ApiFormat::ChatCompletions,
        ApiFormat::Gemini,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .expect_err("Gemini is client-only");
    assert_eq!(
        gemini_upstream.message,
        "Gemini is a client-only format and requires a known native upstream protocol"
    );

    let file_id = convert_request_json(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "store": false,
            "input": [{"type": "input_image", "file_id": "file_1"}]
        }),
    )
    .expect_err("file_id cannot convert");
    assert_eq!(
        file_id.message,
        "Responses input_image.file_id is not supported; use image_url"
    );
}

#[test]
fn convert_request_json_preserves_thinking_and_tool_semantics() {
    let disabled = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": "none"}
        }),
    );
    assert_eq!(disabled.body["thinking"]["type"], "disabled");

    let converted = convert_req(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "store": false,
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "go"}]}
            ],
            "tools": [{
                "type": "namespace",
                "name": "multi_agent_v1",
                "tools": [{"type": "function", "name": "spawn_agent", "parameters": {"type": "object"}}]
            }]
        }),
    );
    assert_eq!(converted.namespace_tools.len(), 1);
    assert_eq!(converted.namespace_tools[0].namespace, "multi_agent_v1");
    assert_eq!(converted.namespace_tools[0].name, "spawn_agent");
    assert_eq!(
        converted.namespace_tools[0].flattened,
        "multi_agent_v1__spawn_agent"
    );
    assert_eq!(
        converted.body["tools"][0]["name"],
        "multi_agent_v1__spawn_agent"
    );
    assert_eq!(converted.body["messages"][0]["role"], "user");
}

#[test]
fn convert_response_json_uses_injected_synthesis_metadata() {
    let converted = convert_resp(
        ApiFormat::Messages,
        ApiFormat::Responses,
        &json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        Some("any-model"),
    );
    assert_eq!(converted["id"], "resp_fixedempty");
    assert_eq!(converted["created_at"], 1_700_000_123);
    assert_eq!(converted["completed_at"], 1_700_000_123);
    assert_eq!(converted["model"], "any-model");

    let named = convert_resp(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        &json!({
            "id": "c",
            "model": "upstream-model",
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
        &[],
        &[],
        None,
    );
    assert_eq!(named["id"], "resp_c");
    assert_eq!(named["created_at"], 1_700_000_123);
    assert_eq!(named["model"], "upstream-model");
}

#[test]
fn convert_response_json_preserves_model_identity_without_client_rewrite() {
    let converted = convert_resp(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        &json!({
            "id": "m1",
            "model": "ocg-generic",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 2, "output_tokens": 1}
        }),
        &[],
        &[],
        Some("minimax-m3"),
    );
    assert_eq!(converted["model"], "minimax-m3");
    assert_ne!(converted["model"], "MiniMax-M3");
    assert_eq!(converted["choices"][0]["message"]["content"], "hi");
}

#[test]
fn convert_response_json_round_trips_signed_thinking_and_namespace_tools() {
    let mapping = NamespaceToolMapping {
        flattened: "multi_agent_v1__spawn_agent".to_string(),
        namespace: "multi_agent_v1".to_string(),
        name: "spawn_agent".to_string(),
        custom: false,
    };
    let converted = convert_resp(
        ApiFormat::Messages,
        ApiFormat::Responses,
        &json!({
            "id": "m1",
            "model": "m",
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "check", "signature": "sig_123"},
                {"type": "tool_use", "id": "call_1", "name": "multi_agent_v1__spawn_agent", "input": {"task": "go"}}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[mapping],
        None,
    );
    let output = converted["output"].as_array().unwrap();
    let reasoning = output
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(
        decode_anthropic_thinking_block(reasoning["encrypted_content"].as_str().unwrap()).unwrap()
            ["thinking"],
        "check"
    );
    let call = output
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(call["namespace"], "multi_agent_v1");
    assert_eq!(call["name"], "spawn_agent");

    let restored = convert_req(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "store": false,
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "start"}]},
                reasoning,
                call
            ],
            "tools": [{
                "type": "namespace",
                "name": "multi_agent_v1",
                "tools": [{"type": "function", "name": "spawn_agent"}]
            }]
        }),
    );
    let assistant = restored.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"][0]["type"], "thinking");
    assert_eq!(assistant["content"][0]["signature"], "sig_123");
    assert_eq!(
        assistant["content"][1]["name"],
        "multi_agent_v1__spawn_agent"
    );
}

#[test]
fn convert_response_json_preserves_reported_minimax_cache() {
    let messages_to_chat = convert_resp(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        &json!({
            "id": "m1",
            "model": "ocg-generic",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 0, "output_tokens": 5, "cache_read_input_tokens": 40500}
        }),
        &[],
        &[],
        Some("minimax-m3"),
    );
    assert_eq!(messages_to_chat["usage"]["prompt_tokens"], 40500);
    assert_eq!(
        messages_to_chat["usage"]["prompt_tokens_details"]["cached_tokens"],
        40500
    );

    let passthrough = convert_resp(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
        &json!({
            "id": "chatcmpl-1",
            "model": "minimax-m3",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 40669,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 40669}
            }
        }),
        &[],
        &[],
        Some("minimax-m3"),
    );
    assert_eq!(passthrough["usage"]["prompt_tokens"], 40669);
    assert_eq!(
        passthrough["usage"]["prompt_tokens_details"]["cached_tokens"],
        40669
    );
    assert_eq!(passthrough["model"], "minimax-m3");
}

#[test]
fn convert_response_json_rejects_non_object_messages_with_exact_string() {
    let error = convert_response_json(
        ApiFormat::Messages,
        ApiFormat::Messages,
        &json!([]),
        &[],
        &[],
        synthesis(),
        Some("minimax-m3"),
    )
    .expect_err("non-object Messages response should be rejected");
    assert_eq!(error.message, "Messages response must be a JSON object");
}
