use super::*;
use crate::gateway::materialize::RouteRejectionCode;
use crate::kernel::protocol::ApiFormat;
use crate::routing_runtime::CandidateAvailability;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct CountingCipher {
    inner: crate::crypto::StaticKeyCipher,
    decrypts: Arc<AtomicUsize>,
}

impl crate::crypto::KeyCipher for CountingCipher {
    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        self.inner.encrypt(plaintext)
    }

    fn decrypt(&self, ciphertext: &str) -> anyhow::Result<String> {
        self.decrypts.fetch_add(1, Ordering::SeqCst);
        self.inner.decrypt(ciphertext)
    }
}

#[test]
fn parse_explain_query_defaults_protocol_and_rejects_blank_or_invalid() {
    let parsed = parse_explain_query(&ExplainQuery {
        model: Some(" glm-5.2 ".into()),
        client_protocol: None,
    })
    .unwrap();
    assert_eq!(parsed.0, "glm-5.2");
    assert_eq!(parsed.1, RoutingClientProtocol::ChatCompletions);

    let responses = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("responses".into()),
    })
    .unwrap();
    assert_eq!(responses.1, RoutingClientProtocol::Responses);

    let messages = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("messages".into()),
    })
    .unwrap();
    assert_eq!(messages.1, RoutingClientProtocol::Messages);

    let gemini = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("gemini".into()),
    })
    .unwrap();
    assert_eq!(gemini.1, RoutingClientProtocol::Gemini);

    assert!(
        parse_explain_query(&ExplainQuery {
            model: None,
            client_protocol: None,
        })
        .is_err()
    );
    assert!(
        parse_explain_query(&ExplainQuery {
            model: Some("   ".into()),
            client_protocol: None,
        })
        .is_err()
    );
    assert!(
        parse_explain_query(&ExplainQuery {
            model: Some("glm-5.2".into()),
            client_protocol: Some("".into()),
        })
        .is_err()
    );
    assert!(
        parse_explain_query(&ExplainQuery {
            model: Some("glm-5.2".into()),
            client_protocol: Some("grpc".into()),
        })
        .is_err()
    );
}

#[test]
fn blank_protocol_is_invalid_even_when_model_is_present() {
    let error = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("   ".into()),
    })
    .unwrap_err();
    assert!(!error.is_empty());
}

#[test]
fn prediction_uncertainty_is_the_fixed_runtime_only_set() {
    assert_eq!(
        RUNTIME_UNCERTAINTY,
        [
            RuntimeOnlyUncertainty::StateChangedAfterSnapshot,
            RuntimeOnlyUncertainty::ConversationBindingNotEvaluated,
            RuntimeOnlyUncertainty::RetryExclusionsNotApplied,
            RuntimeOnlyUncertainty::CredentialRecheckPending,
            RuntimeOnlyUncertainty::UpstreamResultUnknown,
        ]
    );
    assert_eq!(
        RoutingConversationBinding::NotEvaluated,
        RoutingConversationBinding::NotEvaluated
    );
}

#[test]
fn materialize_and_availability_codes_map_to_the_wire_enum() {
    assert_eq!(
        exclusion_code(RouteRejectionCode::MappingProtocolIncompatible),
        RoutingExclusionCode::MappingProtocolIncompatible
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::CredentialDisabled),
        RoutingExclusionCode::CredentialDisabled
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::BindingDisabled),
        RoutingExclusionCode::BindingDisabled
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::ModelScopeDenied),
        RoutingExclusionCode::ModelScopeDenied
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::GoatNotEligible),
        RoutingExclusionCode::GoatNotEligible
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::GoatUnverified),
        RoutingExclusionCode::GoatUnverified
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::CandidateMaterializationFailed),
        RoutingExclusionCode::CandidateMaterializationFailed
    );
    assert_eq!(
        exclusion_code(RouteRejectionCode::ProductionRouteUnsupported),
        RoutingExclusionCode::ProductionRouteUnsupported
    );
    assert_eq!(availability_code(CandidateAvailability::Available), None);
    assert_eq!(
        availability_code(CandidateAvailability::AccountDisabled),
        Some(RoutingExclusionCode::AccountDisabled)
    );
    assert_eq!(
        availability_code(CandidateAvailability::SetupNotReady),
        Some(RoutingExclusionCode::SetupNotReady)
    );
    assert_eq!(
        availability_code(CandidateAvailability::ChannelMismatch),
        Some(RoutingExclusionCode::ChannelMismatch)
    );
    assert_eq!(
        availability_code(CandidateAvailability::CredentialMissing),
        Some(RoutingExclusionCode::CredentialMissing)
    );
    assert_eq!(
        availability_code(CandidateAvailability::AuthError),
        Some(RoutingExclusionCode::AuthError)
    );
    assert_eq!(
        availability_code(CandidateAvailability::CoolingDown),
        Some(RoutingExclusionCode::CoolingDown)
    );
    assert_eq!(
        availability_code(CandidateAvailability::FreeChannelUnavailable),
        Some(RoutingExclusionCode::FreeChannelUnavailable)
    );
}

#[test]
fn minimal_parsed_request_is_non_empty_and_does_not_require_a_send() {
    for protocol in [
        RoutingClientProtocol::ChatCompletions,
        RoutingClientProtocol::Responses,
        RoutingClientProtocol::Messages,
        RoutingClientProtocol::Gemini,
    ] {
        let parsed = minimal_parsed_request(protocol, "glm-5.2").unwrap();
        assert_eq!(parsed.requested_model, "glm-5.2");
        assert!(!parsed.stream);
        match protocol {
            RoutingClientProtocol::ChatCompletions => {
                assert_eq!(parsed.client, ApiFormat::ChatCompletions)
            }
            RoutingClientProtocol::Responses => assert_eq!(parsed.client, ApiFormat::Responses),
            RoutingClientProtocol::Messages => assert_eq!(parsed.client, ApiFormat::Messages),
            RoutingClientProtocol::Gemini => assert_eq!(parsed.client, ApiFormat::Gemini),
        }
    }
}

#[test]
fn explain_reads_real_state_without_logs_decrypts_or_selector_advancement() {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-routing-explain-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let decrypts = Arc::new(AtomicUsize::new(0));
    let cipher: Arc<dyn crate::crypto::KeyCipher + Send + Sync> = Arc::new(CountingCipher {
        inner: crate::crypto::StaticKeyCipher::new("routing-explain-read-only"),
        decrypts: decrypts.clone(),
    });
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let now = chrono::Utc::now();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: "018f6d5e-8a22-7f00-8000-000000000001".into(),
        name: "Explain".into(),
        endpoint_url: "https://explain.invalid/v1".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "explain-public".into(),
            upstream_model: "explain-upstream".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    };
    let account = crate::models::Account {
        id: "routing-explain-account".into(),
        provider_id: runtime.id.clone(),
        credential_kind: runtime.auth_kind.credential_kind(),
        quota_scope: runtime.auth_kind.quota_scope(),
        name: "Explain Key".into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt("test-secret").unwrap(),
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    db.create_dynamic_provider(&runtime, &account).unwrap();
    let state = Arc::new(crate::state::CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let before_revision = state.settings_revision();
    let before_logs: i64 = state
        .db
        .lock()
        .conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap();
    let first = explain_model(
        &state,
        "explain-public",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap_or_else(|error| panic!("real-state routing explanation failed: {error:?}"));
    let second = explain_model(
        &state,
        "explain-public",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap_or_else(|error| panic!("repeated routing explanation failed: {error:?}"));
    let after_logs: i64 = state
        .db
        .lock()
        .conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(state.settings_revision(), before_revision);
    assert_eq!(after_logs, before_logs);
    assert_eq!(decrypts.load(Ordering::SeqCst), 0);
    assert_eq!(
        first.expected_base_policy_first_pick,
        second.expected_base_policy_first_pick
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
