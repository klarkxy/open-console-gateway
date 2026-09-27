//! Pure secret/error sanitizers shared by contracts, dashboard DTOs, and
//! gateway diagnostics.
//!
//! This module is an I/O-free DAG leaf: it must not import gateway, dashboard,
//! db, state, HTTP, filesystem, or async runtime. Policy (which fields are
//! redacted) stays here so every caller sees the same bytes.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const MAX_UPSTREAM_ERROR_BYTES: usize = 2 * 1024;

pub(crate) fn sanitize_upstream_error_value(text: &str) -> Value {
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        redact_value(&mut value, None);
        let encoded = value.to_string();
        if encoded.len() <= MAX_UPSTREAM_ERROR_BYTES {
            return value;
        }
        return json!({
            "summary": truncate_text(&encoded, MAX_UPSTREAM_ERROR_BYTES.saturating_sub(64)),
            "truncated": true });
    }
    let redacted = redact_text(text);
    json!({
        "text": truncate_text(&redacted, MAX_UPSTREAM_ERROR_BYTES.saturating_sub(64)),
        "truncated": redacted.len() > MAX_UPSTREAM_ERROR_BYTES.saturating_sub(64) })
}

/// Redact the exact credential selected for an upstream attempt before applying
/// the generic error sanitizer. Upstream providers sometimes echo credentials
/// in ordinary message fields, where neither a sensitive field name nor a
/// conventional `sk-` prefix is available to identify them.
pub(crate) fn sanitize_upstream_error_value_with_known_secret(
    text: &str,
    known_secret: &str,
) -> Value {
    sanitize_upstream_error_value(&redact_known_secret(text, known_secret))
}

/// Remove an exact known secret while otherwise preserving an upstream body.
/// JSON string values are handled after decoding as well, so credentials that
/// contain quotes, backslashes, or other JSON-escaped characters are covered.
pub(crate) fn redact_known_secret(text: &str, known_secret: &str) -> String {
    if known_secret.is_empty() {
        return text.to_string();
    }

    let directly_redacted = redact_exact_occurrences(text, known_secret);
    let directly_redacted = serde_json::to_string(known_secret)
        .ok()
        .and_then(|encoded| {
            encoded
                .strip_prefix('"')
                .and_then(|encoded| encoded.strip_suffix('"'))
                .map(str::to_string)
        })
        .filter(|encoded| encoded != known_secret)
        .map_or(directly_redacted.clone(), |encoded| {
            redact_exact_occurrences(&directly_redacted, &encoded)
        });
    let Ok(mut value) = serde_json::from_str::<Value>(&directly_redacted) else {
        return directly_redacted;
    };
    redact_known_secret_value(&mut value, known_secret);
    serde_json::to_string(&value).unwrap_or(directly_redacted)
}

fn redact_known_secret_value(value: &mut Value, known_secret: &str) {
    match value {
        Value::String(text) => *text = redact_exact_occurrences(text, known_secret),
        Value::Array(values) => {
            for value in values {
                redact_known_secret_value(value, known_secret);
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut value) in original {
                redact_known_secret_value(&mut value, known_secret);
                values.insert(redact_exact_occurrences(&key, known_secret), value);
            }
        }
        _ => {}
    }
}

pub(crate) fn redact_exact_occurrences(text: &str, secret: &str) -> String {
    let mut redacted = text.replace(secret, "<redacted>");
    while redacted.contains(secret) {
        redacted = redacted.replace(secret, "");
    }
    redacted
}

fn redact_value(value: &mut Value, key_hint: Option<&str>) {
    if key_hint.is_some_and(is_sensitive_key) {
        *value = Value::String("<redacted>".to_string());
        return;
    }
    if key_hint.is_some_and(is_content_key) {
        let encoded = match value {
            Value::String(text) => text.as_bytes().to_vec(),
            _ => serde_json::to_vec(value).unwrap_or_default(),
        };
        *value = json!({
            "bytes": encoded.len(),
            "sha256": &sha256_hex(&encoded)[..12] });
        return;
    }
    match value {
        Value::String(text) => *text = redact_text(text),
        Value::Array(values) => {
            for value in values {
                redact_value(value, key_hint);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                redact_value(value, Some(key));
            }
        }
        _ => {}
    }
}

fn is_content_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "prompt"
            | "input"
            | "text"
            | "input_text"
            | "output_text"
            | "instructions"
            | "system"
            | "content"
            | "arguments"
            | "tool_arguments"
            | "tool_name"
            | "url"
            | "image_url"
            | "file"
            | "file_data"
            | "base64"
            | "data"
            | "body"
    )
}

pub(crate) fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let normalized = lower.replace(['-', '_', '.', ' '], "");
    if matches!(
        normalized.as_str(),
        "authorization"
            | "apikey"
            | "token"
            | "cookie"
            | "password"
            | "passwd"
            | "secret"
            | "credential"
            | "bearer"
            | "privatekey"
    ) || [
        "authorization",
        "apikey",
        "password",
        "passwd",
        "secret",
        "secretkey",
        "secretaccesskey",
        "accesskeyid",
        "credential",
        "privatekey",
        "token",
        "accesstoken",
        "refreshtoken",
        "sessiontoken",
        "idtoken",
        "authtoken",
        "bearertoken",
        "apitoken",
        "clienttoken",
    ]
    .iter()
    .any(|suffix| normalized.ends_with(suffix))
    {
        return true;
    }

    let parts = lower
        .split(['-', '_', '.', ' '])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.iter().any(|part| {
        matches!(
            *part,
            "authorization" | "cookie" | "password" | "passwd" | "secret" | "credential" | "bearer"
        )
    }) {
        return true;
    }
    parts.windows(2).any(|pair| {
        matches!(
            pair,
            ["api", "key"]
                | ["private", "key"]
                | ["access", "token"]
                | ["refresh", "token"]
                | ["session", "token"]
                | ["id", "token"]
                | ["auth", "token"]
                | ["bearer", "token"]
                | ["client", "token"]
                | ["token", "value"]
                | ["token", "key"]
        )
    })
}

pub(crate) fn redact_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key-----") {
        return "<redacted private key>".to_string();
    }

    let mut redact_next_line = false;
    text.lines()
        .map(|line| {
            if redact_next_line {
                if line.trim().is_empty() {
                    return line.to_string();
                }
                redact_next_line = false;
                return "<redacted>".to_string();
            }

            if let Some(index) = sensitive_assignment_start(line) {
                redact_next_line = sensitive_assignment_value_is_empty(line, index);
            }
            redact_text_line(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_text_line(text: &str) -> String {
    if let Some(index) = sensitive_assignment_start(text) {
        return format!("{}<redacted>", &text[..index]);
    }

    let mut output = Vec::new();
    let mut redact_next = false;
    for token in text.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let label =
            lower.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-'));
        if redact_next {
            if label.is_empty() || matches!(label, "bearer" | "basic") {
                output.push(token.to_string());
            } else {
                output.push("<redacted>".to_string());
                redact_next = false;
            }
        } else if lower.contains("sk-") {
            output.push("<redacted>".to_string());
        } else if let Some(value) = sensitive_assignment_value(&lower) {
            output.push("<redacted>".to_string());
            let value = value
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-'));
            redact_next = value.is_empty() || matches!(value, "bearer" | "basic");
        } else if matches!(label, "bearer" | "basic") {
            output.push(token.to_string());
            redact_next = true;
        } else if is_standalone_sensitive_label(label) {
            output.push(token.to_string());
            output.push("<redacted>".to_string());
            break;
        } else {
            output.push(token.to_string());
        }
    }
    output.join(" ")
}

fn sensitive_assignment_start(text: &str) -> Option<usize> {
    const LABELS: &[&str] = &[
        "proxy-authorization",
        "authorization",
        "set-cookie",
        "cookie",
        "x-api-key",
        "x_api_key",
        "api-key",
        "api_key",
        "api.key",
        "api key",
        "access-token",
        "access_token",
        "access token",
        "refresh-token",
        "refresh_token",
        "refresh token",
        "client-secret",
        "client_secret",
        "client secret",
        "private-key",
        "private_key",
        "private key",
        "password",
        "passwd",
        "credential",
        "secret",
        "token",
    ];

    let lower = text.to_ascii_lowercase();
    let mut earliest = generic_sensitive_assignment_start(&lower);
    for label in LABELS {
        for (index, _) in lower.match_indices(label) {
            let boundary_before = index == 0
                || lower[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !is_label_char(ch));
            if !boundary_before {
                continue;
            }
            let value_start = index + label.len();
            let remainder = lower[value_start..]
                .trim_start()
                .trim_start_matches(['\"', '\''])
                .trim_start();
            let separator = remainder.chars().next();
            if matches!(separator, Some('=' | ':')) {
                earliest = Some(earliest.map_or(index, |current: usize| current.min(index)));
            }
        }
    }
    earliest
}

fn generic_sensitive_assignment_start(text: &str) -> Option<usize> {
    text.char_indices()
        .filter(|(_, ch)| matches!(ch, '=' | ':'))
        .filter_map(|(separator_index, _)| {
            let prefix = text[..separator_index].trim_end();
            let prefix = prefix.trim_end_matches(['\"', '\'']).trim_end();
            let key_end = prefix.len();
            let key_start = prefix[..key_end]
                .char_indices()
                .rev()
                .find(|(_, ch)| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.'))
                .map_or(0, |(index, ch)| index + ch.len_utf8());
            let key = &prefix[key_start..key_end];
            (!key.is_empty() && is_sensitive_key(key)).then_some(key_start)
        })
        .min()
}

fn sensitive_assignment_value_is_empty(text: &str, assignment_start: usize) -> bool {
    text[assignment_start..]
        .char_indices()
        .find(|(_, ch)| matches!(ch, '=' | ':'))
        .is_some_and(|(index, ch)| {
            text[assignment_start + index + ch.len_utf8()..]
                .trim()
                .is_empty()
        })
}

fn is_label_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

fn sensitive_assignment_value(text: &str) -> Option<&str> {
    text.char_indices()
        .filter(|(_, ch)| matches!(ch, '=' | ':'))
        .find_map(|(index, ch)| {
            let key = &text[..index];
            is_sensitive_key(key).then(|| &text[index + ch.len_utf8()..])
        })
}

fn is_standalone_sensitive_label(label: &str) -> bool {
    !label.is_empty() && is_sensitive_key(label)
}

pub(crate) fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text[..end].to_string()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_error_redacts_nested_secrets_and_is_bounded() {
        let value = sanitize_upstream_error_value(
            &json!({
                "error": {
                    "message": "Bearer abc sk-secret",
                    "api_key": "top-secret",
                    "input": "private prompt",
                    "tool_name": "private_tool"
                },
                "password": "hunter2",
                "padding": "x".repeat(8_000)
            })
            .to_string(),
        );
        let encoded = value.to_string();
        assert!(!encoded.contains("abc"));
        assert!(!encoded.contains("sk-secret"));
        assert!(!encoded.contains("top-secret"));
        assert!(!encoded.contains("hunter2"));
        assert!(!encoded.contains("private prompt"));
        assert!(!encoded.contains("private_tool"));
        assert!(encoded.len() <= MAX_UPSTREAM_ERROR_BYTES + 128);

        let plain = sanitize_upstream_error_value(
            "authorization: Bearer abc token=def password=hunter2 cookie: yum sk-last",
        )
        .to_string();
        for secret in ["abc", "def", "hunter2", "yum", "sk-last"] {
            assert!(!plain.contains(secret), "plain error leaked {secret}");
        }
    }

    #[test]
    fn upstream_error_redacts_an_exact_known_secret_with_any_format() {
        let secret = "opaque/credential+with=punctuation";
        let json_error = format!(
            r#"{{"error":{{"message":"provider rejected {secret}","detail":"{secret}"}}}}"#
        );
        let encoded =
            sanitize_upstream_error_value_with_known_secret(&json_error, secret).to_string();
        assert!(!encoded.contains(secret), "known secret leaked: {encoded}");

        let escaped_secret = "opaque/\"quoted\"\\credential";
        let escaped_error = serde_json::json!({
            "error": {"message": format!("provider rejected {escaped_secret}")}
        })
        .to_string();
        let encoded =
            sanitize_upstream_error_value_with_known_secret(&escaped_error, escaped_secret)
                .to_string();
        assert!(
            !encoded.contains("opaque/"),
            "JSON-escaped known secret leaked: {encoded}"
        );

        let plain = redact_known_secret(&format!("unexpected credential: {secret}"), secret);
        assert_eq!(plain, "unexpected credential: <redacted>");

        let marker_overlap = redact_known_secret("credential=redacted", "redacted");
        assert!(!marker_overlap.contains("redacted"));
    }

    #[test]
    fn upstream_error_redacts_common_plain_text_secret_boundaries() {
        let cases = [
            (
                "authorization=Bearer bearer-inline-value",
                "bearer-inline-value",
            ),
            (
                "authorization=Basic basic-inline-value",
                "basic-inline-value",
            ),
            (
                "Authorization:Bearer compact-bearer-value",
                "compact-bearer-value",
            ),
            ("prefix (sk-parenthesized-value)", "sk-parenthesized-value"),
            ("\"sk-quoted-value\"", "sk-quoted-value"),
            ("prefix=sk-assignment-value", "sk-assignment-value"),
            ("api_key = separated-key-value", "separated-key-value"),
            (
                "https://example.invalid/?code=bad&api_key=url-query-value",
                "url-query-value",
            ),
            (
                "password=\"correct horse battery staple\"",
                "correct horse battery staple",
            ),
            (
                "Cookie: sid=cookie-session-value; refresh=cookie-refresh-value",
                "cookie-session-value",
            ),
            (
                "Cookie: sid=cookie-session-value; refresh=cookie-refresh-value",
                "cookie-refresh-value",
            ),
            ("private_key=pk-live-private-value", "pk-live-private-value"),
            (
                "database_password=\"correct horse battery staple\"",
                "correct horse battery staple",
            ),
            ("api.key=plain-api-value", "plain-api-value"),
            ("api key: natural-api-value", "natural-api-value"),
            (
                "The password is \"correct horse battery staple\"",
                "correct horse battery staple",
            ),
            (
                "database_password value is correct horse battery staple",
                "correct horse battery staple",
            ),
            (
                "github_token=\"plain credential value\"",
                "plain credential value",
            ),
            ("secretKey=\"plain secret value\"", "plain secret value"),
            (
                "awsSecretAccessKey=\"plain aws secret value\"",
                "plain aws secret value",
            ),
            (
                "password (string): correct horse battery staple",
                "correct horse battery staple",
            ),
        ];
        for (text, secret) in cases {
            let encoded = sanitize_upstream_error_value(text).to_string();
            assert!(
                !encoded.contains(secret),
                "plain error leaked {secret}: {encoded}"
            );
        }

        let encoded = sanitize_upstream_error_value(
            r#"{"message":"authorization=Bearer json-message-value"}"#,
        )
        .to_string();
        assert!(!encoded.contains("json-message-value"));

        let encoded = sanitize_upstream_error_value(
            r#"{"message":"database_password=\"correct horse battery staple\""}"#,
        )
        .to_string();
        assert!(!encoded.contains("correct horse battery staple"));

        let encoded =
            sanitize_upstream_error_value(r#"payload={"password":"correct horse battery staple"}"#)
                .to_string();
        assert!(!encoded.contains("correct horse battery staple"));

        let multiline =
            sanitize_upstream_error_value("password:\ncorrect horse battery staple").to_string();
        assert!(!multiline.contains("correct horse battery staple"));

        let tokenizer =
            sanitize_upstream_error_value("tokenizer failed to encode input").to_string();
        assert!(tokenizer.contains("tokenizer failed to encode input"));

        let token_limit = sanitize_upstream_error_value(
            r#"{"error":{"message":"max_tokens must be <= 4096","max_tokens":8192}}"#,
        );
        assert_eq!(token_limit["error"]["max_tokens"], 8192);
        assert_eq!(
            token_limit["error"]["message"],
            "max_tokens must be <= 4096"
        );

        let echoed_prompt = sanitize_upstream_error_value(
            r#"{"error":{"message":"invalid input","text":"customer SSN 123-45-6789","instructions":"private system prompt"}}"#,
        );
        let encoded = echoed_prompt.to_string();
        assert!(!encoded.contains("123-45-6789"));
        assert!(!encoded.contains("private system prompt"));
        assert_eq!(echoed_prompt["error"]["text"]["bytes"], 24);

        let private_key = sanitize_upstream_error_value(
            "-----BEGIN PRIVATE KEY-----\nprivate-material\n-----END PRIVATE KEY-----",
        )
        .to_string();
        assert!(!private_key.contains("private-material"));
    }
}
