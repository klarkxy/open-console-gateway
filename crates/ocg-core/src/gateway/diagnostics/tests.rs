use super::*;

#[test]
fn request_summary_never_keeps_content_or_tool_arguments() {
    let secret = "private prompt sk-super-secret";
    let body = serde_json::to_vec(&json!({
            "model": "kimi-k2.7-code",
            "stream": true,
            "messages": [{"role": "user", "content": secret}],
            "tools": [{"type": "function", "function": {"name": "private_tool", "arguments": {"password": "hunter2"}}}],
            "image_url": "https://secret.example/token"
        }))
        .unwrap();
    let (summary, fingerprint) = summarize_request(&body);
    let encoded = summary.to_string();
    assert!(!encoded.contains(secret));
    assert!(!encoded.contains("private_tool"));
    assert!(!encoded.contains("hunter2"));
    assert!(!encoded.contains("secret.example"));
    assert_eq!(fingerprint.len(), 64);
    assert!(encoded.len() <= MAX_REQUEST_SUMMARY_BYTES);
    let (again, again_fingerprint) = summarize_request(&body);
    assert_eq!(summary, again);
    assert_eq!(fingerprint, again_fingerprint);
}

#[test]
fn diagnostic_facade_reexports_pure_sanitizers() {
    let encoded = sanitize_upstream_error_value_with_known_secret("secret=abc", "abc").to_string();
    assert!(!encoded.contains("abc"));
    assert_eq!(redact_known_secret("token abc", "abc"), "token <redacted>");
}

#[test]
fn success_value_redaction_never_changes_json_keys() {
    let mut value = json!({
        "data": "data",
        "metadata": {
            "database": "safe data value",
            "nested": ["data", 42]
        }
    });
    redact_known_secret_values(&mut value, "data");

    assert!(value.get("data").is_some());
    assert!(value["metadata"].get("database").is_some());
    assert_eq!(value["data"], "<redacted>");
    assert_eq!(value["metadata"]["database"], "safe <redacted> value");
    assert_eq!(value["metadata"]["nested"][0], "<redacted>");
}

#[test]
fn success_value_redaction_preserves_protocol_control_values() {
    let mut value = json!({
        "type": "text",
        "object": "chat.completion",
        "status": "completed",
        "id": "text",
        "model": "text",
        "name": "text",
        "text": "before text after",
        "detail": "text"
    });
    redact_known_secret_values(&mut value, "text");

    assert_eq!(value["type"], "text");
    assert_eq!(value["object"], "chat.completion");
    assert_eq!(value["status"], "completed");
    for key in ["id", "model", "name"] {
        assert_eq!(value[key], "<redacted>", "free-form field {key} leaked");
    }
    assert_eq!(value["text"], "before <redacted> after");
    assert_eq!(value["detail"], "<redacted>");

    for event_type in [
        "response.reasoning_summary_part.added",
        "response.output_text.done",
        "response.reasoning_summary_text.done",
    ] {
        let mut event = json!({"type":event_type});
        redact_known_secret_values(&mut event, event_type);
        assert_eq!(event["type"], event_type);
    }
}

#[test]
fn success_value_redaction_does_not_trust_arbitrary_control_field_text() {
    let secret = "opaque/account+key=42";
    let mut value = json!({
        "type": format!("echo {secret}"),
        "status": format!("failed: {secret}"),
        "stop_sequence": secret,
        "nested": {"reason": format!("provider said {secret}")}
    });
    redact_known_secret_values(&mut value, secret);

    assert!(!value.to_string().contains(secret), "{value}");
    assert_eq!(value["type"], "echo <redacted>");
    assert_eq!(value["stop_sequence"], "<redacted>");
}

#[test]
fn success_value_redaction_removes_known_opaque_replays_with_the_secret() {
    let secret = "opaque/account+key=42";
    let anthropic = super::super::protocol::encode_anthropic_thinking_block(&json!({
        "type":"thinking",
        "thinking":format!("before {secret} after"),
        "signature":"sig_123"
    }))
    .unwrap();
    let chat =
        super::super::protocol::encode_chat_reasoning(&format!("before {secret} after")).unwrap();
    let mut value = json!({
        "output":[
            {"type":"reasoning","encrypted_content":anthropic},
            {"type":"reasoning","encrypted_content":chat}
        ]
    });
    redact_known_secret_values(&mut value, secret);

    assert_eq!(value["output"][0]["encrypted_content"], "");
    assert_eq!(value["output"][1]["encrypted_content"], "");

    let safe = super::super::protocol::encode_anthropic_thinking_block(&json!({
        "type":"redacted_thinking",
        "data":"safe"
    }))
    .unwrap();
    let mut safe_value = json!({"encrypted_content":safe});
    redact_known_secret_values(&mut safe_value, "data");
    assert_eq!(safe_value["encrypted_content"], safe);
}

#[test]
fn success_value_redaction_parses_nested_tool_arguments() {
    for secret in ["data", "a\"b", "a\\b"] {
        let mut value = json!({
            "arguments": json!({"data":"safe","type":secret,"token":secret}).to_string()
        });
        redact_known_secret_values(&mut value, secret);
        let arguments: Value = serde_json::from_str(value["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(
            arguments,
            json!({"data":"safe","type":"<redacted>","token":"<redacted>"}),
            "nested arguments leaked or corrupted {secret:?}"
        );
    }
}

#[test]
fn serialized_diagnostic_is_valid_json_and_bounded() {
    let trace = RequestTrace::new();
    let mut diagnostic =
        ErrorDiagnostic::new(&trace, 1, "upstream", "upstream_http", ApiFormat::Responses);
    diagnostic.request_summary = Some(json!({"padding": "x".repeat(10_000)}));
    diagnostic.upstream_error = Some(json!({"padding": "y".repeat(10_000)}));
    let encoded = serialize_diagnostic(diagnostic);
    assert!(encoded.len() <= MAX_DIAGNOSTIC_BYTES);
    let parsed: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(parsed["truncated"], true);
}

#[test]
fn upstream_header_capture_uses_an_explicit_allowlist() {
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "provider-123".parse().unwrap());
    headers.insert("cf-ray", "ray-456".parse().unwrap());
    headers.insert("authorization", "Bearer secret".parse().unwrap());
    headers.insert("set-cookie", "session=secret".parse().unwrap());
    let safe = safe_upstream_headers(&headers, None);
    assert_eq!(
        safe.get("x-request-id").map(String::as_str),
        Some("provider-123")
    );
    assert_eq!(safe.get("cf-ray").map(String::as_str), Some("ray-456"));
    assert!(!safe.contains_key("authorization"));
    assert!(!safe.contains_key("set-cookie"));
}

#[test]
fn legacy_tool_compat_emission_is_request_id_profile_version_and_dropped_types_only() {
    let _ = take_legacy_tool_compat_emissions();
    emit_legacy_tool_compat(
        "ocg-test-request",
        "legacy_compat",
        1,
        &["web_search".into(), "file_search".into()],
    );
    let emissions = take_legacy_tool_compat_emissions();
    assert_eq!(emissions.len(), 1);
    let payload = &emissions[0];
    assert_eq!(payload["request_id"], "ocg-test-request");
    assert_eq!(payload["profile"], "legacy_compat");
    assert_eq!(payload["version"], 1);
    assert_eq!(
        payload["dropped_hosted_tools"],
        json!(["web_search", "file_search"])
    );
    let object = payload.as_object().expect("compat diagnostic is an object");
    assert_eq!(object.len(), 4);
    assert!(object.contains_key("request_id"));
    assert!(object.contains_key("profile"));
    assert!(object.contains_key("version"));
    assert!(object.contains_key("dropped_hosted_tools"));
}
