use super::*;

fn declare_test_catalog_protocols(h: &FallbackHarness, models: &[&str]) {
    use ocg_core::provider::UpstreamProtocolKind;
    let scope = ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let ids = models
        .iter()
        .map(|model| model.to_string())
        .collect::<Vec<_>>();
    let baseline = ocg_core::dashboard_v3::OfficialProtocolBaseline::mapped_protocols(
        models
            .iter()
            .map(|model| (*model, vec![UpstreamProtocolKind::ChatCompletions])),
    );
    h.state
        .db
        .lock()
        .apply_official_protocol_baseline(&scope, &ids, &baseline, Utc::now())
        .unwrap();
    h.state.reload_provider_contracts().unwrap();
    let contracts = h.state.provider_contracts();
    for model in models {
        assert!(
            contracts
                .scope(&scope)
                .unwrap()
                .model(model)
                .unwrap()
                .routable
        );
    }
}

#[tokio::test]
async fn goat_sonnet_5_5_is_listed_in_aliases_and_models_and_uses_saved_messages_route() {
    let model = "claude-sonnet-5-5";
    let (h, _) = start_goat(
        &[("goat-key", &[ok_messages(), ok_messages()])],
        &[model],
        true,
        true,
    )
    .await;
    let (status, aliases) = v4_get(h.port, "/pages/aliases?search=claude-sonnet-5-5").await;
    assert_eq!(status, StatusCode::OK, "{aliases}");
    let group = aliases["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["publicModel"] == model)
        .expect("the refreshed Sonnet model must appear on the alias page");
    assert!(group["rows"].as_array().unwrap().iter().any(|row| {
        row["providerId"] == COMMAND_CODE_PROVIDER_ID && row["upstreamModel"] == model
    }));

    let (status, models) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{models}");
    let models: serde_json::Value = serde_json::from_str(&models).unwrap();
    let published = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == model)
        .expect("the authorized Sonnet alias must be listed for clients");
    assert_eq!(published["ocg"]["protocols"]["preferred"], "messages");
    assert_eq!(
        published["ocg"]["protocols"]["supported"],
        serde_json::json!(["messages"])
    );
    for path in ["/v1/messages", "/v1/responses"] {
        let (status, body) = h.protocol(path, model).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    let calls = h.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| {
        call.path == "/provider/v1/messages"
            && call.authorization.as_deref() == Some("Bearer goat-key")
    }));
    disable_command_protocols(&h.state, model);
    let (status, models) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{models}");
    let models: serde_json::Value = serde_json::from_str(&models).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != model)
    );
}

#[tokio::test]
async fn goat_arbitrary_new_models_appear_on_alias_page_and_in_client_catalog() {
    let model = "future-catalog-model-99";
    let (h, _) = start_goat(&[("goat-key", &[ok()])], &[model], true, true).await;
    // The generic harness forces protocols on to exercise routing. Discovery
    // alone uses Auto, so remove that explicit configuration for this phase.
    let scope = ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let switches = ocg_core::provider::UpstreamProtocolKind::ALL
        .into_iter()
        .map(|protocol| {
            (
                model.to_string(),
                protocol,
                ocg_core::provider_contracts::ProtocolOverrideState::Auto,
            )
        })
        .collect::<Vec<_>>();
    h.state
        .db
        .lock()
        .set_model_protocol_overrides(&scope, &switches, Utc::now())
        .unwrap();
    h.state.reload_provider_contracts().unwrap();
    let (status, models) = h.models().await;
    assert_eq!(status, StatusCode::OK);
    let models: serde_json::Value = serde_json::from_str(&models).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != model),
        "an alias alone must not invent protocol evidence"
    );
    declare_test_catalog_protocols(&h, &[model]);
    let (status, aliases) = v4_get(h.port, "/pages/aliases?search=future-catalog-model-99").await;
    assert_eq!(status, StatusCode::OK, "{aliases}");
    assert!(
        aliases["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group["publicModel"] == model),
        "alias page: {aliases}"
    );
    let (status, models) = h.models().await;
    assert_eq!(status, StatusCode::OK);
    let models: serde_json::Value = serde_json::from_str(&models).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == model)
    );
    let (status, body) = h.protocol("/v1/chat/completions", model).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn goat_alias_page_and_client_catalog_both_omit_ambiguous_generated_names() {
    let (h, _) = start_goat(&[], &["vendor/next-model", "next-model"], true, true).await;
    declare_test_catalog_protocols(&h, &["vendor/next-model", "next-model"]);
    let (status, aliases) = v4_get(h.port, "/pages/aliases?search=next-model").await;
    assert_eq!(status, StatusCode::OK, "{aliases}");
    assert!(aliases["groups"].as_array().unwrap().is_empty());
    let (status, models) = h.models().await;
    assert_eq!(status, StatusCode::OK);
    let models: serde_json::Value = serde_json::from_str(&models).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != "next-model")
    );
    assert!(h.calls.lock().unwrap().is_empty());
}
