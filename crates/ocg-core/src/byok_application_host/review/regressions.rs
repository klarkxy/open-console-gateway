//! Acceptance cases for restoring client selections and foreign references.
use super::*;

#[test]
fn minimax_takeover_undo_restores_cleared_preferences_and_preserves_later_replacements() {
    for later_edit in [false, true] {
        let h = harness("minimax-takeover-selection");
        let client = ByokClient::Minimax;
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let target = h.host.paths.resolve(client, None).unwrap();
        let mut baseline: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(&target.path).unwrap()).unwrap();
        let original_default = baseline["defaultModel"].clone();
        baseline["defaultModelThinking"] = serde_yaml_ng::Value::String("high".into());
        baseline["defaultModelContextWindow"] = serde_yaml_ng::to_value(12_000).unwrap();
        fs::write(&target.path, serde_yaml_ng::to_string(&baseline).unwrap()).unwrap();
        let store = Store::open(&h.host.data_dir, &target).unwrap();
        fs::remove_file(store.receipt_path()).unwrap();
        let desired = vec![model("b", 100_000, Some(8_192))];
        let preview = preview_update(&h.host, client, desired.clone());
        assert!(preview.preview.as_ref().unwrap().requires_takeover);
        reviewed_update(&h.host, client, &preview, desired, true).unwrap();
        let mut applied: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(&target.path).unwrap()).unwrap();
        assert!(applied["defaultModelThinking"].is_null());
        assert!(applied["defaultModelContextWindow"].is_null());
        if later_edit {
            applied["defaultModelThinking"] = serde_yaml_ng::Value::String("low".into());
            applied["defaultModelContextWindow"] = serde_yaml_ng::to_value(6_000).unwrap();
            fs::write(&target.path, serde_yaml_ng::to_string(&applied).unwrap()).unwrap();
        }
        let view = inspect(&h.host, client);
        remove(&h.host, client, view.fingerprint.as_deref().unwrap()).unwrap();
        let restored: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(&target.path).unwrap()).unwrap();
        assert_eq!(restored["defaultModel"], original_default);
        assert_eq!(
            restored["defaultModelThinking"].as_str(),
            Some(if later_edit { "low" } else { "high" })
        );
        assert_eq!(
            restored["defaultModelContextWindow"].as_u64(),
            Some(if later_edit { 6_000 } else { 12_000 })
        );
        assert!(!store.receipt_path().exists());
    }
}

#[test]
fn codex_takeover_undo_refuses_later_provider_or_catalog_selection_changes_without_writes() {
    for key in ["model_provider", "model_catalog_json"] {
        let h = harness("codex-takeover-selection");
        let client = ByokClient::Codex;
        let desired = vec![model("a", 100_000, Some(8_192))];
        configured_reviewed(&h, client, desired.clone());
        let target = h.host.paths.resolve(client, None).unwrap();
        let store = Store::open(&h.host.data_dir, &target).unwrap();
        fs::remove_file(store.receipt_path()).unwrap();
        let preview = preview_update(&h.host, client, desired.clone());
        reviewed_update(&h.host, client, &preview, desired, true).unwrap();
        let mut doc: toml_edit::DocumentMut =
            fs::read_to_string(&target.path).unwrap().parse().unwrap();
        doc[key] = toml_edit::value(if key == "model_provider" {
            "foreign".into()
        } else {
            h.host
                .data_dir
                .join("user-catalog.json")
                .to_string_lossy()
                .into_owned()
        });
        fs::write(&target.path, doc.to_string()).unwrap();
        let before = fs::read(&target.path).unwrap();
        let before_catalog = fs::read(catalog_path(&target).unwrap()).unwrap();
        let before_receipt = fs::read(store.receipt_path()).unwrap();
        let view = inspect(&h.host, client);
        let error = remove(&h.host, client, view.fingerprint.as_deref().unwrap()).unwrap_err();
        assert_eq!(error.kind, ByokErrorKind::Conflict);
        assert_eq!(fs::read(&target.path).unwrap(), before);
        assert_eq!(
            fs::read(catalog_path(&target).unwrap()).unwrap(),
            before_catalog
        );
        assert_eq!(fs::read(store.receipt_path()).unwrap(), before_receipt);
    }
}

#[test]
fn zcode_takeover_undo_refuses_to_delete_a_provider_still_referenced_by_a_manual_rule() {
    let h = harness("zcode-takeover-reference");
    let client = ByokClient::Zcode;
    let original = vec![model("a", 100_000, Some(8_192))];
    configured_reviewed(&h, client, original.clone());
    let target = h.host.paths.resolve(client, None).unwrap();
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    fs::remove_file(store.receipt_path()).unwrap();
    let preview = preview_update(&h.host, client, original.clone());
    reviewed_update(&h.host, client, &preview, original, true).unwrap();
    let message_model = with_protocols(
        model("b", 100_000, Some(8_192)),
        PublishedUpstreamProtocol::Messages,
        &[PublishedUpstreamProtocol::Messages],
    );
    let desired = vec![model("a", 100_000, Some(8_192)), message_model];
    let preview = preview_update(&h.host, client, desired.clone());
    reviewed_update(&h.host, client, &preview, desired, false).unwrap();
    // Prove that a clean restoration is possible before introducing the foreign reference.
    let clean = fs::read(&target.path).unwrap();
    let receipt = store.load().unwrap().unwrap();
    h.host
        .undo_takeover(client, &target, &store, &receipt, Some(&clean), None)
        .unwrap();
    let mut root: serde_json::Value =
        serde_json::from_slice(&fs::read(&target.path).unwrap()).unwrap();
    root["config"]["modelConfigRules"]["manualProviderModelRules"] = json!([
        {"providerId": "ocg-messages", "modelId": "foreign-model", "config": {"enabled": true, "user_note": "retain"}}
    ]);
    fs::write(&target.path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    let before = fs::read(&target.path).unwrap();
    let before_receipt = fs::read(store.receipt_path()).unwrap();
    let view = inspect(&h.host, client);
    let error = remove(&h.host, client, view.fingerprint.as_deref().unwrap()).unwrap_err();
    assert_eq!(error.kind, ByokErrorKind::Conflict);
    assert_eq!(fs::read(&target.path).unwrap(), before);
    assert_eq!(fs::read(store.receipt_path()).unwrap(), before_receipt);
}

// These fixtures remove every additive v2 field, rather than only changing its version.
fn true_v1_receipt(h: &Harness, client: ByokClient) -> Store {
    let target = normalize_target(&h.host.paths.resolve(client, None).unwrap()).unwrap();
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(store.receipt_path()).unwrap()).unwrap();
    receipt["version"] = json!(1);
    for field in [
        "last_generated",
        "adopted",
        "copilot_token_budget",
        "adopted_secret_hashes",
    ] {
        receipt.as_object_mut().unwrap().remove(field);
    }
    if client == ByokClient::Codex {
        let bytes = fs::read(catalog_path(&target).unwrap()).unwrap();
        receipt["last_managed"]["owned"]["catalog_sha256"] =
            json!(crate::byok_application_host::fs::content_hash(Some(&bytes)));
        receipt["last_managed"]["owned"]
            .as_object_mut()
            .unwrap()
            .remove("catalog");
    }
    fs::write(
        store.receipt_path(),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    store
}

#[test]
fn unchanged_large_codex_catalog_has_no_review_changes_and_preserves_native_bytes() {
    let h = harness("large-codex-review");
    let client = ByokClient::Codex;
    let models = (0..250)
        .map(|index| model(&format!("vendor/model.{index:03}"), 100_000, Some(8_192)))
        .collect::<Vec<_>>();
    configured_reviewed(&h, client, models.clone());
    let target = normalize_target(&h.host.paths.resolve(client, None).unwrap()).unwrap();
    let config = fs::read(&target.path).unwrap();
    let catalog = fs::read(catalog_path(&target).unwrap()).unwrap();
    let preview = preview_update(&h.host, client, models.clone());
    let changes = preview.preview.as_ref().unwrap();
    assert!(changes.added_model_ids.is_empty());
    assert!(changes.removed_model_ids.is_empty());
    assert!(changes.updated_model_ids.is_empty());
    assert!(!changes.requires_overwrite);
    assert_eq!(fs::read(&target.path).unwrap(), config);
    assert_eq!(fs::read(catalog_path(&target).unwrap()).unwrap(), catalog);
    reviewed_update(&h.host, client, &preview, models, false).unwrap();
    assert_eq!(fs::read(&target.path).unwrap(), config);
    assert_eq!(fs::read(catalog_path(&target).unwrap()).unwrap(), catalog);
}

#[test]
fn provider_route_changes_update_every_retained_model_in_all_clients() {
    for client in ByokClient::ALL {
        let h = harness("review-route-change");
        let models = vec![
            model("a", 100_000, Some(8_192)),
            model("b", 100_000, Some(8_192)),
        ];
        configured_reviewed(&h, client, models.clone());
        let preview = h
            .host
            .execute(ByokHostRequest::Preview {
                client,
                target_path: None,
                gateway_v1_url: "http://127.0.0.1:9043/v1".into(),
                models,
                copilot_token_budget: None,
            })
            .unwrap();
        assert_eq!(
            preview.preview.unwrap().updated_model_ids,
            vec!["a", "b"],
            "{client:?}"
        );
    }
}

#[test]
fn true_v1_custom_budget_removal_is_reviewed_before_the_first_upgrade_all_clients() {
    for client in ByokClient::ALL {
        let h = harness("legacy-v1-custom-removal");
        configured_reviewed(
            &h,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        let store = true_v1_receipt(&h, client);
        edit_managed(&h.host, client, |owned| {
            let row = managed_model_mut(client, owned, "b");
            match client {
                ByokClient::Codex => row["context_window"] = json!(4_000),
                ByokClient::Kimi => row["max_context_size"] = json!(4_000),
                ByokClient::Minimax => row["limit"]["output"] = json!(4_000),
                ByokClient::Zcode => {
                    row["config"]["optionSpecs"]["maxOutputTokens"]["default"] = json!(4_000)
                }
                ByokClient::Copilot => row["maxOutputTokens"] = json!(4_000),
            }
        });
        let target = h.host.paths.resolve(client, None).unwrap();
        let before = fs::read(&target.path).unwrap();
        let before_receipt = fs::read(store.receipt_path()).unwrap();
        let desired = vec![model("a", 100_000, Some(8_192))];
        let preview = preview_update(&h.host, client, desired.clone());
        assert_eq!(
            preview
                .preview
                .as_ref()
                .unwrap()
                .removed_models_with_customizations,
            vec!["b"],
            "{client:?}"
        );
        assert!(reviewed_update(&h.host, client, &preview, desired.clone(), false).is_err());
        assert_eq!(fs::read(&target.path).unwrap(), before);
        assert_eq!(fs::read(store.receipt_path()).unwrap(), before_receipt);
        reviewed_update(&h.host, client, &preview, desired, true).unwrap();
        assert_eq!(store.load().unwrap().unwrap().version, 2);
    }
}

#[test]
fn true_v1_generated_limits_refresh_while_unchanged_native_metadata_is_authenticated() {
    for client in ByokClient::ALL {
        let h = harness("legacy-v1-generated-limits");
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let store = true_v1_receipt(&h, client);
        let prior_input = store.load().unwrap().unwrap().last_managed.owned["provider"]["models"]
            [0]["maxInputTokens"]
            .clone();
        let desired = vec![model("a", 120_000, Some(8_192))];
        let preview = preview_update(&h.host, client, desired.clone());
        assert!(
            !preview.preview.as_ref().unwrap().requires_overwrite,
            "{client:?}"
        );
        reviewed_update(&h.host, client, &preview, desired, false).unwrap();
        let target = h.host.paths.resolve(client, None).unwrap();
        let bytes = fs::read(&target.path).unwrap();
        let catalog = catalog_path(&target).map(|path| fs::read(path).unwrap());
        let mut owned = adapter(client)
            .managed(Some(&bytes), catalog.as_deref())
            .unwrap();
        let row = managed_model_mut(client, &mut owned, "a");
        match client {
            ByokClient::Codex => assert_eq!(row["context_window"], 120_000),
            ByokClient::Kimi => assert_eq!(row["max_context_size"], 120_000),
            ByokClient::Minimax => assert_eq!(row["limit"]["context"], 120_000),
            ByokClient::Zcode => assert_eq!(row["config"]["properties"]["contextWindow"], 120_000),
            ByokClient::Copilot => assert_eq!(row["maxInputTokens"], prior_input),
        }
        assert_eq!(store.load().unwrap().unwrap().version, 2);
    }
}

#[test]
fn true_v1_codex_formatting_proof_accepts_semantic_json_without_rewriting_on_preview() {
    let h = harness("legacy-v1-codex-format");
    let client = ByokClient::Codex;
    let desired = vec![
        model("a", 100_000, Some(8_192)),
        model("b", 100_000, Some(8_192)),
    ];
    configured_reviewed(&h, client, desired.clone());
    let store = true_v1_receipt(&h, client);
    let before_receipt = fs::read(store.receipt_path()).unwrap();
    let before = inspect(&h.host, client);
    let target = h.host.paths.resolve(client, None).unwrap();
    let path = catalog_path(&target).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let changed = fs::read(&path).unwrap();
    let plan = preview_update(&h.host, client, desired);
    assert_ne!(plan.fingerprint, before.fingerprint);
    assert!(!plan.preview.as_ref().unwrap().requires_overwrite);
    assert_eq!(fs::read(&path).unwrap(), changed);
    assert_eq!(fs::read(store.receipt_path()).unwrap(), before_receipt);
    assert_eq!(store.load().unwrap().unwrap().version, 1);
}

#[test]
fn true_v1_copilot_custom_global_envelope_is_not_replaced_by_new_defaults_without_an_edit() {
    let h = harness("legacy-v1-copilot-envelope");
    let client = ByokClient::Copilot;
    let mut row = model("a", 100_000, Some(8_192));
    row.metadata.context_window = None;
    row.metadata.max_output_tokens = None;
    let models = vec![row];
    let budget = crate::byok_application::CopilotTokenBudget {
        max_input_tokens: 50_000,
        max_output_tokens: 4_000,
    };
    let preview = h
        .host
        .execute(ByokHostRequest::Preview {
            client,
            target_path: None,
            gateway_v1_url: GATEWAY.into(),
            models: models.clone(),
            copilot_token_budget: Some(budget),
        })
        .unwrap();
    h.host
        .execute(ByokHostRequest::ConfigureReviewed {
            client,
            target_path: None,
            expected_fingerprint: preview.fingerprint.unwrap(),
            gateway_v1_url: GATEWAY.into(),
            secret: secret(),
            models: models.clone(),
            client_closed: true,
            copilot_token_budget: Some(budget),
            review: crate::byok_application::ByokReview {
                preview_fingerprint: Some(preview.preview.unwrap().plan_fingerprint),
                ..Default::default()
            },
        })
        .unwrap();
    let store = true_v1_receipt(&h, client);
    for _ in 0..2 {
        let preview = preview_update(&h.host, client, models.clone());
        reviewed_update(&h.host, client, &preview, models.clone(), false).unwrap();
        let target = h.host.paths.resolve(client, None).unwrap();
        let mut owned = adapter(client)
            .managed(Some(&fs::read(target.path).unwrap()), None)
            .unwrap();
        let row = managed_model_mut(client, &mut owned, "a");
        assert_eq!(row["maxInputTokens"], 50_000);
        assert_eq!(row["maxOutputTokens"], 4_000);
    }
    assert_eq!(store.load().unwrap().unwrap().version, 2);
}

#[test]
fn copilot_above_global_envelope_budgets_survive_refresh_until_real_caps_shrink() {
    for legacy in [false, true] {
        let h = harness("copilot-above-envelope");
        let client = ByokClient::Copilot;
        let models = vec![model("a", 200_000, Some(16_384))];
        configured_reviewed(&h, client, models.clone());
        let target = h.host.paths.resolve(client, None).unwrap();
        let bytes = fs::read(&target.path).unwrap();
        let mut owned = adapter(client).managed(Some(&bytes), None).unwrap();
        let row = managed_model_mut(client, &mut owned, "a");
        row["maxInputTokens"] = json!(150_000);
        row["maxOutputTokens"] = json!(10_000);
        fs::write(
            &target.path,
            adapter(client)
                .replace_managed(Some(&bytes), &owned)
                .unwrap(),
        )
        .unwrap();
        if legacy {
            true_v1_receipt(&h, client);
        }
        for _ in 0..2 {
            let preview = preview_update(&h.host, client, models.clone());
            reviewed_update(&h.host, client, &preview, models.clone(), false).unwrap();
            let mut actual = adapter(client)
                .managed(Some(&fs::read(&target.path).unwrap()), None)
                .unwrap();
            let row = managed_model_mut(client, &mut actual, "a");
            assert_eq!(row["maxInputTokens"], 150_000);
            assert_eq!(row["maxOutputTokens"], 10_000);
        }
        let smaller = vec![model("a", 100_000, Some(6_000))];
        let preview = preview_update(&h.host, client, smaller.clone());
        reviewed_update(&h.host, client, &preview, smaller, false).unwrap();
        let mut actual = adapter(client)
            .managed(Some(&fs::read(&target.path).unwrap()), None)
            .unwrap();
        let row = managed_model_mut(client, &mut actual, "a");
        assert!(
            row["maxInputTokens"].as_u64().unwrap() + row["maxOutputTokens"].as_u64().unwrap()
                <= 100_000
        );
        assert!(row["maxOutputTokens"].as_u64().unwrap() <= 6_000);
    }
}
