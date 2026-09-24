use super::diagnostics::{
    redact_known_secret, redact_known_secret_stream_values, redact_known_secret_values,
};
use super::protocol::{
    ApiFormat, NamespaceToolMapping, ProtocolError, RequestPlan, UsageCounts,
    encode_anthropic_thinking_block, encode_chat_reasoning, responses_id,
    rewrite_existing_visible_model, unix_seconds,
};
use super::wire::WireNormalization;
use bytes::{Bytes, BytesMut};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_PENDING_SSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DEFERRED_SSE_FRAMES: usize = 1024;

pub(crate) struct StreamConverter {
    source: ApiFormat,
    target: ApiFormat,
    model: String,
    client_model: String,
    custom_tools: BTreeSet<String>,
    namespace_tools: BTreeMap<String, NamespaceToolMapping>,
    response_parallel_tool_calls: bool,
    response_tool_choice: Value,
    response_tools: Vec<Value>,
    pending: BytesMut,
    input: InputState,
    output: OutputState,
    secret_redactor: StreamSecretRedactor,
    passthrough_tainted: bool,
    deferred_passthrough: Vec<DeferredPassthroughFrame>,
    deferred_passthrough_bytes: usize,
    /// Per-attempt provider wire normalization. When set, parsed upstream JSON
    /// frames are normalized before passthrough/conversion; `[DONE]` and
    /// non-JSON frames are untouched by construction (they never parse).
    wire_normalization: WireNormalization,
}

struct DeferredPassthroughFrame {
    passthrough: Bytes,
    converted: Vec<Bytes>,
}

#[derive(Default)]
struct InputState {
    started: bool,
    terminal: bool,
    message_delta_seen: bool,
    next_block: usize,
    active: BTreeMap<usize, BlockKind>,
    text_block: Option<usize>,
    reasoning_block: Option<usize>,
    chat_tools: BTreeMap<u64, ChatTool>,
    response_tools: BTreeMap<u64, ResponseTool>,
    response_parts: BTreeMap<(u64, u64, bool), usize>,
    response_delta_seen: BTreeSet<(u64, u64, bool)>,
    anthropic_reasoning: BTreeMap<usize, Value>,
    pending_stop: Option<String>,
    usage: Usage,
    saw_tool: bool,
}

#[derive(Default)]
struct OutputState {
    terminal: bool,
    id: String,
    model: String,
    created_at: u64,
    usage: Usage,
    stop_reason: Option<String>,
    finish_emitted: bool,
    next_tool_index: u64,
    next_output_index: u64,
    sequence: u64,
    blocks: BTreeMap<usize, OutputBlock>,
}

#[derive(Clone, Default)]
struct Usage {
    seen: bool,
    input: u64,
    output: u64,
    cached: u64,
    cache_creation: u64,
}

#[derive(Clone)]
enum BlockKind {
    Text,
    Reasoning,
    Tool { id: String, name: String },
}

#[derive(Default)]
struct ChatTool {
    block: Option<usize>,
    id: String,
    name: String,
    pending_arguments: String,
    started: bool,
}

#[derive(Default)]
struct ResponseTool {
    block: Option<usize>,
    arguments_seen: bool,
    arguments_closed: bool,
    custom: bool,
}

struct OutputBlock {
    kind: BlockKind,
    content: String,
    tool_index: Option<u64>,
    output_index: Option<u64>,
    custom_input_emitted: usize,
    closed: bool,
}

enum PivotEvent {
    Start {
        id: String,
        model: String,
        usage: Usage,
    },
    BlockStart {
        index: usize,
        kind: BlockKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ReasoningDelta {
        index: usize,
        text: String,
    },
    SignatureDelta {
        index: usize,
        signature: String,
    },
    ArgumentsDelta {
        index: usize,
        arguments: String,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: String,
        usage: Usage,
    },
    Stop,
    Error {
        kind: String,
        message: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SecretChannelKind {
    Text,
    Reasoning,
    Signature,
    Arguments,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SecretChannel {
    kind: SecretChannelKind,
    index: usize,
}

#[derive(Default)]
struct StreamSecretRedactor {
    secret: Option<String>,
    pending: BTreeMap<SecretChannel, PendingSecret>,
    argument_states: BTreeMap<usize, JsonArgumentRedactor>,
    deferred_boundary: Option<DeferredBoundary>,
}

struct DeferredBoundary {
    index: usize,
    pending: Vec<(SecretChannelKind, PendingSecret)>,
}

struct PendingSecret {
    text: String,
    reasoning_origins: BTreeSet<usize>,
}

#[derive(Clone, Copy)]
enum JsonArgumentContext {
    Object { expecting_key: bool },
    Array,
}

#[derive(Default)]
struct JsonArgumentRedactor {
    contexts: Vec<JsonArgumentContext>,
    in_string: bool,
    string_is_value: bool,
    escaped: bool,
    escape_buffer: String,
    high_surrogate: Option<(u16, String)>,
    pending_value: VecDeque<JsonStringToken>,
}

struct JsonStringToken {
    decoded: char,
    raw: String,
}

impl JsonArgumentRedactor {
    fn process(&mut self, input: &str, secret: &str) -> (String, bool, bool) {
        let had_pending_value = !self.pending_value.is_empty()
            || !self.escape_buffer.is_empty()
            || self.high_surrogate.is_some();
        let mut output = String::with_capacity(input.len());
        let mut matched = false;
        for character in input.chars() {
            if self.in_string {
                if self.string_is_value {
                    if self.escape_buffer.is_empty() && character == '"' {
                        output.push_str(&self.release_pending_value());
                        self.high_surrogate = None;
                        output.push(character);
                        self.in_string = false;
                        self.string_is_value = false;
                    } else {
                        matched |= self.push_value_character(character, secret, &mut output);
                    }
                    continue;
                }

                if !self.escaped && character == '"' {
                    output.push(character);
                    self.in_string = false;
                    self.string_is_value = false;
                    continue;
                }

                output.push(character);
                if self.escaped {
                    self.escaped = false;
                } else if character == '\\' {
                    self.escaped = true;
                }
                continue;
            }

            output.push(character);
            match character {
                '{' => self.contexts.push(JsonArgumentContext::Object {
                    expecting_key: true,
                }),
                '[' => self.contexts.push(JsonArgumentContext::Array),
                '}' | ']' => {
                    self.contexts.pop();
                }
                ',' => {
                    if let Some(JsonArgumentContext::Object { expecting_key }) =
                        self.contexts.last_mut()
                    {
                        *expecting_key = true;
                    }
                }
                ':' => {
                    if let Some(JsonArgumentContext::Object { expecting_key }) =
                        self.contexts.last_mut()
                    {
                        *expecting_key = false;
                    }
                }
                '"' => {
                    let is_key = matches!(
                        self.contexts.last(),
                        Some(JsonArgumentContext::Object {
                            expecting_key: true
                        })
                    );
                    self.in_string = true;
                    self.string_is_value = !is_key;
                    self.escaped = false;
                    if is_key
                        && let Some(JsonArgumentContext::Object { expecting_key }) =
                            self.contexts.last_mut()
                    {
                        *expecting_key = false;
                    }
                }
                _ => {}
            }
        }
        let changed = had_pending_value || output != input;
        (output, changed, matched)
    }

    fn push_value_character(&mut self, character: char, secret: &str, output: &mut String) -> bool {
        if self.escape_buffer.is_empty() {
            if character == '\\' {
                self.escape_buffer.push(character);
                return false;
            }
            // A high surrogate not followed by another escape cannot form valid
            // JSON. Drop it rather than forwarding an ambiguous secret spelling.
            self.high_surrogate = None;
            return self.push_value_token(
                JsonStringToken {
                    decoded: character,
                    raw: character.to_string(),
                },
                secret,
                output,
            );
        }

        self.escape_buffer.push(character);
        if self.escape_buffer.len() == 2 && character != 'u' {
            let raw = std::mem::take(&mut self.escape_buffer);
            let decoded = match character {
                '"' => Some('"'),
                '\\' => Some('\\'),
                '/' => Some('/'),
                'b' => Some('\u{0008}'),
                'f' => Some('\u{000c}'),
                'n' => Some('\n'),
                'r' => Some('\r'),
                't' => Some('\t'),
                _ => None,
            };
            self.high_surrogate = None;
            if let Some(decoded) = decoded {
                return self.push_value_token(JsonStringToken { decoded, raw }, secret, output);
            }
            return false;
        }
        if !self.escape_buffer.starts_with("\\u") || self.escape_buffer.len() < 6 {
            return false;
        }

        let raw = std::mem::take(&mut self.escape_buffer);
        let Ok(unit) = u16::from_str_radix(&raw[2..], 16) else {
            self.high_surrogate = None;
            return false;
        };
        if (0xD800..=0xDBFF).contains(&unit) {
            self.high_surrogate = Some((unit, raw));
            return false;
        }
        if (0xDC00..=0xDFFF).contains(&unit) {
            let Some((high, high_raw)) = self.high_surrogate.take() else {
                return false;
            };
            let scalar = 0x10000 + (((high as u32 - 0xD800) << 10) | (unit as u32 - 0xDC00));
            if let Some(decoded) = char::from_u32(scalar) {
                return self.push_value_token(
                    JsonStringToken {
                        decoded,
                        raw: format!("{high_raw}{raw}"),
                    },
                    secret,
                    output,
                );
            }
            return false;
        }

        self.high_surrogate = None;
        if let Some(decoded) = char::from_u32(unit as u32) {
            return self.push_value_token(JsonStringToken { decoded, raw }, secret, output);
        }
        false
    }

    fn push_value_token(
        &mut self,
        token: JsonStringToken,
        secret: &str,
        output: &mut String,
    ) -> bool {
        self.pending_value.push_back(token);
        let secret = secret.chars().collect::<Vec<_>>();
        if self.pending_value.len() == secret.len()
            && self
                .pending_value
                .iter()
                .map(|token| token.decoded)
                .eq(secret.iter().copied())
        {
            self.pending_value.clear();
            return true;
        }
        while !self.pending_value.is_empty()
            && !self
                .pending_value
                .iter()
                .map(|token| token.decoded)
                .eq(secret.iter().copied().take(self.pending_value.len()))
        {
            if let Some(token) = self.pending_value.pop_front() {
                output.push_str(&token.raw);
            }
        }
        false
    }

    fn has_pending(&self) -> bool {
        !self.pending_value.is_empty()
            || !self.escape_buffer.is_empty()
            || self.high_surrogate.is_some()
    }

    fn release_pending_value(&mut self) -> String {
        self.pending_value
            .drain(..)
            .map(|token| token.raw)
            .collect()
    }

    fn finish(mut self, _secret: &str) -> String {
        // Incomplete escapes/surrogate pairs are invalid JSON and are discarded
        // fail-closed. A decoded proper prefix alone is safe at this boundary.
        self.escape_buffer.clear();
        self.high_surrogate = None;
        self.release_pending_value()
    }
}

impl StreamSecretRedactor {
    fn new(secret: Option<&str>) -> Self {
        Self {
            secret: secret
                .filter(|secret| !secret.is_empty())
                .map(str::to_string),
            pending: BTreeMap::new(),
            argument_states: BTreeMap::new(),
            deferred_boundary: None,
        }
    }

    fn redact_events(&mut self, events: Vec<PivotEvent>) -> RedactedEvents {
        let Some(secret) = self.secret.clone() else {
            return RedactedEvents {
                events,
                matched: false,
                reasoning_indexes: BTreeSet::new(),
            };
        };
        let mut output = Vec::new();
        let mut matched = false;
        let mut reasoning_indexes = BTreeSet::new();
        for event in events {
            match event {
                PivotEvent::Start { id, model, usage } => {
                    output.extend(self.flush_deferred_boundary());
                    output.push(PivotEvent::Start { id, model, usage });
                }
                PivotEvent::BlockStart { index, kind } => {
                    output.extend(self.start_after_deferred_boundary(index, &kind));
                    output.push(PivotEvent::BlockStart { index, kind });
                }
                PivotEvent::TextDelta { index, text } => {
                    let (text, _, delta_matched, _) = self.redact_delta(
                        SecretChannel {
                            kind: SecretChannelKind::Text,
                            index,
                        },
                        &text,
                        &secret,
                    );
                    matched |= delta_matched;
                    if !text.is_empty() {
                        output.push(PivotEvent::TextDelta { index, text });
                    }
                }
                PivotEvent::ReasoningDelta { index, text } => {
                    let (text, _, delta_matched, matched_indexes) = self.redact_delta(
                        SecretChannel {
                            kind: SecretChannelKind::Reasoning,
                            index,
                        },
                        &text,
                        &secret,
                    );
                    matched |= delta_matched;
                    reasoning_indexes.extend(matched_indexes);
                    if !text.is_empty() {
                        output.push(PivotEvent::ReasoningDelta { index, text });
                    }
                }
                PivotEvent::SignatureDelta { index, signature } => {
                    let (signature, _, delta_matched, matched_indexes) = self.redact_delta(
                        SecretChannel {
                            kind: SecretChannelKind::Signature,
                            index,
                        },
                        &signature,
                        &secret,
                    );
                    matched |= delta_matched;
                    reasoning_indexes.extend(matched_indexes);
                    if !signature.is_empty() {
                        output.push(PivotEvent::SignatureDelta { index, signature });
                    }
                }
                PivotEvent::ArgumentsDelta { index, arguments } => {
                    let (arguments, _, delta_matched, _) = self.redact_delta(
                        SecretChannel {
                            kind: SecretChannelKind::Arguments,
                            index,
                        },
                        &arguments,
                        &secret,
                    );
                    matched |= delta_matched;
                    if !arguments.is_empty() {
                        output.push(PivotEvent::ArgumentsDelta { index, arguments });
                    }
                }
                PivotEvent::BlockStop { index } => {
                    output.extend(self.flush_deferred_boundary());
                    output.extend(self.flush_argument_index(index));
                    let channels = self
                        .pending
                        .keys()
                        .copied()
                        .filter(|channel| channel.index == index)
                        .collect::<Vec<_>>();
                    let pending = channels
                        .into_iter()
                        .filter_map(|channel| {
                            self.pending
                                .remove(&channel)
                                .map(|value| (channel.kind, value))
                        })
                        .collect::<Vec<_>>();
                    if pending.is_empty() {
                        output.push(PivotEvent::BlockStop { index });
                    } else {
                        self.deferred_boundary = Some(DeferredBoundary { index, pending });
                    }
                }
                PivotEvent::MessageDelta { stop_reason, usage } => {
                    output.extend(self.flush_deferred_boundary());
                    output.push(PivotEvent::MessageDelta { stop_reason, usage });
                }
                PivotEvent::Stop => {
                    output.extend(self.flush_all());
                    output.push(PivotEvent::Stop);
                }
                PivotEvent::Error { kind, message } => {
                    output.extend(self.flush_all());
                    let (message, message_changed) = redact_stream_value(&message, &secret);
                    matched |= message_changed;
                    output.push(PivotEvent::Error { kind, message });
                }
            }
        }
        RedactedEvents {
            events: output,
            matched,
            reasoning_indexes,
        }
    }

    fn redact_delta(
        &mut self,
        channel: SecretChannel,
        input: &str,
        secret: &str,
    ) -> (String, bool, bool, BTreeSet<usize>) {
        if channel.kind == SecretChannelKind::Arguments {
            let (output, changed, matched) = self
                .argument_states
                .entry(channel.index)
                .or_default()
                .process(input, secret);
            return (output, changed, matched, BTreeSet::new());
        }
        let prior = self.pending.remove(&channel);
        let had_pending = prior.is_some();
        let mut reasoning_origins = prior
            .as_ref()
            .map(|pending| pending.reasoning_origins.clone())
            .unwrap_or_default();
        if matches!(
            channel.kind,
            SecretChannelKind::Reasoning | SecretChannelKind::Signature
        ) {
            reasoning_origins.insert(channel.index);
        }
        let mut combined = prior.map(|pending| pending.text).unwrap_or_default();
        combined.push_str(input);
        let contained_secret = combined.contains(secret);
        let matched_indexes = if contained_secret {
            reasoning_origins.clone()
        } else {
            BTreeSet::new()
        };
        while combined.contains(secret) {
            combined = combined.replace(secret, "");
        }
        let keep = longest_secret_prefix_suffix(&combined, secret);
        let emitted_len = combined.len().saturating_sub(keep);
        let emitted = combined[..emitted_len].to_string();
        if keep > 0 {
            self.pending.insert(
                channel,
                PendingSecret {
                    text: combined[emitted_len..].to_string(),
                    reasoning_origins,
                },
            );
        }
        (
            emitted,
            had_pending || contained_secret || keep > 0,
            contained_secret,
            matched_indexes,
        )
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
            || self.deferred_boundary.is_some()
            || self
                .argument_states
                .values()
                .any(JsonArgumentRedactor::has_pending)
    }

    fn flush_argument_index(&mut self, index: usize) -> Vec<PivotEvent> {
        let mut events = Vec::new();
        if let Some(state) = self.argument_states.remove(&index) {
            let value = state.finish(self.secret.as_deref().unwrap_or(""));
            if !value.is_empty() {
                events.push(PivotEvent::ArgumentsDelta {
                    index,
                    arguments: value,
                });
            }
        }
        events
    }

    fn start_after_deferred_boundary(
        &mut self,
        index: usize,
        block_kind: &BlockKind,
    ) -> Vec<PivotEvent> {
        let Some(boundary) = self.deferred_boundary.take() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for (kind, mut pending) in boundary.pending {
            let compatible = matches!(
                (kind, block_kind),
                (SecretChannelKind::Text, BlockKind::Text)
                    | (SecretChannelKind::Reasoning, BlockKind::Reasoning)
            );
            if compatible {
                if matches!(
                    kind,
                    SecretChannelKind::Reasoning | SecretChannelKind::Signature
                ) {
                    pending.reasoning_origins.insert(index);
                }
                self.pending.insert(SecretChannel { kind, index }, pending);
            } else {
                events.push(channel_event(kind, boundary.index, pending.text));
            }
        }
        events.push(PivotEvent::BlockStop {
            index: boundary.index,
        });
        events
    }

    fn flush_deferred_boundary(&mut self) -> Vec<PivotEvent> {
        let Some(boundary) = self.deferred_boundary.take() else {
            return Vec::new();
        };
        let mut events = boundary
            .pending
            .into_iter()
            .map(|(kind, pending)| channel_event(kind, boundary.index, pending.text))
            .collect::<Vec<_>>();
        events.push(PivotEvent::BlockStop {
            index: boundary.index,
        });
        events
    }

    fn flush_all(&mut self) -> Vec<PivotEvent> {
        let mut events = self.flush_deferred_boundary();
        let pending = std::mem::take(&mut self.pending);
        events.extend(
            pending
                .into_iter()
                .map(|(channel, pending)| channel_event(channel.kind, channel.index, pending.text)),
        );
        let arguments = std::mem::take(&mut self.argument_states);
        let secret = self.secret.as_deref().unwrap_or("");
        events.extend(arguments.into_iter().filter_map(|(index, state)| {
            let value = state.finish(secret);
            (!value.is_empty()).then_some(PivotEvent::ArgumentsDelta {
                index,
                arguments: value,
            })
        }));
        events
    }
}

fn channel_event(kind: SecretChannelKind, index: usize, value: String) -> PivotEvent {
    match kind {
        SecretChannelKind::Text => PivotEvent::TextDelta { index, text: value },
        SecretChannelKind::Reasoning => PivotEvent::ReasoningDelta { index, text: value },
        SecretChannelKind::Signature => PivotEvent::SignatureDelta {
            index,
            signature: value,
        },
        SecretChannelKind::Arguments => PivotEvent::ArgumentsDelta {
            index,
            arguments: value,
        },
    }
}

#[cfg(test)]
fn json_string_contents(value: &str) -> String {
    serde_json::to_string(value)
        .ok()
        .and_then(|encoded| {
            encoded
                .strip_prefix('"')
                .and_then(|encoded| encoded.strip_suffix('"'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| value.to_string())
}

struct RedactedEvents {
    events: Vec<PivotEvent>,
    matched: bool,
    reasoning_indexes: BTreeSet<usize>,
}

fn redact_stream_value(value: &str, secret: &str) -> (String, bool) {
    if secret.is_empty() || !value.contains(secret) {
        return (value.to_string(), false);
    }
    let mut redacted = value.to_string();
    while redacted.contains(secret) {
        redacted = redacted.replace(secret, "");
    }
    (redacted, true)
}

fn longest_secret_prefix_suffix(value: &str, secret: &str) -> usize {
    secret
        .char_indices()
        .map(|(index, _)| index)
        .filter(|index| *index > 0)
        .rev()
        .find(|index| value.ends_with(&secret[..*index]))
        .unwrap_or(0)
}

impl StreamConverter {
    #[cfg(test)]
    pub(crate) fn new(plan: &RequestPlan) -> Self {
        Self::new_with_known_secret_and_normalization(plan, None, WireNormalization::None)
    }

    #[cfg(test)]
    pub(crate) fn new_with_known_secret(plan: &RequestPlan, known_secret: Option<&str>) -> Self {
        Self::new_with_known_secret_and_normalization(plan, known_secret, WireNormalization::None)
    }

    fn visible_model(&self) -> &str {
        if self.client_model.is_empty() {
            &self.model
        } else {
            &self.client_model
        }
    }

    /// Build with a per-attempt wire normalization marker (the forwarder
    /// derives it from the attempt that actually served the request, so mixed
    /// candidate chains never leak another family's rewrite).
    pub(crate) fn new_with_known_secret_and_normalization(
        plan: &RequestPlan,
        known_secret: Option<&str>,
        wire_normalization: WireNormalization,
    ) -> Self {
        Self {
            source: plan.upstream,
            target: plan.client,
            model: plan.model.clone(),
            client_model: plan.response_model().to_string(),
            custom_tools: plan.custom_tools.iter().cloned().collect(),
            namespace_tools: plan
                .namespace_tools
                .iter()
                .cloned()
                .map(|mapping| (mapping.flattened.clone(), mapping))
                .collect(),
            response_parallel_tool_calls: plan.response_parallel_tool_calls,
            response_tool_choice: plan.response_tool_choice.clone(),
            response_tools: plan.response_tools.clone(),
            pending: BytesMut::new(),
            input: InputState::default(),
            output: OutputState::default(),
            secret_redactor: StreamSecretRedactor::new(known_secret),
            passthrough_tainted: false,
            deferred_passthrough: Vec::new(),
            deferred_passthrough_bytes: 0,
            wire_normalization,
        }
    }

    pub(crate) fn process_chunk(&mut self, chunk: Bytes) -> Result<Vec<Bytes>, ProtocolError> {
        if self.source == self.target {
            if self.is_terminal() {
                return Ok(Vec::new());
            }
            if self.pending.len() + chunk.len() > MAX_PENDING_SSE_BYTES {
                self.pending.clear();
                return Err(ProtocolError::new("SSE event exceeds 8 MiB"));
            }
            self.pending.extend_from_slice(&chunk);
            let frames = drain_frames(&mut self.pending);
            let mut output = Vec::new();
            for frame in frames {
                if self.input.terminal {
                    break;
                }
                let (passthrough, converted, secret_matched) = self.same_protocol_frame(frame)?;
                self.emit_same_protocol_frame(passthrough, converted, secret_matched, &mut output);
            }
            return Ok(output);
        }
        if self.pending.len() + chunk.len() > MAX_PENDING_SSE_BYTES {
            self.pending.clear();
            return Err(ProtocolError::new("SSE event exceeds 8 MiB"));
        }
        self.pending.extend_from_slice(&chunk);
        let frames = drain_frames(&mut self.pending);
        self.convert_frames(frames).map(|(output, _)| output)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<Bytes>, ProtocolError> {
        if self.source == self.target {
            let mut frames = drain_frames(&mut self.pending);
            if !self.pending.is_empty() {
                frames.push(self.pending.split().freeze());
            }
            let mut output = Vec::new();
            for frame in frames {
                if self.input.terminal {
                    break;
                }
                let (passthrough, converted, secret_matched) = self.same_protocol_frame(frame)?;
                self.emit_same_protocol_frame(passthrough, converted, secret_matched, &mut output);
            }
            if self.input.terminal {
                return Ok(output);
            }
            let can_finish = matches!(self.source, ApiFormat::ChatCompletions)
                && self.input.pending_stop.is_some()
                || matches!(self.source, ApiFormat::Messages) && self.input.message_delta_seen;
            if !can_finish {
                return Err(ProtocolError::new(
                    "upstream SSE ended before a terminal event",
                ));
            }
            if self.passthrough_tainted {
                let events = self.finish_input();
                let (chunks, _) = self.encode_redacted(events)?;
                output.extend(chunks);
            } else {
                output.extend(self.release_deferred_passthrough(false));
                self.input.terminal = true;
                self.output.terminal = true;
                output.push(match self.source {
                    ApiFormat::ChatCompletions => done_frame(),
                    ApiFormat::Messages => {
                        sse_json(Some("message_stop"), &json!({"type":"message_stop"}))
                    }
                    ApiFormat::Responses | ApiFormat::Gemini => unreachable!(),
                });
            }
            return Ok(output);
        }

        let mut frames = drain_frames(&mut self.pending);
        if !self.pending.is_empty() {
            frames.push(self.pending.split().freeze());
        }
        let (mut output, _) = self.convert_frames(frames)?;
        if !self.input.terminal {
            if self.input.pending_stop.is_none() && !self.input.message_delta_seen {
                return Err(ProtocolError::new(
                    "upstream SSE ended before a terminal event",
                ));
            }
            let events = self.finish_input();
            output.extend(self.encode_redacted(events)?.0);
        }
        Ok(output)
    }

    #[cfg(test)]
    pub(crate) fn error_event(&self, message: &str) -> Vec<Bytes> {
        if self.is_terminal() {
            return Vec::new();
        }
        let frames = match self.target {
            ApiFormat::Messages => vec![sse_json(
                Some("error"),
                &json!({"type":"error","error":{"type":"api_error","message":message}}),
            )],
            ApiFormat::ChatCompletions => vec![
                sse_json(
                    None,
                    &json!({"error":{"type":"server_error","message":message}}),
                ),
                done_frame(),
            ],
            ApiFormat::Responses => vec![sse_json(
                Some("response.failed"),
                &json!({
                    "type":"response.failed",
                    "sequence_number":self.output.sequence,
                    "response":self.failed_response_object("server_error", message)
                }),
            )],
            ApiFormat::Gemini => vec![sse_json(
                None,
                &json!({
                    "error":{"code":500,"message":message,"status":"INTERNAL"}
                }),
            )],
        };
        self.sanitize_generated_frames(frames)
    }

    pub(crate) fn outcome_unknown_event(&self, message: &str) -> Vec<Bytes> {
        if self.is_terminal() {
            return Vec::new();
        }
        let frames = match self.target {
            ApiFormat::Messages => vec![sse_json(
                Some("error"),
                &json!({"type":"error","error":{"type":"upstream_outcome_unknown","message":message}}),
            )],
            ApiFormat::ChatCompletions => vec![
                sse_json(
                    None,
                    &json!({"error":{"type":"upstream_outcome_unknown","code":"upstream_outcome_unknown","message":message}}),
                ),
                done_frame(),
            ],
            ApiFormat::Responses => vec![sse_json(
                Some("response.failed"),
                &json!({
                    "type":"response.failed",
                    "sequence_number":self.output.sequence,
                    "response":self.failed_response_object("upstream_outcome_unknown", message)
                }),
            )],
            ApiFormat::Gemini => vec![sse_json(
                None,
                &json!({
                    "error":{"code":500,"message":message,"status":"UPSTREAM_OUTCOME_UNKNOWN"}
                }),
            )],
        };
        self.sanitize_generated_frames(frames)
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.input.terminal || self.output.terminal
    }

    pub(crate) fn captured_usage(&self) -> Option<UsageCounts> {
        self.input.usage.seen.then_some(UsageCounts {
            input_tokens: self.input.usage.input,
            output_tokens: self.input.usage.output,
            cached_tokens: self.input.usage.cached,
            cache_creation_tokens: self.input.usage.cache_creation,
        })
    }

    fn emit_same_protocol_frame(
        &mut self,
        passthrough: Bytes,
        converted: Vec<Bytes>,
        secret_matched: bool,
        output: &mut Vec<Bytes>,
    ) {
        if self.passthrough_tainted {
            output.extend(converted);
            return;
        }
        if secret_matched {
            self.passthrough_tainted = true;
            output.extend(self.release_deferred_passthrough(true));
            output.extend(converted);
        } else if self.secret_redactor.has_pending() || !self.deferred_passthrough.is_empty() {
            let converted_bytes = converted.iter().map(Bytes::len).sum::<usize>();
            let frame_bytes = passthrough.len().saturating_add(converted_bytes);
            if self.deferred_passthrough.len() >= MAX_DEFERRED_SSE_FRAMES
                || self.deferred_passthrough_bytes.saturating_add(frame_bytes)
                    > MAX_PENDING_SSE_BYTES
            {
                self.passthrough_tainted = true;
                output.extend(self.release_deferred_passthrough(true));
                output.extend(converted);
                return;
            }
            self.deferred_passthrough_bytes += frame_bytes;
            self.deferred_passthrough.push(DeferredPassthroughFrame {
                passthrough,
                converted,
            });
            if !self.secret_redactor.has_pending() {
                output.extend(self.release_deferred_passthrough(false));
            }
        } else {
            output.push(passthrough);
        }
    }

    fn release_deferred_passthrough(&mut self, converted: bool) -> Vec<Bytes> {
        self.deferred_passthrough_bytes = 0;
        std::mem::take(&mut self.deferred_passthrough)
            .into_iter()
            .flat_map(|frame| {
                if converted {
                    frame.converted
                } else {
                    vec![frame.passthrough]
                }
            })
            .collect()
    }

    /// Same-protocol passthrough for a single SSE frame. The frame is parsed
    /// exactly once here: the sanitized passthrough copy and the converted
    /// fallback share the same `Value`, and untouched frames are emitted
    /// byte-for-byte without copying. A wire-normalization marker rewrites
    /// only the parsed JSON (data field); event lines and `[DONE]` stay
    /// byte-identical.
    fn same_protocol_frame(
        &mut self,
        frame: Bytes,
    ) -> Result<(Bytes, Vec<Bytes>, bool), ProtocolError> {
        let secret = self.secret_redactor.secret.as_deref();
        let Some((event_name, payload)) = parse_sse_frame(&frame)? else {
            // No data lines: nothing to convert or redact as JSON; passthrough.
            let passthrough = sanitize_passthrough_sse_frame(
                self.source,
                frame,
                &self.model,
                &self.client_model,
                secret,
                None,
                self.wire_normalization,
            );
            return Ok((passthrough, Vec::new(), false));
        };
        let payload = payload.trim();
        let mut value = parse_sse_payload(payload)?;
        // Sanitize reads the raw parsed payload so its internal diff sees the
        // normalization rewrite and replaces the data field; the conversion
        // path below then consumes the normalized value.
        let passthrough = sanitize_passthrough_sse_frame(
            self.source,
            frame,
            &self.model,
            &self.client_model,
            secret,
            value.as_ref(),
            self.wire_normalization,
        );
        if let Some(parsed) = value.as_mut() {
            self.wire_normalization.normalize_response_value(parsed);
        }
        let (converted, secret_matched) = self.convert_parsed_frame(event_name, payload, value)?;
        Ok((passthrough, converted, secret_matched))
    }

    fn convert_frames(&mut self, frames: Vec<Bytes>) -> Result<(Vec<Bytes>, bool), ProtocolError> {
        let mut output = Vec::new();
        let mut secret_changed = false;
        for frame in frames {
            if self.input.terminal {
                break;
            }
            let Some((event_name, payload)) = parse_sse_frame(&frame)? else {
                continue;
            };
            let payload = payload.trim();
            let mut value = parse_sse_payload(payload)?;
            if let Some(parsed) = value.as_mut() {
                self.wire_normalization.normalize_response_value(parsed);
            }
            let (chunks, matched) = self.convert_parsed_frame(event_name, payload, value)?;
            output.extend(chunks);
            secret_changed |= matched;
        }
        Ok((output, secret_changed))
    }

    fn convert_parsed_frame(
        &mut self,
        event_name: Option<String>,
        payload: &str,
        value: Option<Value>,
    ) -> Result<(Vec<Bytes>, bool), ProtocolError> {
        if self.input.terminal {
            return Ok((Vec::new(), false));
        }
        let events = if payload.is_empty() {
            Vec::new()
        } else if payload == "[DONE]" {
            self.finish_input()
        } else {
            let value =
                value.ok_or_else(|| ProtocolError::new("invalid SSE JSON: missing payload"))?;
            match self.source {
                ApiFormat::Messages => self.decode_messages(value),
                ApiFormat::ChatCompletions => self.decode_chat(value),
                ApiFormat::Responses => self.decode_responses(event_name.as_deref(), value),
                ApiFormat::Gemini => {
                    return Err(ProtocolError::new("Gemini is a client-only stream format"));
                }
            }
        };
        let (chunks, matched) = self.encode_redacted(events)?;
        Ok((chunks, matched))
    }

    fn encode_redacted(
        &mut self,
        events: Vec<PivotEvent>,
    ) -> Result<(Vec<Bytes>, bool), ProtocolError> {
        let redacted = self.secret_redactor.redact_events(events);
        for index in redacted.reasoning_indexes {
            self.input.anthropic_reasoning.remove(&index);
        }
        let encoded = self.encode_all(redacted.events)?;
        let output = self.sanitize_generated_frames(encoded);
        Ok((output, redacted.matched))
    }

    fn sanitize_generated_frames(&self, frames: Vec<Bytes>) -> Vec<Bytes> {
        let Some(secret) = self.secret_redactor.secret.as_deref() else {
            return frames;
        };
        frames
            .into_iter()
            .map(|frame| {
                sanitize_passthrough_sse_frame(
                    self.target,
                    frame,
                    &self.model,
                    &self.client_model,
                    Some(secret),
                    None,
                    WireNormalization::None,
                )
            })
            .collect()
    }

    fn encode_all(&mut self, events: Vec<PivotEvent>) -> Result<Vec<Bytes>, ProtocolError> {
        let mut output = Vec::new();
        for event in events {
            output.extend(match self.target {
                ApiFormat::Messages => self.encode_messages(event),
                ApiFormat::ChatCompletions => self.encode_chat(event),
                ApiFormat::Responses => self.encode_responses(event),
                ApiFormat::Gemini => self.encode_gemini(event)?,
            });
        }
        Ok(output)
    }

    fn start_if_needed(&mut self, value: &Value) -> Vec<PivotEvent> {
        if self.input.started {
            return Vec::new();
        }
        self.input.started = true;
        let id = value
            .pointer("/message/id")
            .or_else(|| value.pointer("/response/id"))
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("msg_ocg")
            .to_string();
        let model = value
            .pointer("/message/model")
            .or_else(|| value.pointer("/response/model"))
            .or_else(|| value.get("model"))
            .and_then(Value::as_str)
            .unwrap_or(&self.model)
            .to_string();
        vec![PivotEvent::Start {
            id,
            model,
            usage: self.input.usage.clone(),
        }]
    }

    fn next_block(&mut self, kind: BlockKind) -> (usize, PivotEvent) {
        let index = self.input.next_block;
        self.input.next_block += 1;
        self.input.active.insert(index, kind.clone());
        (index, PivotEvent::BlockStart { index, kind })
    }

    fn finish_input(&mut self) -> Vec<PivotEvent> {
        if self.input.terminal {
            return Vec::new();
        }
        let mut events = Vec::new();

        let pending_tools: Vec<u64> = self
            .input
            .chat_tools
            .iter()
            .filter_map(|(index, tool)| (!tool.started).then_some(*index))
            .collect();
        for tool_index in pending_tools {
            events.extend(self.start_chat_tool(tool_index));
        }

        let indexes: Vec<usize> = self.input.active.keys().copied().collect();
        for index in indexes {
            self.input.active.remove(&index);
            events.push(PivotEvent::BlockStop { index });
        }
        if !self.input.message_delta_seen {
            self.input.message_delta_seen = true;
            events.push(PivotEvent::MessageDelta {
                stop_reason: self.input.pending_stop.clone().unwrap_or_else(|| {
                    if self.input.saw_tool {
                        "tool_use".to_string()
                    } else {
                        "end_turn".to_string()
                    }
                }),
                usage: self.input.usage.clone(),
            });
        }
        self.input.terminal = true;
        events.push(PivotEvent::Stop);
        events
    }

    fn decode_messages(&mut self, value: Value) -> Vec<PivotEvent> {
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "message_start" => {
                self.input.started = true;
                self.input
                    .usage
                    .merge(anthropic_usage(value.pointer("/message/usage")));
                vec![PivotEvent::Start {
                    id: string_at(&value, "/message/id", "msg_ocg"),
                    model: string_at(&value, "/message/model", &self.model),
                    usage: self.input.usage.clone(),
                }]
            }
            "content_block_start" => {
                let index =
                    u64_at(&value, "/index").unwrap_or(self.input.next_block as u64) as usize;
                self.input.next_block = self.input.next_block.max(index + 1);
                let block = value.pointer("/content_block").unwrap_or(&Value::Null);
                let mut initial_text = None;
                let mut initial_reasoning = None;
                let mut initial_signature = None;
                let kind = match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        initial_text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                        BlockKind::Text
                    }
                    "thinking" | "redacted_thinking" => {
                        let replay_is_safe =
                            self.secret_redactor.secret.as_deref().is_none_or(|secret| {
                                let mut redacted = block.clone();
                                redact_known_secret_values(&mut redacted, secret);
                                redacted == *block
                            });
                        if replay_is_safe {
                            self.input.anthropic_reasoning.insert(index, block.clone());
                        }
                        initial_reasoning = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                        initial_signature = block
                            .get("signature")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                        BlockKind::Reasoning
                    }
                    "tool_use" | "server_tool_use" => {
                        self.input.saw_tool = true;
                        BlockKind::Tool {
                            id: string_at(block, "/id", &format!("toolu_{index}")),
                            name: string_at(block, "/name", "tool"),
                        }
                    }
                    _ => return Vec::new(),
                };
                self.input.active.insert(index, kind.clone());
                let mut events = vec![PivotEvent::BlockStart { index, kind }];
                if let Some(text) = initial_text {
                    events.push(PivotEvent::TextDelta { index, text });
                }
                if let Some(text) = initial_reasoning {
                    events.push(PivotEvent::ReasoningDelta { index, text });
                }
                if let Some(signature) = initial_signature {
                    events.push(PivotEvent::SignatureDelta { index, signature });
                }
                events
            }
            "content_block_delta" => {
                let index = u64_at(&value, "/index").unwrap_or(0) as usize;
                let delta = value.pointer("/delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => vec![PivotEvent::TextDelta {
                        index,
                        text: string_at(delta, "/text", ""),
                    }],
                    "thinking_delta" => {
                        let text = string_at(delta, "/thinking", "");
                        append_json_string(
                            self.input.anthropic_reasoning.get_mut(&index),
                            "thinking",
                            &text,
                        );
                        vec![PivotEvent::ReasoningDelta { index, text }]
                    }
                    "signature_delta" => {
                        let signature = string_at(delta, "/signature", "");
                        append_json_string(
                            self.input.anthropic_reasoning.get_mut(&index),
                            "signature",
                            &signature,
                        );
                        vec![PivotEvent::SignatureDelta { index, signature }]
                    }
                    "input_json_delta" => vec![PivotEvent::ArgumentsDelta {
                        index,
                        arguments: string_at(delta, "/partial_json", ""),
                    }],
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let index = u64_at(&value, "/index").unwrap_or(0) as usize;
                self.input.active.remove(&index);
                vec![PivotEvent::BlockStop { index }]
            }
            "message_delta" => {
                self.input.message_delta_seen = true;
                let usage = anthropic_usage(value.get("usage"));
                self.input.usage.merge(usage);
                let stop_reason = string_at(
                    &value,
                    "/delta/stop_reason",
                    if self.input.saw_tool {
                        "tool_use"
                    } else {
                        "end_turn"
                    },
                );
                self.input.pending_stop = Some(stop_reason.clone());
                vec![PivotEvent::MessageDelta {
                    stop_reason,
                    usage: self.input.usage.clone(),
                }]
            }
            "message_stop" => self.finish_input(),
            "error" => {
                self.input.terminal = true;
                vec![PivotEvent::Error {
                    kind: string_at(&value, "/error/type", "api_error"),
                    message: string_at(&value, "/error/message", "upstream stream error"),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn decode_chat(&mut self, value: Value) -> Vec<PivotEvent> {
        if let Some(error) = value.get("error") {
            self.input.terminal = true;
            return vec![PivotEvent::Error {
                kind: error
                    .get("type")
                    .or_else(|| error.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("api_error")
                    .to_string(),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("upstream stream error")
                    .to_string(),
            }];
        }

        self.input.usage.merge(chat_usage(value.get("usage")));
        let mut events = self.start_if_needed(&value);
        let Some(choices) = value.get("choices").and_then(Value::as_array) else {
            return events;
        };
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            let reasoning = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty());
            if let Some(text) = reasoning {
                self.close_chat_text_block(&mut events);
                let index = if let Some(index) = self.input.reasoning_block {
                    index
                } else {
                    let (index, start) = self.next_block(BlockKind::Reasoning);
                    self.input.reasoning_block = Some(index);
                    events.push(start);
                    index
                };
                events.push(PivotEvent::ReasoningDelta {
                    index,
                    text: text.to_string(),
                });
            }
            for text in [delta.get("content"), delta.get("refusal")]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                self.close_chat_reasoning_block(&mut events);
                let index = if let Some(index) = self.input.text_block {
                    index
                } else {
                    let (index, start) = self.next_block(BlockKind::Text);
                    self.input.text_block = Some(index);
                    events.push(start);
                    index
                };
                events.push(PivotEvent::TextDelta {
                    index,
                    text: text.to_string(),
                });
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                if !tool_calls.is_empty() {
                    self.close_chat_non_tool_blocks(&mut events);
                }
                for call in tool_calls {
                    let tool_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let tool = self.input.chat_tools.entry(tool_index).or_default();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        tool.id = id.to_string();
                    }
                    if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                        tool.name = name.to_string();
                    }
                    let arguments = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if tool.started {
                        if !arguments.is_empty() {
                            events.push(PivotEvent::ArgumentsDelta {
                                index: tool.block.expect("started tool has block"),
                                arguments,
                            });
                        }
                    } else {
                        tool.pending_arguments.push_str(&arguments);
                        if !tool.name.is_empty() {
                            events.extend(self.start_chat_tool(tool_index));
                        }
                    }
                }
            }
            if let Some(function_call) = delta.get("function_call") {
                self.close_chat_non_tool_blocks(&mut events);
                let tool_index = 0;
                let tool = self.input.chat_tools.entry(tool_index).or_default();
                if let Some(name) = function_call.get("name").and_then(Value::as_str) {
                    tool.name = name.to_string();
                }
                let arguments = function_call
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if tool.started {
                    if !arguments.is_empty() {
                        events.push(PivotEvent::ArgumentsDelta {
                            index: tool.block.expect("started tool has block"),
                            arguments,
                        });
                    }
                } else {
                    tool.pending_arguments.push_str(&arguments);
                    if !tool.name.is_empty() {
                        events.extend(self.start_chat_tool(tool_index));
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.input.pending_stop = Some(chat_stop_to_anthropic(reason).to_string());
            }
        }
        events
    }

    fn close_chat_text_block(&mut self, events: &mut Vec<PivotEvent>) {
        if let Some(index) = self.input.text_block.take() {
            self.input.active.remove(&index);
            events.push(PivotEvent::BlockStop { index });
        }
    }

    fn close_chat_reasoning_block(&mut self, events: &mut Vec<PivotEvent>) {
        if let Some(index) = self.input.reasoning_block.take() {
            self.input.active.remove(&index);
            events.push(PivotEvent::BlockStop { index });
        }
    }

    fn close_chat_non_tool_blocks(&mut self, events: &mut Vec<PivotEvent>) {
        self.close_chat_reasoning_block(events);
        self.close_chat_text_block(events);
    }

    fn start_chat_tool(&mut self, tool_index: u64) -> Vec<PivotEvent> {
        let Some(tool) = self.input.chat_tools.get_mut(&tool_index) else {
            return Vec::new();
        };
        if tool.started {
            return Vec::new();
        }
        if tool.id.is_empty() {
            tool.id = format!("call_{tool_index}");
        }
        if tool.name.is_empty() {
            tool.name = "tool".to_string();
        }
        let kind = BlockKind::Tool {
            id: tool.id.clone(),
            name: tool.name.clone(),
        };
        let index = self.input.next_block;
        self.input.next_block += 1;
        self.input.active.insert(index, kind.clone());
        self.input.saw_tool = true;
        tool.block = Some(index);
        tool.started = true;
        let arguments = std::mem::take(&mut tool.pending_arguments);
        let mut events = vec![PivotEvent::BlockStart { index, kind }];
        if !arguments.is_empty() {
            events.push(PivotEvent::ArgumentsDelta { index, arguments });
        }
        events
    }

    fn decode_responses(&mut self, event_name: Option<&str>, value: Value) -> Vec<PivotEvent> {
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or(event_name)
            .unwrap_or("");
        if event_type == "error" || event_type == "response.failed" {
            self.input.terminal = true;
            return vec![PivotEvent::Error {
                kind: value
                    .pointer("/response/error/code")
                    .or_else(|| value.pointer("/error/code"))
                    .or_else(|| value.pointer("/error/type"))
                    .and_then(Value::as_str)
                    .unwrap_or("api_error")
                    .to_string(),
                message: value
                    .pointer("/response/error/message")
                    .or_else(|| value.pointer("/error/message"))
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream stream error")
                    .to_string(),
            }];
        }

        let mut events = self.start_if_needed(&value);
        match event_type {
            "response.created" | "response.in_progress" => {
                self.input
                    .usage
                    .merge(responses_usage(value.pointer("/response/usage")));
            }
            "response.output_item.added" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                if let Some(item) = value.get("item") {
                    let item_type = item.get("type").and_then(Value::as_str);
                    if matches!(item_type, Some("function_call" | "custom_tool_call")) {
                        events.extend(self.start_response_tool(output_index, item));
                        if let Some(tool) = self.input.response_tools.get_mut(&output_index) {
                            let custom = item_type == Some("custom_tool_call");
                            let initial = if custom {
                                item.get("input").and_then(Value::as_str)
                            } else {
                                item.get("arguments").and_then(Value::as_str)
                            }
                            .filter(|value| !value.is_empty());
                            if let (Some(index), Some(initial)) = (tool.block, initial) {
                                tool.custom = custom;
                                tool.arguments_seen = true;
                                events.push(PivotEvent::ArgumentsDelta {
                                    index,
                                    arguments: if custom {
                                        encode_custom_input_delta(initial, true)
                                    } else {
                                        initial.to_string()
                                    },
                                });
                            }
                        }
                    }
                }
            }
            "response.content_part.added" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                let content_index = u64_at(&value, "/content_index").unwrap_or(0);
                let part = value.get("part").unwrap_or(&Value::Null);
                let reasoning = matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("reasoning_text" | "summary_text")
                );
                events.extend(self.start_response_part(output_index, content_index, reasoning));
                let initial = part
                    .get("text")
                    .or_else(|| part.get("refusal"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                if let Some(text) = initial {
                    let key = (output_index, content_index, reasoning);
                    self.input.response_delta_seen.insert(key);
                    if let Some(index) = self.input.response_parts.get(&key).copied() {
                        events.push(if reasoning {
                            PivotEvent::ReasoningDelta {
                                index,
                                text: text.to_string(),
                            }
                        } else {
                            PivotEvent::TextDelta {
                                index,
                                text: text.to_string(),
                            }
                        });
                    }
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                let content_index = u64_at(&value, "/content_index").unwrap_or(0);
                events.extend(self.start_response_part(output_index, content_index, false));
                let key = (output_index, content_index, false);
                let text = string_at(&value, "/delta", "");
                if !text.is_empty() {
                    self.input.response_delta_seen.insert(key);
                    if let Some(index) = self.input.response_parts.get(&key).copied() {
                        events.push(PivotEvent::TextDelta { index, text });
                    }
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                let content_index = u64_at(&value, "/summary_index")
                    .or_else(|| u64_at(&value, "/content_index"))
                    .unwrap_or(0);
                events.extend(self.start_response_part(output_index, content_index, true));
                let key = (output_index, content_index, true);
                let text = string_at(&value, "/delta", "");
                if !text.is_empty() {
                    self.input.response_delta_seen.insert(key);
                    if let Some(index) = self.input.response_parts.get(&key).copied() {
                        events.push(PivotEvent::ReasoningDelta { index, text });
                    }
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                if !self.input.response_tools.contains_key(&output_index) {
                    events.extend(self.start_response_tool(output_index, &value));
                }
                if let Some(tool) = self.input.response_tools.get_mut(&output_index) {
                    if event_type == "response.custom_tool_call_input.delta" {
                        tool.custom = true;
                    }
                    let delta = string_at(&value, "/delta", "");
                    if !delta.is_empty()
                        && let Some(index) = tool.block
                    {
                        let first_delta = !tool.arguments_seen;
                        tool.arguments_seen = true;
                        events.push(PivotEvent::ArgumentsDelta {
                            index,
                            arguments: if tool.custom {
                                encode_custom_input_delta(&delta, first_delta)
                            } else {
                                delta
                            },
                        });
                    }
                }
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                if !self.input.response_tools.contains_key(&output_index) {
                    events.extend(self.start_response_tool(output_index, &value));
                }
                if let Some(tool) = self.input.response_tools.get_mut(&output_index) {
                    if event_type == "response.custom_tool_call_input.done" {
                        tool.custom = true;
                    }
                    if let Some(index) = tool.block {
                        if !tool.arguments_seen {
                            let arguments = if tool.custom {
                                json!({"input": string_at(&value, "/input", "")}).to_string()
                            } else {
                                string_at(&value, "/arguments", "")
                            };
                            events.push(PivotEvent::ArgumentsDelta { index, arguments });
                            tool.arguments_seen = true;
                            tool.arguments_closed = true;
                        } else if tool.custom && !tool.arguments_closed {
                            events.push(PivotEvent::ArgumentsDelta {
                                index,
                                arguments: "\"}".to_string(),
                            });
                            tool.arguments_closed = true;
                        } else {
                            tool.arguments_closed = true;
                        }
                    }
                }
            }
            "response.output_item.done" => {
                let output_index = u64_at(&value, "/output_index").unwrap_or(0);
                if let Some(item) = value.get("item") {
                    events.extend(self.complete_response_item(output_index, item));
                }
            }
            "response.completed" | "response.incomplete" => {
                self.input
                    .usage
                    .merge(responses_usage(value.pointer("/response/usage")));
                let status = value
                    .pointer("/response/status")
                    .and_then(Value::as_str)
                    .unwrap_or(if event_type == "response.incomplete" {
                        "incomplete"
                    } else {
                        "completed"
                    });
                self.input.pending_stop = Some(if status == "incomplete" {
                    match value
                        .pointer("/response/incomplete_details/reason")
                        .and_then(Value::as_str)
                    {
                        Some("content_filter") => "refusal".to_string(),
                        _ => "max_tokens".to_string(),
                    }
                } else if self.input.saw_tool {
                    "tool_use".to_string()
                } else {
                    "end_turn".to_string()
                });
                events.extend(self.finish_input());
            }
            _ => {}
        }
        events
    }

    fn start_response_part(
        &mut self,
        output_index: u64,
        content_index: u64,
        reasoning: bool,
    ) -> Vec<PivotEvent> {
        let key = (output_index, content_index, reasoning);
        if self.input.response_parts.contains_key(&key) {
            return Vec::new();
        }
        let kind = if reasoning {
            BlockKind::Reasoning
        } else {
            BlockKind::Text
        };
        let (index, start) = self.next_block(kind);
        self.input.response_parts.insert(key, index);
        vec![start]
    }

    fn start_response_tool(&mut self, output_index: u64, item: &Value) -> Vec<PivotEvent> {
        if self.input.response_tools.contains_key(&output_index) {
            return Vec::new();
        }
        let id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("call_ocg")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let custom = item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
        let (index, start) = self.next_block(BlockKind::Tool {
            id: id.clone(),
            name: name.clone(),
        });
        self.input.saw_tool = true;
        self.input.response_tools.insert(
            output_index,
            ResponseTool {
                block: Some(index),
                arguments_seen: false,
                arguments_closed: false,
                custom,
            },
        );
        vec![start]
    }

    fn complete_response_item(&mut self, output_index: u64, item: &Value) -> Vec<PivotEvent> {
        let mut events = Vec::new();
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "function_call" | "custom_tool_call" => {
                events.extend(self.start_response_tool(output_index, item));
                let mut stop_index = None;
                if let Some(tool) = self.input.response_tools.get_mut(&output_index) {
                    if !tool.arguments_seen {
                        let value = if tool.custom {
                            item.get("input").and_then(Value::as_str)
                        } else {
                            item.get("arguments").and_then(Value::as_str)
                        };
                        if let (Some(index), Some(arguments)) = (tool.block, value) {
                            events.push(PivotEvent::ArgumentsDelta {
                                index,
                                arguments: if tool.custom {
                                    json!({"input":arguments}).to_string()
                                } else {
                                    arguments.to_string()
                                },
                            });
                        }
                        tool.arguments_seen = true;
                        tool.arguments_closed = true;
                    } else if tool.custom
                        && !tool.arguments_closed
                        && let Some(index) = tool.block
                    {
                        events.push(PivotEvent::ArgumentsDelta {
                            index,
                            arguments: "\"}".to_string(),
                        });
                        tool.arguments_closed = true;
                    }
                    stop_index = tool.block;
                }
                if let Some(index) = stop_index {
                    self.input.active.remove(&index);
                    events.push(PivotEvent::BlockStop { index });
                }
            }
            "message" => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for (content_index, part) in parts.iter().enumerate() {
                        let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                        let reasoning = matches!(kind, "reasoning_text" | "summary_text");
                        let key = (output_index, content_index as u64, reasoning);
                        events.extend(self.start_response_part(
                            output_index,
                            content_index as u64,
                            reasoning,
                        ));
                        if !self.input.response_delta_seen.contains(&key)
                            && let Some(index) = self.input.response_parts.get(&key).copied()
                        {
                            let text = part
                                .get("text")
                                .or_else(|| part.get("refusal"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            events.push(if reasoning {
                                PivotEvent::ReasoningDelta { index, text }
                            } else {
                                PivotEvent::TextDelta { index, text }
                            });
                        }
                        if let Some(index) = self.input.response_parts.get(&key).copied() {
                            self.input.active.remove(&index);
                            events.push(PivotEvent::BlockStop { index });
                        }
                    }
                }
            }
            _ => {}
        }
        events
    }

    fn encode_messages(&mut self, event: PivotEvent) -> Vec<Bytes> {
        match event {
            PivotEvent::Start {
                id,
                model: _,
                usage,
            } => vec![sse_json(
                Some("message_start"),
                &json!({
                    "type":"message_start",
                    "message":{"id":anthropic_id(&id),"type":"message","role":"assistant","content":[],"model":self.visible_model(),
                    "stop_reason":null,"stop_sequence":null,"usage":anthropic_usage_json(&usage)}
                }),
            )],
            PivotEvent::BlockStart { index, kind } => {
                let content_block = match kind {
                    BlockKind::Text => json!({"type":"text","text":""}),
                    BlockKind::Reasoning => json!({"type":"thinking","thinking":"","signature":""}),
                    BlockKind::Tool { id, name } => {
                        json!({"type":"tool_use","id":id,"name":name,"input":{}})
                    }
                };
                vec![sse_json(
                    Some("content_block_start"),
                    &json!({"type":"content_block_start","index":index,"content_block":content_block}),
                )]
            }
            PivotEvent::TextDelta { index, text } => vec![sse_json(
                Some("content_block_delta"),
                &json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}),
            )],
            PivotEvent::ReasoningDelta { index, text } => vec![sse_json(
                Some("content_block_delta"),
                &json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}}),
            )],
            PivotEvent::SignatureDelta { index, signature } => vec![sse_json(
                Some("content_block_delta"),
                &json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":signature}}),
            )],
            PivotEvent::ArgumentsDelta { index, arguments } => vec![sse_json(
                Some("content_block_delta"),
                &json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":arguments}}),
            )],
            PivotEvent::BlockStop { index } => vec![sse_json(
                Some("content_block_stop"),
                &json!({"type":"content_block_stop","index":index}),
            )],
            PivotEvent::MessageDelta { stop_reason, usage } => vec![sse_json(
                Some("message_delta"),
                &json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":anthropic_usage_json(&usage)}),
            )],
            PivotEvent::Stop => vec![sse_json(
                Some("message_stop"),
                &json!({"type":"message_stop"}),
            )],
            PivotEvent::Error { kind, message } => vec![sse_json(
                Some("error"),
                &json!({"type":"error","error":{"type":kind,"message":message}}),
            )],
        }
    }

    fn encode_chat(&mut self, event: PivotEvent) -> Vec<Bytes> {
        match event {
            PivotEvent::Start {
                id,
                model: _,
                usage,
            } => {
                self.output.id = chat_id(&id);
                self.output.model = self.visible_model().to_string();
                self.output.usage.merge(usage);
                vec![self.chat_chunk(json!({"role":"assistant"}), Value::Null)]
            }
            PivotEvent::BlockStart { index, kind } => {
                let tool_index = matches!(kind, BlockKind::Tool { .. }).then(|| {
                    let value = self.output.next_tool_index;
                    self.output.next_tool_index += 1;
                    value
                });
                let mut frames = Vec::new();
                if let BlockKind::Tool { ref id, ref name } = kind {
                    frames.push(self.chat_chunk(
                        json!({"tool_calls":[{"index":tool_index.unwrap_or(0),"id":id,"type":"function","function":{"name":name,"arguments":""}}]}),
                        Value::Null,
                    ));
                }
                self.output.blocks.insert(
                    index,
                    OutputBlock {
                        kind,
                        content: String::new(),
                        tool_index,
                        output_index: None,
                        custom_input_emitted: 0,
                        closed: false,
                    },
                );
                frames
            }
            PivotEvent::TextDelta { index, text } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.content.push_str(&text);
                }
                vec![self.chat_chunk(json!({"content":text}), Value::Null)]
            }
            PivotEvent::ReasoningDelta { index, text } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.content.push_str(&text);
                }
                vec![self.chat_chunk(json!({"reasoning_content":text}), Value::Null)]
            }
            PivotEvent::SignatureDelta { .. } => Vec::new(),
            PivotEvent::ArgumentsDelta { index, arguments } => {
                let Some(block) = self.output.blocks.get_mut(&index) else {
                    return Vec::new();
                };
                block.content.push_str(&arguments);
                let tool_index = block.tool_index.unwrap_or(0);
                vec![self.chat_chunk(
                    json!({"tool_calls":[{"index":tool_index,"function":{"arguments":arguments}}]}),
                    Value::Null,
                )]
            }
            PivotEvent::BlockStop { index } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.closed = true;
                }
                Vec::new()
            }
            PivotEvent::MessageDelta { stop_reason, usage } => {
                self.output.stop_reason = Some(stop_reason);
                self.output.usage.merge(usage);
                Vec::new()
            }
            PivotEvent::Stop => {
                let mut frames = Vec::new();
                if !self.output.finish_emitted {
                    let reason = self
                        .output
                        .stop_reason
                        .clone()
                        .unwrap_or_else(|| "end_turn".to_string());
                    frames.extend(self.emit_chat_finish(&reason));
                }
                if !self.output.terminal {
                    self.output.terminal = true;
                    frames.push(done_frame());
                }
                frames
            }
            PivotEvent::Error { kind, message } => {
                self.output.terminal = true;
                vec![
                    sse_json(None, &json!({"error":{"type":kind,"message":message}})),
                    done_frame(),
                ]
            }
        }
    }

    fn chat_chunk(&self, delta: Value, finish_reason: Value) -> Bytes {
        sse_json(
            None,
            &json!({
                "id":if self.output.id.is_empty() { "chatcmpl-ocg" } else { &self.output.id },
                "object":"chat.completion.chunk","created":unix_seconds(),
                "model":self.visible_model(),
                "choices":[{"index":0,"delta":delta,"finish_reason":finish_reason}]
            }),
        )
    }

    fn emit_chat_finish(&mut self, stop_reason: &str) -> Vec<Bytes> {
        if self.output.finish_emitted {
            return Vec::new();
        }
        self.output.finish_emitted = true;
        let mut frames =
            vec![self.chat_chunk(json!({}), json!(anthropic_stop_to_chat(stop_reason)))];
        if self.output.usage.seen {
            frames.push(sse_json(
                None,
                &json!({
                    "id":self.output.id,"object":"chat.completion.chunk","created":unix_seconds(),
                    "model":self.visible_model(),"choices":[],"usage":chat_usage_json(&self.output.usage)
                }),
            ));
        }
        frames
    }

    fn encode_gemini(&mut self, event: PivotEvent) -> Result<Vec<Bytes>, ProtocolError> {
        match event {
            PivotEvent::Start {
                id,
                model: _,
                usage,
            } => {
                self.output.id = id;
                self.output.model = self.visible_model().to_string();
                self.output.usage.merge(usage);
                Ok(Vec::new())
            }
            PivotEvent::BlockStart { index, kind } => {
                self.output.blocks.insert(
                    index,
                    OutputBlock {
                        kind,
                        content: String::new(),
                        tool_index: None,
                        output_index: None,
                        custom_input_emitted: 0,
                        closed: false,
                    },
                );
                Ok(Vec::new())
            }
            PivotEvent::TextDelta { index, text } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.content.push_str(&text);
                }
                if text.is_empty() {
                    Ok(Vec::new())
                } else {
                    Ok(vec![self.gemini_chunk(
                        vec![json!({ "text": text })],
                        None,
                        false,
                    )])
                }
            }
            PivotEvent::ReasoningDelta { index, text } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.content.push_str(&text);
                }
                Ok(Vec::new())
            }
            PivotEvent::SignatureDelta { .. } => Ok(Vec::new()),
            PivotEvent::ArgumentsDelta { index, arguments } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.content.push_str(&arguments);
                }
                Ok(Vec::new())
            }
            PivotEvent::BlockStop { index } => {
                if let Some(block) = self.output.blocks.get_mut(&index) {
                    block.closed = true;
                }
                Ok(Vec::new())
            }
            PivotEvent::MessageDelta { stop_reason, usage } => {
                self.output.stop_reason = Some(stop_reason);
                self.output.usage.merge(usage);
                Ok(Vec::new())
            }
            PivotEvent::Stop => {
                if self.output.terminal {
                    return Ok(Vec::new());
                }
                let mut has_text = false;
                let mut tool_parts = Vec::new();
                for block in self.output.blocks.values() {
                    match &block.kind {
                        BlockKind::Text => has_text |= !block.content.is_empty(),
                        BlockKind::Tool { id, name } => {
                            let args = if block.content.trim().is_empty() {
                                json!({})
                            } else {
                                serde_json::from_str::<Value>(&block.content).map_err(|error| {
                                    ProtocolError::new(format!(
                                        "upstream tool arguments are invalid JSON: {error}"
                                    ))
                                })?
                            };
                            if !args.is_object() {
                                return Err(ProtocolError::new(
                                    "upstream tool arguments must be a JSON object",
                                ));
                            }
                            tool_parts.push(json!({
                                "functionCall": { "id": id, "name": name, "args": args },
                                "thoughtSignature": "skip_thought_signature_validator"
                            }));
                        }
                        BlockKind::Reasoning => {}
                    }
                }
                let stop_reason = self.output.stop_reason.as_deref().unwrap_or("end_turn");
                let finish_reason = match stop_reason {
                    "max_tokens" | "model_context_window_exceeded" => "MAX_TOKENS",
                    "refusal" => "SAFETY",
                    "end_turn" | "stop_sequence" | "tool_use" => "STOP",
                    _ => "OTHER",
                };
                if !has_text && tool_parts.is_empty() && finish_reason == "STOP" {
                    return Err(ProtocolError::new(
                        "upstream stream ended without text or a function call",
                    ));
                }
                self.output.terminal = true;
                Ok(vec![self.gemini_chunk(
                    tool_parts,
                    Some(finish_reason),
                    true,
                )])
            }
            PivotEvent::Error { message, .. } => {
                self.output.terminal = true;
                Ok(vec![sse_json(
                    None,
                    &json!({
                        "error":{"code":500,"message":message,"status":"INTERNAL"}
                    }),
                )])
            }
        }
    }

    fn gemini_chunk(
        &self,
        parts: Vec<Value>,
        finish_reason: Option<&str>,
        include_usage: bool,
    ) -> Bytes {
        let mut candidate = json!({ "index": 0 });
        if !parts.is_empty() {
            candidate["content"] = json!({ "role": "model", "parts": parts });
        }
        if let Some(reason) = finish_reason {
            candidate["finishReason"] = json!(reason);
        }
        let mut response = json!({
            "candidates": [candidate],
            "modelVersion": self.visible_model(),
            "responseId": if self.output.id.is_empty() { "ocg_response" } else { &self.output.id }
        });
        if include_usage && self.output.usage.seen {
            response["usageMetadata"] = gemini_usage_json(&self.output.usage);
        }
        sse_json(None, &response)
    }

    fn encode_responses(&mut self, event: PivotEvent) -> Vec<Bytes> {
        match event {
            PivotEvent::Start { id, model, usage } => {
                self.output.id = responses_id(&id);
                self.output.model = self.visible_model().to_string();
                let _ = model;
                self.output.created_at = unix_seconds();
                self.output.usage.merge(usage);
                let response = self.response_object("in_progress", Value::Null, Vec::new());
                vec![self.responses_event("response.created", json!({"response":response}))]
            }
            PivotEvent::BlockStart { index, kind } => {
                let output_index = self.output.next_output_index;
                self.output.next_output_index += 1;
                let mut frames = Vec::new();
                let item = match &kind {
                    BlockKind::Text => {
                        json!({"type":"message","id":format!("msg_{output_index}"),"status":"in_progress","role":"assistant","content":[]})
                    }
                    BlockKind::Reasoning => {
                        json!({"type":"reasoning","id":format!("rs_{output_index}"),"summary":[]})
                    }
                    BlockKind::Tool { id, name } => {
                        let (response_name, namespace, custom) = self.response_tool_identity(name);
                        let mut item = if custom {
                            json!({"type":"custom_tool_call","id":format!("ctc_{output_index}"),"call_id":id,"name":response_name,"input":"","status":"in_progress"})
                        } else {
                            json!({"type":"function_call","id":format!("fc_{output_index}"),"call_id":id,"name":response_name,"arguments":"","status":"in_progress"})
                        };
                        if let Some(namespace) = namespace {
                            item["namespace"] = json!(namespace);
                        }
                        item
                    }
                };
                frames.push(self.responses_event(
                    "response.output_item.added",
                    json!({"output_index":output_index,"item":item}),
                ));
                match kind {
                    BlockKind::Text => frames.push(self.responses_event(
                        "response.content_part.added",
                        json!({"item_id":format!("msg_{output_index}"),"output_index":output_index,"content_index":0,"part":{"type":"output_text","text":"","annotations":[],"logprobs":[]}}),
                    )),
                    BlockKind::Reasoning => frames.push(self.responses_event(
                        "response.reasoning_summary_part.added",
                        json!({"item_id":format!("rs_{output_index}"),"output_index":output_index,"summary_index":0,"part":{"type":"summary_text","text":""}}),
                    )),
                    BlockKind::Tool { .. } => {}
                }
                self.output.blocks.insert(
                    index,
                    OutputBlock {
                        kind,
                        content: String::new(),
                        tool_index: None,
                        output_index: Some(output_index),
                        custom_input_emitted: 0,
                        closed: false,
                    },
                );
                frames
            }
            PivotEvent::TextDelta { index, text } => {
                let Some(block) = self.output.blocks.get_mut(&index) else {
                    return Vec::new();
                };
                block.content.push_str(&text);
                let output_index = block.output_index.unwrap_or(0);
                vec![self.responses_event(
                    "response.output_text.delta",
                    json!({"item_id":format!("msg_{output_index}"),"output_index":output_index,"content_index":0,"delta":text,"logprobs":[]}),
                )]
            }
            PivotEvent::ReasoningDelta { index, text } => {
                let Some(block) = self.output.blocks.get_mut(&index) else {
                    return Vec::new();
                };
                block.content.push_str(&text);
                let output_index = block.output_index.unwrap_or(0);
                vec![self.responses_event(
                    "response.reasoning_summary_text.delta",
                    json!({"item_id":format!("rs_{output_index}"),"output_index":output_index,"summary_index":0,"delta":text}),
                )]
            }
            PivotEvent::SignatureDelta { .. } => Vec::new(),
            PivotEvent::ArgumentsDelta { index, arguments } => {
                let Some(block) = self.output.blocks.get_mut(&index) else {
                    return Vec::new();
                };
                block.content.push_str(&arguments);
                let output_index = block.output_index.unwrap_or(0);
                if matches!(
                    &block.kind,
                    BlockKind::Tool { name, .. } if self.custom_tools.contains(name)
                ) {
                    let Some(input) = custom_tool_input_prefix(&block.content) else {
                        return Vec::new();
                    };
                    let Some(delta) = input.get(block.custom_input_emitted..) else {
                        return Vec::new();
                    };
                    let delta = delta.to_string();
                    block.custom_input_emitted = input.len();
                    if delta.is_empty() {
                        return Vec::new();
                    }
                    return vec![self.responses_event(
                        "response.custom_tool_call_input.delta",
                        json!({"item_id":format!("ctc_{output_index}"),"output_index":output_index,"delta":delta}),
                    )];
                }
                vec![self.responses_event(
                    "response.function_call_arguments.delta",
                    json!({"item_id":format!("fc_{output_index}"),"output_index":output_index,"delta":arguments}),
                )]
            }
            PivotEvent::BlockStop { index } => self.close_response_block(index),
            PivotEvent::MessageDelta { stop_reason, usage } => {
                self.output.stop_reason = Some(stop_reason);
                self.output.usage.merge(usage);
                Vec::new()
            }
            PivotEvent::Stop => self.emit_response_completed(),
            PivotEvent::Error { kind, message } => {
                self.output.terminal = true;
                let response = self.failed_response_object(&kind, &message);
                vec![self.responses_event("response.failed", json!({"response":response}))]
            }
        }
    }

    fn close_response_block(&mut self, index: usize) -> Vec<Bytes> {
        let Some(block) = self.output.blocks.get_mut(&index) else {
            return Vec::new();
        };
        if block.closed {
            return Vec::new();
        }
        block.closed = true;
        let output_index = block.output_index.unwrap_or(0);
        let content = block.content.clone();
        let kind = block.kind.clone();
        match kind {
            BlockKind::Text => vec![
                self.responses_event("response.output_text.done", json!({"item_id":format!("msg_{output_index}"),"output_index":output_index,"content_index":0,"text":content,"logprobs":[]})),
                self.responses_event("response.content_part.done", json!({"item_id":format!("msg_{output_index}"),"output_index":output_index,"content_index":0,"part":{"type":"output_text","text":content,"annotations":[],"logprobs":[]}})),
                self.responses_event("response.output_item.done", json!({"output_index":output_index,"item":{"type":"message","id":format!("msg_{output_index}"),"status":"completed","role":"assistant","content":[{"type":"output_text","text":content,"annotations":[],"logprobs":[]}]}})),
            ],
            BlockKind::Reasoning => {
                let item = self.response_reasoning_item(index, output_index, &content);
                vec![
                    self.responses_event("response.reasoning_summary_text.done", json!({"item_id":format!("rs_{output_index}"),"output_index":output_index,"summary_index":0,"text":content})),
                    self.responses_event("response.reasoning_summary_part.done", json!({"item_id":format!("rs_{output_index}"),"output_index":output_index,"summary_index":0,"part":{"type":"summary_text","text":content}})),
                    self.responses_event("response.output_item.done", json!({"output_index":output_index,"item":item})),
                ]
            }
            BlockKind::Tool { id, name } => {
                let (response_name, namespace, custom) = self.response_tool_identity(&name);
                if custom {
                    let input = custom_tool_input(&content);
                    let mut item = json!({"type":"custom_tool_call","id":format!("ctc_{output_index}"),"call_id":id,"name":response_name,"input":input,"status":"completed"});
                    if let Some(namespace) = namespace {
                        item["namespace"] = json!(namespace);
                    }
                    vec![
                        self.responses_event("response.custom_tool_call_input.done", json!({"item_id":format!("ctc_{output_index}"),"output_index":output_index,"input":input})),
                        self.responses_event("response.output_item.done", json!({"output_index":output_index,"item":item})),
                    ]
                } else {
                    let mut item = json!({"type":"function_call","id":format!("fc_{output_index}"),"call_id":id,"name":response_name,"arguments":content,"status":"completed"});
                    if let Some(namespace) = namespace {
                        item["namespace"] = json!(namespace);
                    }
                    vec![
                        self.responses_event("response.function_call_arguments.done", json!({"item_id":format!("fc_{output_index}"),"output_index":output_index,"name":response_name,"arguments":content})),
                        self.responses_event("response.output_item.done", json!({"output_index":output_index,"item":item})),
                    ]
                }
            }
        }
    }

    fn emit_response_completed(&mut self) -> Vec<Bytes> {
        if self.output.terminal {
            return Vec::new();
        }
        let open: Vec<usize> = self
            .output
            .blocks
            .iter()
            .filter_map(|(index, block)| (!block.closed).then_some(*index))
            .collect();
        let mut frames = Vec::new();
        for index in open {
            frames.extend(self.close_response_block(index));
        }
        self.output.terminal = true;
        let (status, details) = match self.output.stop_reason.as_deref() {
            Some("max_tokens" | "model_context_window_exceeded") => {
                ("incomplete", json!({"reason":"max_output_tokens"}))
            }
            Some("refusal") => ("incomplete", json!({"reason":"content_filter"})),
            _ => ("completed", Value::Null),
        };
        let output = self.response_output_items();
        let response = self.response_object(status, details, output);
        frames.push(self.responses_event(
            if status == "incomplete" {
                "response.incomplete"
            } else {
                "response.completed"
            },
            json!({"response":response}),
        ));
        frames
    }

    fn response_output_items(&self) -> Vec<Value> {
        let mut blocks: Vec<(&usize, &OutputBlock)> = self.output.blocks.iter().collect();
        blocks.sort_by_key(|(_, block)| block.output_index.unwrap_or(u64::MAX));
        blocks
            .into_iter()
            .map(|(source_index, block)| {
                let output_index = block.output_index.unwrap_or(0);
                match &block.kind {
                    BlockKind::Text => json!({"type":"message","id":format!("msg_{output_index}"),"status":"completed","role":"assistant","content":[{"type":"output_text","text":block.content,"annotations":[],"logprobs":[]}]}),
                    BlockKind::Reasoning => self.response_reasoning_item(*source_index, output_index, &block.content),
                    BlockKind::Tool { id, name } => {
                        let (response_name, namespace, custom) =
                            self.response_tool_identity(name);
                        let mut item = if custom {
                            json!({"type":"custom_tool_call","id":format!("ctc_{output_index}"),"call_id":id,"name":response_name,"input":custom_tool_input(&block.content),"status":"completed"})
                        } else {
                            json!({"type":"function_call","id":format!("fc_{output_index}"),"call_id":id,"name":response_name,"arguments":block.content,"status":"completed"})
                        };
                        if let Some(namespace) = namespace {
                            item["namespace"] = json!(namespace);
                        }
                        item
                    }
                }
            })
            .collect()
    }

    fn response_tool_identity(&self, upstream_name: &str) -> (String, Option<String>, bool) {
        if let Some(mapping) = self.namespace_tools.get(upstream_name) {
            return (
                mapping.name.clone(),
                Some(mapping.namespace.clone()),
                mapping.custom,
            );
        }
        (
            upstream_name.to_string(),
            None,
            self.custom_tools.contains(upstream_name),
        )
    }

    fn response_reasoning_item(
        &self,
        source_index: usize,
        output_index: u64,
        content: &str,
    ) -> Value {
        let summary = if content.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"summary_text","text":content})]
        };
        let mut item = json!({
            "type":"reasoning",
            "id":format!("rs_{output_index}"),
            "summary":summary
        });
        let encrypted_content = self
            .input
            .anthropic_reasoning
            .get(&source_index)
            .and_then(encode_anthropic_thinking_block)
            .or_else(|| {
                (self.source == ApiFormat::ChatCompletions)
                    .then(|| encode_chat_reasoning(content))
                    .flatten()
            });
        if let Some(encrypted_content) = encrypted_content {
            item["encrypted_content"] = json!(encrypted_content);
        }
        item
    }

    fn response_object(
        &self,
        status: &str,
        incomplete_details: Value,
        output: Vec<Value>,
    ) -> Value {
        let created_at = if self.output.created_at == 0 {
            unix_seconds()
        } else {
            self.output.created_at
        };
        json!({
            "id":if self.output.id.is_empty() { "resp_ocg" } else { &self.output.id },
            "object":"response","created_at":created_at,"status":status,"background":false,
            "completed_at":if status == "completed" { json!(unix_seconds()) } else { Value::Null },"error":null,
            "incomplete_details":incomplete_details,"instructions":null,"max_output_tokens":null,
            "max_tool_calls":null,
            "model":self.visible_model(),
            "output":output,"parallel_tool_calls":self.response_parallel_tool_calls,"previous_response_id":null,
            "reasoning":{"effort":null,"summary":null},"store":false,"temperature":null,
            "text":{"format":{"type":"text"}},"tool_choice":self.response_tool_choice,"tools":self.response_tools,"top_p":null,
            "truncation":"disabled","usage":if self.output.usage.seen { responses_usage_json(&self.output.usage) } else { Value::Null },
            "user":null,"metadata":{}
        })
    }

    fn failed_response_object(&self, code: &str, message: &str) -> Value {
        let mut response = self.response_object("failed", Value::Null, Vec::new());
        response["error"] = json!({"code":code,"message":message});
        response
    }

    fn responses_event(&mut self, event_type: &str, fields: Value) -> Bytes {
        let mut object = match fields {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        object.insert("type".to_string(), json!(event_type));
        object.insert("sequence_number".to_string(), json!(self.output.sequence));
        self.output.sequence += 1;
        sse_json(Some(event_type), &Value::Object(object))
    }
}

impl Usage {
    fn merge(&mut self, other: Usage) {
        if !other.seen {
            return;
        }
        self.seen = true;
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cached = self.cached.max(other.cached);
        self.cache_creation = self.cache_creation.max(other.cache_creation);
    }
}

fn drain_frames(buffer: &mut BytesMut) -> Vec<Bytes> {
    let mut frames = Vec::new();
    while let Some((index, delimiter_len)) = find_boundary(buffer) {
        let frame = buffer.split_to(index + delimiter_len);
        frames.push(frame.freeze());
    }
    frames
}

fn find_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|i| (i, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn parse_sse_frame(frame: &[u8]) -> Result<Option<(Option<String>, String)>, ProtocolError> {
    let text = std::str::from_utf8(frame)
        .map_err(|e| ProtocolError::new(format!("invalid UTF-8 in SSE event: {e}")))?;
    let mut event = None;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        } else if line == "data" {
            data.push("");
        }
    }
    if data.is_empty() {
        Ok(None)
    } else {
        Ok(Some((event, data.join("\n"))))
    }
}

/// Parse a trimmed SSE data payload. Empty payloads and the `[DONE]` sentinel
/// have no JSON body and map to `None`; anything else must be valid JSON.
fn parse_sse_payload(payload: &str) -> Result<Option<Value>, ProtocolError> {
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str::<Value>(payload)
        .map(Some)
        .map_err(|e| ProtocolError::new(format!("invalid SSE JSON: {e}")))
}

fn sse_json(event: Option<&str>, value: &Value) -> Bytes {
    let prefix = event.map_or_else(String::new, |name| format!("event: {name}\n"));
    Bytes::from(format!("{prefix}data: {value}\n\n"))
}

fn done_frame() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

/// Sanitize model-specific bogus usage without weakening same-protocol SSE passthrough.
/// Frames that do not need a correction are returned byte-for-byte unchanged (the
/// input `Bytes` handle is reused, no copy). When a correction is needed, only the
/// data field is replaced; event IDs, retry hints, comments, and the original
/// line-ending style remain intact. `parsed` may supply the already-parsed data
/// payload so callers on the hot path parse each frame once; `None` makes this
/// function parse the frame itself (used for freshly generated frames).
fn sanitize_passthrough_sse_frame(
    format: ApiFormat,
    frame: Bytes,
    _model_hint: &str,
    client_model: &str,
    known_secret: Option<&str>,
    parsed: Option<&Value>,
    wire_normalization: WireNormalization,
) -> Bytes {
    let secret = known_secret.filter(|secret| !secret.is_empty());
    let mut output = frame;
    // Obtain the frame's JSON value, parsing lazily only when the caller did
    // not supply one (freshly generated frames take that path).
    let parsed = match parsed {
        Some(value) => Some(value.clone()),
        None => parse_sse_frame(&output)
            .ok()
            .flatten()
            .and_then(|(_, data)| serde_json::from_str::<Value>(&data).ok()),
    };
    if let Some(source_value) = parsed {
        let mut value = source_value.clone();
        wire_normalization.normalize_response_value(&mut value);
        if let Some(secret) = secret {
            redact_known_secret_stream_values(&mut value, secret);
        }
        rewrite_existing_visible_model(format, &mut value, client_model);
        if value != source_value {
            output = rewrite_sse_data(&output, &value);
        }
    }
    match secret {
        Some(secret) => redact_sse_metadata(output, secret),
        None => output,
    }
}

fn redact_sse_metadata(frame: Bytes, known_secret: &str) -> Bytes {
    let Ok(text) = std::str::from_utf8(&frame) else {
        return frame;
    };
    let mut output = String::with_capacity(text.len());
    let mut changed = false;
    for raw_line in text.split_inclusive('\n') {
        let (line, ending) = if let Some(line) = raw_line.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = raw_line.strip_suffix('\n') {
            (line, "\n")
        } else {
            (raw_line, "")
        };
        let value = line.split_once(':').and_then(|(field, value)| {
            // Data lines are parsed and redacted as JSON above. Every other
            // field is metadata, including provider extension fields.
            (field != "data").then_some((field, value))
        });
        if let Some((field, value)) = value {
            let redacted = redact_known_secret(value, known_secret);
            changed |= redacted != value;
            output.push_str(field);
            output.push(':');
            output.push_str(&redacted);
            output.push_str(ending);
        } else {
            output.push_str(raw_line);
        }
    }
    if changed { Bytes::from(output) } else { frame }
}

fn rewrite_sse_data(frame: &[u8], value: &Value) -> Bytes {
    let Ok(text) = std::str::from_utf8(frame) else {
        return Bytes::copy_from_slice(frame);
    };
    let replacement = value.to_string();
    let mut output = String::with_capacity(text.len());
    let mut replaced = false;
    for raw_line in text.split_inclusive('\n') {
        let (line, ending) = if let Some(line) = raw_line.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = raw_line.strip_suffix('\n') {
            (line, "\n")
        } else {
            (raw_line, "")
        };
        let data_prefix = if line == "data" {
            Some("data: ")
        } else if let Some(payload) = line.strip_prefix("data:") {
            Some(if payload.starts_with(' ') {
                "data: "
            } else {
                "data:"
            })
        } else {
            None
        };
        if let Some(prefix) = data_prefix {
            if !replaced {
                output.push_str(prefix);
                output.push_str(&replacement);
                output.push_str(ending);
                replaced = true;
            }
        } else {
            output.push_str(raw_line);
        }
    }
    if replaced {
        Bytes::from(output)
    } else {
        Bytes::copy_from_slice(frame)
    }
}

fn append_json_string(value: Option<&mut Value>, key: &str, suffix: &str) {
    let Some(object) = value.and_then(Value::as_object_mut) else {
        return;
    };
    let mut combined = object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    combined.push_str(suffix);
    object.insert(key.to_string(), json!(combined));
}

fn custom_tool_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| value.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| arguments.to_string())
}

fn encode_custom_input_delta(delta: &str, first: bool) -> String {
    let encoded = serde_json::to_string(delta).unwrap_or_else(|_| "\"\"".to_string());
    let inner = encoded
        .strip_prefix('"')
        .and_then(|encoded| encoded.strip_suffix('"'))
        .unwrap_or_default();
    if first {
        format!("{{\"input\":\"{inner}")
    } else {
        inner.to_string()
    }
}

fn custom_tool_input_prefix(arguments: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(arguments) {
        return value
            .get("input")
            .and_then(Value::as_str)
            .or_else(|| value.as_str())
            .map(str::to_string);
    }
    let encoded = if let Some(offset) = arguments.find("\"input\"") {
        let after_key = &arguments[offset + "\"input\"".len()..];
        after_key
            .split_once(':')?
            .1
            .trim_start()
            .strip_prefix('"')?
    } else {
        arguments.trim_start().strip_prefix('"')?
    };
    let mut escaped = false;
    let mut end = encoded.len();
    for (index, character) in encoded.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            end = index;
            break;
        }
    }
    serde_json::from_str(&format!("\"{}\"", &encoded[..end])).ok()
}

fn string_at(value: &Value, pointer: &str, fallback: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn u64_at(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn anthropic_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value.filter(|v| v.is_object()) else {
        return Usage::default();
    };
    let cached = value
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_creation = value
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        seen: true,
        input: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(cached)
            .saturating_add(cache_creation),
        output: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached,
        cache_creation,
    }
}

fn chat_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value.filter(|v| v.is_object()) else {
        return Usage::default();
    };
    Usage {
        seen: true,
        input: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation: 0,
    }
}

fn responses_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value.filter(|v| v.is_object()) else {
        return Usage::default();
    };
    Usage {
        seen: true,
        input: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached: value
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation: 0,
    }
}

fn anthropic_usage_json(usage: &Usage) -> Value {
    let uncached = usage
        .input
        .saturating_sub(usage.cached.saturating_add(usage.cache_creation));
    json!({
        "input_tokens":uncached,"output_tokens":usage.output,
        "cache_read_input_tokens":usage.cached,"cache_creation_input_tokens":usage.cache_creation
    })
}

fn chat_usage_json(usage: &Usage) -> Value {
    json!({
        "prompt_tokens":usage.input,"completion_tokens":usage.output,
        "total_tokens":usage.input + usage.output,
        "prompt_tokens_details":{"cached_tokens":usage.cached}
    })
}

fn responses_usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens":usage.input,"output_tokens":usage.output,
        "total_tokens":usage.input + usage.output,
        "input_tokens_details":{"cached_tokens":usage.cached},
        "output_tokens_details":{"reasoning_tokens":0}
    })
}

fn gemini_usage_json(usage: &Usage) -> Value {
    json!({
        "promptTokenCount": usage.input,
        "candidatesTokenCount": usage.output,
        "totalTokenCount": usage.input.saturating_add(usage.output),
        "cachedContentTokenCount": usage.cached,
        "thoughtsTokenCount": 0
    })
}

fn chat_stop_to_anthropic(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn anthropic_stop_to_chat(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

fn anthropic_id(id: &str) -> String {
    if id.starts_with("msg_") {
        id.to_string()
    } else {
        format!("msg_{id}")
    }
}

fn chat_id(id: &str) -> String {
    if id.starts_with("chatcmpl-") {
        id.to_string()
    } else {
        format!("chatcmpl-{id}")
    }
}

#[cfg(test)]
mod tests;
