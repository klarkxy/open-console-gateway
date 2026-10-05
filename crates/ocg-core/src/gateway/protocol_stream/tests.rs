use super::*;

fn plan(client: ApiFormat, upstream: ApiFormat) -> RequestPlan {
    RequestPlan {
        client,
        upstream,
        model: "test-model".to_string(),
        client_model: "test-model".to_string(),
        stream: true,
        body: Bytes::new(),
        channel: crate::models::UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        resolved_alias: None,
        custom_route: None,
        replay_domain: None,
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        legacy_tool_compat: None,
        response_parallel_tool_calls: true,
        response_tool_choice: json!("auto"),
        response_tools: Vec::new(),
    }
}

fn convert(client: ApiFormat, upstream: ApiFormat, source: &str) -> String {
    let mut converter = StreamConverter::new(&plan(client, upstream));
    let bytes = source.as_bytes();
    let split = source.find('好').unwrap_or(bytes.len() / 2) + 1;
    let mut output = converter
        .process_chunk(Bytes::copy_from_slice(&bytes[..split]))
        .expect("first split should parse");
    output.extend(
        converter
            .process_chunk(Bytes::copy_from_slice(&bytes[split..]))
            .expect("second split should parse"),
    );
    output.extend(converter.finish().expect("stream should finish"));
    String::from_utf8(output.concat()).expect("output must be UTF-8")
}

#[test]
fn stream_rewrites_model_fields_to_client_name() {
    let mut chat_plan = plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions);
    chat_plan.model = "deepseek-v4-flash-free".into();
    chat_plan.client_model = "deepseek-v4-flash".into();
    let mut converter = StreamConverter::new(&chat_plan);
    let input = concat!(
        "data: {\"id\":\"chat-stream\",\"model\":\"upstream-should-not-leak\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n"
    );
    let output = converter
        .process_chunk(Bytes::from_static(input.as_bytes()))
        .unwrap();
    let text = String::from_utf8(output.concat()).unwrap();
    assert!(
        text.contains("\"model\":\"deepseek-v4-flash\""),
        "chat-model"
    );
    assert!(!text.contains("upstream-should-not-leak"), "chat-leak");

    let mut messages_plan = plan(ApiFormat::Messages, ApiFormat::Messages);
    messages_plan.model = "glm-5.2".into();
    messages_plan.client_model = "claude-sonnet-4-6".into();
    let mut converter = StreamConverter::new(&messages_plan);
    let input = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"upstream-should-not-leak\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = converter
        .process_chunk(Bytes::from_static(input.as_bytes()))
        .unwrap();
    let text = String::from_utf8(output.concat()).unwrap();
    assert!(
        text.contains("\"model\":\"claude-sonnet-4-6\""),
        "messages-model"
    );
    assert!(!text.contains("upstream-should-not-leak"), "messages-leak");
    assert!(text.contains("\"type\":\"message_stop\""), "messages-stop");

    let mut gemini_plan = plan(ApiFormat::Gemini, ApiFormat::ChatCompletions);
    gemini_plan.model = "deepseek-v4-flash-free".into();
    gemini_plan.client_model = "deepseek-v4-flash".into();
    let mut converter = StreamConverter::new(&gemini_plan);
    let input = concat!(
        "data: {\"id\":\"chat-stream\",\"model\":\"upstream-should-not-leak\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n"
    );
    let output = converter
        .process_chunk(Bytes::from_static(input.as_bytes()))
        .unwrap();
    let text = String::from_utf8(output.concat()).unwrap();
    assert!(
        text.contains("\"modelVersion\":\"deepseek-v4-flash\""),
        "gemini-model-version"
    );
    assert!(!text.contains("upstream-should-not-leak"), "gemini-leak");
}

fn messages_text(output: &str) -> String {
    output
        .split("\n\n")
        .filter_map(|frame| parse_sse_frame(frame.as_bytes()).ok().flatten())
        .filter_map(|(_, payload)| serde_json::from_str::<Value>(&payload).ok())
        .filter_map(|value| {
            (value.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta")).then(
                || {
                    value
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                },
            )
        })
        .collect()
}

fn messages_arguments(output: &str) -> String {
    output
        .split("\n\n")
        .filter_map(|frame| parse_sse_frame(frame.as_bytes()).ok().flatten())
        .filter_map(|(_, payload)| serde_json::from_str::<Value>(&payload).ok())
        .filter_map(|value| {
            (value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta")).then(
                || {
                    value
                        .pointer("/delta/partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                },
            )
        })
        .collect()
}

fn pivot_text(events: Vec<PivotEvent>) -> String {
    events
        .into_iter()
        .filter_map(|event| match event {
            PivotEvent::TextDelta { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn responses_custom_input(output: &str) -> String {
    output
        .split("\n\n")
        .filter_map(|frame| parse_sse_frame(frame.as_bytes()).ok().flatten())
        .filter_map(|(_, payload)| serde_json::from_str::<Value>(&payload).ok())
        .filter(|value| {
            value.get("type").and_then(Value::as_str)
                == Some("response.custom_tool_call_input.delta")
        })
        .filter_map(|value| {
            value
                .get("delta")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn same_protocol_is_byte_passthrough() {
    let mut plan = plan(ApiFormat::Messages, ApiFormat::Messages);
    plan.client_model = "m".into();
    let mut converter = StreamConverter::new(&plan);
    let chunk = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
    assert_eq!(
        converter.process_chunk(chunk.clone()).unwrap().concat(),
        chunk.as_ref()
    );
    assert!(converter.finish().unwrap().is_empty());
}

#[test]
fn same_protocol_redacts_a_secret_split_across_text_deltas() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Messages, ApiFormat::Messages),
        Some(secret),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"before opaque/account+\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"key=42 after\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains(secret), "stream leaked secret: {output}");
    assert_eq!(messages_text(&output), "before  after");
}

#[test]
fn p05_same_protocol_keeps_unknown_fields_after_a_false_key_prefix() {
    let mut plan = plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions);
    plan.client_model = "m".into();
    let mut converter = StreamConverter::new_with_known_secret(&plan, Some("sk-real"));
    let source = concat!(
        "event: chunk\ndata: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"s\"}}],\"logprobs\":{\"content\":[{\"token\":\"s\",\"vendor_score\":0.7}]},\"vendor_extension\":{\"trace_id\":\"trace_1\"}}\n\n",
        "event: chunk\ndata: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"afe\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mut output = converter
        .process_chunk(Bytes::from_static(source.as_bytes()))
        .unwrap();
    output.extend(converter.finish().unwrap());
    assert_eq!(output.concat(), source.as_bytes());
}

#[test]
fn same_protocol_false_prefix_buffer_is_bounded() {
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some("sk-real"),
    );
    let prefix = Bytes::from_static(
            b"data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"s\"}}]}\n\n",
        );
    assert!(converter.process_chunk(prefix).unwrap().is_empty());

    let filler = "x".repeat(MAX_PENDING_SSE_BYTES / 4);
    let heartbeat = Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "id":"c",
            "model":"m",
            "choices":[],
            "vendor_extension":filler
        })
    ));
    for _ in 0..5 {
        let _ = converter.process_chunk(heartbeat.clone()).unwrap();
    }

    assert!(converter.passthrough_tainted);
    assert!(converter.deferred_passthrough.is_empty());
    assert_eq!(converter.deferred_passthrough_bytes, 0);
}

#[test]
fn same_protocol_false_prefix_frame_count_is_bounded() {
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some("sk-real"),
    );
    let prefix = Bytes::from_static(
            b"data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"s\"}}]}\n\n",
        );
    assert!(converter.process_chunk(prefix).unwrap().is_empty());

    for _ in 0..MAX_DEFERRED_SSE_FRAMES {
        let _ = converter
            .process_chunk(Bytes::from_static(b"data:\n\n"))
            .unwrap();
    }

    assert!(converter.passthrough_tainted);
    assert!(converter.deferred_passthrough.is_empty());
    assert_eq!(converter.deferred_passthrough_bytes, 0);
}

#[test]
fn messages_text_start_and_delta_cannot_reconstruct_a_secret() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Messages, ApiFormat::Messages),
        Some(secret),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"opaque/account+\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"key=42\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains(secret), "start+delta leaked Key: {output}");
    assert!(!messages_text(&output).contains(secret), "{output}");
}

#[test]
fn responses_text_start_and_delta_cannot_reconstruct_a_secret() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Responses),
        Some(secret),
    );
    let source = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\",\"status\":\"in_progress\"}}\n\n",
        "event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"opaque/account+\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"key=42\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_0\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"opaque/account+key=42\"}]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"model\":\"m\",\"status\":\"completed\"}}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains(secret), "start+delta leaked Key: {output}");
}

#[test]
fn adjacent_text_blocks_cannot_reconstruct_a_split_secret() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Messages, ApiFormat::Messages),
        Some(secret),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"opaque/account+\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"key=42\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    let reconstructed = messages_text(&output);
    assert!(!output.contains(secret), "stream leaked secret: {output}");
    assert!(
        !reconstructed.contains(secret),
        "text blocks reconstructed secret: {reconstructed}"
    );
}

#[test]
fn signature_prefix_stays_with_its_original_reasoning_block() {
    let mut redactor = StreamSecretRedactor::new(Some("data"));
    let first = redactor.redact_events(vec![
        PivotEvent::BlockStart {
            index: 0,
            kind: BlockKind::Reasoning,
        },
        PivotEvent::SignatureDelta {
            index: 0,
            signature: "da".into(),
        },
        PivotEvent::BlockStop { index: 0 },
    ]);
    assert!(
        first
            .events
            .iter()
            .all(|event| !matches!(event, PivotEvent::SignatureDelta { .. }))
    );

    let second = redactor.redact_events(vec![
        PivotEvent::BlockStart {
            index: 1,
            kind: BlockKind::Reasoning,
        },
        PivotEvent::SignatureDelta {
            index: 1,
            signature: "sig".into(),
        },
        PivotEvent::BlockStop { index: 1 },
        PivotEvent::Stop,
    ]);
    let signatures = second
        .events
        .into_iter()
        .filter_map(|event| match event {
            PivotEvent::SignatureDelta { index, signature } => Some((index, signature)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(signatures, vec![(0, "da".into()), (1, "sig".into())]);
}

#[test]
fn converted_stream_redacts_a_secret_split_across_text_deltas() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Messages),
        Some(secret),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"opaque/account+\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"key=42\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains(secret), "stream leaked secret: {output}");
    assert!(output.contains("response.completed"), "{output}");
}

#[test]
fn stream_secret_redactor_releases_false_prefixes_and_handles_overlap() {
    let mut false_prefix = StreamSecretRedactor::new(Some("opaque/account+key=42"));
    let first = false_prefix.redact_events(vec![PivotEvent::TextDelta {
        index: 0,
        text: "opaque/".into(),
    }]);
    assert!(first.events.is_empty());
    let second = false_prefix.redact_events(vec![PivotEvent::TextDelta {
        index: 0,
        text: "other".into(),
    }]);
    assert_eq!(pivot_text(second.events), "opaque/other");

    let mut overlap = StreamSecretRedactor::new(Some("aaaa"));
    let first = overlap.redact_events(vec![PivotEvent::TextDelta {
        index: 0,
        text: "aa".into(),
    }]);
    assert!(first.events.is_empty());
    let second = overlap.redact_events(vec![
        PivotEvent::TextDelta {
            index: 0,
            text: "aaa".into(),
        },
        PivotEvent::BlockStop { index: 0 },
    ]);
    assert!(pivot_text(second.events).is_empty());
    let terminal = overlap.redact_events(vec![PivotEvent::Stop]);
    assert_eq!(pivot_text(terminal.events), "a");
}

#[test]
fn safe_secret_prefix_is_released_at_the_message_boundary() {
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Messages, ApiFormat::Messages),
        Some("data"),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"panda\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert_eq!(messages_text(&output), "panda", "{output}");
}

#[test]
fn short_secret_redaction_preserves_sse_framing_and_json_keys() {
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some("data"),
    );
    let source = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"metadata\":{\"database\":\"safe\",\"echo\":\"data\"},\"choices\":[]}\n\n",
        "data: [DONE]\n\n"
    );
    let output = converter
        .process_chunk(Bytes::from_static(source.as_bytes()))
        .unwrap()
        .concat();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("data: "), "{output}");
    assert!(output.contains("\"metadata\""), "{output}");
    assert!(output.contains("\"database\""), "{output}");
    assert!(output.contains("\"echo\":\"<redacted>\""), "{output}");
    assert!(output.contains("data: [DONE]"), "{output}");
}

#[test]
fn tool_arguments_are_redacted_across_delta_boundaries() {
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Messages, ApiFormat::Messages),
        Some("data"),
    );
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"run\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"data\\\":\\\"safe\\\",\\\"token\\\":\\\"da\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ta\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    let arguments = messages_arguments(&output);
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).unwrap(),
        json!({"data":"safe","token":""})
    );
}

#[test]
fn json_argument_redactor_preserves_keys_and_handles_escaped_secrets() {
    for secret in ["data", "a\"b", "a\\b"] {
        let source = json!({"data":"safe","token":secret}).to_string();
        let encoded_secret = json_string_contents(secret);
        let start = source.find(&encoded_secret).unwrap();
        let split = start + encoded_secret.len() / 2;
        let mut redactor = JsonArgumentRedactor::default();
        let (first, _, _) = redactor.process(&source[..split], secret);
        let (second, _, _) = redactor.process(&source[split..], secret);
        let output = format!("{first}{second}{}", redactor.finish(secret));
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"data":"safe","token":""}),
            "failed to redact {secret:?}: {output}"
        );
    }
}

#[test]
fn json_argument_redactor_decodes_equivalent_json_escapes_across_chunks() {
    for (secret, source) in [
        ("A", r#"{"data":"safe","token":"\u0041"}"#),
        ("/", r#"{"data":"safe","token":"\/"}"#),
        ("😀", r#"{"data":"safe","token":"\uD83D\uDE00"}"#),
    ] {
        for split in 1..source.len() {
            let mut redactor = JsonArgumentRedactor::default();
            let (first, _, _) = redactor.process(&source[..split], secret);
            let (second, _, _) = redactor.process(&source[split..], secret);
            let output = format!("{first}{second}{}", redactor.finish(secret));
            assert_eq!(
                serde_json::from_str::<Value>(&output).unwrap(),
                json!({"data":"safe","token":""}),
                "failed to redact {secret:?} at split {split}: {output}"
            );
        }
    }
}

#[test]
fn json_argument_redactor_streams_safe_open_string_content_immediately() {
    let source = r#"{"input":"hello world"#;
    let mut redactor = JsonArgumentRedactor::default();
    let (output, changed, _) = redactor.process(source, "opaque/account+key=42");
    assert_eq!(output, source);
    assert!(!changed);
}

#[test]
fn legacy_chat_function_arguments_are_semantically_redacted() {
    let secret = "A";
    let arguments = r#"{"data":"safe","token":"\u0041"}"#;
    let source = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"id":"c","model":"m","choices":[{"delta":{"function_call":{"name":"run","arguments":arguments}},"finish_reason":null}]}),
        json!({"id":"c","model":"m","choices":[{"delta":{},"finish_reason":"function_call"}]})
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some(secret),
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains("\\u0041"), "escaped Key leaked: {output}");

    let arguments = output
        .split("\n\n")
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .filter_map(|value| {
            value
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<String>();
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).unwrap(),
        json!({"data":"safe","token":""})
    );
}

#[test]
fn chat_refusal_key_is_redacted_across_delta_boundaries() {
    let secret = "opaque/account+key=42";
    let source = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"refusal\":\"opaque/account+\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"refusal\":\"key=42\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some(secret),
    );
    let mut output = converter
        .process_chunk(Bytes::from_static(source.as_bytes()))
        .unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(
        !output.contains(secret),
        "refusal stream leaked Key: {output}"
    );
    assert!(output.contains("data: [DONE]"), "{output}");
}

#[test]
fn responses_custom_tool_input_is_redacted_across_delta_and_done_events() {
    let secret = "opaque/account+key=42";
    let mut custom_plan = plan(ApiFormat::Responses, ApiFormat::Responses);
    custom_plan.custom_tools = vec!["apply_patch".to_string()];
    let mut converter = StreamConverter::new_with_known_secret(&custom_plan, Some(secret));
    let source = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_0\",\"call_id\":\"call_0\",\"name\":\"apply_patch\",\"input\":\"\",\"status\":\"in_progress\"}}\n\n",
        "event: response.custom_tool_call_input.delta\ndata: {\"type\":\"response.custom_tool_call_input.delta\",\"output_index\":0,\"delta\":\"before opaque/account+\"}\n\n",
        "event: response.custom_tool_call_input.delta\ndata: {\"type\":\"response.custom_tool_call_input.delta\",\"output_index\":0,\"delta\":\"key=42 after\"}\n\n",
        "event: response.custom_tool_call_input.done\ndata: {\"type\":\"response.custom_tool_call_input.done\",\"output_index\":0,\"input\":\"before opaque/account+key=42 after\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_0\",\"call_id\":\"call_0\",\"name\":\"apply_patch\",\"input\":\"before opaque/account+key=42 after\",\"status\":\"completed\"}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"model\":\"m\",\"status\":\"completed\"}}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(
        !output.contains(secret),
        "custom stream leaked Key: {output}"
    );
    assert_eq!(responses_custom_input(&output), "before  after");
    assert!(output.contains("response.completed"), "{output}");
}

#[test]
fn responses_initial_tool_payload_is_redacted_before_the_first_chunk_returns() {
    let secret = "opaque/account+key=42";
    for item in [
        json!({
            "type":"function_call",
            "id":"fc_0",
            "call_id":"call_0",
            "name":"run",
            "arguments":json!({"token":secret}).to_string(),
            "status":"in_progress"
        }),
        json!({
            "type":"custom_tool_call",
            "id":"ctc_0",
            "call_id":"call_0",
            "name":"apply_patch",
            "input":format!("before {secret} after"),
            "status":"in_progress"
        }),
    ] {
        let mut converter = StreamConverter::new_with_known_secret(
            &plan(ApiFormat::Responses, ApiFormat::Responses),
            Some(secret),
        );
        let source = format!(
            "event: response.created\ndata: {}\n\nevent: response.output_item.added\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"r","model":"m","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":item})
        );
        let output = converter
            .process_chunk(Bytes::from(source))
            .unwrap()
            .concat();
        let output = String::from_utf8(output).unwrap();
        assert!(
            !output.contains(secret),
            "initial tool payload leaked: {output}"
        );
    }
}

#[test]
fn responses_empty_tool_delta_does_not_hide_authoritative_done_arguments() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Responses),
        Some(secret),
    );
    let arguments = json!({"token":secret}).to_string();
    let source = format!(
        "event: response.created\ndata: {}\n\nevent: response.output_item.added\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.function_call_arguments.done\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        json!({"type":"response.created","response":{"id":"r","model":"m","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_0","call_id":"call_0","name":"run","arguments":"","status":"in_progress"}}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":""}),
        json!({"type":"response.function_call_arguments.done","output_index":0,"arguments":arguments}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_0","call_id":"call_0","name":"run","arguments":arguments,"status":"completed"}}),
        json!({"type":"response.completed","response":{"id":"r","model":"m","status":"completed"}})
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(
        !output.contains(secret),
        "empty delta bypass leaked Key: {output}"
    );
    let done = output
        .split("\n\n")
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .find(|value| {
            value.get("type").and_then(Value::as_str)
                == Some("response.function_call_arguments.done")
        })
        .expect("arguments done event");
    assert_eq!(
        serde_json::from_str::<Value>(done["arguments"].as_str().unwrap()).unwrap(),
        json!({"token":""})
    );
}

#[test]
fn same_protocol_redacts_all_non_data_sse_metadata_values() {
    let secret = "opaque/account+key=42";
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions),
        Some(secret),
    );
    let source = format!(
        ": keep {secret}\r\nid: request-{secret}\r\nevent: chunk-{secret}\r\nretry: 500-{secret}\r\nx-provider-meta: trace-{secret}\r\ndata: {{\"id\":\"c\",\"model\":\"m\",\"choices\":[]}}\r\n\r\ndata: [DONE]\r\n\r\n"
    );
    let output = converter
        .process_chunk(Bytes::from(source))
        .unwrap()
        .concat();
    let output = String::from_utf8(output).unwrap();
    assert!(
        !output.contains(secret),
        "SSE metadata leaked Key: {output}"
    );
    assert!(output.contains(": keep <redacted>\r\n"), "{output}");
    assert!(output.contains("id: request-<redacted>\r\n"), "{output}");
    assert!(output.contains("event: chunk-<redacted>\r\n"), "{output}");
    assert!(output.contains("retry: 500-<redacted>\r\n"), "{output}");
    assert!(
        output.contains("x-provider-meta: trace-<redacted>\r\n"),
        "{output}"
    );
    assert!(output.contains("data: "), "{output}");
    assert!(output.contains("data: [DONE]\r\n\r\n"), "{output}");
}

#[test]
fn generated_stream_errors_redact_the_known_secret() {
    let converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Messages),
        Some("opaque/account+key=42"),
    );
    let output = converter
        .outcome_unknown_event("provider echoed opaque/account+key=42")
        .concat();
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("opaque/account+key=42"), "{output}");
}

#[test]
fn chat_passthrough_keeps_non_minimax_frames_byte_identical() {
    let mut qwen_plan = plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions);
    qwen_plan.model = "qwen3.7-max".into();
    qwen_plan.client_model = "qwen3.7-max".into();
    let mut converter = StreamConverter::new(&qwen_plan);
    let frame = Bytes::from_static(
            b": keepalive\r\nid: 7\r\nretry: 1000\r\nevent: chunk\r\ndata: { \"model\": \"qwen3.7-max\", \"choices\": [], \"usage\": {\"prompt_tokens\":10,\"completion_tokens\":1,\"prompt_tokens_details\":{\"cached_tokens\":10}} }\r\n\r\n",
        );
    assert_eq!(
        converter.process_chunk(frame.clone()).unwrap().concat(),
        frame.as_ref()
    );
}

#[test]
fn chat_passthrough_keeps_unchanged_minimax_frames_byte_identical() {
    let mut minimax_plan = plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions);
    minimax_plan.model = "minimax-m3".into();
    minimax_plan.client_model = "ocg-generic".into();
    let mut converter = StreamConverter::new(&minimax_plan);
    let frame = Bytes::from_static(
            b"id: 8\nevent: chunk\ndata: { \"model\": \"ocg-generic\", \"choices\": [{\"delta\":{\"content\":\"hi\"}}] }\n\n",
        );
    assert_eq!(
        converter.process_chunk(frame.clone()).unwrap().concat(),
        frame.as_ref()
    );
}

#[test]
fn chat_passthrough_preserves_reported_minimax_usage_data() {
    let mut minimax_plan = plan(ApiFormat::ChatCompletions, ApiFormat::ChatCompletions);
    minimax_plan.model = "minimax-m3".into();
    let mut converter = StreamConverter::new(&minimax_plan);
    let frame = Bytes::from_static(
            b": keepalive\r\nid: 9\r\nretry: 1500\r\nevent: chunk\r\ndata: {\"model\":\"ocg-generic\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":1,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\r\n\r\n",
        );
    let output = converter.process_chunk(frame).unwrap().concat();
    let output = String::from_utf8(output).unwrap();
    assert!(output.starts_with(": keepalive\r\nid: 9\r\nretry: 1500\r\nevent: chunk\r\n"));
    assert!(output.ends_with("\r\n\r\n"));
    assert!(output.contains("\"prompt_tokens\":10"), "{output}");
    assert!(output.contains("\"cached_tokens\":10"), "{output}");
}

#[test]
fn messages_passthrough_preserves_minimax_usage_and_sse_fields() {
    let mut minimax_plan = plan(ApiFormat::Messages, ApiFormat::Messages);
    minimax_plan.model = "minimax-m3".into();
    let mut converter = StreamConverter::new(&minimax_plan);
    let frame = Bytes::from_static(
            b": keepalive\r\nid: msg-9\r\nretry: 1500\r\nevent: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"ocg-generic\",\"usage\":{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}}\r\n\r\n",
        );
    let output = converter.process_chunk(frame).unwrap().concat();
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.starts_with(": keepalive\r\nid: msg-9\r\nretry: 1500\r\nevent: message_start\r\n")
    );
    assert!(output.ends_with("\r\n\r\n"));
    assert!(output.contains("\"input_tokens\":0"), "{output}");
    assert!(
        output.contains("\"cache_read_input_tokens\":40500"),
        "{output}"
    );
}

#[test]
fn same_protocol_drops_events_after_terminal() {
    let mut converter = StreamConverter::new(&plan(ApiFormat::Messages, ApiFormat::Messages));
    let chunk = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"m\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"late\"}}\n\n",
        );
    let output = converter.process_chunk(chunk).unwrap().concat();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("message_stop"));
    assert!(!output.contains("late"));

    let later = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"later\"}}\n\n",
    );
    assert!(converter.process_chunk(later).unwrap().is_empty());
    assert!(converter.finish().unwrap().is_empty());
}

#[test]
fn empty_data_heartbeat_is_ignored() {
    let mut converter =
        StreamConverter::new(&plan(ApiFormat::Messages, ApiFormat::ChatCompletions));
    assert!(
        converter
            .process_chunk(Bytes::from_static(b"data:\n\n"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn chat_to_gemini_streams_text_usage_and_finishes_without_done_sentinel() {
    let source = concat!(
        "data: {\"id\":\"resp_1\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"resp_1\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":1}}}\n\n",
        "data: [DONE]\n\n"
    );
    let output = convert(ApiFormat::Gemini, ApiFormat::ChatCompletions, source);
    assert!(output.contains("\"text\":\"Hel\""));
    assert!(output.contains("\"text\":\"lo\""));
    assert!(output.contains("\"finishReason\":\"STOP\""));
    assert!(output.contains("\"promptTokenCount\":7"));
    assert!(output.contains("\"candidatesTokenCount\":2"));
    assert!(!output.contains("[DONE]"));
    assert_eq!(output.matches("\"responseId\":\"resp_1\"").count(), 3);
}

#[test]
fn messages_to_gemini_buffers_parallel_function_calls_until_valid_json() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"model\":\"minimax-m3\",\"usage\":{\"input_tokens\":12}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_a\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_b\",\"name\":\"list_dir\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = convert(ApiFormat::Gemini, ApiFormat::Messages, source);
    assert_eq!(output.matches("\"functionCall\"").count(), 2);
    assert!(output.contains("\"id\":\"call_a\""));
    assert!(output.contains("\"path\":\"Cargo.toml\""));
    assert!(output.contains("skip_thought_signature_validator"));
    assert!(output.contains("\"finishReason\":\"STOP\""));
    assert!(output.contains("\"promptTokenCount\":12"));
    assert!(!output.contains("[DONE]"));
}

#[test]
fn gemini_stream_errors_use_google_envelope_without_done() {
    let converter = StreamConverter::new(&plan(ApiFormat::Gemini, ApiFormat::Messages));
    let output = String::from_utf8(converter.error_event("boom").concat()).unwrap();
    assert!(output.contains("\"code\":500"));
    assert!(output.contains("\"status\":\"INTERNAL\""));
    assert!(output.contains("\"message\":\"boom\""));
    assert!(!output.contains("[DONE]"));
}

#[test]
fn chat_to_messages_handles_utf8_reasoning_parallel_tools_and_usage() {
    let source = concat!(
        "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"想好\",\"content\":\"你好\",\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"one\",\"arguments\":\"{\\\"x\\\":\"}},{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"two\",\"arguments\":\"{\\\"y\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}},{\"index\":1,\"function\":{\"arguments\":\"2}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n",
        "data: [DONE]\n\n"
    );
    let output = convert(ApiFormat::Messages, ApiFormat::ChatCompletions, source);
    assert!(output.contains("thinking_delta"));
    assert!(output.contains("你好"));
    assert_eq!(output.matches("tool_use\"").count(), 3); // two starts + stop reason
    assert!(output.contains("\"output_tokens\":4"));
    assert!(output.contains("message_stop"));

    let mut open_non_tool = None;
    for frame in output.split("\n\n") {
        let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let kind = value
                    .pointer("/content_block/type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if matches!(kind, "thinking" | "text") {
                    assert!(open_non_tool.is_none(), "non-tool blocks must not overlap");
                    open_non_tool = value.get("index").and_then(Value::as_u64);
                } else if kind == "tool_use" {
                    assert!(
                        open_non_tool.is_none(),
                        "thinking/text must close before a tool block"
                    );
                }
            }
            Some("content_block_stop")
                if value.get("index").and_then(Value::as_u64) == open_non_tool =>
            {
                open_non_tool = None;
            }
            _ => {}
        }
    }
    assert!(open_non_tool.is_none());
}

#[test]
fn messages_to_chat_translates_tools_usage_and_done() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":5}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = convert(ApiFormat::ChatCompletions, ApiFormat::Messages, source);
    assert!(output.contains("tool_calls"));
    assert!(output.contains("\"completion_tokens\":2"));
    assert!(output.ends_with("data: [DONE]\n\n"));
}

#[test]
fn responses_can_feed_both_other_protocols() {
    let source = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"好\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\n"
    );
    let messages = convert(ApiFormat::Messages, ApiFormat::Responses, source);
    let chat = convert(ApiFormat::ChatCompletions, ApiFormat::Responses, source);
    assert!(messages.contains("text_delta"));
    assert!(messages.contains("message_stop"));
    assert!(chat.contains("\"content\":\"好\""));
    assert!(chat.ends_with("data: [DONE]\n\n"));
}

#[test]
fn both_other_protocols_can_feed_responses() {
    let chat = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let messages = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    for output in [
        convert(ApiFormat::Responses, ApiFormat::ChatCompletions, chat),
        convert(ApiFormat::Responses, ApiFormat::Messages, messages),
    ] {
        assert!(output.contains("response.output_item.added"));
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.completed"));
        let timestamps = output
            .split("\n\n")
            .filter(|frame| {
                frame.starts_with("event: response.created")
                    || frame.starts_with("event: response.completed")
            })
            .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
            .map(|payload| {
                serde_json::from_str::<Value>(payload).unwrap()["response"]["created_at"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(timestamps.len(), 2);
        assert!(timestamps[0] > 0);
        assert_eq!(timestamps[0], timestamps[1]);
    }
}

#[test]
fn responses_tool_arguments_done_includes_function_name() {
    let source = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let output = convert(ApiFormat::Responses, ApiFormat::ChatCompletions, source);
    let frame = output
        .split("\n\n")
        .find(|frame| frame.contains("response.function_call_arguments.done"))
        .expect("arguments done event");
    let payload = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("event data");
    let value: Value = serde_json::from_str(payload).expect("valid event JSON");
    assert_eq!(value["name"], "read");
}

#[test]
fn responses_restores_custom_tool_call_shape() {
    let mut custom_plan = plan(ApiFormat::Responses, ApiFormat::Messages);
    custom_plan.custom_tools = vec!["apply_patch".to_string()];
    custom_plan.response_parallel_tool_calls = false;
    custom_plan.response_tool_choice = json!("required");
    custom_plan.response_tools = vec![json!({"type":"custom","name":"apply_patch"})];
    let mut converter = StreamConverter::new(&custom_plan);
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"apply_patch\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"*** Begin\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" Patch\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(output.contains("\"type\":\"custom_tool_call\""));
    assert!(output.contains("\"name\":\"apply_patch\""));
    assert!(output.contains("\"input\":\"*** Begin Patch\""));
    assert!(!output.contains("response.function_call_arguments.delta"));
    let created = output
        .split("\n\n")
        .find(|frame| frame.starts_with("event: response.created"))
        .and_then(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .map(|payload| serde_json::from_str::<Value>(payload).unwrap())
        .unwrap();
    assert_eq!(created["response"]["parallel_tool_calls"], false);
    assert_eq!(created["response"]["tool_choice"], "required");
    assert_eq!(created["response"]["tools"][0]["name"], "apply_patch");
    let deltas = output
        .split("\n\n")
        .filter(|frame| frame.contains("response.custom_tool_call_input.delta"))
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .map(|payload| serde_json::from_str::<Value>(payload).unwrap()["delta"].clone())
        .collect::<Vec<_>>();
    assert_eq!(deltas, [json!("*** Begin"), json!(" Patch")]);
}

#[test]
fn responses_restores_namespace_tool_identity() {
    let mut namespace_plan = plan(ApiFormat::Responses, ApiFormat::Messages);
    namespace_plan.namespace_tools = vec![NamespaceToolMapping {
        flattened: "multi_agent_v1__spawn_agent".to_string(),
        namespace: "multi_agent_v1".to_string(),
        name: "spawn_agent".to_string(),
        custom: false,
    }];
    let mut converter = StreamConverter::new(&namespace_plan);
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"multi_agent_v1__spawn_agent\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(output.contains("\"namespace\":\"multi_agent_v1\""));
    assert!(output.contains("\"name\":\"spawn_agent\""));
    assert!(!output.contains("\"name\":\"multi_agent_v1__spawn_agent\""));
}

#[test]
fn streaming_usage_normalizes_cached_tokens_for_each_target() {
    let messages = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":6,\"cache_read_input_tokens\":4,\"cache_creation_input_tokens\":2}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let chat = convert(ApiFormat::ChatCompletions, ApiFormat::Messages, messages);
    assert!(chat.contains("\"prompt_tokens\":12"));
    assert!(chat.contains("\"cached_tokens\":4"));

    let chat_source = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
        "data: [DONE]\n\n"
    );
    let anthropic = convert(ApiFormat::Messages, ApiFormat::ChatCompletions, chat_source);
    assert!(anthropic.contains("\"input_tokens\":8"));
    assert!(anthropic.contains("\"cache_read_input_tokens\":4"));
}

#[test]
fn chat_converter_captures_trailing_include_usage_chunk() {
    let mut converter = StreamConverter::new(&plan(
        ApiFormat::ChatCompletions,
        ApiFormat::ChatCompletions,
    ));
    let source = concat!(
        "data: {\"id\":\"c\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"c\",\"model\":\"deepseek-v4-flash\",\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":4,\"prompt_tokens_details\":{\"cached_tokens\":2}}}\n\n",
        "data: [DONE]\n\n"
    );
    converter
        .process_chunk(Bytes::from(source))
        .expect("stream should parse");
    converter.finish().expect("stream should finish");
    let usage = converter.captured_usage().expect("trailing usage chunk");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(usage.cached_tokens, 2);
}

#[test]
fn streaming_messages_to_chat_preserves_reported_minimax_all_cache() {
    // OpenCode Go may omit the model field in message_start or report the id in
    // mixed case ("MiniMax-M3"); usage stays provider-reported in every shape.
    for model in ["minimax-m3", "MiniMax-M3"] {
        for start_model in [Some(model), None] {
            let message = match start_model {
                Some(model) => json!({
                    "id":"msg_1","model":model,
                    "usage":{"input_tokens":0,"cache_read_input_tokens":40500}
                }),
                None => json!({
                    "id":"msg_1",
                    "usage":{"input_tokens":0,"cache_read_input_tokens":40500}
                }),
            };
            let source = format!(
                "event: message_start\ndata: {}\n\n\
                     event: message_delta\ndata: {}\n\n\
                     event: message_stop\ndata: {}\n\n",
                json!({"type":"message_start","message":message}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
                json!({"type":"message_stop"}),
            );
            let mut request = plan(ApiFormat::ChatCompletions, ApiFormat::Messages);
            request.model = model.into();
            let mut converter = StreamConverter::new(&request);
            let bytes = source.as_bytes();
            let mut output = converter
                .process_chunk(Bytes::copy_from_slice(bytes))
                .unwrap();
            output.extend(converter.finish().unwrap());
            let output = String::from_utf8(output.concat()).unwrap();
            assert!(
                output.contains("\"prompt_tokens\":40500"),
                "model={model} start_model={start_model:?}"
            );
            assert!(
                output.contains("\"cached_tokens\":40500"),
                "model={model} start_model={start_model:?}"
            );
        }
    }
}

fn replay_domain() -> ocg_gateway::protocol::ReplayDomain {
    ocg_gateway::protocol::ReplayDomain::parse(&"ef".repeat(32)).unwrap()
}

fn finish_stream(plan: &RequestPlan, source: &str) -> Result<String, ProtocolError> {
    let mut converter = StreamConverter::new(plan);
    let mut output = converter.process_chunk(Bytes::from(source.to_string()))?;
    output.extend(converter.finish()?);
    Ok(String::from_utf8(output.concat()).unwrap())
}

#[test]
fn same_protocol_messages_binds_signature_once_per_block() {
    let mut request = plan(ApiFormat::Messages, ApiFormat::Messages);
    let domain = replay_domain();
    request.replay_domain = Some(domain);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"hello\",\"signature\":\"sig-1\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-2\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque-data\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"text\",\"text\":\"plain\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":4,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let output = finish_stream(&request, source).unwrap();
    assert!(output.contains(&format!("{prefix}sig-1")), "{output}");
    assert!(output.contains("sig-2"), "{output}");
    assert!(
        !output.contains(&format!("{prefix}sig-2")),
        "later signature chunks stay raw: {output}"
    );
    assert!(output.contains(&format!("{prefix}opaque-data")), "{output}");
    assert_eq!(output.matches(prefix.as_str()).count(), 2, "{output}");
    assert!(output.contains("\"text\":\"plain\""), "{output}");
    assert!(output.contains("\"id\":\"toolu_1\""), "{output}");
    assert!(output.contains("\"signature\":\"\""), "{output}");
}

#[test]
fn split_signature_frame_still_binds_once() {
    let mut request = plan(ApiFormat::Messages, ApiFormat::Messages);
    let domain = replay_domain();
    request.replay_domain = Some(domain);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    let source = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"split-sig\"}}\n\n";
    let mut converter = StreamConverter::new(&request);
    let split = source.len() / 2;
    let mut output = converter
        .process_chunk(Bytes::from(source[..split].to_string()))
        .unwrap();
    output.extend(
        converter
            .process_chunk(Bytes::from(source[split..].to_string()))
            .unwrap(),
    );
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(output.contains(&format!("{prefix}split-sig")), "{output}");
    assert_eq!(output.matches(prefix.as_str()).count(), 1, "{output}");
}

#[test]
fn repeated_responses_encrypted_content_is_bound_once_per_value() {
    let mut request = plan(ApiFormat::Responses, ApiFormat::Responses);
    let domain = replay_domain();
    request.replay_domain = Some(domain);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    let item = json!({"type":"reasoning","id":"rs_1","encrypted_content":"cipher-1","summary":[]});
    let source = format!(
        "event: response.output_item.added\ndata: {}\n\n\
         event: response.output_item.done\ndata: {}\n\n\
         event: response.completed\ndata: {}\n\n",
        json!({"type":"response.output_item.added","output_index":0,"item":item}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[item]}}),
    );
    let output = finish_stream(&request, &source).unwrap();
    let bound = format!("{prefix}cipher-1");
    assert_eq!(output.matches(&bound).count(), 3, "{output}");
    assert_eq!(output.matches(prefix.as_str()).count(), 3, "{output}");
}

#[test]
fn messages_to_responses_wraps_an_already_bound_signature_once() {
    let mut request = plan(ApiFormat::Responses, ApiFormat::Messages);
    let domain = replay_domain();
    request.replay_domain = Some(domain);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"test-model\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"hello\",\"signature\":\"sig-1\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let output = finish_stream(&request, source).unwrap();
    let encrypted = output
        .split("encrypted_content\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("");
    assert!(
        encrypted.starts_with("ocg-anthropic-thinking-v1:"),
        "{output}"
    );
    assert!(!encrypted.starts_with("ocg-replay-v1:"), "{encrypted}");
    let block = super::super::protocol::decode_anthropic_thinking_block(encrypted)
        .expect("wrapper decodes");
    assert_eq!(block["signature"], format!("{prefix}sig-1"));
    assert_eq!(
        block["signature"]
            .as_str()
            .unwrap()
            .matches(prefix.as_str())
            .count(),
        1
    );
}

#[test]
fn responses_encrypted_history_is_not_dropped_on_messages_client() {
    let mut request = plan(ApiFormat::Messages, ApiFormat::Responses);
    request.replay_domain = Some(replay_domain());
    let source = "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"encrypted_content\":\"cipher-1\"}}\n\n";
    let error = finish_stream(&request, source).unwrap_err();
    assert!(
        error.message.contains("cannot be preserved"),
        "{}",
        error.message
    );
}

fn responses_created_and_completed(response: Value) -> String {
    format!(
        "event: response.created\ndata: {}\n\n\
         event: response.completed\ndata: {}\n\n",
        json!({"type":"response.created","response":{"id":"resp_1","model":"m","status":"in_progress"}}),
        json!({"type":"response.completed","response":response}),
    )
}

fn ordinary_reasoning_lookalike() -> Value {
    json!({"type":"reasoning","encrypted_content":"ticket-123"})
}

fn assistant_message_item(text: &str) -> Value {
    json!({
        "type":"message",
        "id":"msg_1",
        "role":"assistant",
        "status":"completed",
        "content":[{"type":"output_text","text":text}]
    })
}

/// Same-protocol Responses: replay markers bind native encrypted reasoning,
/// not ordinary `metadata` / tool payload trees that only look like it.
#[test]
fn responses_ordinary_data_is_not_bound_as_native_opaque() {
    let mut request = plan(ApiFormat::Responses, ApiFormat::Responses);
    let domain = replay_domain();
    request.replay_domain = Some(domain);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    let source = responses_created_and_completed(json!({
        "id":"resp_1",
        "status":"completed",
        "output":[
            {
                "type":"reasoning",
                "id":"rs_1",
                "summary":[],
                "encrypted_content":"cipher-1"
            },
            assistant_message_item("ok"),
            {
                "type":"function_call",
                "id":"call_1",
                "name":"lookup",
                "arguments": ordinary_reasoning_lookalike(),
                "args": ordinary_reasoning_lookalike(),
                "parameters": ordinary_reasoning_lookalike(),
                "input":{"nested": ordinary_reasoning_lookalike()}
            }
        ],
        "metadata": ordinary_reasoning_lookalike()
    }));
    let output = finish_stream(&request, &source).unwrap();
    assert!(
        output.contains(&format!("{prefix}cipher-1")),
        "native encrypted reasoning must still bind: {output}"
    );
    assert!(
        output.contains("\"encrypted_content\":\"ticket-123\""),
        "ordinary metadata/payload lookalikes must stay raw: {output}"
    );
    assert!(
        !output.contains(&format!("{prefix}ticket-123")),
        "ordinary data must not receive a replay marker: {output}"
    );
    assert_eq!(
        output.matches(prefix.as_str()).count(),
        1,
        "only the native cipher is marked: {output}"
    );
}

/// Converting Responses must keep a visible answer when the only
/// `type: reasoning` object lives under ordinary data keys. A real native
/// encrypted reasoning item on the same stream still fails closed.
#[test]
fn responses_ordinary_data_does_not_block_protocol_conversion() {
    let lookalike = responses_created_and_completed(json!({
        "id":"resp_1",
        "status":"completed",
        "output":[assistant_message_item("ok")],
        "metadata": ordinary_reasoning_lookalike()
    }));
    for client in [ApiFormat::ChatCompletions, ApiFormat::Messages] {
        let mut request = plan(client, ApiFormat::Responses);
        request.replay_domain = Some(replay_domain());
        let output = finish_stream(&request, &lookalike).unwrap();
        match client {
            ApiFormat::ChatCompletions => {
                assert!(
                    output.contains("\"content\":\"ok\""),
                    "Chat must keep the answer: {output}"
                );
                assert!(output.contains("data: [DONE]"), "{output}");
            }
            ApiFormat::Messages => {
                assert_eq!(messages_text(&output), "ok", "{output}");
                assert!(output.contains("message_stop"), "{output}");
            }
            _ => unreachable!(),
        }
        assert!(
            !output.contains("ocg-replay-v1:"),
            "ordinary metadata must not be marked during conversion: {output}"
        );
    }

    let native = responses_created_and_completed(json!({
        "id":"resp_1",
        "status":"completed",
        "output":[{
            "type":"reasoning",
            "id":"rs_1",
            "summary":[],
            "encrypted_content":"cipher-1"
        }],
        "metadata": ordinary_reasoning_lookalike()
    }));
    for client in [ApiFormat::ChatCompletions, ApiFormat::Messages] {
        let mut request = plan(client, ApiFormat::Responses);
        request.replay_domain = Some(replay_domain());
        let error = finish_stream(&request, &native).unwrap_err();
        assert!(
            error.message.contains("cannot be preserved"),
            "{client:?} {}",
            error.message
        );
        assert!(!error.message.contains("ticket-123"), "{}", error.message);
        assert!(!error.message.contains("cipher-1"), "{}", error.message);
    }
}

#[test]
fn signature_redaction_rejects_a_bound_route_without_leaking_the_secret() {
    let mut request = plan(ApiFormat::Messages, ApiFormat::Messages);
    request.replay_domain = Some(replay_domain());
    let source = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"hello\",\"signature\":\"pre-SECRET-post\"}}\n\n";
    let mut converter = StreamConverter::new_with_known_secret(&request, Some("SECRET"));
    let error = converter
        .process_chunk(Bytes::from(source.to_string()))
        .unwrap_err();
    assert!(
        error
            .message
            .contains("signed native history cannot be preserved by secret redaction"),
        "{}",
        error.message
    );
    assert!(!error.message.contains("SECRET"), "{}", error.message);
}

#[test]
fn chat_finish_uses_last_anthropic_message_delta_usage() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":2}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = convert(ApiFormat::ChatCompletions, ApiFormat::Messages, source);
    assert!(output.contains("\"completion_tokens\":7"));
    assert_eq!(output.matches("\"finish_reason\":\"stop\"").count(), 1);
    assert!(output.ends_with("data: [DONE]\n\n"));
}

#[test]
fn responses_errors_and_incomplete_stops_use_codex_events() {
    let converter = StreamConverter::new(&plan(ApiFormat::Responses, ApiFormat::ChatCompletions));
    let error = String::from_utf8(converter.error_event("boom").concat()).unwrap();
    assert!(error.contains("event: response.failed"));
    assert!(error.contains("\"status\":\"failed\""));
    assert!(error.contains("\"message\":\"boom\""));

    let refusal = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = convert(ApiFormat::Responses, ApiFormat::Messages, refusal);
    assert!(output.contains("event: response.incomplete"));
    assert!(output.contains("\"reason\":\"content_filter\""));
}

#[test]
fn messages_signed_thinking_is_preserved_for_responses_replay() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"check\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_123\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = convert(ApiFormat::Responses, ApiFormat::Messages, source);
    let frame = output
        .split("\n\n")
        .find(|frame| {
            frame.contains("response.output_item.done") && frame.contains("\"type\":\"reasoning\"")
        })
        .expect("reasoning output item");
    let payload = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("event data");
    let value: Value = serde_json::from_str(payload).expect("valid event JSON");
    let restored = super::super::protocol::decode_anthropic_thinking_block(
        value["item"]["encrypted_content"].as_str().unwrap(),
    )
    .expect("signed block decodes");
    assert_eq!(restored["thinking"], "check");
    assert_eq!(restored["signature"], "sig_123");
}

#[test]
fn safe_reasoning_prefix_does_not_invalidate_signed_replay() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"panda\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_123\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Messages),
        Some("data"),
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    let frame = output
        .split("\n\n")
        .find(|frame| {
            frame.contains("response.output_item.done") && frame.contains("\"type\":\"reasoning\"")
        })
        .expect("reasoning output item");
    let payload = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let value: Value = serde_json::from_str(payload).unwrap();
    let restored = super::super::protocol::decode_anthropic_thinking_block(
        value["item"]["encrypted_content"].as_str().unwrap(),
    )
    .expect("signed block decodes");
    assert_eq!(restored["thinking"], "panda");
    assert_eq!(restored["signature"], "sig_123");
}

#[test]
fn unsafe_initial_reasoning_is_never_preserved_in_opaque_replay() {
    for content_block in [
        json!({"type":"thinking","thinking":"opaque/account+key=42","signature":"sig_123"}),
        json!({"type":"redacted_thinking","data":"opaque/account+key=42"}),
    ] {
        let source = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"model\":\"m\"}}}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{content_block}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":1}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
        );
        let mut converter = StreamConverter::new_with_known_secret(
            &plan(ApiFormat::Responses, ApiFormat::Messages),
            Some("opaque/account+key=42"),
        );
        let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
        output.extend(converter.finish().unwrap());
        let output = String::from_utf8(output.concat()).unwrap();
        assert!(!output.contains("opaque/account+key=42"), "{output}");
        assert!(!output.contains("encrypted_content"), "{output}");
    }
}

#[test]
fn same_protocol_removes_known_opaque_replays_that_decode_to_the_secret() {
    let secret = "opaque/account+key=42";
    let wrappers = [
        super::super::protocol::encode_anthropic_thinking_block(&json!({
            "type":"thinking",
            "thinking":format!("before {secret} after"),
            "signature":"sig_123"
        }))
        .unwrap(),
        super::super::protocol::encode_chat_reasoning(&format!("before {secret} after")).unwrap(),
    ];
    for encrypted_content in wrappers {
        let mut converter = StreamConverter::new_with_known_secret(
            &plan(ApiFormat::Responses, ApiFormat::Responses),
            Some(secret),
        );
        let source = format!(
            "event: response.created\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"r","model":"m","status":"in_progress"}}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","summary":[],"encrypted_content":encrypted_content}}),
            json!({"type":"response.completed","response":{"id":"r","model":"m","status":"completed"}})
        );
        let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
        output.extend(converter.finish().unwrap());
        let output = String::from_utf8(output.concat()).unwrap();
        let item = output
            .split("\n\n")
            .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
            .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
            .find(|value| {
                value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
            })
            .expect("reasoning done event");
        assert_eq!(item["item"]["encrypted_content"], "", "{output}");
    }
}

#[test]
fn reasoning_split_between_block_start_and_delta_cannot_enter_opaque_replay() {
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"opaque/account+\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"key=42\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_123\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &plan(ApiFormat::Responses, ApiFormat::Messages),
        Some("opaque/account+key=42"),
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains("opaque/account+key=42"), "{output}");
    assert!(!output.contains("encrypted_content"), "{output}");
}

#[test]
fn chat_reasoning_is_preserved_for_responses_replay() {
    let source = concat!(
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"reasoning_content\":\"reason\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let output = convert(ApiFormat::Responses, ApiFormat::ChatCompletions, source);
    let frame = output
        .split("\n\n")
        .find(|frame| {
            frame.contains("response.output_item.done") && frame.contains("\"type\":\"reasoning\"")
        })
        .expect("reasoning output item");
    let payload = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("event data");
    let value: Value = serde_json::from_str(payload).expect("valid event JSON");
    let restored = super::super::protocol::decode_chat_reasoning(
        value["item"]["encrypted_content"].as_str().unwrap(),
    )
    .expect("chat reasoning decodes");
    assert_eq!(restored, "reason");
}

#[test]
fn truncated_stream_is_not_synthesized_as_success() {
    let mut converter =
        StreamConverter::new(&plan(ApiFormat::Responses, ApiFormat::ChatCompletions));
    converter
            .process_chunk(Bytes::from_static(
                b"data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            ))
            .expect("partial event converts");
    let error = converter.finish().expect_err("truncated stream must fail");
    assert!(error.message.contains("terminal event"));
}

fn domain_plan(client: ApiFormat, upstream: ApiFormat) -> RequestPlan {
    let mut request = plan(client, upstream);
    request.replay_domain = Some(replay_domain());
    request
}

fn assert_frame_error(client: ApiFormat, upstream: ApiFormat, source: &str, phrase: &str) {
    let mut converter = StreamConverter::new(&domain_plan(client, upstream));
    let error = converter
        .process_chunk(Bytes::from(source.to_string()))
        .unwrap_err();
    assert!(error.message.contains(phrase), "{}", error.message);
}

fn reasoning_final_item() -> Value {
    json!({
        "type": "reasoning",
        "id": "rs1",
        "summary": [],
        "content": [{"type": "reasoning_text", "text": "R"}],
        "vendor_note": "keep"
    })
}

fn responses_done_and_completed(item: &Value) -> String {
    format!(
        "event: response.output_item.done\ndata: {}\n\n\
         event: response.completed\ndata: {}\n\n",
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({
            "type":"response.completed",
            "response":{"id":"resp_1","status":"completed","output":[item]}
        }),
    )
}

#[test]
fn bound_sse_rejects_non_string_native_fields_on_both_paths() {
    let signature = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"hello\",\"signature\":123}}\n\n";
    let redacted = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":{}}}\n\n";
    let empty_redacted = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"\"}}\n\n";
    let encrypted = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs1\",\"summary\":[],\"encrypted_content\":[]}}\n\n";
    let malformed = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs1\",\"summary\":[],\"encrypted_content\":\"ocg-anthropic-thinking-v1:\"}}\n\n";

    for (client, upstream, source, phrase) in [
        (
            ApiFormat::Messages,
            ApiFormat::Messages,
            signature,
            "Messages thinking signature must be a string",
        ),
        (
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            signature,
            "Messages thinking signature must be a string",
        ),
        (
            ApiFormat::Messages,
            ApiFormat::Messages,
            redacted,
            "Messages redacted thinking data must be a nonempty string",
        ),
        (
            ApiFormat::ChatCompletions,
            ApiFormat::Messages,
            redacted,
            "Messages redacted thinking data must be a nonempty string",
        ),
        (
            ApiFormat::Messages,
            ApiFormat::Messages,
            empty_redacted,
            "Messages redacted thinking data must be a nonempty string",
        ),
        (
            ApiFormat::Responses,
            ApiFormat::Responses,
            encrypted,
            "Responses encrypted_content must be a string or null",
        ),
        (
            ApiFormat::Messages,
            ApiFormat::Responses,
            encrypted,
            "Responses encrypted_content must be a string or null",
        ),
        (
            ApiFormat::Responses,
            ApiFormat::Responses,
            malformed,
            "opaque replay carrier is malformed",
        ),
        (
            ApiFormat::ChatCompletions,
            ApiFormat::Responses,
            malformed,
            "opaque replay carrier is malformed",
        ),
    ] {
        assert_frame_error(client, upstream, source, phrase);
    }
}

#[test]
fn bound_sse_keeps_null_signature_and_empty_encrypted_content() {
    let signature = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"hello\",\"signature\":null}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let output = finish_stream(
        &domain_plan(ApiFormat::Messages, ApiFormat::Messages),
        signature,
    )
    .unwrap();
    assert!(output.contains("\"signature\":null"), "{output}");
    assert!(!output.contains("ocg-replay-"), "{output}");

    let item = json!({
        "type": "reasoning",
        "id": "rs1",
        "summary": [],
        "encrypted_content": ""
    });
    let output = finish_stream(
        &domain_plan(ApiFormat::Responses, ApiFormat::Responses),
        &responses_done_and_completed(&item),
    )
    .unwrap();
    assert!(output.contains("\"encrypted_content\":\"\""), "{output}");
    assert!(!output.contains("ocg-replay-"), "{output}");
}

#[test]
fn bound_sse_still_redacts_unsigned_text() {
    let source = concat!(
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"hello SECRET\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &domain_plan(ApiFormat::Messages, ApiFormat::Messages),
        Some("SECRET"),
    );
    let mut output = converter.process_chunk(Bytes::from(source)).unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains("SECRET"), "{output}");
    assert!(output.contains("hello"), "{output}");
}

#[test]
fn bound_sse_rejects_secret_inside_signed_thinking_text() {
    let source = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"see SECRET\",\"signature\":\"sig-1\"}}\n\n";
    for (client, upstream) in [
        (ApiFormat::Messages, ApiFormat::Messages),
        (ApiFormat::ChatCompletions, ApiFormat::Messages),
    ] {
        let mut converter =
            StreamConverter::new_with_known_secret(&domain_plan(client, upstream), Some("SECRET"));
        let error = converter
            .process_chunk(Bytes::from(source.to_string()))
            .unwrap_err();
        assert!(
            error
                .message
                .contains("signed native history cannot be preserved by secret redaction"),
            "{client:?}->{upstream:?} {}",
            error.message
        );
        assert!(!error.message.contains("SECRET"), "{}", error.message);
    }
}

#[test]
fn same_protocol_signature_tail_precedes_content_block_stop() {
    let request = domain_plan(ApiFormat::Messages, ApiFormat::Messages);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(request.replay_domain.unwrap());
    let source = concat!(
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"oc\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"
    );
    let mut converter = StreamConverter::new(&request);
    let output = converter
        .process_chunk(Bytes::from(source.to_string()))
        .unwrap();
    let output = String::from_utf8(output.concat()).unwrap();
    let frames: Vec<&str> = output
        .split("\n\n")
        .filter(|frame| !frame.is_empty())
        .collect();
    let bound = format!("{prefix}oc");
    let signature_at = frames
        .iter()
        .position(|frame| frame.contains(&bound))
        .expect("bound signature frame");
    let stop_at = frames
        .iter()
        .position(|frame| frame.contains("content_block_stop"))
        .expect("stop frame");
    assert!(signature_at < stop_at, "{output}");
    let signatures = frames
        .iter()
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .filter_map(|value| {
            value
                .pointer("/delta/signature")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|signature| !signature.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(signatures, vec![bound], "{output}");
}

#[test]
fn cross_protocol_held_signature_is_reconstructed_once() {
    let request = domain_plan(ApiFormat::Responses, ApiFormat::Messages);
    let prefix = ocg_gateway::protocol::replay_marker_prefix(request.replay_domain.unwrap());
    let source = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"test-model\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"oc\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let output = finish_stream(&request, source).unwrap();
    let encrypted = output
        .split("encrypted_content\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("encrypted wrapper");
    let block = super::super::protocol::decode_anthropic_thinking_block(encrypted)
        .expect("wrapper decodes");
    assert_eq!(block["signature"], format!("{prefix}oc"));
    assert_eq!(
        block["signature"]
            .as_str()
            .unwrap()
            .matches(prefix.as_str())
            .count(),
        1
    );
}

#[test]
fn responses_reasoning_final_is_preserved_without_duplicate_text() {
    let item = reasoning_final_item();
    let same = finish_stream(
        &domain_plan(ApiFormat::Responses, ApiFormat::Responses),
        &responses_done_and_completed(&item),
    )
    .unwrap();
    let done = same
        .split("\n\n")
        .find(|frame| frame.contains("response.output_item.done"))
        .expect("done frame");
    assert!(done.contains("\"vendor_note\":\"keep\""), "{same}");
    assert!(done.contains("\"text\":\"R\""), "{same}");
    assert!(done.contains("\"summary\":[]"), "{same}");

    let cross = finish_stream(
        &domain_plan(ApiFormat::ChatCompletions, ApiFormat::Responses),
        &responses_done_and_completed(&item),
    )
    .unwrap();
    assert_eq!(
        cross.matches("\"reasoning_content\":\"R\"").count(),
        1,
        "{cross}"
    );

    let completed_only = format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type":"response.completed",
            "response":{"id":"resp_1","status":"completed","output":[item.clone()]}
        })
    );
    let completed = finish_stream(
        &domain_plan(ApiFormat::ChatCompletions, ApiFormat::Responses),
        &completed_only,
    )
    .unwrap();
    assert_eq!(
        completed.matches("\"reasoning_content\":\"R\"").count(),
        1,
        "{completed}"
    );

    let streamed = format!(
        "event: response.reasoning_text.delta\ndata: {}\n\n{}",
        json!({
            "type":"response.reasoning_text.delta",
            "output_index":0,
            "content_index":0,
            "delta":"R"
        }),
        responses_done_and_completed(&item)
    );
    let streamed = finish_stream(
        &domain_plan(ApiFormat::ChatCompletions, ApiFormat::Responses),
        &streamed,
    )
    .unwrap();
    assert_eq!(
        streamed.matches("\"reasoning_content\":\"R\"").count(),
        1,
        "{streamed}"
    );

    let done_only = format!(
        "event: response.output_item.done\ndata: {}\n\n",
        json!({"type":"response.output_item.done","output_index":0,"item":item})
    );
    let mut converter = StreamConverter::new(&domain_plan(
        ApiFormat::ChatCompletions,
        ApiFormat::Responses,
    ));
    let done_only = converter.process_chunk(Bytes::from(done_only)).unwrap();
    let done_only = String::from_utf8(done_only.concat()).unwrap();
    assert_eq!(
        done_only.matches("\"reasoning_content\":\"R\"").count(),
        1,
        "{done_only}"
    );
}

struct DecodedMessages {
    thinking: Vec<String>,
    text: Vec<String>,
    stop_reason: Option<String>,
    message_stops: usize,
    delta_after_stop: bool,
    duplicate_stop: bool,
    unstopped_delta: bool,
}

fn responses_sse(event: &str, body: Value) -> String {
    format!("event: {event}\ndata: {body}\n\n")
}

fn summary_delta(text: &str, summary_index: u64) -> String {
    responses_sse(
        "response.reasoning_summary_text.delta",
        json!({
            "type": "response.reasoning_summary_text.delta",
            "output_index": 0,
            "summary_index": summary_index,
            "delta": text
        }),
    )
}

fn content_delta(text: &str, content_index: u64) -> String {
    responses_sse(
        "response.reasoning_text.delta",
        json!({
            "type": "response.reasoning_text.delta",
            "output_index": 0,
            "content_index": content_index,
            "delta": text
        }),
    )
}

fn visible_text_delta(text: &str) -> String {
    responses_sse(
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": text
        }),
    )
}

fn summary_part_added(text: &str) -> String {
    responses_sse(
        "response.content_part.added",
        json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "summary_text", "text": text}
        }),
    )
}

fn reasoning_parts(part_type: &str, texts: &[&str]) -> Vec<Value> {
    texts
        .iter()
        .map(|text| json!({"type": part_type, "text": text}))
        .collect()
}

fn dual_reasoning(summary: &[&str], content: &[&str]) -> Value {
    json!({
        "type": "reasoning",
        "id": "rs1",
        "summary": reasoning_parts("summary_text", summary),
        "content": reasoning_parts("reasoning_text", content)
    })
}

fn output_done(item: &Value) -> String {
    responses_sse(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": item.clone()
        }),
    )
}

fn output_completed(item: &Value) -> String {
    responses_sse(
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {"id":"resp_1","status":"completed","output":[item.clone()]}
        }),
    )
}

fn output_incomplete(item: &Value, reason: Option<&str>) -> String {
    let mut response = json!({"id":"resp_1","status":"incomplete","output":[item.clone()]});
    if let Some(reason) = reason {
        response["incomplete_details"] = json!({"reason": reason});
    }
    responses_sse(
        "response.incomplete",
        json!({"type":"response.incomplete","response":response}),
    )
}

fn messages_output(source: &str, finish: bool) -> String {
    let mut converter = StreamConverter::new(&plan(ApiFormat::Messages, ApiFormat::Responses));
    let mut output = converter
        .process_chunk(Bytes::from(source.to_string()))
        .expect("responses stream converts");
    if finish {
        output.extend(
            converter
                .finish()
                .expect("terminal responses stream finishes"),
        );
    }
    String::from_utf8(output.concat()).expect("messages output is utf-8")
}

fn decode_messages(output: &str) -> DecodedMessages {
    let mut thinking = Vec::new();
    let mut text = Vec::new();
    let mut stopped = Vec::new();
    let mut delta_indexes = Vec::new();
    let mut delta_after_stop = false;
    let mut duplicate_stop = false;
    let mut stop_reason = None;
    let mut message_stops = 0;
    for frame in output.split("\n\n") {
        let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("content_block_delta") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if stopped.contains(&index) {
                    delta_after_stop = true;
                }
                delta_indexes.push(index);
                match value.pointer("/delta/type").and_then(Value::as_str) {
                    Some("thinking_delta") => thinking.push(
                        value
                            .pointer("/delta/thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    Some("text_delta") => text.push(
                        value
                            .pointer("/delta/text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if stopped.contains(&index) {
                    duplicate_stop = true;
                } else {
                    stopped.push(index);
                }
            }
            Some("message_delta") => {
                stop_reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("message_stop") => message_stops += 1,
            _ => {}
        }
    }
    let unstopped_delta = delta_indexes.iter().any(|index| !stopped.contains(index));
    DecodedMessages {
        thinking,
        text,
        stop_reason,
        message_stops,
        delta_after_stop,
        duplicate_stop,
        unstopped_delta,
    }
}

fn assert_messages_reasoning(
    label: &str,
    source: &str,
    finish: bool,
    thinking: &[&str],
    text: &[&str],
    terminal: (Option<&str>, usize),
) {
    let (stop_reason, message_stops) = terminal;
    let output = messages_output(source, finish);
    let facts = decode_messages(&output);
    assert_eq!(
        facts
            .thinking
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        thinking,
        "{label}\n{output}"
    );
    assert_eq!(
        facts.text.iter().map(String::as_str).collect::<Vec<_>>(),
        text,
        "{label}\n{output}"
    );
    assert_eq!(
        facts.stop_reason.as_deref(),
        stop_reason,
        "{label}\n{output}"
    );
    assert_eq!(facts.message_stops, message_stops, "{label}\n{output}");
    assert!(
        !facts.delta_after_stop,
        "{label}: delta followed a block stop\n{output}"
    );
    assert!(
        !facts.duplicate_stop,
        "{label}: block stopped twice\n{output}"
    );
    assert!(
        !facts.unstopped_delta,
        "{label}: delta block was left open\n{output}"
    );
}

#[test]
fn summary_delta_keeps_unseen_reasoning_content_at_the_same_index() {
    let item = dual_reasoning(&["S"], &["R"]);
    let done = format!("{}{}", summary_delta("S", 0), output_done(&item));
    assert_messages_reasoning("done", &done, false, &["S", "R"], &[], (None, 0));
    assert_messages_reasoning(
        "done and completed",
        &format!("{done}{}", output_completed(&item)),
        true,
        &["S", "R"],
        &[],
        (Some("end_turn"), 1),
    );
    assert_messages_reasoning(
        "delta after the summary block stopped",
        &format!(
            "{}{}{}{}",
            summary_delta("S", 0),
            output_done(&item),
            summary_delta("HIDDEN", 0),
            output_completed(&item)
        ),
        true,
        &["S", "R"],
        &[],
        (Some("end_turn"), 1),
    );
}

#[test]
fn content_delta_keeps_unseen_reasoning_summary_at_the_same_index() {
    let item = dual_reasoning(&["S"], &["R"]);
    assert_messages_reasoning(
        "content delta then both finals",
        &format!(
            "{}{}{}{}",
            content_delta("R", 0),
            output_done(&item),
            content_delta("HIDDEN", 0),
            output_completed(&item)
        ),
        true,
        &["R", "S"],
        &[],
        (Some("end_turn"), 1),
    );
}

#[test]
fn streamed_summary_and_content_are_not_repeated_by_finals() {
    let item = dual_reasoning(&["S"], &["R"]);
    assert_messages_reasoning(
        "both deltas then done and completed",
        &format!(
            "{}{}{}{}",
            summary_delta("S", 0),
            content_delta("R", 0),
            output_done(&item),
            output_completed(&item)
        ),
        true,
        &["S", "R"],
        &[],
        (Some("end_turn"), 1),
    );
    assert_messages_reasoning(
        "content then summary deltas",
        &format!(
            "{}{}{}{}",
            content_delta("R", 0),
            summary_delta("S", 0),
            output_done(&item),
            output_completed(&item)
        ),
        true,
        &["R", "S"],
        &[],
        (Some("end_turn"), 1),
    );
}

#[test]
fn completed_and_incomplete_fallbacks_emit_both_reasoning_lanes_once() {
    let item = dual_reasoning(&["S"], &["R"]);
    assert_messages_reasoning(
        "completed only",
        &output_completed(&item),
        true,
        &["S", "R"],
        &[],
        (Some("end_turn"), 1),
    );
    assert_messages_reasoning(
        "incomplete only",
        &output_incomplete(&item, None),
        true,
        &["S", "R"],
        &[],
        (Some("max_tokens"), 1),
    );
    assert_messages_reasoning(
        "incomplete content filter",
        &output_incomplete(&item, Some("content_filter")),
        true,
        &["S", "R"],
        &[],
        (Some("refusal"), 1),
    );
}

#[test]
fn multiple_reasoning_parts_keep_summary_and_content_order() {
    let item = dual_reasoning(&["S0", "S1"], &["R0", "R1"]);
    assert_messages_reasoning(
        "finals only",
        &format!("{}{}", output_done(&item), output_completed(&item)),
        true,
        &["S0", "S1", "R0", "R1"],
        &[],
        (Some("end_turn"), 1),
    );
    assert_messages_reasoning(
        "first summary part already streamed",
        &format!(
            "{}{}{}",
            summary_delta("S0", 0),
            output_done(&item),
            output_completed(&item)
        ),
        true,
        &["S0", "S1", "R0", "R1"],
        &[],
        (Some("end_turn"), 1),
    );
}

#[test]
fn visible_text_summary_and_content_at_the_same_index_stay_independent() {
    let item = dual_reasoning(&["S"], &["R"]);
    assert_messages_reasoning(
        "three lanes",
        &format!(
            "{}{}{}{}{}",
            visible_text_delta("V"),
            summary_delta("S", 0),
            content_delta("R", 0),
            output_done(&item),
            output_completed(&item)
        ),
        true,
        &["S", "R"],
        &["V"],
        (Some("end_turn"), 1),
    );
    assert_messages_reasoning(
        "summary part added then unseen content",
        &format!(
            "{}{}{}",
            summary_part_added("S"),
            output_done(&item),
            output_completed(&item)
        ),
        true,
        &["S", "R"],
        &[],
        (Some("end_turn"), 1),
    );
}

fn finish_secret(
    client: ApiFormat,
    upstream: ApiFormat,
    secret: &str,
    source: &str,
) -> Result<String, ProtocolError> {
    let mut converter =
        StreamConverter::new_with_known_secret(&domain_plan(client, upstream), Some(secret));
    let mut output = converter.process_chunk(Bytes::from(source.to_string()))?;
    output.extend(converter.finish()?);
    Ok(String::from_utf8(output.concat()).unwrap())
}

fn sse_values(output: &str) -> Vec<Value> {
    output
        .split("\n\n")
        .filter(|frame| !frame.is_empty())
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|payload| serde_json::from_str(payload).ok())
        })
        .collect()
}

fn assert_stream_opaque(value: &str, raw: &str) {
    let domain = replay_domain();
    let prefix = ocg_gateway::protocol::replay_marker_prefix(domain);
    assert_eq!(value, format!("{prefix}{raw}"), "{value}");
    match ocg_gateway::protocol::restore_replay_opaque(domain, value).unwrap() {
        ocg_gateway::protocol::RestoredReplay::Bound(restored) => assert_eq!(restored, raw),
        other => panic!("opaque did not restore to {raw}: {other:?}"),
    }
}

fn messages_frame(event: &str, body: Value) -> String {
    format!("event: {event}\ndata: {body}\n\n")
}

fn messages_block(index: usize, block: Value) -> String {
    format!(
        "{}{}",
        messages_frame(
            "content_block_start",
            json!({"type":"content_block_start","index":index,"content_block":block}),
        ),
        messages_frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        ),
    )
}

fn messages_terminal(stop_reason: &str) -> String {
    format!(
        "{}{}",
        messages_frame(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":stop_reason},"usage":{"output_tokens":1}}),
        ),
        messages_frame("message_stop", json!({"type":"message_stop"})),
    )
}

fn messages_native_source(secret: &str, text_first: bool) -> String {
    let text = messages_block(0, json!({"type":"text","text":format!("see {secret}")}));
    let thinking = messages_block(
        1,
        json!({"type":"thinking","thinking":"why","signature":"sig-1"}),
    );
    let redacted = messages_block(2, json!({"type":"redacted_thinking","data":"opaque-data"}));
    let tool = messages_block(
        3,
        json!({"type":"tool_use","id":"toolu_1","name":"search","input":{}}),
    );
    let terminal = messages_terminal("tool_use");
    if text_first {
        format!("{text}{thinking}{redacted}{tool}{terminal}")
    } else {
        format!("{thinking}{redacted}{text}{tool}{terminal}")
    }
}

fn assert_messages_native_roundtrip(secret: &str, text_first: bool, output: &str) {
    let values = sse_values(output);
    let starts: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            (value.get("type").and_then(Value::as_str) == Some("content_block_start"))
                .then(|| value.pointer("/content_block/type")?.as_str())
                .flatten()
        })
        .collect();
    let expected = if text_first {
        ["text", "thinking", "redacted_thinking", "tool_use"]
    } else {
        ["thinking", "redacted_thinking", "text", "tool_use"]
    };
    assert_eq!(starts, expected, "{secret}\n{output}");
    let signatures: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/delta/signature")
                .or_else(|| value.pointer("/content_block/signature"))
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
        })
        .collect();
    assert_eq!(signatures.len(), 1, "{secret} {signatures:?}\n{output}");
    assert_stream_opaque(signatures[0], "sig-1");
    let data: Vec<&str> = values
        .iter()
        .filter_map(|value| value.pointer("/content_block/data").and_then(Value::as_str))
        .collect();
    assert_eq!(data.len(), 1, "{secret} {data:?}\n{output}");
    assert_stream_opaque(data[0], "opaque-data");
    let texts: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            if value.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") {
                value.pointer("/delta/text").and_then(Value::as_str)
            } else if value.pointer("/content_block/type").and_then(Value::as_str) == Some("text") {
                value.pointer("/content_block/text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .filter(|text| !text.is_empty())
        .collect();
    assert_eq!(texts, vec!["see "], "{secret} {texts:?}\n{output}");
    let thinking: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/delta/thinking")
                .or_else(|| value.pointer("/content_block/thinking"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        })
        .collect();
    assert_eq!(thinking, vec!["why"], "{secret}\n{output}");
    let stops: Vec<&Value> = values
        .iter()
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("message_stop"))
        .collect();
    assert_eq!(stops.len(), 1, "{output}");
    assert_eq!(
        values
            .iter()
            .filter_map(|value| value.pointer("/delta/stop_reason").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        vec!["tool_use"]
    );
    let mut open = Vec::new();
    for value in &values {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        let index = value.get("index").and_then(Value::as_u64);
        match kind {
            "content_block_start" => open.push(index),
            "content_block_delta" => assert!(
                open.contains(&index),
                "{secret}: delta after its block stopped\n{output}"
            ),
            "content_block_stop" => assert!(
                open.contains(&index),
                "{secret}: stop without an open block\n{output}"
            ),
            _ => {}
        }
        if kind == "content_block_stop" {
            open.retain(|open_index| *open_index != index);
        }
    }
    assert!(open.is_empty(), "{secret}: unstopped blocks {open:?}");
}

#[test]
fn tainted_messages_keep_signed_and_redacted_fields_around_visible_text() {
    for secret in ["ocg", "replay", "efefefef"] {
        for text_first in [true, false] {
            let output = finish_secret(
                ApiFormat::Messages,
                ApiFormat::Messages,
                secret,
                &messages_native_source(secret, text_first),
            )
            .unwrap_or_else(|error| panic!("{secret} first={text_first}: {}", error.message));
            assert_messages_native_roundtrip(secret, text_first, &output);
        }
    }
}

fn responses_item_done(index: u64, item: &Value) -> String {
    responses_sse(
        "response.output_item.done",
        json!({"type":"response.output_item.done","output_index":index,"item":item}),
    )
}

fn responses_completed(items: &[Value]) -> String {
    responses_sse(
        "response.completed",
        json!({
            "type":"response.completed",
            "response":{"id":"resp_1","status":"completed","output":items}
        }),
    )
}

fn responses_text_item(text: &str) -> Value {
    json!({
        "type":"message",
        "id":"msg_echo",
        "role":"assistant",
        "status":"completed",
        "content":[{"type":"output_text","text":text,"annotations":[]}]
    })
}

fn responses_tool_item() -> Value {
    json!({
        "type":"function_call",
        "id":"fc_echo",
        "call_id":"call_search",
        "name":"search",
        "arguments":"{}",
        "status":"completed"
    })
}

fn responses_reasoning_item(summary: Value, content: Value) -> Value {
    json!({
        "type":"reasoning",
        "id":"rs_echo",
        "summary":summary,
        "content":content,
        "encrypted_content":"cipher-1"
    })
}

fn responses_native_source(
    secret: &str,
    summary: &Value,
    content: &Value,
    text_first: bool,
) -> String {
    let text = responses_item_done(0, &responses_text_item(&format!("see {secret}")));
    let reasoning = responses_item_done(
        1,
        &responses_reasoning_item(summary.clone(), content.clone()),
    );
    let tool = responses_item_done(2, &responses_tool_item());
    let completed = responses_completed(&[
        responses_text_item(&format!("see {secret}")),
        responses_reasoning_item(summary.clone(), content.clone()),
        responses_tool_item(),
    ]);
    if text_first {
        format!("{text}{reasoning}{tool}{completed}")
    } else {
        format!("{reasoning}{text}{tool}{completed}")
    }
}

fn assert_responses_native_roundtrip(
    secret: &str,
    summary: &Value,
    content: &Value,
    text_first: bool,
    output: &str,
) {
    let values = sse_values(output);
    let reasoning_done: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .collect();
    assert_eq!(reasoning_done.len(), 1, "{secret}\n{output}");
    let completed: Vec<&Value> = values
        .iter()
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .collect();
    assert_eq!(completed.len(), 1, "{output}");
    let completed_reasoning: Vec<&Value> = completed[0]
        .pointer("/response/output")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .collect();
    assert_eq!(completed_reasoning.len(), 1, "{secret}\n{output}");
    for item in [
        reasoning_done[0].get("item").unwrap(),
        completed_reasoning[0],
    ] {
        assert_eq!(&item["summary"], summary, "{secret}\n{output}");
        assert_eq!(&item["content"], content, "{secret}\n{output}");
        assert_stream_opaque(item["encrypted_content"].as_str().unwrap(), "cipher-1");
    }
    assert_eq!(
        reasoning_done[0]["item"]["summary"], completed_reasoning[0]["summary"],
        "{secret}: done and completed diverged\n{output}"
    );
    assert_eq!(
        reasoning_done[0]["item"]["content"],
        completed_reasoning[0]["content"]
    );
    assert_eq!(
        reasoning_done[0]["item"]["encrypted_content"],
        completed_reasoning[0]["encrypted_content"]
    );
    let added = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .count();
    if text_first {
        assert_eq!(added, 1, "{secret}: duplicate reasoning item\n{output}");
        let types: Vec<&str> = completed[0]
            .pointer("/response/output")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|item| item.get("type").and_then(Value::as_str))
            .collect();
        assert_eq!(
            types,
            vec!["message", "reasoning", "function_call"],
            "{output}"
        );
    }
    let texts: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            if value.get("type").and_then(Value::as_str) == Some("response.output_text.delta") {
                return value.get("delta").and_then(Value::as_str);
            }
            if value.pointer("/item/type").and_then(Value::as_str) == Some("message") {
                return value
                    .pointer("/item/content/0/text")
                    .and_then(Value::as_str);
            }
            None
        })
        .filter(|text| !text.is_empty())
        .collect();
    assert!(
        texts.iter().all(|text| *text == "see "),
        "{secret} {texts:?}\n{output}"
    );
    assert!(texts.contains(&"see "), "{secret}\n{output}");
    let names: Vec<&str> = values
        .iter()
        .filter_map(|value| value.pointer("/item/name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"search"), "{secret} {names:?}\n{output}");
}

#[test]
fn tainted_responses_keep_content_summary_and_cipher_roles() {
    let content_only = json!([{"type":"reasoning_text","text":"R"}]);
    let both_summary = json!([{"type":"summary_text","text":"S"}]);
    let both_content = json!([{"type":"reasoning_text","text":"R"}]);
    for secret in ["ocg", "replay", "efefefef"] {
        for text_first in [true, false] {
            let output = finish_secret(
                ApiFormat::Responses,
                ApiFormat::Responses,
                secret,
                &responses_native_source(secret, &json!([]), &content_only, text_first),
            )
            .unwrap_or_else(|error| {
                panic!("content {secret} first={text_first}: {}", error.message)
            });
            assert_responses_native_roundtrip(
                secret,
                &json!([]),
                &content_only,
                text_first,
                &output,
            );
            assert!(
                !sse_values(&output).iter().any(|value| {
                    value.get("type").and_then(Value::as_str)
                        == Some("response.reasoning_summary_text.delta")
                }),
                "{secret}: content was copied into a summary delta\n{output}"
            );
            let both = finish_secret(
                ApiFormat::Responses,
                ApiFormat::Responses,
                secret,
                &responses_native_source(secret, &both_summary, &both_content, text_first),
            )
            .unwrap_or_else(|error| panic!("both {secret} first={text_first}: {}", error.message));
            assert_responses_native_roundtrip(
                secret,
                &both_summary,
                &both_content,
                text_first,
                &both,
            );
        }
    }
}

#[test]
fn tainted_responses_reject_a_later_encrypted_secret() {
    let text = responses_item_done(0, &responses_text_item("see replay"));
    let reasoning = responses_item_done(
        1,
        &json!({
            "type":"reasoning",
            "id":"rs_echo",
            "summary":[],
            "content":[{"type":"reasoning_text","text":"R"}],
            "encrypted_content":"cipher-replay-1"
        }),
    );
    let mut converter = StreamConverter::new_with_known_secret(
        &domain_plan(ApiFormat::Responses, ApiFormat::Responses),
        Some("replay"),
    );
    let error = converter
        .process_chunk(Bytes::from(format!("{text}{reasoning}")))
        .unwrap_err();
    assert!(
        error
            .message
            .contains("signed native history cannot be preserved by secret redaction"),
        "{}",
        error.message
    );
    assert!(!error.message.contains("replay"), "{}", error.message);
}

#[test]
fn messages_to_responses_stream_wraps_signed_history_once() {
    let source = format!(
        "{}{}{}",
        messages_block(0, json!({"type":"text","text":"see ocg"})),
        messages_block(
            1,
            json!({"type":"thinking","thinking":"why","signature":"sig-1"})
        ),
        messages_terminal("end_turn"),
    );
    let output = finish_secret(ApiFormat::Responses, ApiFormat::Messages, "ocg", &source).unwrap();
    let values = sse_values(&output);
    let reasoning: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .map(|value| &value["item"])
        .collect();
    assert_eq!(reasoning.len(), 1, "{output}");
    assert_eq!(reasoning[0]["summary"][0]["text"].as_str(), Some("why"));
    assert!(reasoning[0].get("content").is_none());
    let block = super::super::protocol::decode_anthropic_thinking_block(
        reasoning[0]["encrypted_content"].as_str().unwrap(),
    )
    .expect("codec");
    assert_eq!(block["thinking"].as_str(), Some("why"));
    assert_stream_opaque(block["signature"].as_str().unwrap(), "sig-1");
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let completed_reasoning = completed
        .pointer("/response/output")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .expect("completed reasoning");
    assert_eq!(
        completed_reasoning["summary"][0]["text"].as_str(),
        Some("why")
    );
    assert_eq!(
        completed_reasoning["encrypted_content"],
        reasoning[0]["encrypted_content"]
    );
    let texts: Vec<&str> = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/item/content/0/text")
                .and_then(Value::as_str)
        })
        .collect();
    assert!(texts.contains(&"see "), "{texts:?}\n{output}");
}

fn response_message_added(index: u64, id: &str, content: Value) -> String {
    responses_sse(
        "response.output_item.added",
        json!({
            "type":"response.output_item.added",
            "output_index": index,
            "item": {
                "type":"message",
                "id": id,
                "status":"in_progress",
                "role":"assistant",
                "content": content
            }
        }),
    )
}

fn response_text_delta(index: u64, content_index: u64, text: &str) -> String {
    responses_sse(
        "response.output_text.delta",
        json!({
            "type":"response.output_text.delta",
            "output_index": index,
            "content_index": content_index,
            "delta": text
        }),
    )
}

fn response_message_done(index: u64, id: &str, content: Value) -> String {
    responses_sse(
        "response.output_item.done",
        json!({
            "type":"response.output_item.done",
            "output_index": index,
            "item": {
                "type":"message",
                "id": id,
                "status":"completed",
                "role":"assistant",
                "content": content
            }
        }),
    )
}

fn response_terminal() -> String {
    responses_sse(
        "response.completed",
        json!({
            "type":"response.completed",
            "response":{"id":"resp_native","status":"completed"}
        }),
    )
}

fn message_item_events(values: &[Value]) -> (Vec<&Value>, Vec<&Value>) {
    let added = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("message")
        })
        .collect();
    let done = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("message")
        })
        .collect();
    (added, done)
}

#[test]
fn native_message_added_before_secret_delta_stays_one_item() {
    for output_index in [0_u64, 4] {
        let secret = "ocg";
        let id = format!("msg_native_{output_index}");
        let text = format!("see {secret}");
        let output = finish_secret(
            ApiFormat::Responses,
            ApiFormat::Responses,
            secret,
            &format!(
                "{}{}{}{}",
                response_message_added(output_index, &id, json!([])),
                response_text_delta(output_index, 0, &text),
                response_message_done(
                    output_index,
                    &id,
                    json!([{"type":"output_text","text":text}])
                ),
                response_terminal(),
            ),
        )
        .unwrap_or_else(|error| panic!("index {output_index}: {}", error.message));
        assert!(!output.contains(&format!("see {secret}")), "{output}");
        let values = sse_values(&output);
        let (added, done) = message_item_events(&values);
        assert_eq!(added.len(), 1, "duplicate message start\n{output}");
        assert_eq!(done.len(), 1, "duplicate message end\n{output}");
        assert_eq!(added[0]["output_index"], json!(output_index), "{output}");
        assert_eq!(added[0]["item"]["id"], json!(id), "{output}");
        assert_eq!(done[0]["output_index"], json!(output_index), "{output}");
        assert_eq!(done[0]["item"]["id"], json!(id), "{output}");
        assert_eq!(
            done[0]["item"]["content"][0]["text"],
            json!("see "),
            "{output}"
        );
        let deltas: Vec<&Value> = values
            .iter()
            .filter(|value| {
                value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
            })
            .collect();
        assert!(
            deltas.iter().all(|value| {
                value["output_index"] == json!(output_index)
                    && value["content_index"] == json!(0)
                    && value["item_id"] == json!(id)
            }),
            "{deltas:?}\n{output}"
        );
        let completed = values
            .iter()
            .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
            .expect("completed");
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            json!("see ")
        );
        assert_eq!(completed["response"]["output"][0]["id"], json!(id));
    }
}

#[test]
fn native_text_parts_share_one_item_when_the_later_part_is_secret() {
    let secret = "ocg";
    let index = 3_u64;
    let id = "msg_multi";
    let later = format!("see {secret}");
    let reasoning = json!({
        "type":"reasoning",
        "id":"rs_native",
        "summary":[{"type":"summary_text","text":"S"}],
        "content":[{"type":"reasoning_text","text":"R"}],
        "encrypted_content":"cipher-1"
    });
    let tool = json!({
        "type":"function_call",
        "id":"fc_native",
        "call_id":"call_search",
        "name":"search",
        "arguments":"{\"q\":\"leave-exact\"}",
        "status":"completed"
    });
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}{}{}",
            response_message_added(index, id, json!([])),
            response_text_delta(index, 0, "hello"),
            response_text_delta(index, 1, &later),
            response_message_done(
                index,
                id,
                json!([
                    {"type":"output_text","text":"hello"},
                    {"type":"output_text","text":later}
                ])
            ),
            responses_item_done(4, &reasoning),
            responses_item_done(5, &tool),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    // The safe cipher marker begins with the literal prefix `ocg-replay-v1`.
    // Visible text must lose the credential; the marker stays byte-exact.
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let values = sse_values(&output);
    let (added, done) = message_item_events(&values);
    assert_eq!(added.len(), 1, "duplicate message start\n{output}");
    assert_eq!(done.len(), 1, "duplicate message end\n{output}");
    assert_eq!(added[0]["output_index"], json!(index), "{output}");
    assert_eq!(added[0]["item"]["id"], json!(id), "{output}");
    assert_eq!(done[0]["output_index"], json!(index), "{output}");
    assert_eq!(done[0]["item"]["id"], json!(id), "{output}");
    assert_eq!(
        done[0]["item"]["content"][0]["text"],
        json!("hello"),
        "{output}"
    );
    assert_eq!(
        done[0]["item"]["content"][1]["text"],
        json!("see "),
        "{output}"
    );
    let part_indexes: Vec<u64> = values
        .iter()
        .filter(|value| {
            matches!(
                value.get("type").and_then(Value::as_str),
                Some(
                    "response.output_text.delta"
                        | "response.content_part.added"
                        | "response.content_part.done"
                        | "response.output_text.done"
                )
            )
        })
        .map(|value| value["content_index"].as_u64().unwrap())
        .collect();
    assert!(
        part_indexes.contains(&0) && part_indexes.contains(&1),
        "{part_indexes:?}\n{output}"
    );
    assert!(
        values
            .iter()
            .filter(|value| {
                value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
                    && value["content_index"] == json!(1)
            })
            .all(|value| value["output_index"] == json!(index) && value["item_id"] == json!(id)),
        "{output}"
    );
    let reasoning_done: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .collect();
    assert_eq!(reasoning_done.len(), 1, "{output}");
    assert_eq!(reasoning_done[0]["item"]["summary"][0]["text"], json!("S"));
    assert_eq!(reasoning_done[0]["item"]["content"][0]["text"], json!("R"));
    assert_stream_opaque(
        reasoning_done[0]["item"]["encrypted_content"]
            .as_str()
            .unwrap(),
        "cipher-1",
    );
    let calls: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("function_call")
        })
        .collect();
    assert_eq!(calls.len(), 1, "{output}");
    assert_eq!(
        calls[0]["item"]["arguments"],
        json!("{\"q\":\"leave-exact\"}")
    );
    assert_eq!(calls[0]["item"]["name"], json!("search"));
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let items = completed["response"]["output"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{output}");
    assert_eq!(items[0]["id"], json!(id));
    assert_eq!(items[0]["content"][0]["text"], json!("hello"));
    assert_eq!(items[0]["content"][1]["text"], json!("see "));
    assert_eq!(
        items[1]["encrypted_content"],
        reasoning_done[0]["item"]["encrypted_content"]
    );
    assert_eq!(items[2]["arguments"], json!("{\"q\":\"leave-exact\"}"));
    assert_eq!(
        output.matches("\"encrypted_content\"").count(),
        2,
        "cipher must appear once on item.done and once on completed\n{output}"
    );
}

#[test]
fn native_message_start_held_in_the_deferred_queue_is_published_once() {
    let secret = "ocg";
    let index = 6_u64;
    let id = "msg_deferred";
    let mut converter = StreamConverter::new_with_known_secret(
        &domain_plan(ApiFormat::Responses, ApiFormat::Responses),
        Some(secret),
    );
    let held = converter
        .process_chunk(Bytes::from(response_message_added(
            index,
            id,
            json!([{"type":"output_text","text":"oc"}]),
        )))
        .unwrap();
    assert!(
        held.is_empty(),
        "native start was published before the secret resolved: {}",
        String::from_utf8_lossy(&held.concat())
    );
    assert!(
        !converter.deferred_passthrough.is_empty(),
        "native start was not held"
    );
    let mut output = converter
        .process_chunk(Bytes::from(format!(
            "{}{}{}",
            response_text_delta(index, 0, "g tail"),
            response_message_done(
                index,
                id,
                json!([{"type":"output_text","text":format!("oc{secret} tail")}])
            ),
            response_terminal(),
        )))
        .unwrap();
    output.extend(converter.finish().unwrap());
    let output = String::from_utf8(output.concat()).unwrap();
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let values = sse_values(&output);
    let (added, done) = message_item_events(&values);
    assert_eq!(added.len(), 1, "duplicate message start\n{output}");
    assert_eq!(done.len(), 1, "duplicate message end\n{output}");
    assert_eq!(added[0]["output_index"], json!(index), "{output}");
    assert_eq!(added[0]["item"]["id"], json!(id), "{output}");
    assert_eq!(done[0]["output_index"], json!(index), "{output}");
    assert_eq!(done[0]["item"]["id"], json!(id), "{output}");
    assert_eq!(
        done[0]["item"]["content"][0]["text"],
        json!(" tail"),
        "{output}"
    );
}

#[test]
fn messages_content_block_start_before_secret_delta_is_not_duplicated() {
    let secret = "ocg";
    let source = format!(
        "{}{}{}{}{}{}",
        messages_frame(
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":1}}})
        ),
        messages_frame(
            "content_block_start",
            json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}})
        ),
        messages_frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":format!("see {secret}")}})
        ),
        messages_frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":2})
        ),
        messages_frame(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}})
        ),
        messages_frame("message_stop", json!({"type":"message_stop"})),
    );
    let output = finish_secret(ApiFormat::Messages, ApiFormat::Messages, secret, &source)
        .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let values = sse_values(&output);
    let starts = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("content_block_start")
                && value["index"] == json!(2)
        })
        .count();
    let stops = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("content_block_stop")
                && value["index"] == json!(2)
        })
        .count();
    assert_eq!(starts, 1, "{output}");
    assert_eq!(stops, 1, "{output}");
    assert_eq!(
        values
            .iter()
            .filter(|value| value.get("type").and_then(Value::as_str) == Some("message_stop"))
            .count(),
        1,
        "{output}"
    );
}

#[test]
fn sse_ordinary_tool_shapes_are_redacted_and_native_secrets_still_fail() {
    let secret = "ocg";
    let plan = domain_plan(ApiFormat::Messages, ApiFormat::Messages);
    let marker = format!(
        "{}safe",
        ocg_gateway::protocol::replay_marker_prefix(plan.replay_domain.unwrap())
    );
    let shaped = json!({
        "type":"thinking",
        "signature": marker,
        "data": marker,
        "q": format!("see {secret}")
    });
    let messages = format!(
        "{}{}{}{}{}",
        messages_frame(
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_1","model":"m"}})
        ),
        messages_frame(
            "content_block_start",
            json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"tool_use",
                    "id":"toolu_1",
                    "name":"search",
                    "input": shaped
                }
            })
        ),
        messages_frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":0})
        ),
        messages_frame(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}})
        ),
        messages_frame("message_stop", json!({"type":"message_stop"})),
    );
    let output = finish_secret(ApiFormat::Messages, ApiFormat::Messages, secret, &messages)
        .unwrap_or_else(|error| panic!("ordinary tool input: {}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    assert!(!output.contains(&marker), "{output}");
    let values = sse_values(&output);
    let input = values
        .iter()
        .find_map(|value| value.pointer("/content_block/input"))
        .expect("tool input");
    assert_eq!(input["type"], json!("thinking"));
    assert_eq!(input["q"], json!("see <redacted>"));
    assert!(!input["signature"].as_str().unwrap().contains(secret));
    assert!(!input["data"].as_str().unwrap().contains(secret));

    let arguments = serde_json::to_string(&shaped).unwrap();
    let responses = format!(
        "{}{}",
        responses_item_done(
            0,
            &json!({
                "type":"function_call",
                "id":"fc_1",
                "call_id":"call_1",
                "name":"search",
                "arguments": arguments,
                "status":"completed"
            })
        ),
        response_terminal(),
    );
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &responses,
    )
    .unwrap_or_else(|error| panic!("structured arguments: {}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    assert!(!output.contains(&marker), "{output}");
    let call = sse_values(&output)
        .into_iter()
        .find(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("function_call")
        })
        .expect("function call");
    let parsed: Value = serde_json::from_str(call["item"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["type"], json!("thinking"));
    // Stream argument redaction drops the credential bytes. The JSON response
    // path uses the `<redacted>` token; this SSE path does not.
    assert_eq!(parsed["q"], json!("see "));
    assert!(!parsed["signature"].as_str().unwrap().contains(secret));
    assert!(!parsed["data"].as_str().unwrap().contains(secret));

    let signed = format!(
        "{}{}",
        responses_sse(
            "response.output_item.done",
            json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{
                    "type":"reasoning",
                    "id":"rs_1",
                    "summary":[],
                    "encrypted_content": format!("cipher-{secret}")
                }
            })
        ),
        response_terminal(),
    );
    let error = finish_secret(ApiFormat::Responses, ApiFormat::Responses, secret, &signed)
        .expect_err("cipher secret must fail before HTTP");
    assert!(
        error
            .message
            .contains("signed native history cannot be preserved by secret redaction"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(secret), "{}", error.message);

    let signature = format!(
        "{}{}{}",
        messages_frame(
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_1","model":"m"}})
        ),
        messages_frame(
            "content_block_delta",
            json!({
                "type":"content_block_delta",
                "index":0,
                "delta":{"type":"signature_delta","signature": format!("pre-{secret}-post")}
            })
        ),
        messages_frame("message_stop", json!({"type":"message_stop"})),
    );
    let error = finish_secret(ApiFormat::Messages, ApiFormat::Messages, secret, &signature)
        .expect_err("signature secret must fail before HTTP");
    assert!(
        error
            .message
            .contains("signed native history cannot be preserved by secret redaction"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(secret), "{}", error.message);
}

fn response_created() -> String {
    responses_sse(
        "response.created",
        json!({
            "type":"response.created",
            "response":{"id":"resp_native","model":"m","status":"in_progress"}
        }),
    )
}

fn reasoning_ids(values: &[Value]) -> Vec<(u64, String)> {
    values
        .iter()
        .filter(|value| {
            value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
                || value
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|event| event.starts_with("response.reasoning_"))
        })
        .filter_map(|value| {
            let index = value.get("output_index")?.as_u64()?;
            let id = value
                .pointer("/item/id")
                .or_else(|| value.get("item_id"))
                .and_then(Value::as_str)?
                .to_string();
            Some((index, id))
        })
        .collect()
}

#[test]
fn native_reasoning_added_before_message_keeps_one_slot() {
    let secret = "ocg";
    let cipher = "cipher-safe";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}{}{}{}{}",
            response_created(),
            responses_sse(
                "response.output_item.added",
                json!({
                    "type":"response.output_item.added",
                    "output_index":0,
                    "item":{"type":"reasoning","id":"rs_native","status":"in_progress"}
                })
            ),
            response_message_added(1, "msg_native", json!([])),
            response_text_delta(1, 0, &format!("see {secret}")),
            response_message_done(
                1,
                "msg_native",
                json!([{"type":"output_text","text":format!("see {secret}")}])
            ),
            responses_sse(
                "response.reasoning_summary_text.delta",
                json!({
                    "type":"response.reasoning_summary_text.delta",
                    "output_index":0,
                    "summary_index":0,
                    "item_id":"rs_native",
                    "delta":"plan"
                })
            ),
            responses_item_done(
                0,
                &json!({
                    "type":"reasoning",
                    "id":"rs_native",
                    "summary":[{"type":"summary_text","text":"plan"}],
                    "encrypted_content":cipher
                })
            ),
            responses_item_done(
                2,
                &json!({
                    "type":"function_call",
                    "id":"fc_native",
                    "call_id":"call_native",
                    "name":"search",
                    "arguments":"{}",
                    "status":"completed"
                })
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    assert!(!output.contains("rs_0"), "{output}");
    assert!(!output.contains("rs_2"), "{output}");
    let values = sse_values(&output);
    let reasoning_added: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .collect();
    let reasoning_done: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .collect();
    assert_eq!(reasoning_added.len(), 1, "{output}");
    assert_eq!(reasoning_done.len(), 1, "{output}");
    assert_eq!(reasoning_added[0]["output_index"], json!(0));
    assert_eq!(reasoning_added[0]["item"]["id"], json!("rs_native"));
    assert_eq!(reasoning_done[0]["output_index"], json!(0));
    assert_eq!(reasoning_done[0]["item"]["id"], json!("rs_native"));
    assert_eq!(
        reasoning_done[0]["item"]["summary"][0]["text"],
        json!("plan")
    );
    assert_stream_opaque(
        reasoning_done[0]["item"]["encrypted_content"]
            .as_str()
            .unwrap(),
        cipher,
    );
    assert!(
        values
            .iter()
            .all(|value| value.get("output_index").and_then(Value::as_u64) != Some(3)),
        "orphan slot\n{output}"
    );
    let (added, done) = message_item_events(&values);
    assert_eq!(added.len(), 1, "{output}");
    assert_eq!(done.len(), 1, "{output}");
    assert_eq!(added[0]["output_index"], json!(1));
    assert_eq!(added[0]["item"]["id"], json!("msg_native"));
    let calls: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.pointer("/item/type").and_then(Value::as_str) == Some("function_call")
        })
        .collect();
    assert!(
        calls.iter().all(|value| {
            value["output_index"] == json!(2)
                && value["item"]["id"] == json!("fc_native")
                && value["item"]["call_id"] == json!("call_native")
                && value["item"]["name"] == json!("search")
        }),
        "{output}"
    );
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let types: Vec<&str> = completed["response"]["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["reasoning", "message", "function_call"]);
    assert_eq!(
        output.matches("\"encrypted_content\"").count(),
        2,
        "{output}"
    );
}

#[test]
fn native_reasoning_id_survives_a_later_secret() {
    let secret = "ocg";
    let cipher = "cipher-safe";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}",
            response_created(),
            responses_sse(
                "response.output_item.added",
                json!({
                    "type":"response.output_item.added",
                    "output_index":0,
                    "item":{
                        "type":"reasoning",
                        "id":"rs_native",
                        "summary":[{"type":"summary_text","text":"plan"}]
                    }
                })
            ),
            response_message_added(
                1,
                "msg_native",
                json!([{"type":"output_text","text":format!("see {secret}")}])
            ),
            responses_item_done(
                0,
                &json!({
                    "type":"reasoning",
                    "id":"rs_native",
                    "summary":[{"type":"summary_text","text":"plan"}],
                    "encrypted_content":cipher
                })
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains("rs_0"), "{output}");
    let values = sse_values(&output);
    let ids = reasoning_ids(&values);
    assert!(!ids.is_empty(), "{output}");
    assert!(
        ids.iter()
            .all(|(index, id)| *index == 0 && id == "rs_native"),
        "{ids:?}\n{output}"
    );
}

#[test]
fn content_only_reasoning_does_not_invent_an_empty_summary() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}",
            response_created(),
            responses_sse(
                "response.output_item.added",
                json!({
                    "type":"response.output_item.added",
                    "output_index":0,
                    "item":{
                        "type":"reasoning",
                        "id":"rs_native",
                        "content":[{"type":"reasoning_text","text":"R"}]
                    }
                })
            ),
            response_text_delta(1, 0, &format!("see {secret}")),
            responses_item_done(
                0,
                &json!({
                    "type":"reasoning",
                    "id":"rs_native",
                    "summary":[],
                    "content":[{"type":"reasoning_text","text":"R"}],
                    "encrypted_content":"cipher-safe"
                })
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    let values = sse_values(&output);
    let done = values
        .iter()
        .find(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .expect("reasoning done");
    assert_eq!(done["item"]["id"], json!("rs_native"));
    assert_eq!(done["item"]["summary"], json!([]));
    assert_eq!(done["item"]["content"][0]["text"], json!("R"));
    assert!(
        !values.iter().any(|value| {
            value.get("type").and_then(Value::as_str)
                == Some("response.reasoning_summary_part.added")
        }),
        "content-only reasoning gained a summary part\n{output}"
    );
}

#[test]
fn native_tool_id_survives_argument_redaction() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}{}",
            response_created(),
            responses_sse(
                "response.output_item.added",
                json!({
                    "type":"response.output_item.added",
                    "output_index":2,
                    "item":{
                        "type":"function_call",
                        "id":"fc_native",
                        "call_id":"call_native",
                        "name":"search",
                        "arguments":"",
                        "status":"in_progress"
                    }
                })
            ),
            responses_sse(
                "response.function_call_arguments.delta",
                json!({
                    "type":"response.function_call_arguments.delta",
                    "output_index":2,
                    "item_id":"fc_native",
                    "delta":format!("{{\"q\":\"see {secret}\"}}")
                })
            ),
            responses_sse(
                "response.function_call_arguments.done",
                json!({
                    "type":"response.function_call_arguments.done",
                    "output_index":2,
                    "item_id":"fc_native",
                    "name":"search",
                    "arguments":format!("{{\"q\":\"see {secret}\"}}")
                })
            ),
            responses_item_done(
                2,
                &json!({
                    "type":"function_call",
                    "id":"fc_native",
                    "call_id":"call_native",
                    "name":"search",
                    "arguments":format!("{{\"q\":\"see {secret}\"}}"),
                    "status":"completed"
                })
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    assert!(!output.contains("fc_2"), "{output}");
    let values = sse_values(&output);
    let calls: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.pointer("/item/type").and_then(Value::as_str) == Some("function_call")
                || value.get("type").and_then(Value::as_str)
                    == Some("response.function_call_arguments.delta")
                || value.get("type").and_then(Value::as_str)
                    == Some("response.function_call_arguments.done")
        })
        .collect();
    assert!(!calls.is_empty(), "{output}");
    assert!(
        calls.iter().all(|value| {
            value["output_index"] == json!(2)
                && value
                    .pointer("/item/id")
                    .or_else(|| value.get("item_id"))
                    .and_then(Value::as_str)
                    == Some("fc_native")
        }),
        "{output}"
    );
    let done = calls
        .iter()
        .find(|value| value.pointer("/item/type").and_then(Value::as_str) == Some("function_call"))
        .unwrap();
    assert_eq!(done["item"]["call_id"], json!("call_native"));
    assert_eq!(done["item"]["name"], json!("search"));

    let mut request = domain_plan(ApiFormat::Responses, ApiFormat::Responses);
    request.custom_tools = vec!["apply_patch".to_string()];
    let mut converter = StreamConverter::new_with_known_secret(&request, Some(secret));
    let source = format!(
        "{}{}{}{}",
        response_created(),
        responses_sse(
            "response.output_item.added",
            json!({
                "type":"response.output_item.added",
                "output_index":1,
                "item":{
                    "type":"custom_tool_call",
                    "id":"ctc_native",
                    "call_id":"call_native",
                    "name":"apply_patch",
                    "input":"",
                    "status":"in_progress"
                }
            })
        ),
        responses_sse(
            "response.custom_tool_call_input.delta",
            json!({
                "type":"response.custom_tool_call_input.delta",
                "output_index":1,
                "item_id":"ctc_native",
                "delta":format!("see {secret}")
            })
        ),
        responses_item_done(
            1,
            &json!({
                "type":"custom_tool_call",
                "id":"ctc_native",
                "call_id":"call_native",
                "name":"apply_patch",
                "input":format!("see {secret}"),
                "status":"completed"
            })
        ),
    );
    let custom = converter.process_chunk(Bytes::from(source)).unwrap();
    let custom = String::from_utf8(custom.concat()).unwrap();
    assert!(!custom.contains(&format!("see {secret}")), "{custom}");
    assert!(!custom.contains("ctc_1"), "{custom}");
    let values = sse_values(&custom);
    let tools: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.pointer("/item/type").and_then(Value::as_str) == Some("custom_tool_call")
                || value.get("type").and_then(Value::as_str)
                    == Some("response.custom_tool_call_input.delta")
        })
        .collect();
    assert!(
        tools.iter().all(|value| {
            let id = value
                .pointer("/item/id")
                .or_else(|| value.get("item_id"))
                .and_then(Value::as_str);
            id == Some("ctc_native")
        }),
        "{custom}"
    );
    let item = tools
        .iter()
        .find(|value| {
            value.pointer("/item/type").and_then(Value::as_str) == Some("custom_tool_call")
        })
        .expect("custom item");
    assert_eq!(item["item"]["call_id"], json!("call_native"));
    assert_eq!(item["item"]["name"], json!("apply_patch"));
}

#[test]
fn ordinary_tool_input_numeric_native_lookalikes_are_not_type_errors() {
    let input = json!({"type":"thinking","signature":7,"data":7});
    let message = json!({
        "role":"assistant",
        "content":[{
            "type":"tool_use",
            "id":"toolu_1",
            "name":"search",
            "input":input
        }]
    });
    reject_native_carrier_types(ApiFormat::Messages, &message)
        .expect("ordinary tool input is not a native signature");
    let thinking = json!({"type":"thinking","thinking":"","signature":7});
    let error = reject_native_carrier_types(ApiFormat::Messages, &thinking).unwrap_err();
    assert!(
        error.message.contains(SIGNATURE_MUST_BE_STRING),
        "{}",
        error.message
    );
    let redacted = json!({"type":"redacted_thinking","data":7});
    let error = reject_native_carrier_types(ApiFormat::Messages, &redacted).unwrap_err();
    assert!(
        error.message.contains(REDACTED_DATA_MUST_BE_NONEMPTY),
        "{}",
        error.message
    );

    let source = format!(
        "{}{}{}{}",
        messages_frame(
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_1","model":"m"}})
        ),
        messages_frame(
            "content_block_start",
            json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"tool_use",
                    "id":"toolu_1",
                    "name":"search",
                    "input":{"type":"thinking","signature":7,"data":7}
                }
            })
        ),
        messages_frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":0})
        ),
        messages_frame("message_stop", json!({"type":"message_stop"})),
    );
    let mut converter = StreamConverter::new(&plan(ApiFormat::Messages, ApiFormat::Messages));
    let output = converter
        .process_chunk(Bytes::from(source))
        .unwrap_or_else(|error| panic!("ordinary input: {}", error.message));
    let output = String::from_utf8(output.concat()).unwrap();
    let values = sse_values(&output);
    let seen = values
        .iter()
        .find_map(|value| value.pointer("/content_block/input"))
        .expect("tool input");
    assert_eq!(seen["type"], json!("thinking"));
    assert_eq!(seen["signature"], json!(7));
    assert_eq!(seen["data"], json!(7));

    let native = format!(
        "{}{}",
        messages_frame(
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_1","model":"m"}})
        ),
        messages_frame(
            "content_block_start",
            json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{"type":"thinking","thinking":"","signature":7}
            })
        ),
    );
    assert_frame_error(
        ApiFormat::Messages,
        ApiFormat::Messages,
        &native,
        SIGNATURE_MUST_BE_STRING,
    );
}

#[test]
fn huge_native_output_index_does_not_panic() {
    let secret = "ocg";
    for native in [u64::MAX, u64::MAX - 1] {
        let output = finish_secret(
            ApiFormat::Responses,
            ApiFormat::Responses,
            secret,
            &format!(
                "{}{}{}{}{}{}",
                response_created(),
                response_message_added(native, "msg_native", json!([])),
                response_text_delta(native, 0, &format!("see {secret}")),
                response_message_done(
                    native,
                    "msg_native",
                    json!([{"type":"output_text","text":format!("see {secret}")}])
                ),
                responses_sse(
                    "response.output_item.added",
                    json!({
                        "type":"response.output_item.added",
                        "output_index":0,
                        "item":{
                            "type":"reasoning",
                            "id":"rs_native",
                            "summary":[{"type":"summary_text","text":"plan"}]
                        }
                    })
                ),
                response_terminal(),
            ),
        )
        .unwrap_or_else(|error| panic!("index {native}: {}", error.message));
        assert!(
            !output.contains("response.failed"),
            "index {native} failed\n{output}"
        );
        assert!(
            output.contains("response.completed"),
            "index {native} missing final\n{output}"
        );
        let values = sse_values(&output);
        let message = values.iter().find(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("message")
        });
        let message = message.unwrap_or_else(|| panic!("index {native} message\n{output}"));
        assert_eq!(message["output_index"], json!(native), "{output}");
        assert_eq!(message["item"]["id"], json!("msg_native"));
        let reasoning = values
            .iter()
            .find(|value| value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning"))
            .unwrap_or_else(|| panic!("index {native} reasoning\n{output}"));
        assert_eq!(reasoning["output_index"], json!(0), "{output}");
        assert_eq!(reasoning["item"]["id"], json!("rs_native"));
    }
}

#[test]
fn explicit_empty_text_part_keeps_its_position() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}",
            response_created(),
            response_message_added(
                0,
                "msg_native",
                json!([
                    {"type":"output_text","text":""},
                    {"type":"output_text","text":""}
                ])
            ),
            response_text_delta(0, 1, &format!("see {secret}")),
            response_message_done(
                0,
                "msg_native",
                json!([
                    {"type":"output_text","text":""},
                    {"type":"output_text","text":format!("see {secret}")}
                ])
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    let values = sse_values(&output);
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let content = completed["response"]["output"][0]["content"]
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 2, "{output}");
    assert_eq!(content[0]["text"], json!(""));
    assert_eq!(content[1]["text"], json!("see "));
    assert!(
        values.iter().any(|value| {
            value["content_index"] == json!(1)
                && value.get("delta").and_then(Value::as_str) == Some("see ")
        }),
        "source part 1 was not the second final part\n{output}"
    );
}

#[test]
fn provisional_text_seed_is_not_manufactured() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &format!(
            "{}{}{}{}{}",
            response_created(),
            response_message_added(0, "msg_native", json!([])),
            response_text_delta(0, 1, &format!("see {secret}")),
            responses_sse(
                "response.output_item.done",
                json!({
                    "type":"response.output_item.done",
                    "output_index":0,
                    "item":{
                        "type":"message",
                        "id":"msg_native",
                        "status":"completed",
                        "role":"assistant"
                    }
                })
            ),
            response_terminal(),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    let values = sse_values(&output);
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let content = completed["response"]["output"][0]["content"]
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 1, "{content:?}\n{output}");
    assert_eq!(content[0]["text"], json!("see "));
    assert!(
        content.iter().all(|part| part["text"] != json!("")),
        "{content:?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum CipherWire<'a> {
    Omit,
    Null,
    Empty,
    Text(&'a str),
}

fn reasoning_with_cipher(cipher: CipherWire<'_>) -> Value {
    let mut item = json!({
        "type":"reasoning",
        "id":"rs_native",
        "summary":[{"type":"summary_text","text":"plan"}]
    });
    match cipher {
        CipherWire::Omit => {}
        CipherWire::Null => item["encrypted_content"] = Value::Null,
        CipherWire::Empty => item["encrypted_content"] = json!(""),
        CipherWire::Text(text) => item["encrypted_content"] = json!(text),
    }
    item
}

/// Added cipher, then a visible secret, then optional done and completed ciphers.
/// The secret forces the converted handoff that republishes from the stream cache.
fn tainted_reasoning_exchange(
    secret: &str,
    added: Option<CipherWire<'_>>,
    done: Option<CipherWire<'_>>,
    completed: CipherWire<'_>,
) -> String {
    let mut source = response_created();
    if let Some(cipher) = added {
        source.push_str(&responses_sse(
            "response.output_item.added",
            json!({
                "type":"response.output_item.added",
                "output_index":0,
                "item": reasoning_with_cipher(cipher)
            }),
        ));
    }
    source.push_str(&response_message_added(
        1,
        "msg_native",
        json!([{"type":"output_text","text":format!("see {secret}")}]),
    ));
    if let Some(cipher) = done {
        source.push_str(&responses_item_done(0, &reasoning_with_cipher(cipher)));
    }
    source.push_str(&responses_completed(&[reasoning_with_cipher(completed)]));
    source
}

fn reasoning_done_and_completed(output: &str) -> (Value, Value) {
    let values = sse_values(output);
    let done: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .collect();
    assert_eq!(done.len(), 1, "{output}");
    let added = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("response.output_item.added")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
        })
        .count();
    assert_eq!(added, 1, "{output}");
    assert_eq!(done[0]["output_index"], json!(0), "{output}");
    assert_eq!(done[0]["item"]["id"], json!("rs_native"), "{output}");
    assert_eq!(
        done[0]["item"]["summary"][0]["text"],
        json!("plan"),
        "{output}"
    );
    let completed = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("completed");
    let item = completed
        .pointer("/response/output")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
        .expect("completed reasoning")
        .clone();
    assert_eq!(item["id"], json!("rs_native"), "{output}");
    assert_eq!(item["summary"][0]["text"], json!("plan"), "{output}");
    let deltas = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str)
                == Some("response.reasoning_summary_text.delta")
                && value.get("delta").and_then(Value::as_str) == Some("plan")
        })
        .count();
    assert!(deltas <= 1, "summary text was repeated\n{output}");
    (done[0]["item"].clone(), item)
}

fn assert_cipher_wire(item: &Value, cipher: CipherWire<'_>, output: &str) {
    match cipher {
        CipherWire::Omit => assert!(
            item.get("encrypted_content").is_none(),
            "omitted cipher was invented\n{item}\n{output}"
        ),
        CipherWire::Null => assert!(
            item.get("encrypted_content").is_some_and(Value::is_null),
            "null cipher was rewritten\n{item}\n{output}"
        ),
        CipherWire::Empty => assert_eq!(
            item.get("encrypted_content").and_then(Value::as_str),
            Some(""),
            "empty cipher was rewritten\n{item}\n{output}"
        ),
        CipherWire::Text(raw) => {
            assert_stream_opaque(item["encrypted_content"].as_str().unwrap(), raw)
        }
    }
}

#[test]
fn responses_cipher_done_replaces_provisional_added() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &tainted_reasoning_exchange(
            secret,
            Some(CipherWire::Text("cipher-a")),
            Some(CipherWire::Text("cipher-b")),
            CipherWire::Text("cipher-b"),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let (done, completed) = reasoning_done_and_completed(&output);
    assert_cipher_wire(&done, CipherWire::Text("cipher-b"), &output);
    assert_cipher_wire(&completed, CipherWire::Text("cipher-b"), &output);
    assert!(!completed.to_string().contains("cipher-a"), "{output}");
}

#[test]
fn responses_cipher_done_clears_provisional_when_field_is_absent() {
    let secret = "ocg";
    for field in [CipherWire::Omit, CipherWire::Null, CipherWire::Empty] {
        let output = finish_secret(
            ApiFormat::Responses,
            ApiFormat::Responses,
            secret,
            &tainted_reasoning_exchange(
                secret,
                Some(CipherWire::Text("cipher-a")),
                Some(field),
                field,
            ),
        )
        .unwrap_or_else(|error| panic!("{field:?}: {}", error.message));
        assert!(!output.contains(&format!("see {secret}")), "{output}");
        let (done, completed) = reasoning_done_and_completed(&output);
        assert_cipher_wire(&done, field, &output);
        assert_cipher_wire(&completed, field, &output);
        assert!(!done.to_string().contains("cipher-a"), "{output}");
        assert!(!completed.to_string().contains("cipher-a"), "{output}");
    }
}

#[test]
fn responses_cipher_completed_backfills_when_done_omits_it() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &tainted_reasoning_exchange(
            secret,
            Some(CipherWire::Omit),
            Some(CipherWire::Omit),
            CipherWire::Text("cipher-b"),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let (done, completed) = reasoning_done_and_completed(&output);
    assert_cipher_wire(&done, CipherWire::Omit, &output);
    assert_cipher_wire(&completed, CipherWire::Text("cipher-b"), &output);
}

#[test]
fn responses_cipher_done_and_completed_stay_distinct() {
    let secret = "ocg";
    let output = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &tainted_reasoning_exchange(
            secret,
            None,
            Some(CipherWire::Text("cipher-b")),
            CipherWire::Text("cipher-c"),
        ),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(!output.contains(&format!("see {secret}")), "{output}");
    let (done, completed) = reasoning_done_and_completed(&output);
    assert_cipher_wire(&done, CipherWire::Text("cipher-b"), &output);
    assert_cipher_wire(&completed, CipherWire::Text("cipher-c"), &output);
}

#[test]
fn responses_cipher_completed_secret_is_rejected() {
    let secret = "ocg";
    let error = finish_secret(
        ApiFormat::Responses,
        ApiFormat::Responses,
        secret,
        &tainted_reasoning_exchange(
            secret,
            Some(CipherWire::Text("cipher-a")),
            Some(CipherWire::Text("cipher-b")),
            CipherWire::Text("cipher-ocg-final"),
        ),
    )
    .expect_err("final cipher secret must fail before a stale safe cipher is published");
    assert!(
        error
            .message
            .contains("signed native history cannot be preserved by secret redaction"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(secret), "{}", error.message);
}
