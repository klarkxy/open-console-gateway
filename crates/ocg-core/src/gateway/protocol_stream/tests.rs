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
