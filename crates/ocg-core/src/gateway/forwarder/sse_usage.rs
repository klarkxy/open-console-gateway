//! SSE usage framing. Pass-through bytes are not rewritten here; this only observes usage and terminal markers.

use super::*;

#[cfg(test)]
mod tests;

// ----- SSE usage accumulation -----

// ponytail: single Mutex<StreamState> instead of 3 separate Arc<Mutex<>>/
// AtomicBool. Lock is held for a single chunk's processing (microseconds);
// upgrade to per-chunk allocator if cross-stream contention ever shows up.
#[derive(Default)]
pub(super) struct StreamState {
    pub(super) buf: BytesMut,
    pub(super) usage: UsageCounts,
    pub(super) has_usage: bool,
    pub(super) terminal: bool,
    /// Set by the mapped Err arm so the finalizer can skip its status overwrite.
    pub(super) error: bool,
    pub(super) outcome_unknown: bool,
    pub(super) error_message: Option<String>,
    pub(super) diagnostic_recorded: bool,
}

// Match the stream converter's pending-frame cap. The old 64 KiB limit dropped
// DeepSeek flash reasoning bursts before the trailing usage chunk could be parsed.
const MAX_SSE_BUF: usize = 8 * 1024 * 1024;

// Accept LF and CRLF event boundaries in wire order, including mixed endings.
fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    (0..buf.len().saturating_sub(1))
        .find(|&i| buf[i..].starts_with(b"\n\n") || buf[i..].starts_with(b"\r\n\r\n"))
}

fn event_boundary_len(buf: &[u8], start: usize) -> usize {
    if start + 3 < buf.len() && &buf[start..start + 4] == b"\r\n\r\n" {
        4
    } else {
        2
    }
}

fn extract_data_payload(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

// ponytail: ignore_err on JSON parse — SSE frames may be comments or keep-alive
// heartbeats. Silent skip; the last non-null usage frame still wins.
// ponytail: bounded buffer — if the upstream never sends a complete event
// (malformed stream, CRLF-only chunks, dropped keep-alive framing), drop the
// garbage so memory can't grow unbounded.
pub(super) fn process_chunk_for_usage(
    st: &mut StreamState,
    format: ApiFormat,
    chunk: &bytes::Bytes,
    model_hint: Option<&str>,
) {
    if st.terminal {
        return;
    }
    st.buf.extend_from_slice(chunk);
    loop {
        let bytes = st.buf.as_ref();
        let Some(idx) = find_event_boundary(bytes) else {
            break;
        };
        let take = event_boundary_len(bytes, idx);
        let event = st.buf.split_to(idx + take);
        if let Some(payload) = extract_data_payload(&event) {
            let payload = payload.trim();
            if payload == "[DONE]" {
                st.terminal = true;
                st.buf.clear();
                break;
            }
            if payload.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(payload) {
                let is_error = matches!(
                    v.get("type").and_then(Value::as_str),
                    Some("error" | "response.failed")
                ) || v.get("error").is_some_and(|error| !error.is_null());
                if is_error {
                    st.error = true;
                    st.error_message = Some(
                        v.pointer("/response/error/message")
                            .or_else(|| v.pointer("/error/message"))
                            .or_else(|| v.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("upstream stream error")
                            .to_string(),
                    );
                }
                if has_usage(format, &v) {
                    // Always retain the request model as the hint. Some compatible
                    // upstreams rewrite the response model to a generic alias, and
                    // extract_usage already combines that response model with this
                    // original hint when applying model-specific normalization.
                    merge_stream_usage(format, &v, &mut st.usage, model_hint);
                    st.has_usage = true;
                }
                let event_type = v.get("type").and_then(Value::as_str);
                let is_terminal = is_error
                    || match format {
                        ApiFormat::ChatCompletions => false,
                        ApiFormat::Messages => event_type == Some("message_stop"),
                        ApiFormat::Responses => matches!(
                            event_type,
                            Some("response.completed" | "response.incomplete")
                        ),
                        ApiFormat::Gemini => false,
                    };
                if is_terminal {
                    st.terminal = true;
                    st.buf.clear();
                    break;
                }
            }
        }
    }
    if st.buf.len() > MAX_SSE_BUF {
        st.buf.clear();
    }
}

pub(super) fn token_counts(usage: UsageCounts) -> (i64, i64, i64, i64) {
    let to_i64 = |value: u64| value.min(i64::MAX as u64) as i64;
    (
        to_i64(usage.input_tokens),
        to_i64(usage.output_tokens),
        to_i64(usage.cached_tokens),
        to_i64(usage.cache_creation_tokens),
    )
}
