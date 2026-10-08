//! VS Code's native chatLanguageModels.json provider array.
use super::{
    ConfigureInput, FormatAdapter, PROVIDER_NAME, ParsedStatus, display_name, first_owned,
    gateway_v1_base, ownership_conflict, planned_target, preserve_created, snapshot,
};
use crate::byok_application::{ByokClient, ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use crate::model_metadata::PublishedUpstreamProtocol;
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstArray, CstInputValue, CstObject, CstRootNode};
use serde_json::{Value, json};
use std::path::Path;

#[cfg(test)]
mod tests;

pub struct CopilotAdapter;

struct Document {
    root: CstRootNode,
    providers: CstArray,
    managed: Vec<CstObject>,
}

impl Document {
    fn parse(bytes: Option<&[u8]>) -> ByokResult<Self> {
        let text = match bytes {
            Some(bytes) => std::str::from_utf8(bytes)
                .map_err(|_| ByokError::invalid("Copilot model config is not UTF-8 JSONC"))?,
            None => "[]\n",
        };
        // VS Code accepts JSON with comments and trailing commas, not JSON5.
        let options = ParseOptions {
            allow_comments: true,
            allow_trailing_commas: true,
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
            allow_bare_decimal_point_numbers: false,
            allow_non_finite_numbers: false,
            allow_extended_string_escapes: false,
        };
        let root = CstRootNode::parse(text, &options)
            .map_err(|_| ByokError::invalid("Copilot model config is not valid JSONC"))?;
        let providers = root
            .array_value()
            .ok_or_else(|| ByokError::invalid("Copilot model config must be a JSONC array"))?;
        let managed: Vec<CstObject> = providers
            .elements()
            .into_iter()
            .filter_map(|node| node.as_object())
            .filter(|object| {
                object
                    .to_serde_value()
                    .as_ref()
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
                    == Some(PROVIDER_NAME)
            })
            .collect();
        // JSON property names are case-sensitive, while HTTP header names are not.
        // Refuse aliases before planning a canonical Authorization header rather
        // than preserving a second credential as an unrelated client setting.
        for provider in &managed {
            if let Some(value) = provider.to_serde_value() {
                let has_alias = |row: &Value| {
                    row.get("requestHeaders")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| {
                            headers.keys().any(|key| {
                                key.eq_ignore_ascii_case("Authorization") && key != "Authorization"
                            })
                        })
                };
                if has_alias(&value)
                    || value["models"]
                        .as_array()
                        .is_some_and(|models| models.iter().any(has_alias))
                {
                    return Err(ByokError::invalid(
                        "OCG Copilot authentication headers must use a single Authorization property",
                    ));
                }
            }
        }
        Ok(Self {
            root,
            providers,
            managed,
        })
    }

    fn owned(&self) -> Value {
        json!({"provider": self.managed.first().and_then(CstObject::to_serde_value)})
    }

    fn model_ids(&self) -> Vec<String> {
        self.owned()["provider"]["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|model| model.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }

    fn collision(&self, receipt: Option<&Receipt>) -> bool {
        self.managed.len() > 1 || (!self.managed.is_empty() && receipt.is_none())
    }

    fn ensure_owned(&self, receipt: Option<&Receipt>, present: bool) -> ByokResult<()> {
        if self.collision(receipt) {
            return Err(ByokError::conflict(
                "An unowned or duplicate Open Console Gateway Copilot provider exists",
            ));
        }
        if ownership_conflict(ByokClient::Copilot, receipt, present, &self.owned()) {
            return Err(ByokError::conflict(
                "Owned Copilot provider fields changed outside OCG",
            ));
        }
        Ok(())
    }

    /// The parser's remove() also consumes adjacent comments. Keep every
    /// top-level trivia node and omit only the owned object and its separator.
    fn without_provider(&self) -> Vec<u8> {
        let Some(provider) = self.managed.first() else {
            return self.root.to_string().into_bytes();
        };
        let children = self.providers.children();
        let provider_index = provider.child_index();
        let comma_index = provider
            .trailing_comma()
            .map(|comma| comma.child_index())
            .or_else(|| {
                children[..provider_index]
                    .iter()
                    .rposition(|node| node.is_comma())
            });
        let mut output = String::new();
        for node in self.root.children() {
            if node.as_array().is_some() {
                for (index, child) in children.iter().enumerate() {
                    if index != provider_index && Some(index) != comma_index {
                        output.push_str(&child.to_string());
                    }
                }
            } else {
                output.push_str(&node.to_string());
            }
        }
        output.into_bytes()
    }

    fn has_comments(&self) -> bool {
        self.root.children().iter().any(|node| node.is_comment())
            || self
                .providers
                .children()
                .iter()
                .any(|node| node.is_comment())
    }
}

impl FormatAdapter for CopilotAdapter {
    fn raw_default(&self, bytes: Option<&[u8]>) -> ByokResult<Option<String>> {
        let _ = bytes;
        Ok(None)
    }

    fn managed(&self, target_bytes: Option<&[u8]>, _catalog: Option<&[u8]>) -> ByokResult<Value> {
        let document = Document::parse(target_bytes)?;
        if document.managed.len() > 1 {
            return Err(ByokError::invalid(
                "Duplicate Copilot provider entries cannot be adopted",
            ));
        }
        Ok(document.owned())
    }
    fn replace_managed(&self, target_bytes: Option<&[u8]>, owned: &Value) -> ByokResult<Vec<u8>> {
        let document = Document::parse(target_bytes)?;
        let provider = &owned["provider"];
        if provider.is_null() {
            return Ok(document.without_provider());
        }
        let value = cst_input(provider.clone());
        if let Some(existing) = document.managed.first() {
            existing
                .clone()
                .replace_with(value)
                .ok_or_else(|| ByokError::internal("failed to replace Copilot provider"))?;
        } else {
            document.providers.append(value);
        }
        Ok(document.root.to_string().into_bytes())
    }
    fn restore_selection(
        &self,
        target_bytes: &[u8],
        _original: Option<&[u8]>,
        _receipt: &Receipt,
    ) -> ByokResult<Vec<u8>> {
        Ok(target_bytes.to_vec())
    }

    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus {
        let mut status = ParsedStatus {
            incompatible: None,
            collision: false,
            configured_model_ids: Vec::new(),
            current_default: None,
            user_changed_owned: false,
        };
        let Some(bytes) = target_bytes else {
            return status;
        };
        match Document::parse(Some(bytes)) {
            Ok(document) => {
                status.collision = document.collision(receipt);
                status.configured_model_ids = document.model_ids();
                status.user_changed_owned =
                    ownership_conflict(ByokClient::Copilot, receipt, true, &document.owned());
            }
            Err(error) => status.incompatible = Some(error.message),
        }
        status
    }

    fn configure(
        &self,
        target_path: &Path,
        _catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
        input: ConfigureInput<'_>,
    ) -> ByokResult<ApplyPlan> {
        super::validate_models(ByokClient::Copilot, input.models)?;
        for model in input.models {
            let Some((context, output)) = model
                .metadata
                .context_window
                .zip(model.metadata.max_output_tokens)
            else {
                return Err(ByokError::precondition(
                    "Copilot needs explicit input and output token budgets for every model",
                ));
            };
            if output == 0 || context <= output {
                return Err(ByokError::precondition(
                    "Copilot token budgets must leave positive input and output capacity",
                ));
            }
        }
        if input.default_model_id.is_some() {
            return Err(ByokError::invalid(
                "Copilot default model selection is managed in VS Code",
            ));
        }
        let document = Document::parse(target_bytes)?;
        document.ensure_owned(receipt, target_bytes.is_some())?;
        let provider = provider_value(&input);
        let value = cst_input(provider.clone());
        if let Some(existing) = document.managed.first() {
            existing
                .clone()
                .replace_with(value)
                .ok_or_else(|| ByokError::internal("failed to replace owned Copilot provider"))?;
        } else {
            document.providers.append(value);
        }
        let (created_target, _) = preserve_created(receipt, target_bytes.is_none(), false);
        Ok(ApplyPlan {
            files: vec![planned_target(
                target_path.to_path_buf(),
                Some(document.root.to_string().into_bytes()),
            )],
            created_target,
            created_catalog: false,
            baseline_default: None,
            last_applied_default: None,
            managed: snapshot(
                input.models.iter().map(|model| model.id.clone()).collect(),
                json!({"provider": provider}),
                None,
            ),
            first_owned: first_owned(receipt, &Value::Null),
        })
    }

    fn remove(
        &self,
        target_path: &Path,
        _catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: &Receipt,
    ) -> ByokResult<ApplyPlan> {
        let bytes = if target_bytes.is_some() {
            let document = Document::parse(target_bytes)?;
            document.ensure_owned(Some(receipt), true)?;
            if receipt.created_target
                && document.providers.elements().len() == document.managed.len()
                && !document.has_comments()
            {
                None
            } else {
                Some(document.without_provider())
            }
        } else {
            None
        };
        Ok(ApplyPlan {
            files: vec![planned_target(target_path.to_path_buf(), bytes)],
            created_target: receipt.created_target,
            created_catalog: false,
            baseline_default: None,
            last_applied_default: None,
            managed: snapshot(Vec::new(), json!({"provider": null}), None),
            first_owned: receipt.first_owned.clone(),
        })
    }
}

fn provider_value(input: &ConfigureInput<'_>) -> Value {
    json!({
        "name": PROVIDER_NAME,
        "vendor": "customendpoint",
        "models": input.models.iter().map(|model| model_value(model, input)).collect::<Vec<_>>()
    })
}

fn model_value(model: &ByokModel, input: &ConfigureInput<'_>) -> Value {
    let (api_type, endpoint) = match model.protocols.preferred {
        PublishedUpstreamProtocol::ChatCompletions => ("chat-completions", "chat/completions"),
        PublishedUpstreamProtocol::Responses => ("responses", "responses"),
        PublishedUpstreamProtocol::Messages => ("messages", "messages"),
    };
    let mut value = json!({
        "id": model.id,
        "name": display_name(model),
        "apiType": api_type,
        "url": format!("{}/{endpoint}", gateway_v1_base(input.gateway_v1_url)),
        // apiKey is a secret-storage reference in VS Code, not a plain Key.
        "requestHeaders": {"Authorization": format!("Bearer {}", input.secret)}
    });
    let fields = value.as_object_mut().expect("model is an object");
    let metadata = &model.metadata;
    // Native explicit models require both switches. Unknown capabilities are
    // disabled in this client; this does not publish an upstream capability.
    fields.insert(
        "toolCalling".into(),
        (metadata.tool_calling == Some(true)).into(),
    );
    fields.insert("vision".into(), super::has_image(metadata).into());
    if let Some(flag) = metadata.reasoning {
        fields.insert("thinking".into(), flag.into());
    }
    // These saved values are Chat/Responses wire spellings. Messages has a
    // different effort contract, so it must not inherit this menu.
    if model.protocols.preferred != PublishedUpstreamProtocol::Messages
        && metadata.reasoning != Some(false)
        && let Some(efforts) = &metadata.reasoning_efforts
        && !efforts.is_empty()
    {
        let values: std::collections::BTreeSet<_> = efforts.values().collect();
        fields.insert("supportsReasoningEffort".into(), json!(values));
        fields.insert("reasoningEffortFormat".into(), api_type.into());
    }
    if let Some(limit) = metadata.max_output_tokens {
        fields.insert("maxOutputTokens".into(), limit.into());
    }
    if let (Some(context), Some(output)) = (metadata.context_window, metadata.max_output_tokens)
        && let Some(input_limit) = context.checked_sub(output).filter(|limit| *limit > 0)
    {
        fields.insert("maxInputTokens".into(), input_limit.into());
    }
    value
}

fn cst_input(value: Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value),
        Value::Array(values) => CstInputValue::Array(values.into_iter().map(cst_input).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, cst_input(value)))
                .collect(),
        ),
    }
}
