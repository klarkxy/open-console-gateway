use super::*;

fn preview_update(
    host: &ByokNativeHost,
    client: ByokClient,
    models: Vec<ByokModel>,
) -> ByokInspection {
    host.execute(ByokHostRequest::Preview {
        client,
        target_path: None,
        gateway_v1_url: GATEWAY.into(),
        models,
        copilot_token_budget: None,
    })
    .unwrap()
}
fn reviewed_update(
    host: &ByokNativeHost,
    client: ByokClient,
    view: &ByokInspection,
    models: Vec<ByokModel>,
    acknowledge: bool,
) -> ByokResult<ByokInspection> {
    host.execute(ByokHostRequest::ConfigureReviewed {
        client,
        target_path: None,
        expected_fingerprint: view.fingerprint.clone().unwrap(),
        gateway_v1_url: GATEWAY.into(),
        secret: secret(),
        models,
        client_closed: true,
        copilot_token_budget: None,
        review: crate::byok_application::ByokReview {
            preview_fingerprint: view
                .preview
                .as_ref()
                .map(|preview| preview.plan_fingerprint.clone()),
            acknowledge_takeover: acknowledge,
            acknowledge_overwrite: acknowledge,
            acknowledge_removal: acknowledge,
        },
    })
}
fn edit_managed(
    host: &ByokNativeHost,
    client: ByokClient,
    edit: impl FnOnce(&mut serde_json::Value),
) {
    let target = host.paths.resolve(client, None).unwrap();
    let bytes = read_optional(&target.path).unwrap();
    let catalog_path = catalog_path(&target);
    let catalog = catalog_path
        .as_deref()
        .map(read_optional)
        .transpose()
        .unwrap()
        .flatten();
    let mut owned = adapter(client)
        .managed(bytes.as_deref(), catalog.as_deref())
        .unwrap();
    edit(&mut owned);
    fs::write(
        &target.path,
        adapter(client)
            .replace_managed(bytes.as_deref(), &owned)
            .unwrap(),
    )
    .unwrap();
    if let Some(path) = catalog_path {
        fs::write(path, serde_json::to_vec_pretty(&owned["catalog"]).unwrap()).unwrap();
    }
}
fn managed_provider_mut(
    client: ByokClient,
    owned: &mut serde_json::Value,
) -> &mut serde_json::Value {
    match client {
        ByokClient::Codex | ByokClient::Copilot => &mut owned["provider"],
        ByokClient::Kimi | ByokClient::Minimax => owned["providers"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .find(|row| row.is_object())
            .unwrap(),
        ByokClient::Zcode => &mut owned["providers"][0],
    }
}
fn managed_model_mut<'a>(
    client: ByokClient,
    owned: &'a mut serde_json::Value,
    id: &str,
) -> &'a mut serde_json::Value {
    match client {
        ByokClient::Codex => owned["catalog"]["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["slug"] == id)
            .unwrap(),
        ByokClient::Copilot => owned["provider"]["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["id"] == id)
            .unwrap(),
        ByokClient::Kimi => &mut owned["models"][format!("ocg/{id}")],
        ByokClient::Minimax => owned["providers"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .find_map(|provider| {
                provider
                    .get_mut("models")
                    .and_then(|models| models.get_mut(id))
            })
            .unwrap(),
        ByokClient::Zcode => owned["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["modelId"] == id)
            .unwrap(),
    }
}
fn configured_reviewed(h: &Harness, client: ByokClient, models: Vec<ByokModel>) -> ByokInspection {
    let preview = preview_update(&h.host, client, models.clone());
    reviewed_update(&h.host, client, &preview, models, false).unwrap()
}
#[test]
fn reviewed_preview_is_read_only_and_clean_model_add_remove_readd_updates_all_clients() {
    for client in ByokClient::ALL {
        let h = harness(&format!("review-{}", client.id()));
        let first = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert!(!h.host.paths.resolve(client, None).unwrap().path.exists());
        assert!(!h.host.data_dir.join("applications").exists());
        assert_eq!(first.preview.as_ref().unwrap().added_model_ids, vec!["a"]);
        reviewed_update(
            &h.host,
            client,
            &first,
            vec![model("a", 100_000, Some(8_192))],
            false,
        )
        .unwrap();
        edit_managed(&h.host, client, |owned| {
            managed_provider_mut(client, owned)["user_note"] = json!("keep provider note");
            managed_model_mut(client, owned, "a")["user_preference"] =
                json!({"id":"balanced","effort":"low"});
        });
        assert_eq!(inspect(&h.host, client).status, ByokStatus::Configured);
        let next = preview_update(
            &h.host,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        assert!(!next.preview.as_ref().unwrap().requires_overwrite);
        reviewed_update(
            &h.host,
            client,
            &next,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
            false,
        )
        .unwrap();
        let next = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert_eq!(next.preview.as_ref().unwrap().removed_model_ids, vec!["b"]);
        assert!(
            next.preview
                .as_ref()
                .unwrap()
                .removed_models_with_customizations
                .is_empty(),
            "{client:?}"
        );
        reviewed_update(
            &h.host,
            client,
            &next,
            vec![model("a", 100_000, Some(8_192))],
            false,
        )
        .unwrap();
        let next = preview_update(
            &h.host,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        assert_eq!(next.preview.as_ref().unwrap().added_model_ids, vec!["b"]);
        reviewed_update(
            &h.host,
            client,
            &next,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
            false,
        )
        .unwrap();
        let target = h.host.paths.resolve(client, None).unwrap();
        let bytes = fs::read(&target.path).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("keep provider note"));
        let catalog = catalog_path(&target).map(|path| fs::read(path).unwrap());
        let owned = adapter(client)
            .managed(Some(&bytes), catalog.as_deref())
            .unwrap();
        let mut owned = owned;
        assert_eq!(
            managed_model_mut(client, &mut owned, "a")["user_preference"]["effort"],
            "low"
        );
    }
}
#[test]
fn reviewed_customized_model_removal_requires_acknowledgment_all_clients() {
    for client in ByokClient::ALL {
        let h = harness("review-remove");
        configured_reviewed(
            &h,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        edit_managed(&h.host, client, |owned| {
            managed_model_mut(client, owned, "b")["user_note"] = json!("delete only after review")
        });
        let preview = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert_eq!(
            preview
                .preview
                .as_ref()
                .unwrap()
                .removed_models_with_customizations,
            vec!["b"],
            "{client:?}"
        );
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 100_000, Some(8_192))],
                false
            )
            .is_err()
        );
        reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 100_000, Some(8_192))],
            true,
        )
        .unwrap();
    }
}
#[test]
fn reviewed_takeover_and_undo_preserve_later_client_edits_after_another_update_all_clients() {
    for client in ByokClient::ALL {
        let h = harness("review-adopt");
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let target = h.host.paths.resolve(client, None).unwrap();
        let store = Store::open(&h.host.data_dir, &target).unwrap();
        edit_managed(&h.host, client, |owned| {
            managed_provider_mut(client, owned)["baseline_note"] =
                json!("current receipt-loss baseline")
        });
        fs::remove_file(store.receipt_path()).unwrap();
        let preview = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert!(preview.preview.as_ref().unwrap().requires_takeover);
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 100_000, Some(8_192))],
                false
            )
            .is_err()
        );
        let applied = reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 100_000, Some(8_192))],
            true,
        )
        .unwrap();
        assert!(applied.adopted);
        edit_managed(&h.host, client, |owned| {
            managed_provider_mut(client, owned)["later_note"] =
                json!("preserve after refresh and undo");
            let row = managed_model_mut(client, owned, "a");
            row["later_preference"] = json!({"id":"balanced","effort":"low"});
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
        let preview = preview_update(&h.host, client, vec![model("a", 120_000, Some(8_192))]);
        reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 120_000, Some(8_192))],
            false,
        )
        .unwrap();
        let preview = preview_update(&h.host, client, vec![model("a", 130_000, Some(8_192))]);
        let applied = reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 130_000, Some(8_192))],
            false,
        )
        .unwrap();
        remove(&h.host, client, applied.fingerprint.as_deref().unwrap()).unwrap();
        assert!(!store.receipt_path().exists());
        let bytes = fs::read(&target.path).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("current receipt-loss baseline"));
        assert!(
            String::from_utf8_lossy(&bytes).contains("preserve after refresh and undo"),
            "{client:?}"
        );
        let catalog = catalog_path(&target).map(|path| fs::read(path).unwrap());
        let mut owned = adapter(client)
            .managed(Some(&bytes), catalog.as_deref())
            .unwrap();
        assert_eq!(
            managed_model_mut(client, &mut owned, "a")["later_preference"]["effort"],
            "low",
            "{client:?}"
        );
        if client == ByokClient::Copilot {
            assert_eq!(
                managed_model_mut(client, &mut owned, "a")["maxOutputTokens"],
                4_000
            );
        }
    }
}
#[test]
fn reviewed_routing_overwrite_stale_plan_and_byte_fingerprints_are_checked() {
    for client in ByokClient::ALL {
        let h = harness("review-overwrite");
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        edit_managed(&h.host, client, |owned| match client {
            ByokClient::Codex => {
                managed_provider_mut(client, owned)["base_url"] = json!("http://other.invalid/v1")
            }
            ByokClient::Kimi => {
                managed_provider_mut(client, owned)["base_url"] = json!("http://other.invalid/v1")
            }
            ByokClient::Minimax => {
                managed_provider_mut(client, owned)["options"]["baseURL"] =
                    json!("http://other.invalid/v1")
            }
            ByokClient::Zcode => {
                managed_provider_mut(client, owned)["config"]["api"]["baseUrl"] =
                    json!("http://other.invalid/v1")
            }
            ByokClient::Copilot => {
                managed_model_mut(client, owned, "a")["url"] =
                    json!("http://other.invalid/v1/chat/completions")
            }
        });
        let view = inspect(&h.host, client);
        assert_eq!(view.status, ByokStatus::Conflict);
        assert!(view.configure_supported);
        let preview = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert!(preview.preview.as_ref().unwrap().requires_overwrite);
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 100_000, Some(8_192))],
                false
            )
            .is_err()
        );
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 120_000, Some(8_192))],
                true
            )
            .is_err()
        );
        reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 100_000, Some(8_192))],
            true,
        )
        .unwrap();
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 100_000, Some(8_192))],
                true
            )
            .is_err()
        );
    }
}
#[test]
fn codex_catalog_formatting_is_semantically_equal_but_byte_cas_still_changes() {
    let h = harness("review-codex-format");
    let first = configured_reviewed(
        &h,
        ByokClient::Codex,
        vec![model("a", 100_000, Some(8_192))],
    );
    let target = h.host.paths.resolve(ByokClient::Codex, None).unwrap();
    let path = catalog_path(&target).unwrap();
    let catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let view = inspect(&h.host, ByokClient::Codex);
    assert_eq!(view.status, ByokStatus::Configured);
    assert_ne!(view.fingerprint, first.fingerprint);
    let preview = preview_update(
        &h.host,
        ByokClient::Codex,
        vec![model("a", 100_000, Some(8_192))],
    );
    assert!(!preview.preview.unwrap().requires_overwrite);
}

#[test]
fn undo_takeover_removes_clean_models_added_later_and_refuses_customized_new_rows() {
    for client in ByokClient::ALL {
        let h = harness("review-adopt-added");
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let target = h.host.paths.resolve(client, None).unwrap();
        let store = Store::open(&h.host.data_dir, &target).unwrap();
        fs::remove_file(store.receipt_path()).unwrap();
        let preview = preview_update(
            &h.host,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        let applied = reviewed_update(
            &h.host,
            client,
            &preview,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
            true,
        )
        .unwrap();
        remove(&h.host, client, applied.fingerprint.as_deref().unwrap()).unwrap();
        let preview = preview_update(
            &h.host,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
        );
        reviewed_update(
            &h.host,
            client,
            &preview,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 100_000, Some(8_192)),
            ],
            true,
        )
        .unwrap();
        edit_managed(&h.host, client, |owned| {
            managed_model_mut(client, owned, "b")["later_note"] = json!("keep or refuse")
        });
        let preview = preview_update(
            &h.host,
            client,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 120_000, Some(8_192)),
            ],
        );
        let applied = reviewed_update(
            &h.host,
            client,
            &preview,
            vec![
                model("a", 100_000, Some(8_192)),
                model("b", 120_000, Some(8_192)),
            ],
            false,
        )
        .unwrap();
        let before = fs::read(&target.path).unwrap();
        assert!(
            remove(&h.host, client, applied.fingerprint.as_deref().unwrap()).is_err(),
            "{client:?}"
        );
        assert_eq!(fs::read(&target.path).unwrap(), before);
    }
}
#[test]
fn grouped_protocol_changes_preserve_model_client_settings_and_select_a_valid_default() {
    for client in [ByokClient::Kimi, ByokClient::Minimax, ByokClient::Zcode] {
        let h = harness("review-groups");
        let all = [
            PublishedUpstreamProtocol::ChatCompletions,
            PublishedUpstreamProtocol::Responses,
            PublishedUpstreamProtocol::Messages,
        ];
        let a = with_protocols(
            model("a", 100_000, Some(8_192)),
            PublishedUpstreamProtocol::Messages,
            &all,
        );
        configured_reviewed(&h, client, vec![a]);
        edit_managed(&h.host, client, |owned| {
            let row = managed_model_mut(client, owned, "a");
            row["user_preference"] = json!("preserve across groups");
            match client {
                ByokClient::Kimi => row["max_context_size"] = json!(4_000),
                ByokClient::Minimax => row["limit"]["output"] = json!(4_000),
                ByokClient::Zcode => {
                    row["config"]["optionSpecs"]["maxOutputTokens"]["default"] = json!(4_000)
                }
                _ => {}
            }
        });
        let a = with_protocols(
            model("a", 120_000, Some(8_192)),
            PublishedUpstreamProtocol::Responses,
            &all,
        );
        let preview = preview_update(&h.host, client, vec![a.clone()]);
        let applied = reviewed_update(&h.host, client, &preview, vec![a], false).unwrap();
        assert_eq!(applied.default_model_id.as_deref(), Some("a"));
        let target = h.host.paths.resolve(client, None).unwrap();
        let bytes = fs::read(target.path).unwrap();
        let mut owned = adapter(client).managed(Some(&bytes), None).unwrap();
        let row = managed_model_mut(client, &mut owned, "a");
        assert_eq!(row["user_preference"], "preserve across groups");
        match client {
            ByokClient::Kimi => assert_eq!(row["max_context_size"], 4_000),
            ByokClient::Minimax => assert_eq!(row["limit"]["output"], 4_000),
            ByokClient::Zcode => assert_eq!(
                row["config"]["optionSpecs"]["maxOutputTokens"]["default"],
                4_000
            ),
            _ => {}
        }
    }
}
#[test]
fn legacy_private_receipt_inspection_does_not_write_and_reviewed_update_migrates_to_v2() {
    let h = harness("review-receipt-v1");
    configured_reviewed(&h, ByokClient::Kimi, vec![model("a", 100_000, Some(8_192))]);
    let target = h.host.paths.resolve(ByokClient::Kimi, None).unwrap();
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    let mut receipt = store.load().unwrap().unwrap();
    receipt.version = 1;
    store.save_receipt(&receipt).unwrap();
    let before = fs::read(store.receipt_path()).unwrap();
    inspect(&h.host, ByokClient::Kimi);
    preview_update(
        &h.host,
        ByokClient::Kimi,
        vec![model("a", 100_000, Some(8_192))],
    );
    assert_eq!(fs::read(store.receipt_path()).unwrap(), before);
    configured_reviewed(&h, ByokClient::Kimi, vec![model("a", 120_000, Some(8_192))]);
    assert_eq!(store.load().unwrap().unwrap().version, 2);
}

#[cfg(windows)]
#[test]
fn same_existing_windows_file_case_alias_reuses_receipt_and_fingerprint_without_takeover() {
    for client in [
        ByokClient::Codex,
        ByokClient::Kimi,
        ByokClient::Minimax,
        ByokClient::Zcode,
        ByokClient::Copilot,
    ] {
        let h = harness("review-case-alias");
        let applied = configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let target = h.host.paths.resolve(client, None).unwrap();
        let alias = target.path.with_file_name(
            target
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_uppercase(),
        );
        let view = h
            .host
            .execute(ByokHostRequest::Preview {
                client,
                target_path: Some(alias.to_string_lossy().into_owned()),
                gateway_v1_url: GATEWAY.into(),
                models: vec![model("a", 100_000, Some(8_192))],
                copilot_token_budget: None,
            })
            .unwrap();
        assert_eq!(view.fingerprint, applied.fingerprint, "{client:?}");
        assert!(!view.preview.as_ref().unwrap().requires_takeover);
    }
}
#[test]
fn unknown_then_known_copilot_limits_cap_preserved_budgets_and_report_the_model_update() {
    let h = harness("review-copilot-cap");
    let mut a = model("a", 100_000, Some(8_192));
    a.metadata.context_window = None;
    a.metadata.max_output_tokens = None;
    configured_reviewed(&h, ByokClient::Copilot, vec![a]);
    edit_managed(&h.host, ByokClient::Copilot, |owned| {
        let row = managed_model_mut(ByokClient::Copilot, owned, "a");
        row["maxInputTokens"] = json!(40_000);
        row["maxOutputTokens"] = json!(4_000);
    });
    let preview = preview_update(
        &h.host,
        ByokClient::Copilot,
        vec![model("a", 10_000, Some(2_000))],
    );
    assert_eq!(
        preview.preview.as_ref().unwrap().updated_model_ids,
        vec!["a"]
    );
    reviewed_update(
        &h.host,
        ByokClient::Copilot,
        &preview,
        vec![model("a", 10_000, Some(2_000))],
        false,
    )
    .unwrap();
    let bytes = fs::read(
        h.host
            .paths
            .resolve(ByokClient::Copilot, None)
            .unwrap()
            .path,
    )
    .unwrap();
    let mut owned = adapter(ByokClient::Copilot)
        .managed(Some(&bytes), None)
        .unwrap();
    let row = managed_model_mut(ByokClient::Copilot, &mut owned, "a");
    let input = row["maxInputTokens"].as_u64().unwrap();
    let output = row["maxOutputTokens"].as_u64().unwrap();
    assert!(input > 0 && output > 0 && input + output <= 10_000 && output <= 2_000);
}
#[test]
fn reviewed_fault_rollback_retains_the_prior_receipt_and_bytes() {
    let h = harness("review-fault");
    let client = ByokClient::Codex;
    configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
    let target = h.host.paths.resolve(client, None).unwrap();
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    let before = fs::read(&target.path).unwrap();
    let receipt = fs::read(store.receipt_path()).unwrap();
    let catalog = fs::read(catalog_path(&target).unwrap()).unwrap();
    let preview = preview_update(&h.host, client, vec![model("a", 120_000, Some(8_192))]);
    *h.host.fail_after_writes.lock().unwrap() = Some(1);
    assert!(
        reviewed_update(
            &h.host,
            client,
            &preview,
            vec![model("a", 120_000, Some(8_192))],
            false
        )
        .is_err()
    );
    assert_eq!(fs::read(&target.path).unwrap(), before);
    assert_eq!(fs::read(store.receipt_path()).unwrap(), receipt);
    assert_eq!(fs::read(catalog_path(&target).unwrap()).unwrap(), catalog);
    assert!(!store.journal_path().exists());
}

#[test]
fn customized_budgets_still_require_removal_review_after_two_refreshes() {
    for client in ByokClient::ALL {
        let h = harness("review-budget-remove");
        let models = vec![
            model("a", 100_000, Some(8_192)),
            model("b", 100_000, Some(8_192)),
        ];
        configured_reviewed(&h, client, models.clone());
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
        for _ in 0..2 {
            let preview = preview_update(&h.host, client, models.clone());
            reviewed_update(&h.host, client, &preview, models.clone(), false).unwrap();
        }
        let preview = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
        assert_eq!(
            preview
                .preview
                .as_ref()
                .unwrap()
                .removed_models_with_customizations,
            vec!["b"],
            "{client:?}"
        );
        assert!(
            reviewed_update(
                &h.host,
                client,
                &preview,
                vec![model("a", 100_000, Some(8_192))],
                false
            )
            .is_err()
        );
    }
}
#[test]
fn previous_foreign_default_is_visible_and_invalid_copilot_budgets_never_write() {
    let h = harness("review-foreign-default");
    let path = h.host.paths.resolve(ByokClient::Kimi, None).unwrap().path;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"default_model = \"user-selected\"\n").unwrap();
    let preview = preview_update(
        &h.host,
        ByokClient::Kimi,
        vec![model("a", 100_000, Some(8_192))],
    );
    assert_eq!(
        preview
            .preview
            .unwrap()
            .previous_default_model_id
            .as_deref(),
        Some("user-selected")
    );
    for value in [json!(0), json!(-1), json!("4000")] {
        let h = harness("review-invalid-budget");
        configured_reviewed(
            &h,
            ByokClient::Copilot,
            vec![model("a", 100_000, Some(8_192))],
        );
        edit_managed(&h.host, ByokClient::Copilot, |owned| {
            managed_model_mut(ByokClient::Copilot, owned, "a")["maxOutputTokens"] = value
        });
        let path = h
            .host
            .paths
            .resolve(ByokClient::Copilot, None)
            .unwrap()
            .path;
        let before = fs::read(&path).unwrap();
        let result = h.host.execute(ByokHostRequest::Preview {
            client: ByokClient::Copilot,
            target_path: None,
            gateway_v1_url: GATEWAY.into(),
            models: vec![model("a", 100_000, Some(8_192))],
            copilot_token_budget: None,
        });
        assert!(result.is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
#[cfg(windows)]
#[test]
fn missing_file_case_alias_never_bypasses_a_pending_journal() {
    for client in ByokClient::ALL {
        let h = harness("review-case-journal");
        configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
        let target = h.host.paths.resolve(client, None).unwrap();
        let store = Store::open(&h.host.data_dir, &target).unwrap();
        let receipt = store.load().unwrap().unwrap();
        fs::write(
            store.journal_path(),
            serde_json::to_vec(&Journal {
                prior_receipt: Some(receipt),
                kind: PendingKind::Configure,
                files: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        fs::remove_file(&target.path).unwrap();
        let alias = target.path.with_file_name(
            target
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_uppercase(),
        );
        let result = h.host.execute(ByokHostRequest::Preview {
            client,
            target_path: Some(alias.to_string_lossy().into_owned()),
            gateway_v1_url: GATEWAY.into(),
            models: vec![model("a", 100_000, Some(8_192))],
            copilot_token_budget: None,
        });
        assert!(result.is_err(), "{client:?}");
        assert!(store.journal_path().exists());
        assert!(!target.path.exists());
    }
}

#[path = "regressions.rs"]
mod integrity_regressions;

#[test]
fn each_exact_profile_can_recover_its_identityless_journal_with_other_profiles_pending() {
    let h = harness("review-two-journals");
    let client = ByokClient::Kimi;
    let paths = [
        h.root.join("one").join("config.toml"),
        h.root.join("two").join("config.toml"),
    ];
    let mut stores = Vec::new();
    for path in &paths {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"keep = true\n").unwrap();
        let target = ResolvedTarget {
            client,
            path: path.clone(),
            discovery_source: "explicit".into(),
            store_identity_hint: Some(path.clone()),
        };
        stores.push(Store::open(&h.host.data_dir, &target).unwrap());
    }
    for store in &stores {
        fs::create_dir_all(&store.dir).unwrap();
        fs::write(
            store.journal_path(),
            serde_json::to_vec(&Journal {
                prior_receipt: None,
                kind: PendingKind::Configure,
                files: vec![],
            })
            .unwrap(),
        )
        .unwrap();
    }
    for (index, path) in paths.iter().enumerate() {
        let view = h
            .host
            .execute(ByokHostRequest::Inspect {
                client,
                target_path: Some(path.to_string_lossy().into_owned()),
            })
            .unwrap();
        assert_eq!(view.status, ByokStatus::RecoveryRequired);
        let recovered = h
            .host
            .execute(ByokHostRequest::Recover {
                client,
                target_path: Some(path.to_string_lossy().into_owned()),
                expected_fingerprint: view.fingerprint.unwrap(),
                client_closed: true,
            })
            .unwrap();
        assert_eq!(recovered.status, ByokStatus::Ready);
        assert!(!stores[index].journal_path().exists());
    }
}
#[test]
fn adopted_copilot_discovery_url_is_cleared_then_restored_without_losing_later_extras() {
    let h = harness("review-copilot-discovery");
    let client = ByokClient::Copilot;
    configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
    edit_managed(&h.host, client, |owned| {
        owned["provider"]["url"] = json!("http://baseline.invalid/models")
    });
    let target = h.host.paths.resolve(client, None).unwrap();
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    fs::remove_file(store.receipt_path()).unwrap();
    let preview = preview_update(&h.host, client, vec![model("a", 100_000, Some(8_192))]);
    assert_eq!(
        preview.preview.as_ref().unwrap().updated_model_ids,
        vec!["a"]
    );
    reviewed_update(
        &h.host,
        client,
        &preview,
        vec![model("a", 100_000, Some(8_192))],
        true,
    )
    .unwrap();
    let bytes = fs::read(&target.path).unwrap();
    assert!(
        adapter(client).managed(Some(&bytes), None).unwrap()["provider"]
            .get("url")
            .is_none()
    );
    edit_managed(&h.host, client, |owned| {
        owned["provider"]["later_note"] = json!("retain on undo")
    });
    let view = inspect(&h.host, client);
    remove(&h.host, client, view.fingerprint.as_deref().unwrap()).unwrap();
    let bytes = fs::read(target.path).unwrap();
    let owned = adapter(client).managed(Some(&bytes), None).unwrap();
    assert_eq!(owned["provider"]["url"], "http://baseline.invalid/models");
    assert_eq!(owned["provider"]["later_note"], "retain on undo");
}

#[test]
fn native_protocol_change_removes_generated_effort_menus_and_preserves_client_defaults() {
    for client in [ByokClient::Kimi, ByokClient::Minimax, ByokClient::Zcode] {
        let h = harness("review-protocol-menu");
        let mut a = model("a", 100_000, Some(8_192));
        a.metadata.reasoning = Some(true);
        a.metadata.reasoning_efforts = Some(
            [("low".into(), "low".into()), ("high".into(), "high".into())]
                .into_iter()
                .collect(),
        );
        configured_reviewed(&h, client, vec![a.clone()]);
        edit_managed(&h.host, client, |owned| {
            let row = managed_model_mut(client, owned, "a");
            match client {
                ByokClient::Kimi => row["user_effort_default"] = json!("low"),
                ByokClient::Minimax => row["thinking"]["userToggle"] = json!(true),
                ByokClient::Zcode => {
                    row["config"]["optionSpecs"]["reasoningLevel"]["userLabel"] = json!("keep")
                }
                _ => {}
            }
        });
        a = with_protocols(
            a,
            PublishedUpstreamProtocol::Messages,
            &[PublishedUpstreamProtocol::Messages],
        );
        let preview = preview_update(&h.host, client, vec![a.clone()]);
        assert!(!preview.preview.as_ref().unwrap().requires_overwrite);
        reviewed_update(&h.host, client, &preview, vec![a], false).unwrap();
        let target = h.host.paths.resolve(client, None).unwrap();
        let bytes = fs::read(target.path).unwrap();
        let mut owned = adapter(client).managed(Some(&bytes), None).unwrap();
        let row = managed_model_mut(client, &mut owned, "a");
        match client {
            ByokClient::Kimi => {
                assert!(row.get("support_efforts").is_none());
                assert_eq!(row["user_effort_default"], "low");
            }
            ByokClient::Minimax => {
                assert!(row["thinking"].get("effortOptions").is_none());
                assert_eq!(row["thinking"]["userToggle"], true);
            }
            ByokClient::Zcode => {
                assert!(
                    row["config"]["optionSpecs"]["reasoningLevel"]
                        .get("values")
                        .is_none()
                );
                assert!(
                    row["config"]["optionSpecs"]["reasoningLevel"]
                        .get("map")
                        .is_none()
                );
                assert_eq!(
                    row["config"]["optionSpecs"]["reasoningLevel"]["userLabel"],
                    "keep"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn budget_sibling_metadata_updates_and_unknown_zcode_capability_containers_stay_clamped() {
    let h = harness("review-budget-siblings");
    let client = ByokClient::Minimax;
    configured_reviewed(&h, client, vec![model("a", 100_000, Some(8_192))]);
    edit_managed(&h.host, client, |owned| {
        let row = managed_model_mut(client, owned, "a");
        row["limit"]["userUnit"] = json!("tokens");
        row["limit"]["output"] = json!(4_000);
    });
    let preview = preview_update(&h.host, client, vec![model("a", 120_000, Some(16_384))]);
    reviewed_update(
        &h.host,
        client,
        &preview,
        vec![model("a", 120_000, Some(16_384))],
        false,
    )
    .unwrap();
    let bytes = fs::read(h.host.paths.resolve(client, None).unwrap().path).unwrap();
    let mut owned = adapter(client).managed(Some(&bytes), None).unwrap();
    let row = managed_model_mut(client, &mut owned, "a");
    assert_eq!(row["limit"]["context"], 120_000);
    assert_eq!(row["limit"]["output"], 4_000);
    assert_eq!(row["limit"]["userUnit"], "tokens");
    let h = harness("review-zcode-unknown-cap");
    let client = ByokClient::Zcode;
    let mut a = model("a", 100_000, Some(8_192));
    a.metadata.tool_calling = None;
    a.metadata.input_modalities = None;
    configured_reviewed(&h, client, vec![a.clone()]);
    edit_managed(&h.host, client, |owned| {
        let row = managed_model_mut(client, owned, "a");
        row["config"]["properties"]["contextWindow"] = json!(20_000);
        row["config"]["properties"]["userNote"] = json!({"id":"balanced","keep":true});
    });
    a.metadata.context_window = Some(10_000);
    let preview = preview_update(&h.host, client, vec![a.clone()]);
    reviewed_update(&h.host, client, &preview, vec![a], false).unwrap();
    let bytes = fs::read(h.host.paths.resolve(client, None).unwrap().path).unwrap();
    let mut owned = adapter(client).managed(Some(&bytes), None).unwrap();
    let row = managed_model_mut(client, &mut owned, "a");
    assert_eq!(row["config"]["properties"]["contextWindow"], 10_000);
    assert_eq!(row["config"]["properties"]["userNote"]["id"], "balanced");
}
