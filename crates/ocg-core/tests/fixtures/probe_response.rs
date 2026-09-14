//! Protocol-correct minimal successful replies for local probe origins.
pub const CHAT: &str = r#"{"id":"ok","object":"chat.completion","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;

pub fn for_path(path: &str) -> &'static str {
    if path.ends_with("/messages") {
        r#"{"id":"ok","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#
    } else if path.ends_with("/responses") {
        r#"{"id":"ok","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}"#
    } else {
        CHAT
    }
}
