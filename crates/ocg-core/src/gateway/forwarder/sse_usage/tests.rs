use super::*;

use bytes::Bytes;

fn usage_event() -> Vec<u8> {
    b"data: {\"id\":\"x\",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20,\"total_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":5}}}\n\ndata: [DONE]\n\n".to_vec()
}

#[test]
fn captured_allowlisted_upstream_header_redacts_known_secret() {
    let secret = format!("opaque/{}", "account-key".repeat(32));
    let context = ForwardAttemptContext {
        trace: RequestTrace::new(),
        client_body_bytes: 0,
        upstream_body_bytes: 0,
        attempt: 1,
        client_format: ApiFormat::ChatCompletions,
        upstream_format: ApiFormat::ChatCompletions,
        model: "test-model".into(),
        requested_model: "test-model".into(),
        resolved_alias: None,
        upstream_model: "test-model".into(),
        stream: false,
        route: RouteLabel::Proxy,
        known_secret: Some(secret.clone()),
        route_account_id: None,
        provider_id: None,

        credential_account_id: None,
        client_key_id: None,
        client_key_name: None,
        platform_price: None,
        official_price: None,
        restriction_details: None,
        credit_attempt: None,
        credit_log_id: None,
        credit_token_pricing_supported: true,
    };
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", format!("request-{secret}").parse().unwrap());
    let failure = context.failure(FailureSpec {
        error_source: "upstream",
        error_stage: "upstream_http",
        downstream_status: Some(500),
        upstream_status: Some(500),
        upstream_wait_ms: None,
        retry_action: Some("return"),
        upstream_headers: Some(&headers),
        upstream_error: None,
        request_body: None,
    });
    assert!(secret.len() > 256);
    assert!(
        !failure.diagnostic_json.contains(&secret[..256]),
        "truncated header prefix leaked Key: {}",
        failure.diagnostic_json
    );
    let diagnostic: Value = serde_json::from_str(&failure.diagnostic_json).unwrap();
    assert_eq!(
        diagnostic["upstream_headers"]["x-request-id"],
        "request-<redacted>"
    );
}

#[test]
fn single_chunk_extracts_usage() {
    let mut st = StreamState::default();
    let chunk = Bytes::from(usage_event());
    process_chunk_for_usage(&mut st, ApiFormat::ChatCompletions, &chunk, None);
    assert!(st.has_usage, "usage should be set");
    let (p, c, cached, cache_creation) = token_counts(st.usage);
    assert_eq!(p, 10);
    assert_eq!(c, 20);
    assert_eq!(cached, 5);
    assert_eq!(cache_creation, 0);
    assert!(st.buf.is_empty(), "buffer should drain on full events");
}

#[test]
fn chunk_boundary_handling() {
    let full = usage_event();
    let a = &full[..20];
    let b = &full[20..full.len() - 5];
    let c = &full[full.len() - 5..];

    let mut st = StreamState::default();
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::copy_from_slice(a),
        None,
    );
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::copy_from_slice(b),
        None,
    );
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::copy_from_slice(c),
        None,
    );

    assert!(st.has_usage, "usage should be set after boundary");
    let (p, c, cached, cache_creation) = token_counts(st.usage);
    assert_eq!((p, c, cached, cache_creation), (10, 20, 5, 0));
    assert!(st.buf.is_empty(), "buffer should be empty after all chunks");
}

#[test]
fn no_usage_event_yields_none() {
    let mut st = StreamState::default();
    let payload =
        b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n".to_vec();
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::from(payload),
        None,
    );
    assert!(!st.has_usage, "no usage field means no usage");
    assert!(st.buf.is_empty());
}

#[test]
fn last_non_null_usage_wins() {
    let mut st = StreamState::default();
    let first = b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n".to_vec();
    let second = b"data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":200,\"prompt_tokens_details\":{\"cached_tokens\":50}}}\n\n".to_vec();
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::from(first),
        None,
    );
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::from(second),
        None,
    );
    assert!(st.has_usage, "usage set");
    let (p, c, cached, cache_creation) = token_counts(st.usage);
    assert_eq!((p, c, cached, cache_creation), (100, 200, 50, 0));
}

#[test]
fn messages_stream_merges_start_and_delta_usage() {
    let mut st = StreamState::default();
    let start = Bytes::from_static(
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":6,\"cache_read_input_tokens\":4,\"cache_creation_input_tokens\":2}}}\n\n",
    );
    let delta = Bytes::from_static(
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n",
    );
    process_chunk_for_usage(&mut st, ApiFormat::Messages, &start, None);
    process_chunk_for_usage(&mut st, ApiFormat::Messages, &delta, None);
    assert!(st.has_usage);
    assert_eq!(token_counts(st.usage), (12, 7, 4, 2));
}

#[test]
fn messages_stream_keeps_raw_minimax_cache_read_tokens() {
    for (hint, start_model) in [
        ("minimax-m3", None),
        ("minimax-m3", Some("ocg-generic")),
        ("MiniMax-M3", None),
    ] {
        let message = match start_model {
            Some(model) => format!(
                "{{\"model\":\"{model}\",\"usage\":{{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}}}"
            ),
            None => "{\"usage\":{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}".to_string() };
        let start = Bytes::from(format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{message}}}\n\n"
        ));
        let mut st = StreamState::default();
        process_chunk_for_usage(&mut st, ApiFormat::Messages, &start, Some(hint));
        assert!(st.has_usage, "hint={hint} start_model={start_model:?}");
        let (input, output, cached, _) = token_counts(st.usage);
        assert_eq!(
            (input, output, cached),
            (40500, 5, 40500),
            "hint={hint} start_model={start_model:?}"
        );
    }
}

#[test]
fn upstream_stream_error_marks_log_state() {
    let mut st = StreamState::default();
    let event = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}\n\n",
    );
    process_chunk_for_usage(&mut st, ApiFormat::Messages, &event, None);
    assert!(st.error);
    assert_eq!(st.error_message.as_deref(), Some("boom"));

    let mut responses = StreamState::default();
    let event = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"codex boom\"}}}\n\n",
    );
    process_chunk_for_usage(&mut responses, ApiFormat::Responses, &event, None);
    assert!(responses.error);
    assert_eq!(responses.error_message.as_deref(), Some("codex boom"));
}

#[test]
fn terminal_usage_ignores_late_stream_errors() {
    let mut st = StreamState::default();
    let chunk = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"late\"}}}\n\n",
    );
    process_chunk_for_usage(&mut st, ApiFormat::Responses, &chunk, None);
    assert!(st.terminal);
    assert!(!st.error);
    assert_eq!(token_counts(st.usage), (7, 2, 0, 0));

    let later = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"later\"}}}\n\n",
    );
    process_chunk_for_usage(&mut st, ApiFormat::Responses, &later, None);
    assert!(!st.error);
    assert_eq!(token_counts(st.usage), (7, 2, 0, 0));
}

#[test]
fn crlf_event_boundary_is_detected() {
    // \r\n\r\n-terminated event must be split out, not accumulated.
    let mut st = StreamState::default();
    let payload =
        b"data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":11}}\r\n\r\n".to_vec();
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::from(payload),
        None,
    );
    assert!(st.has_usage, "CRLF usage should be parsed");
    let (p, c, _, _) = token_counts(st.usage);
    assert_eq!((p, c), (7, 11));
    assert!(st.buf.is_empty());
}

#[test]
fn buffer_bound_clears_on_oversize() {
    let mut st = StreamState::default();
    // Incomplete leftover larger than MAX_SSE_BUF is dropped after the drain.
    let big = vec![b'x'; MAX_SSE_BUF + 1];
    process_chunk_for_usage(&mut st, ApiFormat::ChatCompletions, &Bytes::from(big), None);
    assert!(
        st.buf.is_empty(),
        "oversize incomplete leftovers are dropped"
    );
    assert!(!st.has_usage);
}

#[test]
fn large_reasoning_chunk_still_captures_trailing_usage() {
    // Flash-style burst: one HTTP chunk larger than the old 64 KiB scanner
    // cap, with usage in a later SSE event of the same chunk.
    let reasoning = "r".repeat(80_000);
    let payload = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":\"{reasoning}\"}}}}]}}\n\n\
         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":9,\"completion_tokens\":3}}}}\n\n\
         data: [DONE]\n\n"
    );
    let mut st = StreamState::default();
    process_chunk_for_usage(
        &mut st,
        ApiFormat::ChatCompletions,
        &Bytes::from(payload),
        Some("deepseek-v4-flash"),
    );
    assert!(
        st.has_usage,
        "usage after a large reasoning event must be kept"
    );
    assert_eq!(token_counts(st.usage), (9, 3, 0, 0));
    assert!(st.terminal);
    assert!(st.buf.is_empty());
}
