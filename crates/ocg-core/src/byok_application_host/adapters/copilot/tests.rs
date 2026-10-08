use super::*;
use crate::byok_application::ByokErrorKind;
use crate::model_metadata::{ModelMetadata, PublishedModelProtocolProfile};

const SECRET: &str = "synthetic-copilot-key";

fn model(id: &str, preferred: PublishedUpstreamProtocol) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            context_window: Some(8192),
            max_output_tokens: Some(1024),
            ..Default::default()
        },
        protocols: PublishedModelProtocolProfile {
            preferred,
            supported: vec![preferred],
        },
    }
}

fn input(models: &[ByokModel]) -> ConfigureInput<'_> {
    ConfigureInput {
        gateway_v1_url: " http://localhost:9/deployment/v1/// ",
        secret: SECRET,
        models,
        default_model_id: None,
    }
}

fn configure(
    original: Option<&[u8]>,
    receipt: Option<&Receipt>,
    models: &[ByokModel],
) -> ByokResult<ApplyPlan> {
    CopilotAdapter.configure(
        Path::new("chatLanguageModels.json"),
        None,
        original,
        None,
        receipt,
        input(models),
    )
}

fn remove(bytes: Option<&[u8]>, receipt: &Receipt) -> ByokResult<ApplyPlan> {
    CopilotAdapter.remove(
        Path::new("chatLanguageModels.json"),
        None,
        bytes,
        None,
        receipt,
    )
}

fn bytes(plan: &ApplyPlan) -> &[u8] {
    plan.files[0].new_bytes.as_deref().unwrap()
}

fn carry(plan: &ApplyPlan) -> Receipt {
    Receipt {
        version: 1,
        client: "copilot".into(),
        target_path: "chatLanguageModels.json".into(),
        identity: "test".into(),
        created_target: plan.created_target,
        created_catalog: false,
        baseline_default: None,
        last_applied_default: None,
        first_owned: plan.first_owned.clone(),
        last_managed: super::super::snapshot(
            plan.managed.model_ids.clone(),
            crate::byok_application_host::receipt::without_secrets(&plan.managed.owned),
            None,
        ),
        adopted: false,
        copilot_token_budget: None,
        adopted_secret_hashes: serde_json::Value::Null,
        last_generated: serde_json::Value::Null,
        pending: None,
    }
}

fn jsonc(bytes: &[u8]) -> Value {
    Document::parse(Some(bytes))
        .unwrap()
        .root
        .to_serde_value()
        .unwrap()
}

fn assert_error(result: ByokResult<ApplyPlan>, kind: ByokErrorKind) {
    let error = result.unwrap_err();
    assert_eq!(error.kind, kind);
    assert!(!error.message.contains(SECRET));
}

#[test]
fn roundtrip_preserves_other_providers_and_comments() {
    let original = br#"// file header
[
  // existing provider
  {"name":"Mine","vendor":"customendpoint","url":"https://example.test",/* inline */"models":[]},
  // array footer
]
// file footer
"#;
    let models = [model(
        "Mixed/Exact_ID",
        PublishedUpstreamProtocol::ChatCompletions,
    )];
    let initial = configure(Some(original), None, &models).unwrap();
    let receipt = carry(&initial);
    let updated = configure(Some(bytes(&initial)), Some(&receipt), &models).unwrap();
    let inspect = CopilotAdapter.inspect_bytes(Some(bytes(&updated)), None, Some(&receipt));
    assert!(!inspect.user_changed_owned);
    assert!(!inspect.collision);
    assert_eq!(inspect.configured_model_ids, ["Mixed/Exact_ID"]);
    let removed = remove(Some(bytes(&updated)), &carry(&updated)).unwrap();
    let text = std::str::from_utf8(bytes(&removed)).unwrap();
    for comment in [
        "// file header",
        "// existing provider",
        "/* inline */",
        "// array footer",
        "// file footer",
    ] {
        assert!(text.contains(comment));
    }
    assert!(text.contains(r#"{"name":"Mine","vendor":"customendpoint","url":"https://example.test",/* inline */"models":[]}"#));
    assert_eq!(jsonc(bytes(&removed)), jsonc(original));
    assert!(!initial.created_target);
    assert!(initial.managed.applied_default.is_none());
    assert!(initial.baseline_default.is_none());
    assert!(initial.last_applied_default.is_none());
}

#[test]
fn same_name_is_unowned_even_under_another_vendor() {
    let original = br#"[{"name":"Open Console Gateway","vendor":"user"}]"#;
    let models = [model("a", PublishedUpstreamProtocol::Responses)];
    assert!(
        CopilotAdapter
            .inspect_bytes(Some(original), None, None)
            .collision
    );
    assert_error(
        configure(Some(original), None, &models),
        ByokErrorKind::Conflict,
    );
}

#[test]
fn duplicate_name_properties_follow_the_native_last_value_semantics() {
    let original = br#"[{"name":"Mine","name":"Open Console Gateway"}]"#;
    let models = [model("a", PublishedUpstreamProtocol::Responses)];
    assert!(
        CopilotAdapter
            .inspect_bytes(Some(original), None, None)
            .collision
    );
    assert_error(
        configure(Some(original), None, &models),
        ByokErrorKind::Conflict,
    );
}

#[test]
fn removal_preserves_comments_in_every_array_position() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    let provider = jsonc(bytes(&plan))[0].to_string();
    for text in [
        format!("[/*before*/{provider}/*after*/,/*next*/{{\"name\":\"Mine\"}}]"),
        format!("[{{\"name\":\"Mine\"}},/*before*/{provider}/*after*/]"),
        format!(
            "[{{\"name\":\"First\"}},/*before*/{provider}/*after*/,/*next*/{{\"name\":\"Last\"}},]"
        ),
    ] {
        let removed = remove(Some(text.as_bytes()), &receipt).unwrap();
        let remaining = std::str::from_utf8(bytes(&removed)).unwrap();
        assert!(remaining.contains("/*before*/"));
        assert!(remaining.contains("/*after*/"));
        if text.contains("/*next*/") {
            assert!(remaining.contains("/*next*/"));
        }
        assert!(!remaining.contains(PROVIDER_NAME));
        assert!(Document::parse(Some(bytes(&removed))).is_ok());
    }
}

#[test]
fn duplicate_owned_provider_is_always_a_collision() {
    let models = [model("a", PublishedUpstreamProtocol::Responses)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    let provider = jsonc(bytes(&plan))[0].clone();
    let duplicated = serde_json::to_vec(&json!([provider, provider])).unwrap();
    assert!(
        CopilotAdapter
            .inspect_bytes(Some(&duplicated), None, Some(&receipt))
            .collision
    );
    assert_error(
        configure(Some(&duplicated), Some(&receipt), &models),
        ByokErrorKind::Conflict,
    );
    assert_error(remove(Some(&duplicated), &receipt), ByokErrorKind::Conflict);
}

#[test]
fn edits_to_owned_extensions_or_models_conflict() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    for modified in [
        {
            let mut value = jsonc(bytes(&plan));
            value[0]["vendor"] = json!("another-vendor");
            value
        },
        {
            let mut value = jsonc(bytes(&plan));
            value[0]["models"][0]["id"] = json!("edited");
            value
        },
        json!([]),
    ] {
        let edited = serde_json::to_vec(&modified).unwrap();
        assert!(
            CopilotAdapter
                .inspect_bytes(Some(&edited), None, Some(&receipt))
                .user_changed_owned
        );
        assert_error(
            configure(Some(&edited), Some(&receipt), &models),
            ByokErrorKind::Conflict,
        );
        assert_error(remove(Some(&edited), &receipt), ByokErrorKind::Conflict);
    }
}

#[test]
fn preserves_exact_public_ids_and_full_protocol_endpoints() {
    let mut models = vec![
        model(
            "Provider/Mixed_ID",
            PublishedUpstreamProtocol::ChatCompletions,
        ),
        model("response-alias", PublishedUpstreamProtocol::Responses),
        model("messages.alias", PublishedUpstreamProtocol::Messages),
    ];
    models[0].metadata.name = Some("Readable model".into());
    let plan = configure(None, None, &models).unwrap();
    let value = jsonc(bytes(&plan));
    let provider = &value[0];
    assert_eq!(provider["name"], PROVIDER_NAME);
    assert_eq!(provider["vendor"], "customendpoint");
    assert!(provider.get("url").is_none());
    assert!(provider.get("apiKey").is_none());
    for (index, api, endpoint) in [
        (0, "chat-completions", "chat/completions"),
        (1, "responses", "responses"),
        (2, "messages", "messages"),
    ] {
        let exported = &provider["models"][index];
        assert_eq!(exported["id"], models[index].id);
        assert_eq!(exported["apiType"], api);
        assert_eq!(
            exported["url"],
            format!("http://localhost:9/deployment/v1/{endpoint}")
        );
        assert_eq!(
            exported["requestHeaders"]["Authorization"],
            format!("Bearer {SECRET}")
        );
        assert!(exported.get("apiKey").is_none());
    }
    assert_eq!(provider["models"][0]["name"], "Readable model");
    assert_eq!(provider["models"][1]["name"], "response-alias");
}

#[test]
fn exports_more_than_250_models_without_filtering() {
    let models: Vec<_> = (0..301)
        .map(|index| {
            model(
                &format!("Public/{index}"),
                PublishedUpstreamProtocol::Messages,
            )
        })
        .collect();
    let plan = configure(None, None, &models).unwrap();
    let value = jsonc(bytes(&plan));
    let exported = value[0]["models"].as_array().unwrap();
    assert_eq!(exported.len(), models.len());
    assert_eq!(exported.last().unwrap()["id"], "Public/300");
    assert_eq!(plan.managed.model_ids.len(), models.len());
}

#[test]
fn unknown_capabilities_are_disabled_and_no_reasoning_or_context_is_invented() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    let plan = configure(None, None, &models).unwrap();
    let value = jsonc(bytes(&plan));
    let exported = &value[0]["models"][0];
    assert_eq!(exported["toolCalling"], false);
    assert_eq!(exported["vision"], false);
    assert_eq!(exported["maxInputTokens"], 7168);
    assert_eq!(exported["maxOutputTokens"], 1024);
    for key in [
        "thinking",
        "contextWindow",
        "supportsReasoningEffort",
        "reasoningEffortFormat",
        "thinkingBudget",
    ] {
        assert!(exported.get(key).is_none());
    }
}

#[test]
fn known_capabilities_are_exported_including_false_reasoning() {
    let mut models = [model("a", PublishedUpstreamProtocol::Responses)];
    models[0].metadata.tool_calling = Some(true);
    models[0].metadata.input_modalities = Some(vec!["text".into(), "image".into()]);
    models[0].metadata.reasoning = Some(false);
    let plan = configure(None, None, &models).unwrap();
    let value = jsonc(bytes(&plan));
    let exported = &value[0]["models"][0];
    assert_eq!(exported["toolCalling"], true);
    assert_eq!(exported["vision"], true);
    assert_eq!(exported["thinking"], false);
}

#[test]
fn effort_menus_preserve_exact_wire_values_deduplicate_and_use_the_matching_protocol() {
    for protocol in [
        PublishedUpstreamProtocol::ChatCompletions,
        PublishedUpstreamProtocol::Responses,
    ] {
        let mut models = [model("a", protocol)];
        models[0].metadata.reasoning = Some(true);
        models[0].metadata.reasoning_efforts = Some(std::collections::BTreeMap::from([
            ("high".into(), "max-custom".into()),
            ("max".into(), "max-custom".into()),
            ("minimal".into(), "none".into()),
            ("off".into(), "off".into()),
            ("low".into(), "on".into()),
        ]));
        let plan = configure(None, None, &models).unwrap();
        let value = jsonc(bytes(&plan));
        let exported = &value[0]["models"][0];
        assert_eq!(
            exported["supportsReasoningEffort"],
            json!(["max-custom", "none", "off", "on"])
        );
        assert_eq!(exported["reasoningEffortFormat"], exported["apiType"]);
    }
}

#[test]
fn messages_false_reasoning_and_empty_maps_do_not_export_chat_efforts() {
    for (protocol, reasoning, efforts) in [
        (
            PublishedUpstreamProtocol::Messages,
            Some(true),
            std::collections::BTreeMap::from([("high".into(), "max-custom".into())]),
        ),
        (
            PublishedUpstreamProtocol::Responses,
            Some(false),
            std::collections::BTreeMap::from([("high".into(), "max-custom".into())]),
        ),
        (
            PublishedUpstreamProtocol::ChatCompletions,
            Some(true),
            std::collections::BTreeMap::new(),
        ),
    ] {
        let mut models = [model("a", protocol)];
        models[0].metadata.reasoning = reasoning;
        models[0].metadata.reasoning_efforts = Some(efforts);
        let plan = configure(None, None, &models).unwrap();
        let value = jsonc(bytes(&plan));
        let exported = &value[0]["models"][0];
        assert!(exported.get("supportsReasoningEffort").is_none());
        assert!(exported.get("reasoningEffortFormat").is_none());
        assert!(exported.get("thinkingBudget").is_none());
    }
}

#[test]
fn missing_or_invalid_token_budgets_fail_before_a_write_plan() {
    for (context, output) in [
        (None, None),
        (Some(8192), None),
        (None, Some(1024)),
        (Some(1024), Some(1024)),
        (Some(1), Some(2)),
        (Some(1), Some(0)),
    ] {
        let mut models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
        models[0].metadata.context_window = context;
        models[0].metadata.max_output_tokens = output;
        assert_error(configure(None, None, &models), ByokErrorKind::Precondition);
    }
}

#[test]
fn malformed_and_non_array_documents_are_incompatible_without_a_plan() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    for original in [
        "",
        "// only a comment",
        "{}",
        "null",
        "[broken]",
        "[{},{}",
        "[{} {}]",
        "[{name:'json5'}]",
    ] {
        let status = CopilotAdapter.inspect_bytes(Some(original.as_bytes()), None, None);
        assert!(status.incompatible.is_some(), "{original}");
        assert_error(
            configure(Some(original.as_bytes()), None, &models),
            ByokErrorKind::Invalid,
        );
    }
}

#[test]
fn recreates_a_missing_owned_target_and_removes_created_empty_file() {
    let models = [model("a", PublishedUpstreamProtocol::Messages)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    let recreated = configure(None, Some(&receipt), &models).unwrap();
    assert!(recreated.created_target);
    assert!(
        !CopilotAdapter
            .inspect_bytes(None, None, Some(&receipt))
            .user_changed_owned
    );
    assert!(
        remove(Some(bytes(&recreated)), &carry(&recreated))
            .unwrap()
            .files[0]
            .new_bytes
            .is_none()
    );
    assert!(remove(None, &receipt).unwrap().files[0].new_bytes.is_none());
}

#[test]
fn remove_preserves_user_comments_and_new_providers_in_a_created_file() {
    let models = [model("a", PublishedUpstreamProtocol::Messages)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    let text = format!(
        "// user comment\n{}",
        std::str::from_utf8(bytes(&plan)).unwrap()
    );
    let removed = remove(Some(text.as_bytes()), &receipt).unwrap();
    assert!(
        std::str::from_utf8(bytes(&removed))
            .unwrap()
            .contains("// user comment")
    );
    assert_eq!(jsonc(bytes(&removed)), json!([]));
    let mut value = jsonc(bytes(&plan));
    value
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"New user provider","models":[]}));
    let extended = serde_json::to_vec(&value).unwrap();
    let removed = remove(Some(&extended), &receipt).unwrap();
    assert_eq!(
        jsonc(bytes(&removed)),
        json!([{"name":"New user provider","models":[]}])
    );
}

#[test]
fn authorization_is_stripped_from_receipts_and_rotation_is_not_an_external_edit() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    let plan = configure(None, None, &models).unwrap();
    let receipt = carry(&plan);
    assert!(!serde_json::to_string(&receipt).unwrap().contains(SECRET));
    assert!(
        receipt.last_managed.owned["provider"]["models"][0]["requestHeaders"]
            .get("Authorization")
            .is_none()
    );
    let mut value = jsonc(bytes(&plan));
    value[0]["models"][0]["requestHeaders"]["Authorization"] = json!("Bearer changed-key");
    let rotated = serde_json::to_vec(&value).unwrap();
    assert!(
        !CopilotAdapter
            .inspect_bytes(Some(&rotated), None, Some(&receipt))
            .user_changed_owned
    );
    assert!(configure(Some(&rotated), Some(&receipt), &models).is_ok());
}

#[test]
fn default_model_request_is_rejected_without_claiming_selection_ownership() {
    let models = [model("a", PublishedUpstreamProtocol::ChatCompletions)];
    let mut requested = input(&models);
    requested.default_model_id = Some("a");
    assert_error(
        CopilotAdapter.configure(
            Path::new("chatLanguageModels.json"),
            None,
            None,
            None,
            None,
            requested,
        ),
        ByokErrorKind::Invalid,
    );
}

#[test]
fn authentication_header_aliases_are_rejected_without_disclosing_values() {
    for provider_level in [false, true] {
        for duplicate in [false, true] {
            let mut provider =
                json!({"name": PROVIDER_NAME, "vendor":"customendpoint", "models":[{"id":"a"}]});
            let mut headers = json!({"authorization":SECRET});
            if duplicate {
                headers["Authorization"] = json!("synthetic-other-key");
            }
            if provider_level {
                provider["requestHeaders"] = headers;
            } else {
                provider["models"][0]["requestHeaders"] = headers;
            }
            let data = serde_json::to_vec(&json!([provider])).unwrap();
            let status = CopilotAdapter.inspect_bytes(Some(&data), None, None);
            assert!(status.incompatible.is_some());
            assert_error(
                configure(
                    Some(&data),
                    None,
                    &[model("a", PublishedUpstreamProtocol::ChatCompletions)],
                ),
                ByokErrorKind::Invalid,
            );
            assert!(CopilotAdapter.managed(Some(&data), None).is_err());
        }
    }
    let foreign = serde_json::to_vec(
        &json!([{"name":"User provider","requestHeaders":{"authorization":SECRET},"models":[]}]),
    )
    .unwrap();
    assert!(
        configure(
            Some(&foreign),
            None,
            &[model("a", PublishedUpstreamProtocol::ChatCompletions)]
        )
        .is_ok()
    );
}
