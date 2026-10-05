//! Exact native dispatch targets for one callable protocol and operation.
//!
//! The matrix is the source dictionary. A missing mode or an unapproved base
//! stays unavailable. The frozen ABI requires `native-final-endpoint-pin-v1`
//! now, as the 16th capability name. This module only recognizes that name.
//! It does not edit the artifact lock or `capabilities.json`, and it does not
//! set verified ready. The old placeholder SHA still blocks trust.
//!
//! A selected official row is validated before any fixture rewrite. Scheme,
//! host, and port may then change through `cpa_test_endpoints`. Endpoint id,
//! origin, and fingerprint use that physical URL. Effective facts stay on
//! the official row. Feature-off builds keep the official URL.

use super::types::{NativeDispatchTarget, NativeEndpointPin, NormalizedRoute, fingerprint};
use ocg_domain::connection::{
    CONNECTION_ID_NAMESPACE, ConnectionId, EndpointOperation, endpoint_id_for_route,
};
use ocg_domain::credential::normalize_origin;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) const NATIVE_ENDPOINT_PIN_CAPABILITY: &str = "native-final-endpoint-pin-v1";
pub(crate) const GEMINI_CALLABLE_PROTOCOL: &str = "chat_completions";
pub(crate) const MAX_NATIVE_TARGETS: usize = 32;

const KIND_EXECUTE: &str = "execute";
const KIND_REFRESH: &str = "refresh-resend";
const KIND_STREAM: &str = "stream";
const KIND_STREAM_REFRESH: &str = "stream-refresh";
const KIND_STREAM_BOOTSTRAP: &str = "stream-bootstrap";
const KIND_INTERNAL: &str = "internal";
const KIND_COUNT: &str = "count-tokens";

const KIND_ORDER: [&str; 7] = [
    KIND_EXECUTE,
    KIND_REFRESH,
    KIND_STREAM,
    KIND_STREAM_REFRESH,
    KIND_STREAM_BOOTSTRAP,
    KIND_INTERNAL,
    KIND_COUNT,
];

const URL_C1: &str = "https://chatgpt.com/backend-api/codex/responses";
const URL_C2: &str = "https://chatgpt.com/backend-api/codex/responses/compact";
const URL_A1: &str = "https://api.anthropic.com/v1/messages?beta=true";
const URL_A2: &str = "https://api.anthropic.com/v1/messages/count_tokens?beta=true";
const URL_KC1: &str = "https://api.kimi.com/coding/v1/chat/completions";
const URL_KC2: &str = "https://api.kimi.com/coding/v1/responses";
const URL_KC3: &str = "https://api.kimi.com/coding/v1/messages?beta=true";
const URL_KC4: &str = "https://api.kimi.com/coding/v1/messages/count_tokens?beta=true";
const URL_KA1: &str = "https://api.kimi.ai/coding/v1/chat/completions";
const URL_KA2: &str = "https://api.kimi.ai/coding/v1/responses";
const URL_KA3: &str = "https://api.kimi.ai/coding/v1/messages?beta=true";
const URL_KA4: &str = "https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true";
const URL_X1: &str = "https://cli-chat-proxy.grok.com/v1/responses";
const URL_X2: &str = "https://api.x.ai/v1/responses";
const URL_X3: &str = "https://api.x.ai/v1/responses/compact";
const URL_G1: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent";
const URL_G2: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";
const URL_G3: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens";

const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
const CLAUDE_BASE: &str = "https://api.anthropic.com";
const ANTIGRAVITY_BASE: &str = "https://daily-cloudcode-pa.googleapis.com";
const KIMI_COM_BASE: &str = "https://api.kimi.com/coding";
const KIMI_COM_V1: &str = "https://api.kimi.com/coding/v1";
const KIMI_AI_BASE: &str = "https://api.kimi.ai/coding";
const KIMI_AI_V1: &str = "https://api.kimi.ai/coding/v1";
const XAI_API_BASE: &str = "https://api.x.ai/v1";
const XAI_CLI_BASE: &str = "https://cli-chat-proxy.grok.com/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeSourceOperation {
    Execute,
    Stream,
    CountTokens,
    SourceCompact,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeAuthorityFacts {
    pub raw_label: String,
    pub provider: String,
    pub mode: String,
    pub reported_base: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeTargetOutcome {
    Network(Vec<NativeDispatchTarget>),
    LocalOnly,
    Unavailable,
}

/// Authenticated authRef facts. Omitted fields are `None`. A present empty base is `Some("")`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectiveNativeWire<'a> {
    pub raw_provider_label: &'a str,
    pub effective_subtype: Option<&'a str>,
    pub effective_mode: Option<&'a str>,
    pub effective_generation_base: Option<&'a str>,
    pub effective_auth_kind: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct DictionaryRow {
    url: &'static str,
    op_key: &'static str,
    map_key: &'static str,
}

enum RowChoice {
    One(DictionaryRow),
    LocalOnly,
    Unavailable,
}

const ROW_C1: DictionaryRow = DictionaryRow {
    url: URL_C1,
    op_key: "response_create",
    map_key: "codex",
};
const ROW_C2: DictionaryRow = DictionaryRow {
    url: URL_C2,
    op_key: "responses_compact",
    map_key: "codex",
};
const ROW_A1: DictionaryRow = DictionaryRow {
    url: URL_A1,
    op_key: "message_create",
    map_key: "anthropic",
};
const ROW_A2: DictionaryRow = DictionaryRow {
    url: URL_A2,
    op_key: "count_tokens",
    map_key: "anthropic",
};
const ROW_KC1: DictionaryRow = DictionaryRow {
    url: URL_KC1,
    op_key: "chat_create",
    map_key: "kimi.com",
};
const ROW_KC2: DictionaryRow = DictionaryRow {
    url: URL_KC2,
    op_key: "response_create",
    map_key: "kimi.com",
};
const ROW_KC3: DictionaryRow = DictionaryRow {
    url: URL_KC3,
    op_key: "message_create",
    map_key: "kimi.com",
};
const ROW_KC4: DictionaryRow = DictionaryRow {
    url: URL_KC4,
    op_key: "count_tokens",
    map_key: "kimi.com",
};
const ROW_KA1: DictionaryRow = DictionaryRow {
    url: URL_KA1,
    op_key: "chat_create",
    map_key: "kimi.ai",
};
const ROW_KA2: DictionaryRow = DictionaryRow {
    url: URL_KA2,
    op_key: "response_create",
    map_key: "kimi.ai",
};
const ROW_KA3: DictionaryRow = DictionaryRow {
    url: URL_KA3,
    op_key: "message_create",
    map_key: "kimi.ai",
};
const ROW_KA4: DictionaryRow = DictionaryRow {
    url: URL_KA4,
    op_key: "count_tokens",
    map_key: "kimi.ai",
};
const ROW_X1: DictionaryRow = DictionaryRow {
    url: URL_X1,
    op_key: "response_create",
    map_key: "xai.cli",
};
const ROW_X2: DictionaryRow = DictionaryRow {
    url: URL_X2,
    op_key: "response_create",
    map_key: "xai.api",
};
const ROW_X3_CLI: DictionaryRow = DictionaryRow {
    url: URL_X3,
    op_key: "responses_compact",
    map_key: "xai.cli",
};
const ROW_X3_API: DictionaryRow = DictionaryRow {
    url: URL_X3,
    op_key: "responses_compact",
    map_key: "xai.api",
};
const ROW_G1: DictionaryRow = DictionaryRow {
    url: URL_G1,
    op_key: "generate_content",
    map_key: "antigravity",
};
const ROW_G2: DictionaryRow = DictionaryRow {
    url: URL_G2,
    op_key: "stream_generate_content",
    map_key: "antigravity",
};
const ROW_G3: DictionaryRow = DictionaryRow {
    url: URL_G3,
    op_key: "count_tokens",
    map_key: "antigravity",
};

/// Canonical absolute URL. Default ports are omitted. Query order is kept.
pub(crate) fn canonical_native_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.fragment().is_some() || parsed.host().is_none() {
        return None;
    }
    Some(parsed.to_string())
}

pub(crate) fn normalize_native_label(raw: &str) -> NativeAuthorityFacts {
    let trimmed = raw.trim();
    let key = trimmed.to_ascii_lowercase();
    let (provider, mode) = match key.as_str() {
        "codex" => ("codex", ""),
        "anthropic" | "claude" => ("anthropic", ""),
        "antigravity" => ("antigravity", ""),
        "kimi" => ("kimi", ""),
        "kimi.com" | "kimi-com" => ("kimi", "com"),
        "kimi.ai" | "kimi-ai" => ("kimi", "ai"),
        "xai" => ("xai", ""),
        _ => ("", ""),
    };
    NativeAuthorityFacts {
        raw_label: trimmed.to_string(),
        provider: provider.to_string(),
        mode: mode.to_string(),
        reported_base: String::new(),
    }
}

/// Map the authenticated wire into the stored raw label, mode, and generation base.
///
/// Product provider `cpa` is not an input. A missing subtype or base, an unknown
/// mode, or an unsupported base clears the provider. The visible base string stays.
pub(crate) fn facts_from_effective(wire: &EffectiveNativeWire<'_>) -> NativeAuthorityFacts {
    let raw = wire.raw_provider_label.trim();
    let subtype = wire
        .effective_subtype
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let mode_text = wire.effective_mode.unwrap_or("").trim();
    let Some(subtype) = subtype else {
        return unavailable_facts(raw, visible_unproven_base(wire.effective_generation_base));
    };
    let mapped = match subtype.as_str() {
        "codex" if mode_text.is_empty() => ("codex", ""),
        "anthropic" if mode_text.is_empty() || mode_text.eq_ignore_ascii_case("claude") => {
            ("anthropic", "")
        }
        "antigravity" if mode_text.is_empty() || mode_text.eq_ignore_ascii_case("daily") => {
            ("antigravity", "")
        }
        "kimi.com" if mode_text.is_empty() || mode_text.eq_ignore_ascii_case("com") => {
            ("kimi", "com")
        }
        "kimi.ai" if mode_text.is_empty() || mode_text.eq_ignore_ascii_case("ai") => ("kimi", "ai"),
        "xai" if mode_text.eq_ignore_ascii_case("cli") => ("xai", "cli"),
        "xai" if mode_text.eq_ignore_ascii_case("api") => ("xai", "api"),
        _ => {
            return unavailable_facts(raw, visible_unproven_base(wire.effective_generation_base));
        }
    };
    if mapped.0 == "xai"
        && !wire
            .effective_auth_kind
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("oauth"))
    {
        return unavailable_facts(raw, visible_unproven_base(wire.effective_generation_base));
    }
    let reported = match resolve_generation_base(mapped.0, mapped.1, wire.effective_generation_base)
    {
        Ok(base) => base,
        Err(visible) => return unavailable_facts(raw, visible),
    };
    if raw_label_conflicts(raw, mapped.0, mapped.1) {
        return unavailable_facts(raw, reported);
    }
    NativeAuthorityFacts {
        raw_label: raw.to_string(),
        provider: mapped.0.to_string(),
        mode: mapped.1.to_string(),
        reported_base: reported,
    }
}

fn unavailable_facts(raw: &str, reported_base: String) -> NativeAuthorityFacts {
    NativeAuthorityFacts {
        raw_label: raw.to_string(),
        provider: String::new(),
        mode: String::new(),
        reported_base,
    }
}

fn visible_unproven_base(base: Option<&str>) -> String {
    let Some(raw) = base else {
        return String::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        canonical_native_url(trimmed).unwrap_or_else(|| trimmed.to_string())
    }
}

fn raw_label_conflicts(raw: &str, provider: &str, mode: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    let normalized = normalize_native_label(raw);
    if normalized.provider.is_empty() {
        return false;
    }
    normalized.provider != provider || (!normalized.mode.is_empty() && normalized.mode != mode)
}

fn resolve_generation_base(
    provider: &str,
    mode: &str,
    base: Option<&str>,
) -> Result<String, String> {
    let Some(raw) = base else {
        return Err(String::new());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(default_generation_base(provider, mode).to_string());
    }
    let Some(canonical) = canonical_native_url(trimmed) else {
        return Err(trimmed.to_string());
    };
    if provider == "xai" && mode == "cli" && same_base(&canonical, XAI_API_BASE) {
        return Ok(XAI_CLI_BASE.to_string());
    }
    if generation_prefix_ok(provider, mode, &canonical) {
        return Ok(canonical.trim_end_matches('/').to_string());
    }
    Err(canonical)
}

fn default_generation_base(provider: &str, mode: &str) -> &'static str {
    match (provider, mode) {
        ("codex", "") => CODEX_BASE,
        ("anthropic", "") => CLAUDE_BASE,
        ("antigravity", "") => ANTIGRAVITY_BASE,
        ("kimi", "com") => KIMI_COM_BASE,
        ("kimi", "ai") => KIMI_AI_BASE,
        ("xai", "cli") => XAI_CLI_BASE,
        ("xai", "api") => XAI_API_BASE,
        _ => "",
    }
}

pub(crate) fn native_targets_for(
    facts: &NativeAuthorityFacts,
    model: &str,
    callable_protocol: &str,
    operation: NativeSourceOperation,
    connection: &ConnectionId,
) -> NativeTargetOutcome {
    if model.trim().is_empty() || !callable_protocol_ok(callable_protocol) || !mode_ready(facts) {
        return NativeTargetOutcome::Unavailable;
    }
    let row = match facts.provider.as_str() {
        "codex" => codex_row(facts, operation),
        "anthropic" => anthropic_row(facts, operation),
        "kimi" => kimi_row(facts, callable_protocol, operation),
        "xai" => xai_row(facts, operation),
        "antigravity" => antigravity_row(facts, model, operation),
        _ => RowChoice::Unavailable,
    };
    match row {
        RowChoice::One(item) => {
            let Some(target) = build_target(connection, callable_protocol, &item, operation) else {
                return NativeTargetOutcome::Unavailable;
            };
            NativeTargetOutcome::Network(vec![target])
        }
        RowChoice::LocalOnly => NativeTargetOutcome::LocalOnly,
        RowChoice::Unavailable => NativeTargetOutcome::Unavailable,
    }
}

/// Execute pin plus every network target for this protocol. Empty means no route.
pub(crate) fn native_route_targets(
    facts: &NativeAuthorityFacts,
    model: &str,
    callable_protocol: &str,
    connection: &ConnectionId,
) -> Option<(NativeEndpointPin, Vec<NativeDispatchTarget>)> {
    let NativeTargetOutcome::Network(execute) = native_targets_for(
        facts,
        model,
        callable_protocol,
        NativeSourceOperation::Execute,
        connection,
    ) else {
        return None;
    };
    let primary = execute.first()?.pin.clone();
    let mut targets = Vec::new();
    for operation in [
        NativeSourceOperation::Execute,
        NativeSourceOperation::Stream,
        NativeSourceOperation::CountTokens,
        NativeSourceOperation::SourceCompact,
        NativeSourceOperation::Internal,
    ] {
        if let NativeTargetOutcome::Network(found) =
            native_targets_for(facts, model, callable_protocol, operation, connection)
        {
            for target in found {
                merge_target(&mut targets, target);
            }
        }
    }
    if targets.is_empty() || targets.len() > MAX_NATIVE_TARGETS {
        return None;
    }
    Some((primary, targets))
}

/// Exact first-bind grant slices. `None` means the caller writes nothing.
pub(crate) fn default_grant_ids(
    facts: &NativeAuthorityFacts,
    models: &[String],
    connection: &ConnectionId,
) -> Option<(Vec<String>, Vec<String>)> {
    if facts.provider.is_empty() || models.is_empty() || !mode_ready(facts) {
        return None;
    }
    let mut ids = Vec::new();
    let mut origins = Vec::new();
    for model in models {
        for protocol in ["chat_completions", "responses", "messages"] {
            for operation in [
                NativeSourceOperation::Execute,
                NativeSourceOperation::Stream,
                NativeSourceOperation::CountTokens,
                NativeSourceOperation::SourceCompact,
                NativeSourceOperation::Internal,
            ] {
                let NativeTargetOutcome::Network(found) =
                    native_targets_for(facts, model, protocol, operation, connection)
                else {
                    continue;
                };
                for target in found {
                    if !ids.iter().any(|id| id == &target.pin.endpoint_id) {
                        ids.push(target.pin.endpoint_id);
                    }
                    if !origins.iter().any(|origin| origin == &target.pin.origin) {
                        origins.push(target.pin.origin);
                    }
                }
            }
        }
    }
    if ids.is_empty() || origins.is_empty() {
        None
    } else {
        Some((ids, origins))
    }
}

/// Every distinct pin on this route for the selected protocol and kind.
///
/// `Some(empty)` is zero network pins. It does not prove a local count.
/// `None` is a refusal: unknown protocol or kind, a malformed or reused set,
/// or a vector past [`MAX_NATIVE_TARGETS`]. Matches are not truncated.
pub(crate) fn targets_for_applied_route(
    route: &NormalizedRoute,
    callable_protocol: &str,
    generation_kind: &str,
) -> Option<Vec<NativeEndpointPin>> {
    if !callable_protocol_ok(callable_protocol) || kind_rank(generation_kind) >= KIND_ORDER.len() {
        return None;
    }
    if !callable_protocol_ok(&route.protocol)
        || !accept_stored_native_targets(&route.native_targets)
    {
        return None;
    }
    if route
        .native_targets
        .iter()
        .any(|target| target.pin.protocol != route.protocol)
    {
        return None;
    }
    if route.protocol != callable_protocol {
        return Some(Vec::new());
    }
    let found = route
        .native_targets
        .iter()
        .filter(|target| {
            target
                .generation_kinds
                .iter()
                .any(|kind| kind == generation_kind)
        })
        .map(|target| target.pin.clone())
        .collect::<Vec<_>>();
    if found.len() > MAX_NATIVE_TARGETS {
        return None;
    }
    Some(found)
}

pub(crate) fn accept_stored_native_targets(targets: &[NativeDispatchTarget]) -> bool {
    if targets.len() > MAX_NATIVE_TARGETS {
        return false;
    }
    let mut seen_ids = BTreeSet::new();
    let mut seen_fingerprints = BTreeSet::new();
    for target in targets {
        if !callable_protocol_ok(&target.pin.protocol) || target.pin.http_method != "POST" {
            return false;
        }
        if target.pin.endpoint_id.is_empty() || target.pin.origin.is_empty() {
            return false;
        }
        if normalize_origin(&target.pin.origin).as_deref() != Some(target.pin.origin.as_str()) {
            return false;
        }
        if !lower_hex_64(&target.pin.endpoint_fingerprint) {
            return false;
        }
        if target.generation_kinds.is_empty() || target.generation_kinds.len() > KIND_ORDER.len() {
            return false;
        }
        let mut kinds = BTreeSet::new();
        for kind in &target.generation_kinds {
            if !KIND_ORDER.contains(&kind.as_str()) || !kinds.insert(kind.as_str()) {
                return false;
            }
        }
        if !seen_ids.insert(target.pin.endpoint_id.clone()) {
            return false;
        }
        if !seen_fingerprints.insert(target.pin.endpoint_fingerprint.clone()) {
            return false;
        }
    }
    true
}

pub(crate) fn endpoint_pin_capability_listed(capabilities: &[String]) -> bool {
    capabilities
        .iter()
        .any(|item| item == NATIVE_ENDPOINT_PIN_CAPABILITY)
}

pub(crate) fn native_targets_preimage(targets: &[NativeDispatchTarget]) -> Value {
    let mut ordered: Vec<&NativeDispatchTarget> = targets.iter().collect();
    ordered.sort_by(|left, right| pin_key(&left.pin).cmp(&pin_key(&right.pin)));
    Value::Array(
        ordered
            .into_iter()
            .map(|target| {
                let mut kinds = target.generation_kinds.clone();
                kinds.sort_by_key(|kind| kind_rank(kind));
                json!({
                    "endpoint_fingerprint": target.pin.endpoint_fingerprint,
                    "endpoint_id": target.pin.endpoint_id,
                    "generation_kinds": kinds,
                    "http_method": target.pin.http_method,
                    "origin": target.pin.origin,
                    "protocol": target.pin.protocol,
                })
            })
            .collect(),
    )
}

pub(crate) fn native_mode_ok(mode: &str) -> bool {
    matches!(mode, "" | "com" | "ai" | "cli" | "api")
}

fn kind_rank(kind: &str) -> usize {
    KIND_ORDER
        .iter()
        .position(|item| *item == kind)
        .unwrap_or(KIND_ORDER.len())
}

fn pin_key(pin: &NativeEndpointPin) -> (String, String, String, String) {
    (
        pin.protocol.clone(),
        pin.endpoint_id.clone(),
        pin.endpoint_fingerprint.clone(),
        pin.http_method.clone(),
    )
}

fn lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn callable_protocol_ok(protocol: &str) -> bool {
    matches!(protocol, "chat_completions" | "responses" | "messages")
}

fn mode_ready(facts: &NativeAuthorityFacts) -> bool {
    match facts.provider.as_str() {
        "codex" | "anthropic" | "antigravity" => facts.mode.is_empty(),
        "kimi" => matches!(facts.mode.as_str(), "com" | "ai"),
        "xai" => matches!(facts.mode.as_str(), "cli" | "api"),
        _ => false,
    }
}

fn kinds(operation: NativeSourceOperation) -> &'static [&'static str] {
    match operation {
        NativeSourceOperation::Execute => &[KIND_EXECUTE, KIND_REFRESH],
        NativeSourceOperation::Stream => &[KIND_STREAM, KIND_STREAM_REFRESH, KIND_STREAM_BOOTSTRAP],
        NativeSourceOperation::CountTokens => &[KIND_COUNT],
        NativeSourceOperation::SourceCompact | NativeSourceOperation::Internal => &[KIND_INTERNAL],
    }
}

fn generation_base_ok(facts: &NativeAuthorityFacts) -> bool {
    if !mode_ready(facts) {
        return false;
    }
    let reported = facts.reported_base.trim();
    if reported.is_empty() {
        return true;
    }
    let Some(canonical) = canonical_native_url(reported) else {
        return false;
    };
    generation_prefix_ok(&facts.provider, &facts.mode, &canonical)
}

fn generation_prefix_ok(provider: &str, mode: &str, canonical: &str) -> bool {
    match (provider, mode) {
        ("codex", "") => base_matches(canonical, &[CODEX_BASE], &[URL_C1, URL_C2]),
        ("anthropic", "") => base_matches(canonical, &[CLAUDE_BASE], &[URL_A1, URL_A2]),
        ("antigravity", "") => {
            base_matches(canonical, &[ANTIGRAVITY_BASE], &[URL_G1, URL_G2, URL_G3])
        }
        ("kimi", "com") => base_matches(
            canonical,
            &[KIMI_COM_BASE, KIMI_COM_V1],
            &[URL_KC1, URL_KC2, URL_KC3, URL_KC4],
        ),
        ("kimi", "ai") => base_matches(
            canonical,
            &[KIMI_AI_BASE, KIMI_AI_V1],
            &[URL_KA1, URL_KA2, URL_KA3, URL_KA4],
        ),
        ("xai", "cli") => base_matches(canonical, &[XAI_CLI_BASE], &[URL_X1]),
        ("xai", "api") => base_matches(canonical, &[XAI_API_BASE], &[URL_X2, URL_X3]),
        _ => false,
    }
}

fn base_matches(canonical: &str, prefixes: &[&str], exact: &[&str]) -> bool {
    let trimmed = canonical.trim_end_matches('/');
    exact
        .iter()
        .any(|item| *item == canonical || *item == trimmed)
        || prefixes
            .iter()
            .any(|prefix| trimmed == prefix.trim_end_matches('/'))
}

fn same_base(canonical: &str, expected: &str) -> bool {
    canonical.trim_end_matches('/') == expected.trim_end_matches('/')
}

fn codex_row(facts: &NativeAuthorityFacts, operation: NativeSourceOperation) -> RowChoice {
    if !generation_base_ok(facts) {
        return RowChoice::Unavailable;
    }
    match operation {
        NativeSourceOperation::Execute
        | NativeSourceOperation::Stream
        | NativeSourceOperation::Internal => RowChoice::One(ROW_C1),
        NativeSourceOperation::CountTokens => RowChoice::LocalOnly,
        NativeSourceOperation::SourceCompact => RowChoice::One(ROW_C2),
    }
}

fn anthropic_row(facts: &NativeAuthorityFacts, operation: NativeSourceOperation) -> RowChoice {
    if !generation_base_ok(facts) {
        return RowChoice::Unavailable;
    }
    match operation {
        NativeSourceOperation::Execute
        | NativeSourceOperation::Stream
        | NativeSourceOperation::Internal => RowChoice::One(ROW_A1),
        NativeSourceOperation::CountTokens => RowChoice::One(ROW_A2),
        NativeSourceOperation::SourceCompact => RowChoice::Unavailable,
    }
}

fn kimi_row(
    facts: &NativeAuthorityFacts,
    protocol: &str,
    operation: NativeSourceOperation,
) -> RowChoice {
    let (chat, responses, messages, count) = match facts.mode.as_str() {
        "com" => (ROW_KC1, ROW_KC2, ROW_KC3, ROW_KC4),
        "ai" => (ROW_KA1, ROW_KA2, ROW_KA3, ROW_KA4),
        _ => return RowChoice::Unavailable,
    };
    if !generation_base_ok(facts) {
        return RowChoice::Unavailable;
    }
    let execute = match protocol {
        "chat_completions" => chat,
        "responses" => responses,
        "messages" => messages,
        _ => return RowChoice::Unavailable,
    };
    match operation {
        NativeSourceOperation::Execute
        | NativeSourceOperation::Stream
        | NativeSourceOperation::Internal => RowChoice::One(execute),
        NativeSourceOperation::CountTokens => RowChoice::One(count),
        NativeSourceOperation::SourceCompact => RowChoice::Unavailable,
    }
}

fn xai_row(facts: &NativeAuthorityFacts, operation: NativeSourceOperation) -> RowChoice {
    let execute = match facts.mode.as_str() {
        "cli" => ROW_X1,
        "api" => ROW_X2,
        _ => return RowChoice::Unavailable,
    };
    if !generation_base_ok(facts) {
        return RowChoice::Unavailable;
    }
    match operation {
        NativeSourceOperation::Execute
        | NativeSourceOperation::Stream
        | NativeSourceOperation::Internal => RowChoice::One(execute),
        NativeSourceOperation::CountTokens => RowChoice::LocalOnly,
        NativeSourceOperation::SourceCompact => RowChoice::One(match facts.mode.as_str() {
            "cli" => ROW_X3_CLI,
            "api" => ROW_X3_API,
            _ => return RowChoice::Unavailable,
        }),
    }
}

fn antigravity_row(
    facts: &NativeAuthorityFacts,
    model: &str,
    operation: NativeSourceOperation,
) -> RowChoice {
    if !generation_base_ok(facts) {
        return RowChoice::Unavailable;
    }
    let streamed = streamed_nonstream(model);
    match operation {
        NativeSourceOperation::CountTokens => RowChoice::One(ROW_G3),
        NativeSourceOperation::Stream => RowChoice::One(ROW_G2),
        NativeSourceOperation::Execute
        | NativeSourceOperation::Internal
        | NativeSourceOperation::SourceCompact => {
            RowChoice::One(if streamed { ROW_G2 } else { ROW_G1 })
        }
    }
}

fn streamed_nonstream(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("claude")
        || model.contains("gemini-3-pro")
        || model.contains("gemini-3.1-flash-image")
}

#[cfg(all(test, feature = "ollama-cloud-loopback-test"))]
thread_local! {
    static FIXTURE_MAPPING: std::cell::Cell<(u64, Option<&'static str>)> =
        const { std::cell::Cell::new((0, None)) };
}

#[cfg(all(test, feature = "ollama-cloud-loopback-test"))]
pub(crate) struct FixtureMapping {
    generation: u64,
}

#[cfg(all(test, feature = "ollama-cloud-loopback-test"))]
impl Drop for FixtureMapping {
    fn drop(&mut self) {
        FIXTURE_MAPPING.with(|slot| {
            let (generation, _) = slot.get();
            if generation == self.generation {
                slot.set((generation, None));
            }
        });
    }
}

/// Test builds pass mapping text here instead of setting the process environment.
#[cfg(all(test, feature = "ollama-cloud-loopback-test"))]
pub(crate) fn install_fixture_mapping(mapping: &'static str) -> FixtureMapping {
    FIXTURE_MAPPING.with(|slot| {
        let next = slot.get().0.wrapping_add(1);
        slot.set((next, Some(mapping)));
        FixtureMapping { generation: next }
    })
}

fn physical_url(map_key: &str, official: &str) -> Option<String> {
    let rewritten = physical_rewrite(map_key, official);
    match rewritten {
        Ok(Some(url)) => Some(url),
        Ok(None) => Some(official.to_string()),
        Err(_) => None,
    }
}

fn physical_rewrite(map_key: &str, official: &str) -> Result<Option<String>, String> {
    #[cfg(all(test, feature = "ollama-cloud-loopback-test"))]
    {
        let mapping = FIXTURE_MAPPING.with(|slot| slot.get().1);
        return crate::cpa_test_endpoints::rewrite_url_with_mapping(map_key, official, mapping);
    }
    #[cfg(not(all(test, feature = "ollama-cloud-loopback-test")))]
    {
        crate::cpa_test_endpoints::rewrite_url(map_key, official)
    }
}

fn build_target(
    connection: &ConnectionId,
    protocol: &str,
    row: &DictionaryRow,
    operation: NativeSourceOperation,
) -> Option<NativeDispatchTarget> {
    let official = canonical_native_url(row.url)?;
    if official != row.url {
        return None;
    }
    let url = physical_url(row.map_key, &official)?;
    let origin = normalize_origin(&url)?;
    Some(NativeDispatchTarget {
        pin: NativeEndpointPin {
            protocol: protocol.to_string(),
            endpoint_id: endpoint_id(connection, row.op_key, &url),
            origin,
            endpoint_fingerprint: fingerprint(url.as_bytes()),
            http_method: "POST".to_string(),
        },
        generation_kinds: kinds(operation)
            .iter()
            .map(|kind| (*kind).to_string())
            .collect(),
    })
}

fn endpoint_id(connection: &ConnectionId, op_key: &str, url: &str) -> String {
    match op_key {
        "chat_create" => endpoint_id_for_route(connection, EndpointOperation::ChatCreate, url)
            .as_str()
            .to_string(),
        "response_create" => {
            endpoint_id_for_route(connection, EndpointOperation::ResponseCreate, url)
                .as_str()
                .to_string()
        }
        "message_create" => {
            endpoint_id_for_route(connection, EndpointOperation::MessageCreate, url)
                .as_str()
                .to_string()
        }
        _ => uuid::Uuid::new_v5(
            &CONNECTION_ID_NAMESPACE,
            format!("endpoint:{}:{op_key}:{url}", connection.as_str()).as_bytes(),
        )
        .to_string(),
    }
}

fn merge_target(targets: &mut Vec<NativeDispatchTarget>, incoming: NativeDispatchTarget) {
    if let Some(existing) = targets.iter_mut().find(|item| {
        item.pin.protocol == incoming.pin.protocol
            && item.pin.endpoint_id == incoming.pin.endpoint_id
            && item.pin.endpoint_fingerprint == incoming.pin.endpoint_fingerprint
            && item.pin.http_method == incoming.pin.http_method
    }) {
        for kind in incoming.generation_kinds {
            if !existing.generation_kinds.iter().any(|saved| saved == &kind) {
                existing.generation_kinds.push(kind);
            }
        }
        existing
            .generation_kinds
            .sort_by_key(|kind| kind_rank(kind));
        return;
    }
    targets.push(incoming);
}
