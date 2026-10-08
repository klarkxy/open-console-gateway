use super::*;
use crate::provider::OPENCODE_PROVIDER_ID;
use crate::provider_contracts::*;
use chrono::Utc;

fn scope() -> EffectiveScopeContract {
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    let mut saved = PersistedContracts::default();
    saved.scopes.insert(
        scope.clone(),
        PersistedScopeRow {
            scope: scope.clone(),
            catalog_models: vec!["z-new-model".into(), "a-new-model".into()],
            catalog_refreshed_at: Some(now),
            catalog_source: "test".into(),
            catalog_source_url: "https://example.test/models".into(),
            revision: 1,
            updated_at: now,
        },
    );
    saved.evidence.insert(
        scope.clone(),
        [
            ("z-new-model", UpstreamProtocolKind::ChatCompletions),
            ("a-new-model", UpstreamProtocolKind::ChatCompletions),
            ("a-new-model", UpstreamProtocolKind::Responses),
            ("a-new-model", UpstreamProtocolKind::Messages),
        ]
        .into_iter()
        .map(|(id, protocol)| PersistedModelProtocol {
            scope: scope.clone(),
            model_id: id.into(),
            protocol,
            source: ContractEvidenceSource::Static,
            verified_at: Some(now),
            observed_at: None,
            last_probe_result: None,
            last_probe_at: None,
            last_probe_error: None,
        })
        .collect(),
    );
    saved.preferences.insert(
        scope,
        vec![("a-new-model".into(), UpstreamProtocolKind::Responses)],
    );
    build_effective_contracts(&Default::default(), &[], saved)
        .providers
        .remove(OPENCODE_PROVIDER_ID)
        .unwrap()
}

#[test]
fn chooses_deterministic_arbitrary_saved_model_and_preferred_supported_protocol() {
    let mut scope = scope();
    let selected = select_verification_model(&scope, None).unwrap();
    assert_eq!(selected.model, "a-new-model");
    assert_eq!(selected.protocol, UpstreamProtocolKind::Responses);
    scope.catalog.models.reverse();
    assert_eq!(select_verification_model(&scope, None).unwrap(), selected);
    scope
        .models
        .get_mut("a-new-model")
        .unwrap()
        .protocols
        .get_mut("responses")
        .unwrap()
        .enabled = false;
    assert_eq!(
        select_verification_model(&scope, None).unwrap().protocol,
        UpstreamProtocolKind::ChatCompletions
    );
}

#[test]
fn explicit_selection_is_preserved_and_never_tries_another_model() {
    let mut scope = scope();
    assert_eq!(
        select_verification_model(&scope, Some("z-new-model"))
            .unwrap()
            .model,
        "z-new-model"
    );
    scope.models.get_mut("z-new-model").unwrap().routable = false;
    assert!(select_verification_model(&scope, Some("z-new-model")).is_err());
    assert!(select_verification_model(&scope, Some("not-in-directory")).is_err());
}

#[test]
fn empty_disabled_or_unsupported_catalog_cannot_select_inference() {
    let mut saved = scope();
    saved.catalog.models.clear();
    assert!(select_verification_model(&saved, None).is_err());
    let mut saved = scope();
    saved.catalog_routable = false;
    assert!(select_verification_model(&saved, None).is_err());
    let mut saved = scope();
    for model in saved.models.values_mut() {
        for row in model.protocols.values_mut() {
            row.available = false;
        }
    }
    assert!(select_verification_model(&saved, None).is_err());
}

#[test]
fn all_protocols_have_exact_minimal_body_and_endpoint() {
    for (protocol, path, expected) in [
        (
            UpstreamProtocolKind::ChatCompletions,
            "/v1/chat/completions",
            serde_json::json!({"model":"arbitrary", "messages":[{"role":"user", "content":"ping"}], "max_tokens":1, "stream":false}),
        ),
        (
            UpstreamProtocolKind::Responses,
            "/v1/responses",
            serde_json::json!({"model":"arbitrary", "input":"ping", "max_output_tokens":16, "store":false, "stream":false}),
        ),
        (
            UpstreamProtocolKind::Messages,
            "/v1/messages",
            serde_json::json!({"model":"arbitrary", "messages":[{"role":"user", "content":"ping"}], "max_tokens":1, "stream":false}),
        ),
    ] {
        let selected = VerificationModel {
            model: "arbitrary".into(),
            protocol,
        };
        assert_eq!(selected.path(), path);
        let headers = selected.go_headers("synthetic-key").unwrap();
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["accept"], "application/json");
        if protocol == UpstreamProtocolKind::Messages {
            assert_eq!(headers["x-api-key"], "synthetic-key");
            assert_eq!(headers["anthropic-version"], "2023-06-01");
            assert!(!headers.contains_key("authorization"));
        } else {
            assert_eq!(headers["authorization"], "Bearer synthetic-key");
            assert!(!headers.contains_key("x-api-key"));
            assert!(!headers.contains_key("anthropic-version"));
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&selected.body("ping", 1).unwrap())
                .unwrap(),
            expected
        );
    }
}

#[test]
fn discovery_requires_actual_protocol_facts_and_preserves_saved_controls() {
    let mut saved_scope = scope();
    saved_scope.catalog.models.clear();
    saved_scope.models.clear();
    let key = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let mut saved = PersistedContracts::default();
    let mut projection = crate::destination_projection::DestinationProjection {
        destinations: vec![
            ocg_domain::destination::destination_from_legacy(
                &ocg_domain::destination::LegacyDestinationFacts::Builtin {
                    provider_id: OPENCODE_PROVIDER_ID.into(),
                },
            )
            .unwrap(),
        ],
        credentials: Vec::new(),
    };
    let undocumented = discovered_go_contract(
        vec!["mimo-v2.5".into()],
        &crate::goat::OfficialProtocolBaseline::Unavailable,
        &saved_scope,
        &saved,
        &projection,
        "https://example.test",
    )
    .unwrap();
    assert!(select_verification_model(&undocumented, None).is_err());
    let baseline = crate::goat::OfficialProtocolBaseline::mapped_protocols([(
        "future-only",
        vec![
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::Messages,
        ],
    )]);
    saved.preferences.insert(
        key.clone(),
        vec![("future-only".into(), UpstreamProtocolKind::Messages)],
    );
    let discovered = discovered_go_contract(
        vec!["future-only".into()],
        &baseline,
        &saved_scope,
        &saved,
        &projection,
        "https://example.test",
    )
    .unwrap();
    assert_eq!(
        select_verification_model(&discovered, None)
            .unwrap()
            .protocol,
        UpstreamProtocolKind::Messages
    );
    saved.overrides.insert(
        key.clone(),
        vec![PersistedModelProtocolOverride {
            scope: key,
            model_id: "future-only".into(),
            protocol: UpstreamProtocolKind::Messages,
            state: ProtocolOverrideState::ForceOff,
            updated_at: Utc::now(),
        }],
    );
    let discovered = discovered_go_contract(
        vec!["future-only".into()],
        &baseline,
        &saved_scope,
        &saved,
        &projection,
        "https://example.test",
    )
    .unwrap();
    assert_eq!(
        select_verification_model(&discovered, None)
            .unwrap()
            .protocol,
        UpstreamProtocolKind::Responses
    );
    projection.destinations[0].enabled = false;
    let discovered = discovered_go_contract(
        vec!["future-only".into()],
        &baseline,
        &saved_scope,
        &saved,
        &projection,
        "https://example.test",
    )
    .unwrap();
    assert!(select_verification_model(&discovered, None).is_err());
    assert!(saved.scopes.is_empty());
    assert!(projection.destinations[0].catalog.is_empty());
}

#[test]
fn go_directory_parser_and_official_docs_enable_a_real_fresh_projection() {
    let dir = std::env::temp_dir().join(format!("ocg-verification-fresh-{}", uuid::Uuid::new_v4()));
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let key = ContractScope::provider(OPENCODE_PROVIDER_ID);
    db.refresh_contract_catalog_preserving_settings(
        &key,
        &[],
        Utc::now(),
        CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
    )
    .unwrap();
    let saved = db.load_persisted_contracts().unwrap();
    let projection = crate::destination_projection::load_runtime(&db).unwrap();
    let mut contracts = build_effective_contracts(&Default::default(), &[], saved.clone());
    contracts.apply_destination_configuration(&projection);
    let saved_scope = contracts.providers.get(OPENCODE_PROVIDER_ID).unwrap();
    assert!(saved_scope.catalog.models.is_empty());
    assert!(!saved_scope.catalog_routable);

    let discovery = crate::goat::parse_provider_catalog_discovery(
        br#"{"data":[{"id":"future-only","supported_endpoints":["/v1/messages"]}]}"#,
        "OpenCode Go",
    )
    .unwrap();
    assert_eq!(
        discovery.protocol_baseline,
        crate::goat::OfficialProtocolBaseline::Unavailable
    );
    let no_docs = discovered_go_contract(
        discovery.models.clone(),
        &discovery.protocol_baseline,
        saved_scope,
        &saved,
        &projection,
        "https://opencode.ai/zen/go",
    )
    .unwrap();
    assert!(select_verification_model(&no_docs, None).is_err());

    let docs = crate::official_protocols::parse_go_official_protocols(
        "<table><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th></tr><tr><td>Future</td><td>future-only</td><td>https://opencode.ai/zen/go/v1/responses</td><td>@ai-sdk/openai</td></tr></table>",
    ).unwrap();
    let baseline = discovery
        .protocol_baseline
        .prefer_catalog(crate::goat::OfficialProtocolBaseline::mapped(docs));
    let discovered = discovered_go_contract(
        discovery.models,
        &baseline,
        saved_scope,
        &saved,
        &projection,
        "https://opencode.ai/zen/go",
    )
    .unwrap();
    assert_eq!(
        select_verification_model(&discovered, None).unwrap(),
        VerificationModel {
            model: "future-only".into(),
            protocol: UpstreamProtocolKind::Responses,
        }
    );
    assert!(
        db.load_persisted_contracts()
            .unwrap()
            .scopes
            .get(&key)
            .unwrap()
            .catalog_models
            .is_empty()
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
