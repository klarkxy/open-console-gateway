//! Responses tool-context mapping: namespace flattening, custom tools, and
//! hosted-tool compatibility markers.

use super::shared::empty_schema;
use super::{
    ConversionError, LEGACY_TOOL_COMPAT_PROFILE, LEGACY_TOOL_COMPAT_VERSION, LegacyToolCompat,
    NamespaceToolMapping,
};
use serde_json::{Value, json};

#[derive(Debug, Default)]
pub(super) struct ResponsesToolContext {
    pub(super) custom_tools: Vec<String>,
    pub(super) namespace_tools: Vec<NamespaceToolMapping>,
}

pub(super) fn responses_tool_context(
    body: &Value,
    converting: bool,
) -> Result<(ResponsesToolContext, Option<LegacyToolCompat>), ConversionError> {
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut context = ResponsesToolContext::default();
    let mut used_names = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            )
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let mut hosted_tools = Vec::new();

    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {}
            Some("custom") => {
                let name = required_tool_name(tool, "Responses custom tool")?;
                context.custom_tools.push(name.to_string());
            }
            Some("namespace") => {
                let namespace = required_tool_name(tool, "Responses namespace")?;
                let nested = tool.get("tools").and_then(Value::as_array).ok_or_else(|| {
                    ConversionError::new("Responses namespace tools are required")
                })?;
                for nested_tool in nested {
                    let kind = nested_tool.get("type").and_then(Value::as_str);
                    if !matches!(kind, Some("function" | "custom")) {
                        return Err(ConversionError::new(
                            "Responses namespace entries must be function or custom tools",
                        ));
                    }
                    let name = required_tool_name(nested_tool, "Responses namespace tool")?;
                    let flattened = unique_namespace_tool_name(namespace, name, &used_names);
                    used_names.push(flattened.clone());
                    if kind == Some("custom") {
                        context.custom_tools.push(flattened.clone());
                    }
                    context.namespace_tools.push(NamespaceToolMapping {
                        flattened,
                        namespace: namespace.to_string(),
                        name: name.to_string(),
                        custom: kind == Some("custom"),
                    });
                }
            }
            Some(kind) if is_hosted_tool(kind) => {
                if !hosted_tools.iter().any(|existing| existing == kind) {
                    hosted_tools.push(kind.to_string());
                }
            }
            Some(kind) => {
                return Err(ConversionError::new(format!(
                    "Responses tool type `{kind}` is not supported by protocol conversion"
                )));
            }
            None => return Err(ConversionError::new("Responses tool type is required")),
        }
    }

    if !converting {
        if requires_tool(body.get("tool_choice"))
            && used_names.is_empty()
            && hosted_tools.is_empty()
        {
            return Err(ConversionError::new(
                "Responses tool_choice `required` has no convertible function, custom, or namespace tool",
            ));
        }
        return Ok((context, None));
    }

    if let Some(kind) = forced_hosted_tool(body.get("tool_choice")) {
        return Err(ConversionError::new(format!(
            "Responses hosted tool `{kind}` cannot be forced through protocol conversion"
        )));
    }
    if !hosted_tools.is_empty() && used_names.is_empty() {
        return Err(ConversionError::new(format!(
            "Responses hosted tool `{}` cannot be preserved by protocol conversion; refusing to strip it and continue",
            hosted_tools[0]
        )));
    }
    if requires_tool(body.get("tool_choice")) && used_names.is_empty() {
        return Err(ConversionError::new(
            "Responses tool_choice `required` has no convertible function, custom, or namespace tool",
        ));
    }
    let legacy_tool_compat = (!hosted_tools.is_empty()).then_some(LegacyToolCompat {
        profile: LEGACY_TOOL_COMPAT_PROFILE,
        version: LEGACY_TOOL_COMPAT_VERSION,
        dropped_hosted_tools: hosted_tools,
    });
    Ok((context, legacy_tool_compat))
}

pub(super) fn required_tool_name<'a>(
    tool: &'a Value,
    label: &str,
) -> Result<&'a str, ConversionError> {
    tool.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ConversionError::new(format!("{label} name is required")))
}

pub(super) fn is_hosted_tool(kind: &str) -> bool {
    matches!(kind, "web_search" | "web_search_preview" | "tool_search")
}

pub(super) fn forced_hosted_tool(choice: Option<&Value>) -> Option<&str> {
    let choice = choice?;
    choice
        .as_str()
        .filter(|kind| is_hosted_tool(kind))
        .or_else(|| {
            choice
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| is_hosted_tool(kind))
        })
}

pub(super) fn requires_tool(choice: Option<&Value>) -> bool {
    choice.is_some_and(|choice| {
        choice.as_str() == Some("required")
            || choice.get("type").and_then(Value::as_str) == Some("required")
    })
}

pub(super) fn unique_namespace_tool_name(namespace: &str, name: &str, used: &[String]) -> String {
    let raw = format!("{namespace}__{name}");
    let mut flattened = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if flattened.len() <= 64 && !used.iter().any(|used| used == &flattened) {
        return flattened;
    }

    let mut hash = 0xcbf29ce484222325_u64;
    for byte in raw.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let suffix = format!("__{hash:016x}");
    flattened.truncate(64 - suffix.len());
    flattened.push_str(&suffix);
    flattened
}

pub(super) fn responses_tool_to_anthropic(
    tool: &Value,
    namespace: Option<&str>,
    mappings: &[NamespaceToolMapping],
) -> Option<Value> {
    let original = tool.get("name")?.as_str()?;
    let name = namespace
        .and_then(|namespace| {
            mappings
                .iter()
                .find(|mapping| mapping.namespace == namespace && mapping.name == original)
                .map(|mapping| mapping.flattened.as_str())
        })
        .unwrap_or(original);
    match tool.get("type").and_then(Value::as_str) {
        Some("function") => Some(json!({
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": tool.get("parameters").cloned().unwrap_or_else(empty_schema)
        })),
        Some("custom") => Some(json!({
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": {
                "type": "object",
                "properties": { "input": { "type": "string" } },
                "required": ["input"],
                "additionalProperties": false
            }
        })),
        _ => None,
    }
}

pub(super) fn responses_history_tool_name(
    item: &Value,
    mappings: &[NamespaceToolMapping],
) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(namespace) = item
        .get("namespace")
        .and_then(Value::as_str)
        .filter(|namespace| !namespace.is_empty())
    else {
        return name.to_string();
    };
    mappings
        .iter()
        .find(|mapping| mapping.namespace == namespace && mapping.name == name)
        .map(|mapping| mapping.flattened.clone())
        .unwrap_or_else(|| unique_namespace_tool_name(namespace, name, &[]))
}
