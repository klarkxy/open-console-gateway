//! Minimal protocol-native upstream replies for the dynamic routing matrix.
pub const RESPONSES: &str = r#"{"id":"resp-ok","object":"response","model":"vendor/opus","status":"completed","output":[{"id":"msg-ok","type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":1,"output_tokens":1}}"#;
pub const MESSAGES: &str = r#"{"id":"msg-ok","type":"message","role":"assistant","model":"vendor/opus","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
pub const CHAT_STREAM: &str = concat!(
    "data: {\"id\":\"chat-ok\",\"object\":\"chat.completion.chunk\",\"model\":\"vendor/opus\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
    "data: [DONE]\n\n"
);
pub const MESSAGES_STREAM: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-ok\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"vendor/opus\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
);
pub const RESPONSES_STREAM: &str = concat!(
    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-ok\",\"object\":\"response\",\"model\":\"vendor/opus\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
    "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-ok\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
    "event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"item_id\":\"msg-ok\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-ok\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-ok\",\"object\":\"response\",\"model\":\"vendor/opus\",\"status\":\"completed\",\"output\":[{\"id\":\"msg-ok\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
);
