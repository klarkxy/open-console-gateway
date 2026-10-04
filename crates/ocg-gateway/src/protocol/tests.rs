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

#[test]
fn chat_conversion_rejects_unpreserved_fields_and_keeps_service_tier() {
    let rejected = convert_request_json(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "registered-model",
            "messages": [{"role": "user", "content": "给出一个标题"}],
            "n": 2,
            "logprobs": true
        }),
    )
    .expect_err("n and logprobs cannot be dropped");
    assert!(
        rejected.message.contains("Chat Completions n"),
        "{rejected:?}"
    );
    let logprobs = convert_request_json(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "registered-model",
            "messages": [{"role": "user", "content": "hello"}],
            "logprobs": true
        }),
    )
    .expect_err("logprobs cannot be dropped");
    assert!(
        logprobs.message.contains("Chat Completions logprobs"),
        "{logprobs:?}"
    );
}

#[test]
fn chat_to_responses_rejects_stop_and_preserves_service_tier_and_order() {
    let stopped = convert_request_json(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        json!({
            "model": "registered-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stop": ["END"]
        }),
    );
    assert!(
        stopped.is_err(),
        "stop must be rejected when Responses cannot express it"
    );

    let converted = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        json!({
            "model": "registered-model",
            "messages": [{
                "role": "assistant",
                "content": "先说明我要查询什么",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]
            }],
            "service_tier": "priority",
            "parallel_tool_calls": false,
            "tools": [{"type": "function", "function": {"name": "lookup", "parameters": {"type": "object"}}}]
        }),
    );
    assert_eq!(converted.body["service_tier"], "priority");
    assert_eq!(converted.body["parallel_tool_calls"], false);
    let input = converted.body["input"].as_array().unwrap();
    let types: Vec<_> = input
        .iter()
        .filter_map(|item| item.get("type").and_then(Value::as_str))
        .collect();
    assert_eq!(types.first().copied(), Some("message"));
    assert!(types.contains(&"function_call"));
    let message_at = types.iter().position(|kind| *kind == "message").unwrap();
    let call_at = types
        .iter()
        .position(|kind| *kind == "function_call")
        .unwrap();
    assert!(message_at < call_at, "{types:?}");
}

#[test]
fn direct_chat_and_responses_preserve_explicit_reasoning_effort_spellings() {
    for effort in [
        "low",
        "high",
        "xhigh",
        "max",
        "none",
        "off",
        "minimal",
        "medium",
        "wire-custom",
    ] {
        let to_responses = convert_req(
            ApiFormat::ChatCompletions,
            ApiFormat::Responses,
            json!({
                "model": "any",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": effort
            }),
        );
        assert_eq!(to_responses.body["reasoning"]["effort"], effort, "{effort}");
        assert_eq!(
            to_responses.body["reasoning"]["summary"], "auto",
            "{effort}"
        );

        let to_chat = convert_req(
            ApiFormat::Responses,
            ApiFormat::ChatCompletions,
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": effort, "summary": "detailed"}
            }),
        );
        assert_eq!(to_chat.body["reasoning_effort"], effort, "{effort}");
        assert!(to_chat.body.get("stream_options").is_none(), "{effort}");
        if effort == "none" {
            assert_eq!(to_chat.body["thinking"]["type"], "disabled");
        } else {
            assert!(to_chat.body.get("thinking").is_none(), "{effort}");
        }
    }

    let streamed_responses = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        json!({
            "model": "any",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    );
    assert_eq!(streamed_responses.body["stream"], true);
    assert_eq!(streamed_responses.body["reasoning"]["effort"], "high");
    assert!(streamed_responses.body.get("stream_options").is_none());

    let streamed_chat = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "stream": true,
            "input": "hi",
            "store": false,
            "reasoning": {"effort": "high"}
        }),
    );
    assert_eq!(streamed_chat.body["stream"], true);
    assert_eq!(streamed_chat.body["reasoning_effort"], "high");
    assert_eq!(streamed_chat.body["stream_options"]["include_usage"], true);
}

#[test]
fn direct_openai_conversion_does_not_fabricate_reasoning_effort() {
    let chat_to_responses = [
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": null
        }),
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": 1
        }),
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning": {"effort": "xhigh"}
        }),
    ];
    for body in chat_to_responses {
        let converted = convert_req(ApiFormat::ChatCompletions, ApiFormat::Responses, body);
        assert!(
            converted.body.get("reasoning").is_none(),
            "{}",
            converted.body
        );
    }

    let responses_to_chat = [
        json!({"model": "any", "input": "hi", "store": false}),
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": null
        }),
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {}
        }),
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": null}
        }),
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": true}
        }),
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning_effort": "xhigh"
        }),
    ];
    for body in responses_to_chat {
        let converted = convert_req(ApiFormat::Responses, ApiFormat::ChatCompletions, body);
        assert!(
            converted.body.get("reasoning_effort").is_none(),
            "{}",
            converted.body
        );
        assert!(
            converted.body.get("thinking").is_none(),
            "{}",
            converted.body
        );
    }
}

#[test]
fn direct_openai_effort_survives_small_max_output_and_forced_tool_choice() {
    let capped = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high",
            "max_completion_tokens": 3000,
            "temperature": 0.2
        }),
    );
    assert_eq!(capped.body["reasoning"]["effort"], "high");
    assert_eq!(capped.body["max_output_tokens"], 3000);

    let forced = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "xhigh",
            "max_completion_tokens": 128,
            "temperature": 0.2,
            "tool_choice": {"type": "function", "function": {"name": "lookup"}},
            "tools": [{
                "type": "function",
                "function": {"name": "lookup", "parameters": {"type": "object"}}
            }]
        }),
    );
    assert_eq!(forced.body["reasoning"]["effort"], "xhigh");
    assert_eq!(forced.body["max_output_tokens"], 128);
    assert_eq!(forced.body["tool_choice"]["type"], "function");
    assert_eq!(forced.body["tool_choice"]["name"], "lookup");
    assert_eq!(forced.body["temperature"], 0.2);

    let capped_chat = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": "max"},
            "max_output_tokens": 3000
        }),
    );
    assert_eq!(capped_chat.body["reasoning_effort"], "max");
    assert_eq!(capped_chat.body["max_tokens"], 3000);
    assert!(capped_chat.body.get("thinking").is_none());

    let forced_chat = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": "wire-custom"},
            "max_output_tokens": 128,
            "temperature": 0.4,
            "tool_choice": "required",
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": {"type": "object"}
            }]
        }),
    );
    assert_eq!(forced_chat.body["reasoning_effort"], "wire-custom");
    assert_eq!(forced_chat.body["max_tokens"], 128);
    assert_eq!(forced_chat.body["tool_choice"], "required");
    assert!(forced_chat.body.get("thinking").is_none());
    assert_eq!(forced_chat.body["temperature"], 0.4);
}

#[test]
fn responses_to_chat_positive_effort_omits_pivot_thinking_disabled() {
    for effort in ["low", "medium", "high", "xhigh", "max", "wire-custom"] {
        let capped = convert_req(
            ApiFormat::Responses,
            ApiFormat::ChatCompletions,
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": effort},
                "max_output_tokens": 1024,
                "temperature": 0.3
            }),
        );
        assert_eq!(capped.body["reasoning_effort"], effort, "{effort}");
        assert_eq!(capped.body["max_tokens"], 1024, "{effort}");
        assert!(capped.body.get("thinking").is_none(), "{effort}");
        assert_eq!(capped.body["temperature"], 0.3, "{effort}");

        let forced = convert_req(
            ApiFormat::Responses,
            ApiFormat::ChatCompletions,
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": effort},
                "max_output_tokens": 8192,
                "temperature": 0.3,
                "tool_choice": "required",
                "tools": [{
                    "type": "function",
                    "name": "lookup",
                    "parameters": {"type": "object"}
                }]
            }),
        );
        assert_eq!(forced.body["reasoning_effort"], effort, "{effort}");
        assert_eq!(forced.body["tool_choice"], "required", "{effort}");
        assert!(forced.body.get("thinking").is_none(), "{effort}");
        assert_eq!(forced.body["temperature"], 0.3, "{effort}");
    }

    for (label, body) in [
        (
            "default",
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": "none"}
            }),
        ),
        (
            "max-1024",
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": "none"},
                "max_output_tokens": 1024
            }),
        ),
        (
            "forced",
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": "none"},
                "max_output_tokens": 8192,
                "tool_choice": "required",
                "tools": [{
                    "type": "function",
                    "name": "lookup",
                    "parameters": {"type": "object"}
                }]
            }),
        ),
    ] {
        let converted = convert_req(ApiFormat::Responses, ApiFormat::ChatCompletions, body);
        assert_eq!(converted.body["reasoning_effort"], "none", "{label}");
        assert_eq!(converted.body["thinking"]["type"], "disabled", "{label}");
    }

    for (label, body) in [
        (
            "max-1024",
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": "off"},
                "max_output_tokens": 1024
            }),
        ),
        (
            "forced",
            json!({
                "model": "any",
                "input": "hi",
                "store": false,
                "reasoning": {"effort": "off"},
                "max_output_tokens": 8192,
                "tool_choice": "required",
                "tools": [{
                    "type": "function",
                    "name": "lookup",
                    "parameters": {"type": "object"}
                }]
            }),
        ),
    ] {
        let converted = convert_req(ApiFormat::Responses, ApiFormat::ChatCompletions, body);
        assert_eq!(converted.body["reasoning_effort"], "off", "{label}");
        assert_eq!(converted.body["thinking"]["type"], "disabled", "{label}");
    }
}

#[test]
fn direct_openai_effort_preservation_keeps_custom_and_namespace_tools() {
    let converted = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "xhigh"},
            "tools": [
                {"type": "custom", "name": "apply_patch"},
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "tools": [{
                        "type": "function",
                        "name": "spawn_agent",
                        "parameters": {"type": "object"}
                    }]
                }
            ]
        }),
    );
    assert_eq!(converted.body["reasoning_effort"], "xhigh");
    assert_eq!(converted.custom_tools, vec!["apply_patch".to_string()]);
    assert_eq!(converted.namespace_tools.len(), 1);
    assert_eq!(
        converted.namespace_tools[0].flattened,
        "multi_agent_v1__spawn_agent"
    );
    let names = converted.body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(names.contains(&"apply_patch"), "{names:?}");
    assert!(names.contains(&"multi_agent_v1__spawn_agent"), "{names:?}");
}

#[test]
fn messages_conversion_keeps_effort_budget_approximation_and_tool_safety() {
    let approximated = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "reasoning_effort": "xhigh"
        }),
    );
    assert_eq!(
        approximated.body["thinking"],
        json!({"type": "enabled", "budget_tokens": 4096})
    );
    assert!(approximated.body.get("reasoning_effort").is_none());
    assert!(approximated.body.get("temperature").is_none());

    let forced = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "reasoning_effort": "high",
            "tool_choice": "required",
            "tools": [{
                "type": "function",
                "function": {"name": "lookup", "parameters": {"type": "object"}}
            }]
        }),
    );
    assert_eq!(forced.body["thinking"]["type"], "disabled");
    assert_eq!(forced.body["tool_choice"]["type"], "any");
    assert_eq!(forced.body["temperature"], 0.2);
    assert!(forced.body.get("reasoning_effort").is_none());

    for effort in ["max", "xhigh"] {
        let converted = convert_req(
            ApiFormat::Messages,
            ApiFormat::ChatCompletions,
            json!({
                "model": "any",
                "max_tokens": 8192,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": effort}
            }),
        );
        assert_eq!(converted.body["reasoning_effort"], "high", "{effort}");
    }

    for (budget, effort) in [(1024_u64, "low"), (4096, "medium"), (16384, "high")] {
        let converted = convert_req(
            ApiFormat::Messages,
            ApiFormat::ChatCompletions,
            json!({
                "model": "any",
                "max_tokens": 32000,
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "enabled", "budget_tokens": budget}
            }),
        );
        assert_eq!(converted.body["reasoning_effort"], effort, "{budget}");
    }

    let to_responses = convert_req(
        ApiFormat::Messages,
        ApiFormat::Responses,
        json!({
            "model": "any",
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": "hi"}],
            "output_config": {"effort": "xhigh"}
        }),
    );
    assert_eq!(to_responses.body["reasoning"]["effort"], "high");
    assert_eq!(to_responses.body["reasoning"]["summary"], "auto");

    let to_messages = convert_req(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "max_output_tokens": 8192,
            "reasoning": {"effort": "max"}
        }),
    );
    assert_eq!(
        to_messages.body["thinking"],
        json!({"type": "enabled", "budget_tokens": 4096})
    );
    assert!(to_messages.body.get("output_config").is_none());
    assert!(to_messages.body.pointer("/reasoning/effort").is_none());
}

#[test]
fn same_protocol_passthrough_keeps_reasoning_effort_bytes() {
    let chat = convert_req(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
        json!({
            "model": "any",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "wire-custom"
        }),
    );
    assert_eq!(chat.body["reasoning_effort"], "wire-custom");
    assert!(chat.body.get("reasoning").is_none());

    let responses = convert_req(
        ApiFormat::Responses,
        ApiFormat::Responses,
        json!({
            "model": "any",
            "input": "hi",
            "store": false,
            "reasoning": {"effort": "max", "summary": "detailed"}
        }),
    );
    assert_eq!(responses.body["reasoning"]["effort"], "max");
    assert_eq!(responses.body["reasoning"]["summary"], "detailed");
}

#[test]
fn gemini_conversion_does_not_preserve_openai_reasoning_effort() {
    let body = json!({
        "model": "any",
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "generationConfig": {
            "thinkingConfig": {"thinkingLevel": "high"},
            "maxOutputTokens": 8192
        },
        "reasoning": {"effort": "xhigh"}
    });
    let chat = convert_req(ApiFormat::Gemini, ApiFormat::ChatCompletions, body.clone());
    assert!(chat.body.get("reasoning_effort").is_none());
    assert!(chat.body.get("thinking").is_none());

    let responses = convert_req(ApiFormat::Gemini, ApiFormat::Responses, body);
    assert!(responses.body.get("reasoning").is_none());
}

fn route_domain(seed: &str) -> ReplayDomain {
    let mut token = String::new();
    while token.len() < 64 {
        token.push_str(seed);
    }
    token.truncate(64);
    ReplayDomain::parse(&token).expect("64 hex domain")
}

#[test]
fn replay_domain_and_marker_keep_exact_opaque_bytes() {
    let domain = route_domain("ab");
    assert_eq!(domain, ReplayDomain::parse(&"AB".repeat(32)).unwrap());
    assert_eq!(domain.hex(), "ab".repeat(32));
    assert!(
        ReplayDomain::parse(&"ab".repeat(31))
            .unwrap_err()
            .message
            .contains("64 hexadecimal")
    );
    assert!(
        ReplayDomain::parse(&format!("{}g", "ab".repeat(31)))
            .unwrap_err()
            .message
            .contains("64 hexadecimal")
    );

    let prefix = replay_marker_prefix(domain);
    assert_eq!(prefix.len(), 79);
    assert_eq!(prefix, format!("ocg-replay-v1:{}:", domain.hex()));
    assert_eq!(bind_replay_opaque(domain, "").unwrap(), "");

    let raw = "sig/+=\n中文";
    let bound = bind_replay_opaque(domain, raw).unwrap();
    assert_eq!(&bound[..prefix.len()], prefix);
    assert_eq!(&bound[prefix.len()..], raw);
    assert_eq!(
        restore_replay_opaque(domain, &bound).unwrap(),
        RestoredReplay::Bound(raw.to_string())
    );
    assert_eq!(
        restore_replay_opaque(domain, "").unwrap(),
        RestoredReplay::Absent
    );
    assert!(
        restore_replay_opaque(route_domain("cd"), &bound)
            .unwrap_err()
            .message
            .contains("does not match")
    );
    assert!(
        bind_replay_opaque(domain, &bound)
            .unwrap_err()
            .message
            .contains("nested")
    );
}

#[test]
fn replay_chunk_binder_emits_one_prefix_when_bytes_arrive() {
    let domain = route_domain("ab");
    let mut idle = ReplayChunkBinder::new(domain);
    assert_eq!(idle.push("").unwrap(), "");
    assert_eq!(idle.finish().unwrap(), "");

    let mut binder = ReplayChunkBinder::new(domain);
    assert_eq!(binder.push("").unwrap(), "");
    assert_eq!(binder.push("oc").unwrap(), "");
    let first = binder.push("ZZ").unwrap();
    assert_eq!(first, format!("{}ocZZ", replay_marker_prefix(domain)));
    assert_eq!(binder.push("_123").unwrap(), "_123");
    assert_eq!(binder.finish().unwrap(), "");
    assert_eq!(
        bind_replay_opaque(domain, "ocZZ_123").unwrap(),
        format!("{first}_123")
    );

    let mut initial = ReplayChunkBinder::new(domain);
    let opened = initial.push("sig").unwrap();
    assert!(opened.starts_with(&replay_marker_prefix(domain)));
    assert!(opened.ends_with("sig"));
    assert_eq!(initial.push("_123").unwrap(), "_123");
    assert_eq!(
        format!("{opened}_123"),
        bind_replay_opaque(domain, "sig_123").unwrap()
    );

    let mut nested = ReplayChunkBinder::new(domain);
    assert!(
        nested
            .push("ocg-replay-v2:already")
            .unwrap_err()
            .message
            .contains("nested")
    );
}

#[test]
fn replay_restore_rejects_unknown_version_malformed_and_nested_markers() {
    let domain = route_domain("ab");
    let token = domain.hex();
    let cases = [
        (
            format!("ocg-replay-v2:{token}:payload"),
            "version is not supported",
        ),
        ("ocg-replay-".to_string(), "version is not supported"),
        (format!("ocg-replay-v1:{token}payload"), "malformed"),
        (
            format!("ocg-replay-v1:{}:payload", "zz".repeat(32)),
            "malformed",
        ),
        (format!("ocg-replay-v1:{token}:"), "malformed"),
        (
            format!(
                "ocg-replay-v1:{token}:{}",
                bind_replay_opaque(domain, "again").unwrap()
            ),
            "nested",
        ),
        ("foreign-ciphertext".to_string(), "no route domain"),
    ];
    for (value, needle) in cases {
        let message = restore_replay_opaque(domain, &value).unwrap_err().message;
        assert!(
            message.contains(needle),
            "{value} => {message}, want {needle}"
        );
    }
}

#[test]
fn replay_request_restores_multi_block_history_and_keeps_tools() {
    let domain = route_domain("ab");
    let first = "sig/+=\n中文";
    let second = "sig_initial";
    let redacted = "opaque-data";
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "check", "signature": bind_replay_opaque(domain, first).unwrap()},
                {"type": "thinking", "thinking": "", "signature": bind_replay_opaque(domain, second).unwrap()},
                {"type": "redacted_thinking", "data": bind_replay_opaque(domain, redacted).unwrap()},
                {"type": "text", "text": "answer"},
                {"type": "tool_use", "id": "c1", "name": "read", "input": {"path": "a"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "c1", "content": "ok"}
            ]}
        ]
    });
    let converted =
        convert_request_json_with_replay(ApiFormat::Messages, ApiFormat::Messages, body, domain)
            .expect("same messages route restores every opaque field");
    let messages = converted.body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"][0]["text"], "hello");
    let content = messages[1]["content"].as_array().unwrap();
    assert_eq!(content[0]["signature"], first);
    assert_eq!(content[0]["thinking"], "check");
    assert_eq!(content[1]["signature"], second);
    assert_eq!(content[1]["thinking"], "");
    assert_eq!(content[2]["type"], "redacted_thinking");
    assert_eq!(content[2]["data"], redacted);
    assert_eq!(content[3]["text"], "answer");
    assert_eq!(content[4]["type"], "tool_use");
    assert_eq!(content[4]["name"], "read");
    assert_eq!(content[4]["input"]["path"], "a");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1");
    assert_eq!(messages[2]["content"][0]["content"], "ok");
}

#[test]
fn replay_request_rejects_wrong_domain_unmarked_and_conflicting_history() {
    let domain = route_domain("ab");
    let other = route_domain("cd");
    let signed = |signature: String| {
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "check", "signature": signature},
                    {"type": "text", "text": "answer"},
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {}}
                ]
            }]
        })
    };
    let wrong = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        signed(bind_replay_opaque(other, "sig_123").unwrap()),
        domain,
    )
    .unwrap_err();
    assert!(wrong.message.contains("does not match"), "{wrong:?}");

    let unmarked = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        signed("sig_123".to_string()),
        domain,
    )
    .unwrap_err();
    assert!(unmarked.message.contains("no route domain"), "{unmarked:?}");
    let legacy = convert_req(
        ApiFormat::Messages,
        ApiFormat::Messages,
        signed("sig_123".to_string()),
    );
    assert_eq!(
        legacy.body["messages"][0]["content"][0]["signature"],
        "sig_123"
    );

    let conflicting = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "one", "signature": bind_replay_opaque(domain, "one").unwrap()},
                    {"type": "redacted_thinking", "data": bind_replay_opaque(other, "two").unwrap()},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        conflicting.message.contains("does not match"),
        "{conflicting:?}"
    );
}

#[test]
fn replay_wrapper_restores_inner_fields_without_a_second_envelope() {
    let domain = route_domain("ab");
    let raw = "sig/+=\n中文";
    let upstream = json!({
        "id": "m1",
        "model": "m",
        "stop_reason": "end_turn",
        "content": [
            {"type": "thinking", "thinking": "", "signature": raw},
            {"type": "redacted_thinking", "data": "redacted-raw"},
            {"type": "text", "text": "answer"},
            {"type": "tool_use", "id": "call_1", "name": "read", "input": {"path": "a"}}
        ],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let converted = convert_response_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Responses,
        &upstream,
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("responses client can carry an already-bound messages block");
    let output = converted.body["output"].as_array().unwrap();
    let carriers: Vec<&Value> = output
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect();
    assert_eq!(carriers.len(), 2);
    let prefix = replay_marker_prefix(domain);
    for carrier in &carriers {
        let encrypted = carrier["encrypted_content"].as_str().unwrap();
        assert!(
            encrypted.starts_with("ocg-anthropic-thinking-v1:"),
            "{encrypted}"
        );
        assert!(!encrypted.starts_with("ocg-replay-"), "{encrypted}");
    }
    let thinking =
        decode_anthropic_thinking_block(carriers[0]["encrypted_content"].as_str().unwrap())
            .unwrap();
    assert_eq!(thinking["signature"], format!("{prefix}{raw}"));
    assert_eq!(thinking["thinking"], "");
    let redacted =
        decode_anthropic_thinking_block(carriers[1]["encrypted_content"].as_str().unwrap())
            .unwrap();
    assert_eq!(redacted["data"], format!("{prefix}redacted-raw"));

    let mut echoed = json!({"output": [carriers[0].clone()]});
    let second =
        bind_outgoing_native_replay(ApiFormat::Responses, &mut echoed, domain).unwrap_err();
    assert!(second.message.contains("nested"), "{second:?}");

    let call = output
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap()
        .clone();
    let request = json!({
        "model": "m",
        "store": false,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "start"}]},
            carriers[0].clone(),
            carriers[1].clone(),
            call
        ]
    });
    let restored = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        request.clone(),
        domain,
    )
    .expect("messages upstream unwraps the carrier");
    let assistant = restored.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"][0]["signature"], raw);
    assert_eq!(assistant["content"][0]["thinking"], "");
    assert_eq!(assistant["content"][1]["data"], "redacted-raw");
    assert_eq!(assistant["content"][2]["name"], "read");
    assert_eq!(assistant["content"][2]["input"]["path"], "a");

    let wrong_target = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        request,
        domain,
    )
    .unwrap_err();
    assert!(
        wrong_target
            .message
            .contains("Anthropic thinking history cannot be preserved"),
        "{wrong_target:?}"
    );

    let legacy = convert_resp(
        ApiFormat::Messages,
        ApiFormat::Responses,
        &upstream,
        &[],
        &[],
        None,
    );
    let unmarked = legacy["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap()
        .clone();
    let unmarked_request = json!({
        "model": "m",
        "store": false,
        "input": [
            {"type": "message", "role": "user", "content": "start"},
            unmarked
        ]
    });
    let missing = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        unmarked_request.clone(),
        domain,
    )
    .unwrap_err();
    assert!(missing.message.contains("no route domain"), "{missing:?}");
    let legacy_request = convert_req(ApiFormat::Responses, ApiFormat::Messages, unmarked_request);
    let legacy_assistant = legacy_request.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(legacy_assistant["content"][0]["signature"], raw);
}

#[test]
fn replay_request_rejects_foreign_opaque_targets_and_keeps_chat_codec() {
    let domain = route_domain("ab");
    let raw = "native-ciphertext/+=\n中文";
    let bound = bind_replay_opaque(domain, raw).unwrap();
    let native = json!({
        "model": "m",
        "store": false,
        "input": [
            {"type": "message", "role": "user", "content": "start"},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "visible"}], "encrypted_content": bound}
        ]
    });
    let restored = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        native.clone(),
        domain,
    )
    .expect("responses passthrough returns the original ciphertext");
    assert_eq!(restored.body["input"][1]["encrypted_content"], raw);
    assert_eq!(restored.body["input"][1]["summary"][0]["text"], "visible");

    for upstream in [ApiFormat::Messages, ApiFormat::ChatCompletions] {
        let rejected = convert_request_json_with_replay(
            ApiFormat::Responses,
            upstream,
            native.clone(),
            domain,
        )
        .unwrap_err();
        assert!(
            rejected
                .message
                .contains("Responses encrypted reasoning cannot be preserved"),
            "{upstream:?} {rejected:?}"
        );
    }

    let foreign = json!({
        "model": "m",
        "store": false,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "start"}]},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "foreign"}], "encrypted_content": "foreign-ciphertext"},
            {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
        ]
    });
    let missing = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        foreign.clone(),
        domain,
    )
    .unwrap_err();
    assert!(missing.message.contains("no route domain"), "{missing:?}");
    let legacy = convert_req(ApiFormat::Responses, ApiFormat::Messages, foreign);
    let assistant = legacy.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert!(
        assistant["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] != "thinking")
    );
    assert_eq!(assistant["content"][0]["type"], "tool_use");
    assert_eq!(assistant["content"][0]["name"], "read");

    let double = format!(
        "{}{}",
        replay_marker_prefix(domain),
        encode_anthropic_thinking_block(&json!({
            "type": "thinking",
            "thinking": "check",
            "signature": bind_replay_opaque(domain, "sig_123").unwrap()
        }))
        .unwrap()
    );
    let nested = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        json!({
            "model": "m",
            "store": false,
            "input": [{"type": "reasoning", "encrypted_content": double}]
        }),
        domain,
    )
    .unwrap_err();
    assert!(nested.message.contains("nested"), "{nested:?}");

    let chat = encode_chat_reasoning("check first").unwrap();
    let mut chat_body = json!({
        "output": [{"type": "reasoning", "encrypted_content": chat}]
    });
    bind_outgoing_native_replay(ApiFormat::Responses, &mut chat_body, domain)
        .expect("chat codec is not a replay field");
    assert_eq!(chat_body["output"][0]["encrypted_content"], chat);
    let history = json!({
        "model": "m",
        "store": false,
        "input": [
            {"type": "message", "role": "user", "content": "start"},
            {"type": "reasoning", "encrypted_content": chat},
            {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
        ]
    });
    let to_chat = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        history.clone(),
        domain,
    )
    .expect("ordinary chat reasoning stays on its codec");
    assert_eq!(
        to_chat.body["messages"][1]["reasoning_content"],
        "check first"
    );
    assert_eq!(to_chat.body["messages"][2]["role"], "tool");
    let to_messages = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        history,
        domain,
    )
    .unwrap_err();
    assert!(
        to_messages
            .message
            .contains("Chat reasoning history cannot be preserved"),
        "{to_messages:?}"
    );
}

#[test]
fn replay_request_preserves_unsigned_text_and_rejects_lossy_controls() {
    let domain = route_domain("ab");
    let ordinary = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [
                {"role": "developer", "content": "dev"},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "plain", "signature": ""},
                    {"type": "text", "text": "answer"},
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {"path": "a"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "c1", "content": "ok"}
                ]}
            ]
        }),
        domain,
    )
    .expect("empty signature and ordinary tools convert");
    let messages = ordinary.body["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|message| message["role"] != "developer")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "system" && message["content"] == "dev")
    );
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"], "answer");
    assert_eq!(assistant["reasoning_content"], "plain");
    assert!(assistant.get("signature").is_none());
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(
        messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap()["content"],
        "ok"
    );

    let signed = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "check", "signature": bind_replay_opaque(domain, "sig_123").unwrap()},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        signed
            .message
            .contains("Messages thinking signature cannot be preserved"),
        "{signed:?}"
    );
    let redacted = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Responses,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "redacted_thinking", "data": bind_replay_opaque(domain, "opaque").unwrap()},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        redacted
            .message
            .contains("Messages redacted thinking cannot be preserved"),
        "{redacted:?}"
    );

    let unsigned = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "draft", "signature": ""},
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {}}
                ]
            }]
        }),
        domain,
    )
    .expect("empty signature is not provenance");
    let kept = unsigned.body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(kept[0]["type"], "thinking");
    assert_eq!(kept[0]["thinking"], "draft");
    assert_eq!(kept[0]["signature"], "");
    assert_eq!(kept[1]["type"], "tool_use");
    assert_eq!(kept[1]["id"], "c1");

    let dropped_summary = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "store": false,
            "input": [
                {"type": "message", "role": "user", "content": "start"},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "unsigned"}]}
            ]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        dropped_summary
            .message
            .contains("Responses reasoning summary cannot be preserved"),
        "{dropped_summary:?}"
    );

    let summary = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "high", "summary": "detailed"}
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        summary
            .message
            .contains("Responses reasoning.summary cannot be preserved"),
        "{summary:?}"
    );
    let effort = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "high"}
        }),
        domain,
    )
    .expect("string effort still converts");
    assert_eq!(effort.body["reasoning_effort"], "high");

    let custom = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "custom"}
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        custom
            .message
            .contains("Messages thinking.type cannot be preserved"),
        "{custom:?}"
    );

    let chat_text = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": "answer", "reasoning_content": "check first"},
                {"role": "user", "content": "next"}
            ]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        chat_text
            .message
            .contains("Chat reasoning history cannot be preserved"),
        "{chat_text:?}"
    );
    let chat_keep = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": "answer", "reasoning_content": "check first"}
            ]
        }),
        domain,
    )
    .expect("chat passthrough keeps reasoning text");
    assert_eq!(
        chat_keep.body["messages"][0]["reasoning_content"],
        "check first"
    );

    let thought = convert_request_json_with_replay(
        ApiFormat::Gemini,
        ApiFormat::Messages,
        json!({
            "contents": [{"role": "user", "parts": [
                {"text": "hi"},
                {"text": "secret", "thought": true}
            ]}]
        }),
        domain,
    )
    .unwrap_err();
    assert!(
        thought
            .message
            .contains("Gemini thought history cannot be preserved"),
        "{thought:?}"
    );
    let visible = convert_request_json_with_replay(
        ApiFormat::Gemini,
        ApiFormat::Messages,
        json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        }),
        domain,
    )
    .expect("gemini text still converts");
    assert_eq!(visible.body["messages"][0]["content"][0]["text"], "hi");
    let legacy_thought = convert_req(
        ApiFormat::Gemini,
        ApiFormat::Messages,
        json!({
            "contents": [{"role": "user", "parts": [
                {"text": "hi"},
                {"text": "secret", "thought": true},
                {"functionCall": {"name": "read", "args": {"path": "a"}}}
            ]}]
        }),
    );
    let legacy_blocks = legacy_thought.body["messages"][0]["content"]
        .as_array()
        .unwrap();
    assert_eq!(legacy_blocks[0]["text"], "hi");
    assert_eq!(legacy_blocks[1]["type"], "tool_use");
    assert!(legacy_blocks.iter().all(|block| block["text"] != "secret"));
}

#[test]
fn replay_response_binds_native_fields_and_rejects_lossy_targets() {
    let domain = route_domain("ab");
    let native = json!({
        "id": "r1",
        "model": "m",
        "status": "completed",
        "output": [{
            "type": "reasoning",
            "summary": [],
            "encrypted_content": "native-ciphertext/+="
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let bound = convert_response_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        &native,
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("responses passthrough binds ciphertext");
    assert_eq!(
        bound.body["output"][0]["encrypted_content"],
        bind_replay_opaque(domain, "native-ciphertext/+=").unwrap()
    );
    for client in [ApiFormat::Messages, ApiFormat::ChatCompletions] {
        let rejected = convert_response_json_with_replay(
            ApiFormat::Responses,
            client,
            &native,
            &[],
            &[],
            synthesis(),
            None,
            domain,
        )
        .unwrap_err();
        assert!(
            rejected
                .message
                .contains("Responses encrypted reasoning cannot be preserved"),
            "{client:?} {rejected:?}"
        );
    }

    let summary = json!({
        "id": "r1",
        "model": "m",
        "status": "completed",
        "output": [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "reason"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "answer"}]}
        ],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let text = convert_response_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        &summary,
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("summary text is ordinary reasoning");
    assert_eq!(text.body["content"][0]["thinking"], "reason");
    assert_eq!(text.body["content"][0]["signature"], "");
    assert_eq!(text.body["content"][1]["text"], "answer");

    let signed = json!({
        "id": "m1",
        "model": "m",
        "content": [
            {"type": "thinking", "thinking": "check", "signature": "sig_123"},
            {"type": "text", "text": "answer"}
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let chat = convert_response_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        &signed,
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .unwrap_err();
    assert!(
        chat.message
            .contains("Messages thinking signature cannot be preserved"),
        "{chat:?}"
    );
    let redacted = convert_response_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Gemini,
        &json!({
            "id": "m1",
            "content": [
                {"type": "redacted_thinking", "data": "opaque"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .unwrap_err();
    assert!(
        redacted
            .message
            .contains("Messages redacted thinking cannot be preserved"),
        "{redacted:?}"
    );

    let unsigned = convert_response_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        &json!({
            "id": "m1",
            "content": [
                {"type": "thinking", "thinking": "plain", "signature": ""},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("unsigned thinking text can cross to chat");
    assert_eq!(
        unsigned.body["choices"][0]["message"]["reasoning_content"],
        "plain"
    );
    assert_eq!(unsigned.body["choices"][0]["message"]["content"], "answer");

    let chat_response = convert_response_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        &json!({
            "id": "c1",
            "model": "m",
            "choices": [{"message": {
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "check first"
            }, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("chat reasoning uses the existing codec");
    let encrypted = chat_response.body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap()["encrypted_content"]
        .as_str()
        .unwrap();
    assert!(
        encrypted.starts_with("ocg-chat-reasoning-v1:"),
        "{encrypted}"
    );
    assert!(!encrypted.starts_with("ocg-replay-"), "{encrypted}");
    assert_eq!(
        decode_chat_reasoning(encrypted).as_deref(),
        Some("check first")
    );
}

#[test]
fn chat_upstream_maps_developer_role_to_system() {
    let responses = convert_req(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "store": false,
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "dev"}]},
                {"type": "message", "role": "user", "content": "hello"}
            ]
        }),
    );
    let messages = responses.body["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|message| message["role"] != "developer")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "system" && message["content"] == "dev")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == "hello")
    );
}

fn assert_rejected(error: ConversionError, needle: &str) {
    assert!(error.message.contains(needle), "{error:?}");
}

#[test]
fn replay_request_rejects_approximated_reasoning_controls() {
    let domain = route_domain("ab");
    let chat = |effort: &str, max_tokens: u64, forced: bool| {
        let mut body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": max_tokens,
            "reasoning_effort": effort
        });
        if forced {
            body["tools"] = json!([{
                "type": "function",
                "function": {"name": "read", "parameters": {"type": "object"}}
            }]);
            body["tool_choice"] = json!("required");
        }
        body
    };
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            chat("high", 8192, false),
            domain,
        )
        .unwrap_err(),
        "Chat Completions reasoning_effort cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            chat("off", 8192, false),
            domain,
        )
        .unwrap_err(),
        "Chat Completions reasoning_effort cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            chat("high", 512, false),
            domain,
        )
        .unwrap_err(),
        "Chat Completions reasoning_effort cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            chat("high", 8192, true),
            domain,
        )
        .unwrap_err(),
        "Chat Completions reasoning_effort cannot be preserved",
    );
    let disabled = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        chat("none", 512, true),
        domain,
    )
    .expect("explicit none stays disabled without a budget");
    assert_eq!(disabled.body["thinking"]["type"], "disabled");
    assert!(disabled.body["thinking"].get("budget_tokens").is_none());
    assert_eq!(disabled.body["tools"][0]["name"], "read");

    let responses_none = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "none", "summary": "auto"}
        }),
        domain,
    )
    .expect("responses none projects to thinking disabled");
    assert_eq!(responses_none.body["thinking"]["type"], "disabled");
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Responses,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "store": false,
                "input": "hi",
                "reasoning": {"effort": "high", "summary": "auto"}
            }),
            domain,
        )
        .unwrap_err(),
        "Responses reasoning.effort cannot be preserved",
    );

    let carried = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
        chat("max", 8192, true),
        domain,
    )
    .expect("chat to responses keeps the effort spelling");
    assert_eq!(carried.body["reasoning"]["effort"], "max");
    let from_responses = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "xhigh"}
        }),
        domain,
    )
    .expect("responses to chat keeps the effort spelling");
    assert_eq!(from_responses.body["reasoning_effort"], "xhigh");
    let none_chat = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "none"}
        }),
        domain,
    )
    .expect("none stays an explicit disable on chat");
    assert_eq!(none_chat.body["reasoning_effort"], "none");
    assert_eq!(none_chat.body["thinking"]["type"], "disabled");

    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::ChatCompletions,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "enabled", "budget_tokens": 4096}
            }),
            domain,
        )
        .unwrap_err(),
        "Messages thinking.type cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Responses,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "adaptive"}
            }),
            domain,
        )
        .unwrap_err(),
        "Messages thinking.type cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::ChatCompletions,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "low"}
            }),
            domain,
        )
        .unwrap_err(),
        "Messages output_config.effort cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Responses,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "max"}
            }),
            domain,
        )
        .unwrap_err(),
        "Messages output_config.effort cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Responses,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "disabled"}
            }),
            domain,
        )
        .unwrap_err(),
        "Messages thinking.type cannot be preserved",
    );
    let messages_disabled = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "disabled"}
        }),
        domain,
    )
    .expect("messages disable stays disabled on chat");
    assert_eq!(messages_disabled.body["thinking"]["type"], "disabled");
    assert!(messages_disabled.body.get("reasoning_effort").is_none());

    let same = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        json!({
            "model": "m",
            "store": false,
            "input": "hi",
            "reasoning": {"effort": "max", "summary": "detailed"}
        }),
        domain,
    )
    .expect("same protocol keeps native effort controls");
    assert_eq!(same.body["reasoning"]["effort"], "max");
    assert_eq!(same.body["reasoning"]["summary"], "detailed");
    let same_messages = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096},
            "output_config": {"effort": "max"}
        }),
        domain,
    )
    .expect("same protocol keeps messages controls");
    assert_eq!(same_messages.body["thinking"]["budget_tokens"], 4096);
    assert_eq!(same_messages.body["output_config"]["effort"], "max");

    let ordinary = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "tool_calls": [{
                    "id": "c1",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "c1", "content": "ok"}
            ],
            "tools": [{
                "type": "function",
                "function": {"name": "read", "parameters": {"type": "object"}}
            }]
        }),
        domain,
    )
    .expect("ordinary text and tools do not require an effort control");
    assert_eq!(ordinary.body["tools"][0]["name"], "read");
    assert_eq!(ordinary.body["messages"][0]["content"][0]["text"], "hi");
    assert_eq!(ordinary.body["messages"][1]["content"][0]["name"], "read");
    assert!(ordinary.body.get("thinking").is_none());
}

#[test]
fn replay_request_rejects_malformed_opaque_reasoning_fields() {
    let domain = route_domain("ab");
    let thinking = |signature: Value| {
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "plain", "signature": signature},
                    {"type": "text", "text": "answer"},
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {}}
                ]
            }]
        })
    };
    for signature in [json!(7), json!({"sig": true}), json!(["sig"])] {
        for target in [ApiFormat::ChatCompletions, ApiFormat::Messages] {
            assert_rejected(
                convert_request_json_with_replay(
                    ApiFormat::Messages,
                    target,
                    thinking(signature.clone()),
                    domain,
                )
                .unwrap_err(),
                "Messages thinking signature must be a string",
            );
        }
    }
    let unsigned = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "plain"},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        domain,
    )
    .expect("a missing signature is unsigned thinking text");
    assert_eq!(unsigned.body["messages"][0]["reasoning_content"], "plain");
    assert_eq!(unsigned.body["messages"][0]["content"], "answer");
    let missing_same = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        thinking(Value::Null),
        domain,
    )
    .expect("null signature is unsigned, not invalid redacted data");
    let kept = missing_same.body["messages"][0]["content"]
        .as_array()
        .unwrap();
    assert_eq!(kept[0]["type"], "thinking");
    assert_eq!(kept[0]["thinking"], "plain");
    assert!(kept[0]["signature"].is_null());
    assert_eq!(kept[1]["text"], "answer");
    assert_eq!(kept[2]["type"], "tool_use");

    let redacted = |data: Value| {
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "redacted_thinking", "data": data},
                    {"type": "text", "text": "answer"}
                ]
            }]
        })
    };
    for data in [
        json!(7),
        json!(""),
        Value::Null,
        json!({"opaque": true}),
        json!([1]),
    ] {
        assert_rejected(
            convert_request_json_with_replay(
                ApiFormat::Messages,
                ApiFormat::Messages,
                redacted(data),
                domain,
            )
            .unwrap_err(),
            "Messages redacted thinking data must be a nonempty string",
        );
    }
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "messages": [{
                    "role": "assistant",
                    "content": [
                        {"type": "redacted_thinking"},
                        {"type": "text", "text": "answer"}
                    ]
                }]
            }),
            domain,
        )
        .unwrap_err(),
        "Messages redacted thinking data must be a nonempty string",
    );
    let marked = bind_replay_opaque(domain, "sig_123").unwrap();
    let restored = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        thinking(json!(marked)),
        domain,
    )
    .expect("a string signature is a legal opaque field");
    assert_eq!(
        restored.body["messages"][0]["content"][0]["signature"],
        "sig_123"
    );
    assert_eq!(restored.body["messages"][0]["content"][1]["text"], "answer");

    let legacy = convert_req(
        ApiFormat::Messages,
        ApiFormat::Messages,
        redacted(json!("")),
    );
    let legacy_blocks = legacy.body["messages"][0]["content"].as_array().unwrap();
    assert!(
        legacy_blocks
            .iter()
            .all(|block| block["type"] != "redacted_thinking")
    );
    assert_eq!(legacy_blocks[0]["text"], "answer");

    let encrypted = |value: Value| {
        json!({
            "model": "m",
            "store": false,
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "visible"}],
                    "encrypted_content": value
                }
            ]
        })
    };
    for value in [json!(1), json!({"blob": true}), json!(["cipher"])] {
        assert_rejected(
            convert_request_json_with_replay(
                ApiFormat::Responses,
                ApiFormat::Messages,
                encrypted(value),
                domain,
            )
            .unwrap_err(),
            "Responses encrypted_content must be a string or null",
        );
    }
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Responses,
            ApiFormat::Messages,
            encrypted(Value::Null),
            domain,
        )
        .unwrap_err(),
        "Responses reasoning summary cannot be preserved",
    );
    let same = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        encrypted(Value::Null),
        domain,
    )
    .expect("null encrypted content stays with its summary");
    let item = same.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert!(item["encrypted_content"].is_null());
    assert_eq!(item["summary"][0]["text"], "visible");

    let bad_signature = convert_response_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        &json!({
            "id": "m1",
            "content": [
                {"type": "thinking", "thinking": "plain", "signature": 7},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .unwrap_err();
    assert_rejected(
        bad_signature,
        "Messages thinking signature must be a string",
    );
    let bad_encrypted = convert_response_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        &json!({
            "id": "r1",
            "model": "m",
            "status": "completed",
            "output": [{
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "reason"}],
                "encrypted_content": {"blob": true}
            }],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .unwrap_err();
    assert_rejected(
        bad_encrypted,
        "Responses encrypted_content must be a string or null",
    );
    let null_encrypted = convert_response_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        &json!({
            "id": "r1",
            "model": "m",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "reason"}],
                    "encrypted_content": null
                },
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "answer"}
                ]}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("null encrypted content does not discard summary text");
    assert_eq!(null_encrypted.body["content"][0]["thinking"], "reason");
    assert_eq!(null_encrypted.body["content"][0]["signature"], "");
    assert_eq!(null_encrypted.body["content"][1]["text"], "answer");
}

#[test]
fn replay_messages_keeps_restored_unsigned_assistant_blocks() {
    let domain = route_domain("ab");
    let unsigned = [
        json!({"type": "thinking", "thinking": "plain", "provider": "unfamiliar"}),
        json!({
            "type": "thinking",
            "thinking": "plain",
            "signature": null,
            "provider": "unfamiliar"
        }),
        json!({
            "type": "thinking",
            "thinking": "plain",
            "signature": "",
            "provider": "unfamiliar"
        }),
    ];
    for thinking in unsigned {
        let mut body = json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "id": "msg_1",
                "content": []
            }]
        });
        body["messages"][0]["content"] = Value::Array(vec![
            json!({"type": "tool_use", "id": "c1", "name": "read", "input": {"path": "a"}}),
            json!({"type": "text", "text": "before"}),
            thinking.clone(),
        ]);
        let converted = convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            body,
            domain,
        )
        .expect("unsigned native thinking is admitted history");
        let message = &converted.body["messages"][0];
        assert_eq!(message["id"], "msg_1");
        let blocks = message["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["name"], "read");
        assert_eq!(blocks[0]["input"]["path"], "a");
        assert_eq!(blocks[1]["text"], "before");
        assert_eq!(blocks[2], thinking);
    }

    for thinking in [
        json!({"type": "thinking", "thinking": "only"}),
        json!({"type": "thinking", "thinking": "only", "signature": null}),
        json!({"type": "thinking", "thinking": "only", "signature": ""}),
    ] {
        let mut body = json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": []},
                {"role": "user", "content": "next"}
            ]
        });
        body["messages"][0]["content"] = Value::Array(vec![thinking.clone()]);
        let converted = convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            body,
            domain,
        )
        .expect("thinking-only assistant history stays");
        let messages = converted.body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(messages[0]["content"][0], thinking);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "next");
    }

    let hoisted = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "system": [{"type": "text", "text": "prior"}],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "developer", "content": "dev"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "draft", "signature": ""},
                    {"type": "text", "text": "answer"}
                ]},
                {"role": "system", "content": "sys"}
            ]
        }),
        domain,
    )
    .expect("system hoisting still runs around kept thinking");
    assert_eq!(
        hoisted.body["system"],
        json!([
            {"type": "text", "text": "prior"},
            {"type": "text", "text": "dev"},
            {"type": "text", "text": "sys"}
        ])
    );
    let messages = hoisted.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message["role"] != "system" && message["role"] != "developer")
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hi");
    assert_eq!(messages[1]["content"][0]["type"], "thinking");
    assert_eq!(messages[1]["content"][0]["thinking"], "draft");
    assert_eq!(messages[1]["content"][0]["signature"], "");
    assert_eq!(messages[1]["content"][1]["text"], "answer");

    let marked = bind_replay_opaque(domain, "sig_123").unwrap();
    let restored = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "before"},
                    {
                        "type": "thinking",
                        "thinking": "check",
                        "signature": marked,
                        "provider": "unfamiliar"
                    },
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {}}
                ]
            }]
        }),
        domain,
    )
    .expect("signed thinking restores inside the kept block");
    let signed = &restored.body["messages"][0]["content"];
    assert_eq!(signed[0]["text"], "before");
    assert_eq!(signed[1]["signature"], "sig_123");
    assert_eq!(signed[1]["thinking"], "check");
    assert_eq!(signed[1]["provider"], "unfamiliar");
    assert_eq!(signed[2]["type"], "tool_use");

    for thinking in [
        json!({"type": "thinking", "thinking": "plain", "provider": "unfamiliar"}),
        json!({"type": "thinking", "thinking": "plain", "signature": null}),
        json!({"type": "thinking", "thinking": "plain", "signature": ""}),
    ] {
        let mut body = json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "answer"}
                ]
            }]
        });
        let mut blocks = vec![thinking];
        blocks.push(body["messages"][0]["content"][0].clone());
        body["messages"][0]["content"] = Value::Array(blocks);
        let legacy = convert_req(ApiFormat::Messages, ApiFormat::Messages, body);
        let kept = legacy.body["messages"][0]["content"].as_array().unwrap();
        assert!(kept.iter().all(|block| block["type"] != "thinking"));
        assert_eq!(kept[0]["text"], "answer");
    }
    let legacy_only = convert_req(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "only", "signature": null}
                ]},
                {"role": "user", "content": "next"}
            ]
        }),
    );
    let legacy_messages = legacy_only.body["messages"].as_array().unwrap();
    assert_eq!(legacy_messages.len(), 1);
    assert_eq!(legacy_messages[0]["role"], "user");
    assert_eq!(legacy_messages[0]["content"], "next");
    let legacy_hoist = convert_req(
        ApiFormat::Messages,
        ApiFormat::Messages,
        json!({
            "model": "m",
            "messages": [
                {"role": "developer", "content": "dev"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "draft", "signature": ""},
                    {"type": "text", "text": "answer"}
                ]}
            ]
        }),
    );
    assert_eq!(
        legacy_hoist.body["system"],
        json!([{"type": "text", "text": "dev"}])
    );
    assert_eq!(
        legacy_hoist.body["messages"][0]["content"][0]["text"],
        "answer"
    );
    assert!(
        legacy_hoist.body["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] != "thinking")
    );

    let chat = convert_request_json_with_replay(
        ApiFormat::Messages,
        ApiFormat::ChatCompletions,
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "plain"},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        domain,
    )
    .expect("cross conversion still carries unsigned thinking as text");
    assert_eq!(chat.body["messages"][0]["reasoning_content"], "plain");
    assert_eq!(chat.body["messages"][0]["content"], "answer");

    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "messages": [{
                    "role": "assistant",
                    "content": [{"type": "thinking", "thinking": "plain", "signature": 7}]
                }]
            }),
            domain,
        )
        .unwrap_err(),
        "Messages thinking signature must be a string",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "messages": [{
                    "role": "assistant",
                    "content": [{"type": "redacted_thinking", "data": ""}]
                }]
            }),
            domain,
        )
        .unwrap_err(),
        "Messages redacted thinking data must be a nonempty string",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "messages": [{
                    "role": "assistant",
                    "content": [{
                        "type": "thinking",
                        "thinking": "plain",
                        "signature": "foreign-opaque"
                    }]
                }]
            }),
            domain,
        )
        .unwrap_err(),
        "no route domain",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Messages,
            ApiFormat::Messages,
            json!({
                "model": "m",
                "messages": [{
                    "role": "assistant",
                    "content": [{
                        "type": "thinking",
                        "thinking": "plain",
                        "signature": format!("ocg-replay-v2:{}:payload", domain.hex())
                    }]
                }]
            }),
            domain,
        )
        .unwrap_err(),
        "opaque replay marker version is not supported",
    );
}

#[test]
fn replay_request_rejects_hidden_chat_reasoning_field() {
    let domain = route_domain("ab");
    let assistant = |reasoning_content: Value, reasoning: Value| {
        json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": "answer",
                "reasoning_content": reasoning_content,
                "reasoning": reasoning
            }]
        })
    };
    for hidden in [Value::Null, json!("")] {
        assert_rejected(
            convert_request_json_with_replay(
                ApiFormat::ChatCompletions,
                ApiFormat::Messages,
                assistant(hidden, json!("check first")),
                domain,
            )
            .unwrap_err(),
            "Chat reasoning history cannot be preserved",
        );
    }
    let ordinary = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::Messages,
        assistant(json!(""), Value::Null),
        domain,
    )
    .expect("empty chat reasoning is not history");
    assert_eq!(ordinary.body["messages"][0]["content"][0]["text"], "answer");
    assert!(
        ordinary.body["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] != "thinking")
    );
    let same = convert_request_json_with_replay(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
        assistant(Value::Null, json!("check first")),
        domain,
    )
    .expect("same protocol keeps both reasoning fields");
    assert!(same.body["messages"][0]["reasoning_content"].is_null());
    assert_eq!(same.body["messages"][0]["reasoning"], "check first");
    assert_eq!(same.body["messages"][0]["content"], "answer");
}

#[test]
fn replay_request_rejects_responses_reasoning_text_loss() {
    let domain = route_domain("ab");
    let request = |summary: Value, content: Value| {
        json!({
            "model": "m",
            "store": false,
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {
                    "type": "reasoning",
                    "summary": summary,
                    "content": content,
                    "encrypted_content": null
                },
                {"type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}"}
            ]
        })
    };
    let empty = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        request(json!([]), json!([])),
        domain,
    )
    .expect("an empty summary and content array carry no reasoning text");
    assert_eq!(empty.body["messages"][0]["content"][0]["text"], "hi");
    assert_eq!(empty.body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(empty.body["messages"][1]["content"][0]["name"], "read");
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Responses,
            ApiFormat::Messages,
            request(
                json!([{"type": "summary_text", "text": "unsigned"}]),
                json!([]),
            ),
            domain,
        )
        .unwrap_err(),
        "Responses reasoning summary cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Responses,
            ApiFormat::ChatCompletions,
            request(
                json!([]),
                json!([{"type": "reasoning_text", "text": "hidden"}]),
            ),
            domain,
        )
        .unwrap_err(),
        "Responses reasoning text cannot be preserved",
    );
    let blank = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        request(json!([]), json!([{"type": "reasoning_text", "text": ""}])),
        domain,
    )
    .expect("an empty reasoning_text part is not populated reasoning");
    assert_eq!(blank.body["messages"][0]["content"][0]["text"], "hi");
    assert_eq!(blank.body["messages"][1]["content"][0]["name"], "read");

    let same = convert_request_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Responses,
        request(
            json!([]),
            json!([{"type": "reasoning_text", "text": "hidden"}]),
        ),
        domain,
    )
    .expect("same protocol keeps native reasoning_text content");
    let item = same.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(item["content"][0]["type"], "reasoning_text");
    assert_eq!(item["content"][0]["text"], "hidden");
    assert!(item["summary"].as_array().unwrap().is_empty());
    assert!(item["encrypted_content"].is_null());
    assert_eq!(same.body["input"][2]["name"], "read");

    let response = convert_response_json_with_replay(
        ApiFormat::Responses,
        ApiFormat::Messages,
        &json!({
            "id": "r1",
            "model": "m",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [],
                    "content": [{"type": "reasoning_text", "text": "hidden"}],
                    "encrypted_content": null
                },
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "answer"}
                ]},
                {"type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        &[],
        &[],
        synthesis(),
        None,
        domain,
    )
    .expect("response reasoning_text is ordinary reasoning text");
    assert_eq!(response.body["content"][0]["type"], "thinking");
    assert_eq!(response.body["content"][0]["thinking"], "hidden");
    assert_eq!(response.body["content"][0]["signature"], "");
    assert_eq!(response.body["content"][1]["text"], "answer");
    assert_eq!(response.body["content"][2]["type"], "tool_use");
    assert_eq!(response.body["content"][2]["name"], "read");
}

#[test]
fn replay_request_rejects_gemini_controls_without_exact_mapping() {
    let domain = route_domain("ab");
    let gemini = |generation: Value| {
        json!({
            "contents": [{"role": "user", "parts": [
                {"text": "hi"},
                {"functionCall": {"name": "read", "args": {"path": "a"}}}
            ]}],
            "generationConfig": generation
        })
    };
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Gemini,
            ApiFormat::Messages,
            gemini(json!({"thinkingConfig": {"thinkingLevel": "high"}})),
            domain,
        )
        .unwrap_err(),
        "Gemini generationConfig.thinkingConfig cannot be preserved",
    );
    assert_rejected(
        convert_request_json_with_replay(
            ApiFormat::Gemini,
            ApiFormat::ChatCompletions,
            gemini(json!({"topK": 40, "temperature": 0.2})),
            domain,
        )
        .unwrap_err(),
        "Gemini generationConfig.topK cannot be preserved",
    );
    let converted = convert_request_json_with_replay(
        ApiFormat::Gemini,
        ApiFormat::Messages,
        gemini(json!({"temperature": 0.2, "topK": null, "thinkingConfig": null})),
        domain,
    )
    .expect("absent or null gemini controls stay unmapped");
    assert_eq!(converted.body["temperature"], 0.2);
    assert!(converted.body.get("topK").is_none());
    assert!(converted.body.get("thinkingConfig").is_none());
    let blocks = converted.body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "hi");
    assert_eq!(blocks[1]["type"], "tool_use");
    assert_eq!(blocks[1]["name"], "read");

    let legacy = convert_req(
        ApiFormat::Gemini,
        ApiFormat::Messages,
        gemini(json!({
            "temperature": 0.2,
            "topK": 40,
            "thinkingConfig": {"thinkingLevel": "high"}
        })),
    );
    assert_eq!(legacy.body["messages"][0]["content"][0]["text"], "hi");
    assert_eq!(legacy.body["messages"][0]["content"][1]["name"], "read");
    assert!(legacy.body.get("thinkingConfig").is_none());
    assert!(legacy.body.get("topK").is_none());
}
