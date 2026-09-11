use super::*;
use crate::kernel::ids::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
};

fn bytes(value: Value) -> Bytes {
    Bytes::from(serde_json::to_vec(&value).expect("test JSON should encode"))
}

fn plan(client: ApiFormat, upstream: ApiFormat) -> RequestPlan {
    plan_with_model(client, upstream, "test")
}

fn plan_with_model(client: ApiFormat, upstream: ApiFormat, model: &str) -> RequestPlan {
    RequestPlan {
        client,
        upstream,
        model: model.into(),
        client_model: model.into(),
        stream: false,
        body: Bytes::new(),
        channel: UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
        resolved_alias: None,
        custom_route: None,
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        response_parallel_tool_calls: true,
        response_tool_choice: json!("auto"),
        response_tools: Vec::new(),
    }
}

#[test]
fn unknown_models_are_not_in_the_protocol_table() {
    assert!(!is_known_model("x-preview-f-free"));
    assert!(!is_known_model("totally-made-up-xyz"));
    assert!(
        prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": "totally-made-up-xyz",
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .is_err()
    );
}

#[test]
fn muse_spark_contributor_routes_every_client_to_responses() {
    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "muse-spark-1.2-contributor",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .expect("Chat should convert Muse Spark contributor to Responses");
    assert_eq!(chat.upstream, ApiFormat::Responses);
    let responses = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "muse-spark-1.2-contributor",
            "input": "hi",
            "store": false
        })),
    )
    .expect("Responses should passthrough Muse Spark contributor");
    assert_eq!(responses.upstream, ApiFormat::Responses);
    let messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "muse-spark-1.2-contributor",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .expect("Messages should convert Muse Spark contributor to Responses");
    assert_eq!(messages.upstream, ApiFormat::Responses);
}

#[test]
fn muse_spark_contributor_free_is_responses_only() {
    for model in [
        "muse-spark-1.2-contributor-free",
        "muse-spark-1.3-contributor-free",
    ] {
        assert!(is_known_model(model), "{model}");
        let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        )
        .unwrap_or_else(|error| panic!("{model} Chat should convert to Responses: {error}"));
        assert_eq!(chat.upstream, ApiFormat::Responses, "{model}");
        let responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model": model,
                "input": "hi",
                "store": false
            })),
        )
        .unwrap_or_else(|error| panic!("{model} Responses should passthrough: {error}"));
        assert_eq!(responses.upstream, ApiFormat::Responses, "{model}");
    }
}

#[test]
fn official_model_defaults_convert_non_native_client_formats() {
    let flash_responses = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "deepseek-v4-flash",
            "input": "hi",
            "store": false
        })),
    )
    .expect("2026-08-27 live_supported Responses passthroughs");
    assert_eq!(flash_responses.upstream, ApiFormat::Responses);

    let flash_messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "deepseek-v4-flash",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(flash_messages.upstream, ApiFormat::Messages);

    let minimax_chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "minimax-m3",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(minimax_chat.upstream, ApiFormat::ChatCompletions);

    let minimax_messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "minimax-m3",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(minimax_messages.upstream, ApiFormat::Messages);

    let luna_chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "gpt-5.6-luna",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(luna_chat.upstream, ApiFormat::Responses);

    let luna_responses = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "gpt-5.6-luna",
            "input": "hi",
            "store": false
        })),
    )
    .unwrap();
    assert_eq!(luna_responses.upstream, ApiFormat::Responses);

    let grok_responses = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "grok-4.5",
            "input": "hi",
            "store": false
        })),
    )
    .unwrap();
    assert_eq!(grok_responses.upstream, ApiFormat::Responses);
}

#[test]
fn grok_converts_chat_and_messages_to_official_responses() {
    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "grok-4.5",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(chat.upstream, ApiFormat::Responses);

    let messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "grok-4.5",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        })),
    )
    .unwrap();
    assert_eq!(messages.upstream, ApiFormat::Responses);
}

#[test]
fn p01_native_chat_and_messages_select_native_endpoints() {
    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "kimi-k3",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(chat.upstream, ApiFormat::ChatCompletions);

    let messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "kimi-k3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        })),
    )
    .unwrap();
    assert_eq!(messages.upstream, ApiFormat::Messages);
}

#[test]
fn minimax_highspeed_models_route_as_messages_and_preserve_priority_tier() {
    let plan = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "minimax-m2.7-highspeed",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8,
            "service_tier": "priority"
        })),
    )
    .expect("highspeed model should be routable");
    assert_eq!(plan.upstream, ApiFormat::Messages);
    assert_eq!(plan.service_tier.as_deref(), Some("priority"));
}

#[test]
fn service_tier_preserves_string_values_and_ignores_non_string_values() {
    // MiniMax-M3 preferred is Messages; Responses is not live_supported so it converts.
    let plan = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "minimax-m3",
            "input": "hi",
            "store": false,
            "service_tier": "priority"
        })),
    )
    .expect("Responses request should convert to Messages");
    let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
    assert_eq!(plan.upstream, ApiFormat::Messages);
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(plan.service_tier.as_deref(), Some("priority"));

    let ignored = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "minimax-m3",
            "store": false,
            "input": "hi",
            "service_tier": {"tier": "priority"}
        })),
    )
    .expect("non-string service tier should retain compatibility");
    let body: Value = serde_json::from_slice(&ignored.body).expect("body is JSON");
    assert!(body.get("service_tier").is_none());
    assert_eq!(ignored.service_tier, None);

    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "minimax-m3",
            "messages": [{"role": "user", "content": "hi"}],
            "service_tier": "priority"
        })),
    )
    .expect("Chat request passthroughs the 2026-08-27 live_supported Chat path");
    assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
    let chat_body: Value = serde_json::from_slice(&chat.body).expect("body is JSON");
    assert_eq!(chat_body["service_tier"], "priority");
    assert!(chat_body.get("stream_options").is_none());
}

#[test]
fn chat_passthrough_stream_requests_include_usage() {
    let plan = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "deepseek-v4-flash",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .expect("Chat stream should passthrough");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["messages"][0]["content"], "hi");
}

#[test]
fn gemini_request_converts_text_image_tools_and_json_schema_to_messages() {
    let request = json!({
        "systemInstruction":{"parts":[{"text":"Be concise."}]},
        "contents":[
            {"role":"user","parts":[
                {"text":"Read this image."},
                {"inlineData":{"mimeType":"image/png","data":"aGVsbG8="}}
            ]},
            {"role":"model","parts":[
                {"functionCall":{"id":"call_1","name":"read_file","args":{"path":"Cargo.toml"}}}
            ]},
            {"role":"user","parts":[
                {"functionResponse":{"id":"call_1","name":"read_file","response":{"output":"ok"}}}
            ]}
        ],
        "tools":[{"functionDeclarations":[{
            "name":"read_file","description":"Read a file",
            "parametersJsonSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
        }]}],
        "toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["read_file"]}},
        "generationConfig":{
            "maxOutputTokens":512,"temperature":0.2,"topP":0.95,
            "stopSequences":["<END>"],"responseMimeType":"application/json",
            "responseJsonSchema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}
        }
    });
    let plan = prepare_gemini_request("minimax-m3".into(), false, bytes(request))
        .expect("Gemini request should convert");
    assert_eq!(plan.client, ApiFormat::Gemini);
    assert_eq!(plan.upstream, ApiFormat::Messages);
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["model"], "minimax-m3");
    assert_eq!(body["system"], "Be concise.");
    assert_eq!(
        body["messages"][0]["content"][0]["text"],
        "Read this image."
    );
    assert_eq!(body["messages"][0]["content"][1]["type"], "image");
    assert_eq!(body["messages"][1]["content"][0]["id"], "call_1");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        "{\"output\":\"ok\"}"
    );
    assert_eq!(body["tools"][0]["input_schema"]["required"][0], "path");
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], "read_file");
    assert_eq!(body["max_tokens"], 512);
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
}

#[test]
fn gemini_request_converts_to_chat_and_preserves_structured_output() {
    let plan = prepare_gemini_request(
        "deepseek-v4-flash".into(),
        true,
        bytes(json!({
            "contents":[{"role":"user","parts":[
                {"text":"describe"},
                {"inlineData":{"mimeType":"image/jpeg","data":"aGVsbG8="}}
            ]}],
            "generationConfig":{
                "responseMimeType":"application/json",
                "responseJsonSchema":{"type":"object","properties":{"answer":{"type":"string"}}}
            }
        })),
    )
    .expect("Gemini chat-native request should convert");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    assert!(plan.stream);
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert!(
        body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[test]
fn gemini_response_converts_text_tools_finish_and_usage() {
    let response = transform_between(
        ApiFormat::Messages,
        ApiFormat::Gemini,
        &json!({
            "id":"msg_1","model":"minimax-m3","stop_reason":"tool_use",
            "content":[
                {"type":"text","text":"Checking."},
                {"type":"tool_use","id":"call_2","name":"read_file","input":{"path":"Cargo.toml"}}
            ],
            "usage":{"input_tokens":10,"cache_read_input_tokens":2,"output_tokens":3}
        }),
    )
    .expect("Messages response should convert to Gemini");
    assert_eq!(response["candidates"][0]["finishReason"], "STOP");
    assert_eq!(
        response["candidates"][0]["content"]["parts"][0]["text"],
        "Checking."
    );
    assert_eq!(
        response["candidates"][0]["content"]["parts"][1]["functionCall"]["id"],
        "call_2"
    );
    assert_eq!(response["usageMetadata"]["promptTokenCount"], 12);
    assert_eq!(response["usageMetadata"]["candidatesTokenCount"], 3);
    assert_eq!(response["usageMetadata"]["totalTokenCount"], 15);
    assert_eq!(response["responseId"], "msg_1");
}

#[test]
fn gemini_rejects_unknown_models_and_unsupported_features() {
    let unknown = prepare_gemini_request(
        "gemini-3-pro-preview".into(),
        false,
        bytes(json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]})),
    )
    .expect_err("Gemini cannot become an upstream protocol");
    assert!(unknown.message.contains("unknown model"));

    let cases = [
        json!({"contents":[{"role":"user","parts":[{"fileData":{"mimeType":"image/png","fileUri":"x"}}]}]}),
        json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/svg+xml","data":"aGVsbG8="}}]}]}),
        json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"not base64"}}]}]}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"googleSearch":{}}]}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"urlContext":{}}]}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"x","parameters":{},"parametersJsonSchema":{}}]}]}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"toolConfig":{"functionCallingConfig":{"mode":"VALIDATED"}}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"cachedContent":"cachedContents/1"}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"safetySettings":{}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"seed":7}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"presencePenalty":0.5}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"frequencyPenalty":0.5}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseLogprobs":true}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"logprobs":4}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"mediaResolution":"MEDIA_RESOLUTION_HIGH"}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"topK":"64"}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"thinkingConfig":true}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"candidateCount":"1"}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseModalities":"TEXT"}}),
        json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseMimeType":1}}),
    ];
    for request in cases {
        assert!(prepare_gemini_request("minimax-m3".into(), false, bytes(request)).is_err());
    }

    let empty_safety_settings = json!({
        "contents":[{"role":"user","parts":[{"text":"hi"}]}],
        "safetySettings":[]
    });
    prepare_gemini_request("minimax-m3".into(), false, bytes(empty_safety_settings))
        .expect("empty Gemini safety settings do not change semantics");

    let safety_error = prepare_gemini_request(
        "minimax-m3".into(),
        false,
        bytes(json!({
            "contents":[{"role":"user","parts":[{"text":"hi"}]}],
            "safetySettings":[{
                "category":"HARM_CATEGORY_HATE_SPEECH",
                "threshold":"BLOCK_LOW_AND_ABOVE"
            }]
        })),
    )
    .expect_err("non-empty Gemini safety settings cannot be silently discarded");
    assert_eq!(safety_error.status, StatusCode::BAD_REQUEST);
    assert!(safety_error.message.contains("cannot be preserved"));
}

#[test]
fn gemini_cli_generation_hints_are_accepted_but_not_leaked_upstream() {
    let plan = prepare_gemini_request(
        "minimax-m3".into(),
        false,
        bytes(json!({
            "contents":[{"role":"user","parts":[{"text":"hi"}]}],
            "generationConfig":{
                "temperature":1,
                "topP":0.95,
                "topK":64,
                "thinkingConfig":{"includeThoughts":true}
            }
        })),
    )
    .expect("Gemini CLI defaults must remain compatible");
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["temperature"], 1);
    assert_eq!(body["top_p"], 0.95);
    assert!(body.get("topK").is_none());
    assert!(body.get("top_k").is_none());
    assert!(body.get("thinkingConfig").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn gemini_error_and_usage_use_google_envelopes() {
    let body = format_error(
        ApiFormat::Gemini,
        StatusCode::UNAUTHORIZED,
        "invalid gateway key",
        None,
    );
    assert_eq!(body["error"]["code"], 401);
    assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
    assert_eq!(
        extract_usage(
            ApiFormat::Gemini,
            &json!({"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":3,"cachedContentTokenCount":2}}),
            None,
        ),
        UsageCounts {
            input_tokens: 9,
            output_tokens: 3,
            cached_tokens: 2,
            cache_creation_tokens: 0
        }
    );
}

#[test]
fn messages_request_routes_chat_only_model_with_tools_and_usage() {
    let request = json!({
        "model": "hy3",
        "stream": true,
        "system": [{"type":"text","text":"be terse"}],
        "messages": [
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"reason"},
                {"type":"tool_use","id":"call_1","name":"read","input":{"path":"a"}}
            ]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":"call_1","content":"ok"},
                {"type":"text","text":"continue"}
            ]}
        ],
        "tools": [{"name":"read","description":"read file","input_schema":{"type":"object"}}],
        "tool_choice": {"type":"any"},
        "thinking": {"type":"enabled","budget_tokens":4096}
    });
    let plan = prepare_request(ApiFormat::Messages, bytes(request)).expect("request converts");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    assert!(plan.stream);
    let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
    assert_eq!(
        body["messages"][0],
        json!({"role":"system","content":"be terse"})
    );
    assert_eq!(
        body["messages"][1]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(body["messages"][1]["reasoning_content"], "reason");
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[test]
fn responses_no_reasoning_maps_to_chat_thinking_disabled() {
    let plan = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model":"hy3",
            "input":"hello",
            "store":false,
            "reasoning":{"effort":"none"}
        })),
    )
    .expect("Responses request converts");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn p07_responses_requires_explicit_store_false_and_rejects_stateful_async_fields() {
    for store in [None, Some(Value::Null), Some(json!(true))] {
        let mut request = json!({"model":"minimax-m2.7","input":"hi"});
        if let Some(store) = store {
            request["store"] = store;
        }
        let error = prepare_request(ApiFormat::Responses, bytes(request))
            .expect_err("store must be explicitly false");
        assert!(error.message.contains("requires Responses store=false"));
    }

    for (field, value) in [
        ("previous_response_id", json!("resp_previous")),
        ("conversation", json!("conv_1")),
        ("background", json!(true)),
    ] {
        let mut request = json!({"model":"minimax-m2.7","input":"hi","store":false});
        request[field] = value;
        let error = prepare_request(ApiFormat::Responses, bytes(request))
            .expect_err("unsupported Responses state must fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains(field), "{}", error.message);
    }

    prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model":"minimax-m2.7","input":"hi","store":false,"background":false
        })),
    )
    .expect("explicit stateless flags are supported");
}

#[test]
fn p02_cross_protocol_structured_formats_are_rejected() {
    let cases = [
        (
            ApiFormat::Responses,
            json!({
                "model":"minimax-m2.7","input":"hi","store":false,
                "text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"}}}
            }),
            "text.format",
        ),
        // Chat is native for minimax; use Responses→Chat conversion instead.
        (
            ApiFormat::Responses,
            json!({
                "model":"hy3","input":"hi","store":false,
                "text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"}}}
            }),
            "text.format",
        ),
        (
            ApiFormat::Messages,
            json!({
                "model":"hy3","messages":[{"role":"user","content":"hi"}],
                "output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}
            }),
            "output_config.format",
        ),
        (
            ApiFormat::Responses,
            json!({
                "model":"minimax-m2.7","input":"hi","store":false,
                "tools":[{"type":"custom","name":"patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}]
            }),
            "grammar format",
        ),
    ];
    for (format, request, field) in cases {
        let error = prepare_request(format, bytes(request))
            .expect_err("structured conversion must not silently downgrade");
        assert!(error.message.contains(field), "{}", error.message);
    }

    // Same-protocol structured formats may passthrough (not conversion).
    prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model":"hy3",
                "messages":[{"role":"user","content":"hi"}],
                "response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}}
            })),
        )
        .expect("Chat-native structured format may passthrough");

    prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model":"minimax-m2.7","input":"hi","store":false,
            "text":{"format":{"type":"text"}}
        })),
    )
    .expect("explicit plain text remains convertible");
}

#[test]
fn responses_input_image_file_id_is_rejected() {
    let error = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model":"minimax-m2.7",
            "store":false,
            "input":[{"role":"user","content":[{"type":"input_image","file_id":"file_1"}]}]
        })),
    )
    .expect_err("file-backed images require Files API support");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("input_image.file_id"));
}

#[test]
fn chat_native_tool_history_backfills_nonempty_reasoning() {
    let requests = [
        (
            ApiFormat::Messages,
            json!({
                "model":"hy3",
                "messages":[
                    {"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"f","input":{}}]},
                    {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}
                ]
            }),
        ),
        (
            ApiFormat::Responses,
            json!({
                "model":"hy3",
                "store":false,
                "input":[
                    {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"},
                    {"type":"function_call_output","call_id":"c1","output":"ok"}
                ]
            }),
        ),
    ];

    for (format, request) in requests {
        let plan = prepare_request(format, bytes(request)).expect("request converts");
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        let assistant = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message.get("tool_calls").is_some())
            .expect("assistant tool call exists");
        assert_eq!(
            assistant["reasoning_content"], "Tool call reasoning unavailable.",
            "{format:?}"
        );
    }
}

#[test]
fn chat_request_routes_minimax_to_messages_with_image_and_tool_result() {
    // The current snapshot exposes MiniMax M2.7 through Messages only.
    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "minimax-m2.7",
            "max_tokens": 5000,
            "messages": [{"role":"user","content":"hi"}]
        })),
    )
    .expect("Chat request converts to Messages");
    assert_eq!(chat.upstream, ApiFormat::Messages);

    // Conversion to preferred Messages still runs for unsupported client formats.
    let request = json!({
        "model": "minimax-m2.7",
        "store": false,
        "max_output_tokens": 5000,
        "instructions": "system",
        "input": [
            {"type":"message","role":"user","content":[
                {"type":"input_text","text":"look"},
                {"type":"input_image","image_url":"data:image/png;base64,abc"}
            ]},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{\"x\":1}"},
            {"type":"function_call_output","call_id":"c1","output":"done"}
        ],
        "tools": [{"type":"function","name":"f","parameters":{"type":"object"}}],
        "tool_choice": {"type":"function","name":"f"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("request converts");
    assert_eq!(plan.upstream, ApiFormat::Messages);
    let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
    assert_eq!(body["system"], "system");
    assert_eq!(
        body["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][0]["input"]["x"], 1);
    assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(body["tool_choice"], json!({"type":"tool","name":"f"}));
}

#[test]
fn responses_request_routes_known_model_and_unknown_is_rejected() {
    let request = json!({
        "model":"hy3",
        "store":false,
        "instructions":"system",
        "input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
            {"type":"function_call","call_id":"c","name":"f","arguments":"{}"},
            {"type":"function_call_output","call_id":"c","output":"ok"}
        ],
        "tools":[{"type":"function","name":"f","parameters":{"type":"object"}}]
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("known model routes");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["name"],
        "f"
    );
    assert_eq!(body["messages"][3]["role"], "tool");

    let error = prepare_request(
        ApiFormat::Responses,
        bytes(json!({"model":"unknown","input":"hi","store":false})),
    )
    .expect_err("unknown Responses model must not guess a protocol");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[test]
fn unknown_chat_and_messages_models_fail_closed() {
    let chat = prepare_request(
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model":"custom",
                "messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]}]
            })),
        )
        .expect_err("unknown Chat must not stay native");
    let messages = prepare_request(
        ApiFormat::Messages,
        bytes(json!({"model":"custom","max_tokens":1,"messages":[]})),
    )
    .expect_err("unknown Messages must not stay native");
    assert_eq!(chat.status, StatusCode::BAD_REQUEST);
    assert!(chat.message.contains("unknown model"));
    assert_eq!(messages.status, StatusCode::BAD_REQUEST);
    assert!(messages.message.contains("unknown model"));
}

#[test]
fn unknown_zen_free_suffix_defaults_to_chat() {
    let chat = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "brand-new-promo-free",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1
        })),
    )
    .expect("unknown -free IDs materialize as Chat");
    assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
    assert_eq!(chat.model, "brand-new-promo-free");

    let converted = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "brand-new-promo-free",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .expect("other client formats convert to Chat for unknown -free IDs");
    assert_eq!(converted.upstream, ApiFormat::ChatCompletions);
}

#[test]
fn messages_upstream_moves_system_roles_to_top_level() {
    let native = prepare_request(
            ApiFormat::Messages,
            bytes(json!({
                "model":"minimax-m2.7",
                "max_tokens":128,
                "system":[{"type":"text","text":"existing","cache_control":{"type":"ephemeral"}}],
                "messages":[
                    {"role":"system","content":[{"type":"text","text":"system role","cache_control":{"type":"ephemeral"}}]},
                    {"role":"developer","content":"developer role"},
                    {"role":"user","content":"hello"}
                ]
            })),
        )
        .expect("native Messages request normalizes");
    let body: Value = serde_json::from_slice(&native.body).expect("body is JSON");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["system"].as_array().unwrap().len(), 3);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["system"][1]["text"], "system role");
    assert_eq!(body["system"][2]["text"], "developer role");

    let responses = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7",
                "store":false,
                "instructions":"instructions",
                "input":[
                    {"type":"message","role":"developer","content":[{"type":"input_text","text":"dev"}]},
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
                ]
            })),
        )
        .expect("Responses request normalizes");
    let body: Value = serde_json::from_slice(&responses.body).expect("body is JSON");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["system"][0]["text"], "instructions");
    assert_eq!(body["system"][1]["text"], "dev");
}

#[test]
fn chat_upstream_converts_developer_role_to_system() {
    // Responses 与 Messages 客户端入口共享 message_to_chat 转换链，
    // developer 指令角色都必须归一化为 system，禁止透传 developer。
    for (client, payload) in [
        (
            ApiFormat::Responses,
            json!({
                "model":"hy3",
                "store":false,
                "instructions":"instructions",
                "input":[
                    {"type":"message","role":"developer","content":[{"type":"input_text","text":"dev"}]},
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
                ]
            }),
        ),
        (
            ApiFormat::Messages,
            json!({
                "model":"hy3",
                "messages":[
                    {"role":"developer","content":"dev"},
                    {"role":"user","content":"hello"}
                ]
            }),
        ),
    ] {
        let plan = prepare_request(client, bytes(payload)).unwrap_or_else(|_| {
            panic!("{client:?} request to Chat upstream normalizes developer role")
        });
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
        let body: Value = serde_json::from_slice(&plan.body).expect("body is JSON");
        let messages = body["messages"]
            .as_array()
            .expect("chat messages is an array");
        assert!(
            messages.iter().all(|m| m["role"] != "developer"),
            "{client:?}: chat upstream must not carry developer role"
        );
        assert!(
            messages
                .iter()
                .any(|m| m["role"] == "system" && m["content"] == "dev"),
            "{client:?}: developer message should become a system message"
        );
        assert!(
            messages
                .iter()
                .any(|m| m["role"] == "user" && m["content"] == "hello"),
            "{client:?}: user message must be preserved"
        );
    }
}

#[test]
fn messages_upstream_drops_unsigned_thinking_history() {
    let plan = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model":"minimax-m2.7",
            "messages":[
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"from chat","signature":""},
                    {"type":"thinking","thinking":"native","signature":"sig_123"},
                    {"type":"redacted_thinking","data":"opaque"},
                    {"type":"tool_use","id":"c1","name":"f","input":{}}
                ]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}
            ]
        })),
    )
    .expect("native Messages history normalizes");
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    let content = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["thinking"], "native");
    assert_eq!(content[0]["signature"], "sig_123");
    assert_eq!(content[1]["type"], "redacted_thinking");
    assert_eq!(content[2]["type"], "tool_use");
}

#[test]
fn chat_response_maps_reasoning_tools_stop_and_cache_usage_to_messages() {
    let response = json!({
        "id":"chat1","model":"deepseek-v4-flash",
        "choices":[{"message":{
            "role":"assistant","content":"answer","reasoning":"reason",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]
        },"finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}
    });
    let converted = transform_response(
        &plan(ApiFormat::Messages, ApiFormat::ChatCompletions),
        &response,
    )
    .expect("response converts");
    assert_eq!(converted["content"][0]["type"], "thinking");
    assert_eq!(converted["content"][0]["signature"], "");
    assert_eq!(converted["content"][2]["input"]["x"], 1);
    assert_eq!(converted["stop_reason"], "tool_use");
    assert_eq!(converted["usage"]["input_tokens"], 6);
    assert_eq!(converted["usage"]["cache_read_input_tokens"], 4);
}

#[test]
fn messages_response_maps_reasoning_tools_and_usage_to_both_openai_formats() {
    let response = json!({
        "id":"m1","model":"minimax-m2.7",
        "content":[
            {"type":"thinking","thinking":"reason","signature":"sig_123"},
            {"type":"text","text":"answer"},
            {"type":"tool_use","id":"c1","name":"f","input":{"x":1}}
        ],
        "stop_reason":"tool_use",
        "usage":{"input_tokens":6,"output_tokens":2,"cache_read_input_tokens":4}
    });
    let chat = transform_response(
        &plan(ApiFormat::ChatCompletions, ApiFormat::Messages),
        &response,
    )
    .expect("Messages to Chat");
    assert_eq!(chat["choices"][0]["message"]["reasoning_content"], "reason");
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["usage"]["prompt_tokens"], 10);

    let responses = transform_response(&plan(ApiFormat::Responses, ApiFormat::Messages), &response)
        .expect("Messages to Responses");
    assert_eq!(responses["status"], "completed");
    let reasoning = responses["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(
        decode_anthropic_thinking_block(reasoning["encrypted_content"].as_str().unwrap()).unwrap()
            ["signature"],
        "sig_123"
    );
    assert!(
        responses["output"]
            .as_array()
            .expect("output array")
            .iter()
            .any(|item| item["type"] == "function_call")
    );
    assert_eq!(responses["usage"]["input_tokens"], 10);
}

#[test]
fn minimax_bogus_all_cache_usage_is_sanitized() {
    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("minimax-m3"), None, &mut usage);
    assert_eq!(usage["input_tokens"], 40500);
    assert_eq!(usage["cache_read_input_tokens"], 0);

    // Normal MiniMax usage (new input + cache read) is left untouched.
    let mut usage = json!({"input_tokens":108,"output_tokens":91,"cache_read_input_tokens":14813});
    sanitize_minimax_anthropic_usage(Some("minimax-m3"), None, &mut usage);
    assert_eq!(usage["input_tokens"], 108);
    assert_eq!(usage["cache_read_input_tokens"], 14813);

    // The heuristic only applies to MiniMax models.
    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("qwen3.7-max"), None, &mut usage);
    assert_eq!(usage["cache_read_input_tokens"], 40500);

    // OpenCode Go may return a non-MiniMax model identifier while the request plan still
    // points to MiniMax. The hint must still trigger sanitization.
    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("ocg-generic"), Some("minimax-m3"), &mut usage);
    assert_eq!(usage["input_tokens"], 40500);
    assert_eq!(usage["cache_read_input_tokens"], 0);
}

#[test]
fn messages_response_to_chat_sanitizes_minimax_bogus_all_cache() {
    // OpenCode Go's Anthropic-compatible endpoint sometimes returns a rewritten model
    // id or omits the field entirely; the request plan's model must still trigger
    // sanitization either way.
    for model_field in [Some("minimax-m3"), None] {
        let mut response = json!({
            "id":"m1",
            "content":[{"type":"text","text":"hi"}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}
        });
        if let Some(model) = model_field {
            response["model"] = json!(model);
        }
        let chat = transform_response(
            &plan_with_model(
                ApiFormat::ChatCompletions,
                ApiFormat::Messages,
                "minimax-m3",
            ),
            &response,
        )
        .expect("Messages to Chat");
        assert_eq!(chat["usage"]["prompt_tokens"], 40500);
        assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 0);
    }
}

#[test]
fn chat_to_messages_minimax_end_to_end_sanitizes_bogus_all_cache() {
    // MiniMax accepts Chat natively; exercise Messages→Chat conversion via a client
    // that still needs preferred Messages (Responses). OpenCode may rewrite the
    // response model to an internal id while returning a bogus all-cache signature.
    let plan = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "store": false,
            "max_output_tokens": 8
        })),
    )
    .expect("MiniMax-M3 Responses should route to Messages");
    assert_eq!(plan.upstream, ApiFormat::Messages);

    let upstream_response = json!({
        "id": "msg_1",
        "type": "message",
        "model": "ocg-generic",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 0, "output_tokens": 5, "cache_read_input_tokens": 40669}
    });

    let responses = transform_response(&plan, &upstream_response).expect("Messages to Responses");
    assert_eq!(responses["usage"]["input_tokens"], 40669);
    assert_eq!(
        responses["usage"]["input_tokens_details"]["cached_tokens"],
        0
    );

    let counts = extract_usage(plan.upstream, &upstream_response, Some(&plan.model));
    assert_eq!(counts.input_tokens, 40669);
    assert_eq!(counts.cached_tokens, 0);
}

#[test]
fn extract_usage_sanitizes_minimax_with_model_hint() {
    let usage = json!({
        "type":"message_start",
        "message":{"usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}}
    });
    let counts = extract_usage(ApiFormat::Messages, &usage, Some("minimax-m3"));
    assert_eq!(counts.input_tokens, 40500);
    assert_eq!(counts.cached_tokens, 0);

    // When the upstream response contains a non-MiniMax model identifier but the request
    // plan is MiniMax, the hint must still trigger sanitization.
    let usage = json!({
        "type":"message_start",
        "message":{"model":"qwen3.7-max","usage":{"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500}}
    });
    let counts = extract_usage(ApiFormat::Messages, &usage, Some("minimax-m3"));
    assert_eq!(counts.input_tokens, 40500);
    assert_eq!(counts.cached_tokens, 0);
}

#[test]
fn extract_usage_sanitizes_minimax_chat_completion_usage() {
    // When a MiniMax model is routed/answered over the OpenAI Chat Completions wire
    // format, the bogus all-cache signature appears as prompt_tokens == cached_tokens.
    let usage = json!({
        "model": "minimax-m3",
        "usage": {
            "prompt_tokens": 40669,
            "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 40669}
        }
    });
    let counts = extract_usage(ApiFormat::ChatCompletions, &usage, Some("minimax-m3"));
    assert_eq!(counts.input_tokens, 40669);
    assert_eq!(counts.cached_tokens, 0);
}

#[test]
fn transform_response_sanitizes_minimax_chat_completion_passthrough() {
    // If a MiniMax model name does not route to the Anthropic Messages upstream,
    // the response is passed through in Chat Completions format and still needs
    // the bogus all-cache signature removed.
    let response = json!({
        "id": "chatcmpl-1",
        "model": "minimax-m3",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": 40669,
            "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 40669}
        }
    });
    let converted = transform_response(
        &plan_with_model(
            ApiFormat::ChatCompletions,
            ApiFormat::ChatCompletions,
            "minimax-m3",
        ),
        &response,
    )
    .expect("Chat Completions passthrough should sanitize");
    assert_eq!(converted["usage"]["prompt_tokens"], 40669);
    assert_eq!(
        converted["usage"]["prompt_tokens_details"]["cached_tokens"],
        0
    );
}

#[test]
fn transform_response_rejects_non_object_messages_without_panicking() {
    let plan = plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "minimax-m3");
    for body in [json!([]), json!("not an object"), json!(42), json!(true)] {
        let error = transform_response(&plan, &body)
            .expect_err("non-object Messages response should be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("JSON object"));
    }
}

#[test]
fn transform_response_sanitizes_minimax_messages_for_every_client_format() {
    let response = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "ocg-generic",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 5,
            "cache_read_input_tokens": 40500
        }
    });

    let messages = transform_response(
        &plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "minimax-m3"),
        &response,
    )
    .expect("Messages passthrough should sanitize");
    assert_eq!(messages["usage"]["input_tokens"], 40500);
    assert_eq!(messages["usage"]["cache_read_input_tokens"], 0);

    let responses = transform_response(
        &plan_with_model(ApiFormat::Responses, ApiFormat::Messages, "minimax-m3"),
        &response,
    )
    .expect("Messages to Responses should sanitize");
    assert_eq!(responses["usage"]["input_tokens"], 40500);
    assert_eq!(
        responses["usage"]["input_tokens_details"]["cached_tokens"],
        0
    );

    let gemini = transform_response(
        &plan_with_model(ApiFormat::Gemini, ApiFormat::Messages, "minimax-m3"),
        &response,
    )
    .expect("Messages to Gemini should sanitize");
    assert_eq!(gemini["usageMetadata"]["promptTokenCount"], 40500);
    assert_eq!(gemini["usageMetadata"]["cachedContentTokenCount"], 0);
}

#[test]
fn minimax_model_detection_is_case_and_separator_insensitive() {
    // OpenCode Go / Qwen Cloud docs expose MiniMax IDs with capital letters (MiniMax-M3).
    // The sanitizer must still recognize them.
    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("MiniMax-M3"), None, &mut usage);
    assert_eq!(usage["input_tokens"], 40500);
    assert_eq!(usage["cache_read_input_tokens"], 0);

    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("ocg-generic"), Some("MiniMax_M3"), &mut usage);
    assert_eq!(usage["input_tokens"], 40500);
    assert_eq!(usage["cache_read_input_tokens"], 0);

    // Qwen is unaffected.
    let mut usage = json!({"input_tokens":0,"output_tokens":5,"cache_read_input_tokens":40500});
    sanitize_minimax_anthropic_usage(Some("Qwen3.7-Max"), None, &mut usage);
    assert_eq!(usage["cache_read_input_tokens"], 40500);
}

#[test]
fn mixed_case_minimax_routes_to_messages_native_protocol() {
    let plan = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "MiniMax-M3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        })),
    )
    .expect("MiniMax-M3 should be routable");
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(plan.model, "MiniMax-M3");

    let plan = prepare_request(
        ApiFormat::Messages,
        bytes(json!({
            "model": "MiniMax_M2.7",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8
        })),
    )
    .expect("MiniMax_M2.7 should be routable");
    assert_eq!(plan.upstream, ApiFormat::Messages);
}

#[test]
fn signed_anthropic_thinking_round_trips_and_foreign_reasoning_is_dropped() {
    let response = json!({
        "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
        "content":[
            {"type":"thinking","thinking":"check","signature":"sig_123"},
            {"type":"redacted_thinking","data":"opaque"},
            {"type":"tool_use","id":"call_1","name":"read","input":{"path":"a"}}
        ],
        "usage":{"input_tokens":1,"output_tokens":2}
    });
    let converted = transform_response(&plan(ApiFormat::Responses, ApiFormat::Messages), &response)
        .expect("Messages response converts");
    let output = converted["output"].as_array().unwrap();
    let request = json!({
        "model":"minimax-m2.7",
        "store":false,
        "max_output_tokens":4096,
        "input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"start"}]},
            output[0].clone(),
            {"type":"reasoning","summary":[{"type":"summary_text","text":"foreign"}],"encrypted_content":"foreign-ciphertext"},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"unsigned"}]},
            output[1].clone(),
            output[2].clone(),
            {"type":"function_call_output","call_id":"call_1","output":"ok"}
        ]
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("request converts");
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    let assistant = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"].as_array().unwrap().len(), 3);
    assert_eq!(assistant["content"][0]["signature"], "sig_123");
    assert_eq!(assistant["content"][1]["type"], "redacted_thinking");
    assert_eq!(assistant["content"][2]["type"], "tool_use");
}

#[test]
fn anthropic_terminal_reasons_map_to_responses_incomplete() {
    for (stop_reason, expected_reason) in [
        ("max_tokens", "max_output_tokens"),
        ("model_context_window_exceeded", "max_output_tokens"),
        ("refusal", "content_filter"),
    ] {
        let converted = transform_response(
            &plan(ApiFormat::Responses, ApiFormat::Messages),
            &json!({
                "id":"m1","model":"minimax-m2.7",
                "content":[{"type":"text","text":"partial"}],
                "stop_reason":stop_reason,
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
        )
        .expect("terminal response converts");
        assert_eq!(converted["status"], "incomplete", "{stop_reason}");
        assert_eq!(
            converted["incomplete_details"]["reason"], expected_reason,
            "{stop_reason}"
        );
    }
}

#[test]
fn responses_response_maps_reasoning_tool_and_incomplete_status() {
    let response = json!({
        "id":"r1","model":"deepseek-v4-flash","status":"incomplete",
        "incomplete_details":{"reason":"max_output_tokens"},
        "output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"reason"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"}
        ],
        "usage":{"input_tokens":10,"output_tokens":2,"input_tokens_details":{"cached_tokens":4}}
    });
    let messages = transform_response(&plan(ApiFormat::Messages, ApiFormat::Responses), &response)
        .expect("Responses to Messages");
    assert_eq!(messages["content"][0]["thinking"], "reason");
    assert_eq!(messages["content"][0]["signature"], "");
    assert_eq!(messages["content"][2]["type"], "tool_use");
    assert_eq!(messages["stop_reason"], "tool_use");
    assert_eq!(messages["usage"]["input_tokens"], 6);
}

#[test]
fn pivot_converts_chat_response_to_responses() {
    let response = json!({
        "id":"c","model":"deepseek-v4-flash",
        "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"length"}],
        "usage":{"prompt_tokens":3,"completion_tokens":2}
    });
    let converted = transform_response(
        &plan(ApiFormat::Responses, ApiFormat::ChatCompletions),
        &response,
    )
    .expect("pivot converts");
    assert_eq!(converted["status"], "incomplete");
    assert_eq!(
        converted["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(converted["output"][0]["content"][0]["text"], "ok");
    assert_eq!(converted["id"], "resp_c");
    assert!(
        converted["created_at"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    for field in ["parallel_tool_calls", "tool_choice", "tools"] {
        assert!(converted.get(field).is_some(), "missing {field}");
    }
    assert_eq!(converted["store"], false);
}

#[test]
fn chat_reasoning_uses_private_opaque_history_only_for_chat_upstream() {
    let response = json!({
        "id":"c1","model":"deepseek-v4-flash",
        "choices":[{"message":{
            "role":"assistant","content":null,"reasoning_content":"check first",
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{}"}}]
        },"finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":1,"completion_tokens":2}
    });
    let converted = transform_response(
        &plan(ApiFormat::Responses, ApiFormat::ChatCompletions),
        &response,
    )
    .expect("Chat response converts");
    let output = converted["output"].as_array().unwrap();
    let reasoning = output
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(
        decode_chat_reasoning(reasoning["encrypted_content"].as_str().unwrap()).as_deref(),
        Some("check first")
    );

    let input = json!([
        {"type":"message","role":"user","content":[{"type":"input_text","text":"start"}]},
        reasoning,
        output.iter().find(|item| item["type"] == "function_call").unwrap(),
        {"type":"function_call_output","call_id":"call_1","output":"ok"}
    ]);
    let chat = prepare_request(
        ApiFormat::Responses,
        bytes(json!({"model":"hy3","input":input,"store":false})),
    )
    .expect("Chat-native history converts");
    assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
    let chat_body: Value = serde_json::from_slice(&chat.body).unwrap();
    assert_eq!(chat_body["messages"][1]["reasoning_content"], "check first");

    let messages = prepare_request(
        ApiFormat::Responses,
        bytes(json!({"model":"minimax-m2.7","input":input,"store":false})),
    )
    .expect("Messages-native history ignores Chat opaque reasoning");
    let messages_body: Value = serde_json::from_slice(&messages.body).unwrap();
    assert!(
        messages_body["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] != "thinking")
    );
}

#[test]
fn responses_custom_tool_converts_both_ways() {
    let request = json!({
        "model":"minimax-m2.7",
        "store":false,
        "input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"edit"}]},
            {"type":"custom_tool_call","call_id":"c1","name":"apply_patch","input":"*** Begin Patch"},
            {"type":"custom_tool_call_output","call_id":"c1","output":[
                {"type":"input_text","text":"done"},
                {"type":"input_image","image_url":"data:image/png;base64,abc"}
            ]}
        ],
        "tools":[{"type":"custom","name":"apply_patch","description":"patch"}],
        "tool_choice":"required",
        "parallel_tool_calls":false
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("custom converts");
    assert_eq!(plan.custom_tools, vec!["apply_patch"]);
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["tools"][0]["input_schema"]["required"][0], "input");
    assert_eq!(body["tool_choice"]["type"], "any");
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);
    assert_eq!(
        body["messages"][1]["content"][0]["input"]["input"],
        "*** Begin Patch"
    );
    assert_eq!(
        body["messages"][2]["content"][0]["content"][0]["type"],
        "text"
    );
    assert_eq!(
        body["messages"][2]["content"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );

    let converted = transform_response(
            &plan,
            &json!({
                "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
                "content":[{"type":"tool_use","id":"c2","name":"apply_patch","input":{"input":"patch text"}}],
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
        )
        .expect("custom response converts");
    assert_eq!(converted["output"][0]["type"], "custom_tool_call");
    assert_eq!(converted["output"][0]["input"], "patch text");
    assert_eq!(converted["parallel_tool_calls"], false);
    assert_eq!(converted["tool_choice"], "required");
    assert_eq!(converted["tools"][0]["name"], "apply_patch");
}

#[test]
fn responses_thinking_is_bounded_and_forced_tools_disable_it() {
    let base = json!({
        "model":"minimax-m2.7","input":"hi","store":false,"max_output_tokens":8192,
        "temperature":0.5,"top_p":0.9,
        "tools":[{"type":"function","name":"f","parameters":{"type":"object"}}],
        "tool_choice":"auto","parallel_tool_calls":false,
        "reasoning":{"effort":"high"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(base.clone())).unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["thinking"]["budget_tokens"], 4096);
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);

    let mut forced = base;
    forced["tool_choice"] = json!("required");
    let plan = prepare_request(ApiFormat::Responses, bytes(forced)).unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["tool_choice"]["type"], "any");
}

#[test]
fn messages_response_to_responses_preserves_block_order() {
    let converted = transform_response(
        &plan(ApiFormat::Responses, ApiFormat::Messages),
        &json!({
            "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
            "content":[
                {"type":"thinking","thinking":"reason","signature":"sig"},
                {"type":"text","text":"before"},
                {"type":"tool_use","id":"c1","name":"f","input":{}},
                {"type":"text","text":"after"}
            ],
            "usage":{"input_tokens":1,"output_tokens":1}
        }),
    )
    .unwrap();
    let kinds = converted["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["reasoning", "message", "function_call", "message"]);
}

#[test]
fn responses_namespace_tools_flatten_history_and_restore_response_names() {
    let request = json!({
        "model":"minimax-m2.7",
        "store":false,
        "input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"delegate"}]},
            {"type":"function_call","call_id":"c1","namespace":"multi_agent_v1","name":"spawn_agent","arguments":"{\"task\":\"x\"}"},
            {"type":"function_call_output","call_id":"c1","output":"ok"}
        ],
        "tools":[{
            "type":"namespace","name":"multi_agent_v1","description":"agents","tools":[
                {"type":"function","name":"spawn_agent","description":"spawn","strict":false,"parameters":{"type":"object"}},
                {"type":"custom","name":"send_input","description":"send"}
            ]
        }],
        "tool_choice":{"type":"function","namespace":"multi_agent_v1","name":"spawn_agent"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("namespace converts");
    assert_eq!(plan.namespace_tools.len(), 2);
    assert_eq!(
        plan.namespace_tools[0].flattened,
        "multi_agent_v1__spawn_agent"
    );
    assert_eq!(plan.custom_tools, ["multi_agent_v1__send_input"]);

    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["tools"][0]["name"], "multi_agent_v1__spawn_agent");
    assert_eq!(body["tools"][1]["name"], "multi_agent_v1__send_input");
    assert_eq!(
        body["messages"][1]["content"][0]["name"],
        "multi_agent_v1__spawn_agent"
    );
    assert_eq!(body["tool_choice"]["name"], "multi_agent_v1__spawn_agent");

    let converted = transform_response(
            &plan,
            &json!({
                "id":"m1","model":"minimax-m2.7","stop_reason":"tool_use",
                "content":[
                    {"type":"tool_use","id":"c2","name":"multi_agent_v1__spawn_agent","input":{"task":"y"}},
                    {"type":"tool_use","id":"c3","name":"multi_agent_v1__send_input","input":{"input":"hello"}}
                ],
                "usage":{"input_tokens":1,"output_tokens":1}
            }),
        )
        .expect("namespace response restores");
    assert_eq!(converted["output"][0]["type"], "function_call");
    assert_eq!(converted["output"][0]["namespace"], "multi_agent_v1");
    assert_eq!(converted["output"][0]["name"], "spawn_agent");
    assert_eq!(converted["output"][1]["type"], "custom_tool_call");
    assert_eq!(converted["output"][1]["namespace"], "multi_agent_v1");
    assert_eq!(converted["output"][1]["name"], "send_input");
}

#[test]
fn responses_hosted_tools_and_history_are_ignored_unless_forced() {
    let request = json!({
        "model":"minimax-m2.7",
        "store":false,
        "input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
            {"type":"tool_search_call","call_id":"ts1","execution":"client","arguments":{}},
            {"type":"tool_search_output","call_id":"ts1","status":"completed","execution":"client","tools":[]},
            {"type":"web_search_call","id":"ws1","status":"completed","action":{"type":"search","query":"x"}}
        ],
        "tools":[
            {"type":"function","name":"local","parameters":{"type":"object"}},
            {"type":"tool_search","execution":"client"},
            {"type":"web_search","external_web_access":false}
        ],
        "tool_choice":"auto"
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).expect("hosted tools ignored");
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["name"], "local");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);

    for choice in [
        json!({"type":"web_search"}),
        json!({"type":"tool_search"}),
        json!("web_search"),
        json!("required"),
    ] {
        let request = json!({
            "model":"minimax-m2.7","input":"hi","store":false,
            "tools":[{"type":"web_search"}],
            "tool_choice":choice
        });
        assert!(prepare_request(ApiFormat::Responses, bytes(request)).is_err());
    }
}

#[test]
fn p05_same_protocol_keeps_unknown_native_request_fields() {
    let plan = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "hy3",
            "messages": [{"role": "user", "content": "hi"}],
            "vendor_extension": {"trace_id": "trace_1"},
            "logprobs": true
        })),
    )
    .expect("Chat-native unknown fields must survive passthrough");
    assert_eq!(plan.client, ApiFormat::ChatCompletions);
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["vendor_extension"]["trace_id"], "trace_1");
    assert_eq!(body["logprobs"], true);
}

#[test]
fn p08_strict_hosted_tools_fail_closed_before_outbound() {
    for request in [
        json!({
            "model":"minimax-m2.7","input":"hi","store":false,
            "tools":[{"type":"web_search"}]
        }),
        json!({
            "model":"minimax-m2.7","input":"hi","store":false,
            "tools":[{"type":"web_search"}],
            "tool_choice":"required"
        }),
        json!({
            "model":"minimax-m2.7","input":"hi","store":false,
            "tools":[{"type":"web_search"}],
            "tool_choice":{"type":"web_search"}
        }),
    ] {
        let error = prepare_request(ApiFormat::Responses, bytes(request))
            .expect_err("unsupported hosted tools must fail closed before outbound");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(
            error.message.contains("hosted tool") || error.message.contains("web_search"),
            "{}",
            error.message
        );
    }

    let native = prepare_request(
        ApiFormat::Responses,
        bytes(json!({
            "model":"grok-4.6","input":"hi","store":false,
            "tools":[{"type":"web_search"}],
            "tool_choice":{"type":"web_search"}
        })),
    )
    .expect("native Responses must keep hosted tools");
    assert_eq!(native.client, ApiFormat::Responses);
    assert_eq!(native.upstream, ApiFormat::Responses);
    let body: Value = serde_json::from_slice(&native.body).unwrap();
    assert_eq!(body["tools"][0]["type"], "web_search");
}

#[test]
fn p09_legacy_tool_compat_is_versioned_and_records_downgrade() {
    let request = json!({
        "model":"minimax-m2.7",
        "store":false,
        "input":"hi",
        "tools":[
            {"type":"function","name":"local","parameters":{"type":"object"}},
            {"type":"web_search"}
        ],
        "tool_choice":"auto"
    });
    let before = request.clone();
    let converted = ocg_gateway::protocol::convert_request_json(
        ApiFormat::Responses,
        ApiFormat::Messages,
        request,
    )
    .expect("optional hosted tools may drop under versioned legacy_compat");
    let downgrade = converted
        .legacy_tool_compat
        .expect("legacy drop must be recorded");
    assert_eq!(downgrade.profile, LEGACY_TOOL_COMPAT_PROFILE);
    assert_eq!(downgrade.version, LEGACY_TOOL_COMPAT_VERSION);
    assert_eq!(
        downgrade.dropped_hosted_tools,
        vec!["web_search".to_string()]
    );
    assert_eq!(
        before["tools"].as_array().unwrap().len(),
        2,
        "legacy_compat must not rewrite the caller's stored request config"
    );
}

#[test]
fn responses_messages_add_leading_user_and_reject_empty_input() {
    let plan = prepare_request(
            ApiFormat::Responses,
            bytes(json!({
                "model":"minimax-m2.7",
                "store":false,
                "input":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"continued"}]}]
            })),
        )
        .unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][1]["role"], "assistant");

    assert!(
        prepare_request(
            ApiFormat::Responses,
            bytes(json!({"model":"minimax-m2.7","input":[],"store":false})),
        )
        .is_err()
    );
}

#[test]
fn usage_extracts_and_stream_merge_keeps_latest_totals() {
    let mut counts = UsageCounts::default();
    merge_stream_usage(
        ApiFormat::Messages,
        &json!({"type":"message_start","message":{"usage":{"input_tokens":6,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}}}),
        &mut counts,
        None,
    );
    merge_stream_usage(
        ApiFormat::Messages,
        &json!({"type":"message_delta","usage":{"output_tokens":7}}),
        &mut counts,
        None,
    );
    assert_eq!(
        counts,
        UsageCounts {
            input_tokens: 12,
            output_tokens: 7,
            cached_tokens: 4,
            cache_creation_tokens: 2
        }
    );

    assert_eq!(
        extract_usage(
            ApiFormat::Responses,
            &json!({"response":{"usage":{"input_tokens":9,"output_tokens":3,"input_tokens_details":{"cached_tokens":2}}}}),
            None,
        ),
        UsageCounts {
            input_tokens: 9,
            output_tokens: 3,
            cached_tokens: 2,
            cache_creation_tokens: 0
        }
    );
}

#[test]
fn format_error_uses_client_envelope_and_upstream_message() {
    let body = format_error(
        ApiFormat::Messages,
        StatusCode::TOO_MANY_REQUESTS,
        "fallback",
        Some(&json!({"error":{"message":"limited","type":"rate_limit_error"}})),
    );
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["message"], "limited");
    assert_eq!(body["error"]["type"], "rate_limit_error");
}

#[test]
fn muse_spark_aliases_max_reasoning_effort_to_xhigh_on_responses() {
    let request = json!({
        "model":"muse-spark-1.2","input":"hi","store":false,
        "reasoning":{"effort":"max"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "xhigh");
}

#[test]
fn muse_spark_max_alias_reaches_responses_upstream_from_every_client_protocol() {
    let requests = [
        (
            ApiFormat::Responses,
            json!({
                "model":"muse-spark-1.2","input":"hi","store":false,
                "reasoning":{"effort":"max"}
            }),
        ),
        (
            ApiFormat::ChatCompletions,
            json!({
                "model":"muse-spark-1.2","messages":[{"role":"user","content":"hi"}],
                "reasoning_effort":"max"
            }),
        ),
        (
            ApiFormat::Messages,
            json!({
                "model":"muse-spark-1.2","max_tokens":8192,
                "messages":[{"role":"user","content":"hi"}],
                "output_config":{"effort":"max"}
            }),
        ),
    ];
    for (client, request) in requests {
        let plan = prepare_request(client, bytes(request)).unwrap();
        assert_eq!(plan.upstream, ApiFormat::Responses);
        let body: Value = serde_json::from_slice(&plan.body).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
    }
}

#[test]
fn muse_spark_leaves_non_aliased_effort_untouched() {
    let request = json!({
        "model":"muse-spark-1.2","input":"hi","store":false,
        "reasoning":{"effort":"high"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "high");
}

#[test]
fn non_aliased_model_passes_max_through_unchanged() {
    let request = json!({
        "model":"gpt-5.6-luna","input":"hi","store":false,
        "reasoning":{"effort":"max"}
    });
    let plan = prepare_request(ApiFormat::Responses, bytes(request)).unwrap();
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "max");
}

#[test]
fn prepare_request_records_client_model() {
    let plan = prepare_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "MiniMax-M3",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    assert_eq!(plan.model, "MiniMax-M3");
    assert_eq!(plan.client_model, "MiniMax-M3");
    assert_eq!(plan.response_model(), "MiniMax-M3");
    assert_eq!(plan.log_requested_model(), "MiniMax-M3");
    assert_eq!(plan.log_upstream_model(), "MiniMax-M3");
    assert_eq!(
        crate::gateway::materialize::native_log_identity(&plan)
            .resolved_alias
            .as_deref(),
        Some("minimax-m3")
    );
}

#[test]
fn parse_once_materializes_a_different_upstream_model() {
    let parsed = parse_client_request(
        ApiFormat::ChatCompletions,
        bytes(json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "hi"}]
        })),
    )
    .unwrap();
    let go = materialize_parsed_request(
        &parsed,
        &MaterializeSpec {
            client_model: parsed.requested_model.clone(),
            upstream_model: "deepseek-v4-flash".into(),
            resolved_alias: Some("deepseek-v4-flash".into()),
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            allow_go_fallback: false,
            forced_upstream: None,
            custom_route: None,
        },
    )
    .unwrap();
    let free = materialize_parsed_request(
        &parsed,
        &MaterializeSpec {
            client_model: parsed.requested_model.clone(),
            upstream_model: "deepseek-v4-flash-free".into(),
            resolved_alias: Some("deepseek-v4-flash".into()),
            channel: UpstreamChannel::Free,
            upstream_base_override: Some("https://opencode.ai/zen".into()),
            original_model: Some("deepseek-v4-flash".into()),
            allow_go_fallback: true,
            forced_upstream: None,
            custom_route: None,
        },
    )
    .unwrap();
    assert_eq!(go.model, "deepseek-v4-flash");
    assert_eq!(go.client_model, "deepseek-v4-flash");
    assert_eq!(go.log_requested_model(), "deepseek-v4-flash");
    assert_eq!(go.log_upstream_model(), "deepseek-v4-flash");
    assert_eq!(
        crate::gateway::materialize::native_log_identity(&go)
            .resolved_alias
            .as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(go.channel, UpstreamChannel::Go);
    assert_eq!(free.model, "deepseek-v4-flash-free");
    assert_eq!(free.client_model, "deepseek-v4-flash");
    assert_eq!(free.log_requested_model(), "deepseek-v4-flash");
    assert_eq!(free.log_upstream_model(), "deepseek-v4-flash-free");
    assert_eq!(
        crate::gateway::materialize::native_log_identity(&free)
            .resolved_alias
            .as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(free.channel, UpstreamChannel::Free);
    assert_eq!(free.original_model.as_deref(), Some("deepseek-v4-flash"));
    let go_body: Value = serde_json::from_slice(&go.body).unwrap();
    let free_body: Value = serde_json::from_slice(&free.body).unwrap();
    assert_eq!(go_body["model"], "deepseek-v4-flash");
    assert_eq!(free_body["model"], "deepseek-v4-flash-free");
}

#[test]
fn transform_response_rewrites_model_to_client_name() {
    let mut plan = plan_with_model(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
        "deepseek-v4-flash-free",
    );
    plan.client_model = "deepseek-v4-flash".into();
    let converted = transform_response(
            &plan,
            &json!({
                "id": "chatcmpl-1",
                "model": "upstream-should-not-leak",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }),
        )
        .unwrap();
    assert_eq!(converted["model"], "deepseek-v4-flash");

    let mut messages_plan = plan_with_model(ApiFormat::Messages, ApiFormat::Messages, "glm-5.2");
    messages_plan.client_model = "claude-sonnet-4-6".into();
    let converted = transform_response(
        &messages_plan,
        &json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "glm-5.2",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
    )
    .unwrap();
    assert_eq!(converted["model"], "claude-sonnet-4-6");
}

#[test]
fn format_error_exposes_ambiguous_model_id() {
    let message = "ambiguous_model_id: send a preferred alias instead of this raw id";
    let code = Some(crate::alias::AMBIGUOUS_MODEL_ID);
    let chat = format_error_with_code(
        ApiFormat::ChatCompletions,
        StatusCode::BAD_REQUEST,
        message,
        None,
        code,
    );
    assert_eq!(chat["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);

    let messages = format_error_with_code(
        ApiFormat::Messages,
        StatusCode::BAD_REQUEST,
        message,
        None,
        code,
    );
    assert_eq!(messages["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);

    let responses = format_error_with_code(
        ApiFormat::Responses,
        StatusCode::BAD_REQUEST,
        message,
        None,
        code,
    );
    assert_eq!(responses["error"]["type"], crate::alias::AMBIGUOUS_MODEL_ID);
    assert_eq!(responses["error"]["code"], crate::alias::AMBIGUOUS_MODEL_ID);

    let gemini = format_error_with_code(
        ApiFormat::Gemini,
        StatusCode::BAD_REQUEST,
        message,
        None,
        code,
    );
    assert_eq!(gemini["error"]["status"], "INVALID_ARGUMENT");
    assert_eq!(gemini["error"]["reason"], crate::alias::AMBIGUOUS_MODEL_ID);
    assert_eq!(
        gemini["error"]["details"][0]["reason"],
        crate::alias::AMBIGUOUS_MODEL_ID
    );
}

#[test]
fn command_code_descriptor_is_separate_from_opencode_table() {
    let goat = command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
        .expect("official GOAT raw id");
    assert_eq!(goat.alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS);
    assert_eq!(goat.preferred, ApiFormat::ChatCompletions);
    assert_eq!(goat.supported_upstream, &[ApiFormat::ChatCompletions]);
    assert!(command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::ChatCompletions
    ));
    assert!(!command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::Responses
    ));
    assert!(!command_code_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ApiFormat::Messages
    ));
    assert!(
        command_code_model_protocol(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).is_none(),
        "kebab alias must stay OpenCode-owned; GOAT lookup is exact slash raw id only"
    );
    assert!(
        !opencode_supports_upstream(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            ApiFormat::ChatCompletions
        ),
        "GOAT raw id must not resolve through OpenCode MODEL_PROTOCOLS"
    );
    assert!(opencode_supports_upstream(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
        ApiFormat::ChatCompletions
    ));
    assert_eq!(
        command_code_upstream_path(ApiFormat::ChatCompletions),
        Some("/chat/completions")
    );
    assert_eq!(
        command_code_upstream_path(ApiFormat::Messages),
        Some("/messages")
    );
    assert_eq!(command_code_upstream_path(ApiFormat::Responses), None);
    assert_eq!(command_code_upstream_path(ApiFormat::Gemini), None);
}

#[test]
fn command_code_client_formats_convert_to_chat_and_never_emit_responses() {
    for (client, body) in [
        (
            ApiFormat::ChatCompletions,
            bytes(json!({
                "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        ),
        (
            ApiFormat::Responses,
            bytes(json!({
                "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                "input": "hi",
                "store": false
            })),
        ),
        (
            ApiFormat::Messages,
            bytes(json!({
                "model": COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}]
            })),
        ),
    ] {
        let parsed = parse_client_request(client, body).unwrap();
        let plan = materialize_parsed_request(
            &parsed,
            &MaterializeSpec {
                client_model: parsed.requested_model.clone(),
                upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                resolved_alias: Some(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS.into()),
                channel: UpstreamChannel::Go,
                upstream_base_override: None,
                original_model: None,
                allow_go_fallback: false,
                forced_upstream: None,
                custom_route: None,
            },
        )
        .unwrap();
        assert_eq!(plan.upstream, ApiFormat::ChatCompletions, "{client:?}");
        assert_eq!(
            plan.model, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            "{client:?}"
        );
        assert_ne!(plan.upstream, ApiFormat::Responses);
        assert_eq!(
            command_code_upstream_path(plan.upstream),
            Some("/chat/completions")
        );
    }

    let gemini = parse_gemini_request(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
        false,
        bytes(json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]})),
    )
    .unwrap();
    let plan = materialize_parsed_request(
        &gemini,
        &MaterializeSpec {
            client_model: gemini.requested_model.clone(),
            upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            resolved_alias: Some(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS.into()),
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            allow_go_fallback: false,
            forced_upstream: None,
            custom_route: None,
        },
    )
    .unwrap();
    assert_eq!(plan.client, ApiFormat::Gemini);
    assert_eq!(plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(
        command_code_upstream_path(plan.upstream),
        Some("/chat/completions")
    );
}
