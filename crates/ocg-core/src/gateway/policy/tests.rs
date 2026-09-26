use super::*;
use ocg_gateway::classify::{ErrorProfile, ProviderErrorClass};

fn goat_credits() -> PolicyInput {
    PolicyInput {
        adapter: ProviderAdapterKind::CommandCodeGoat,
        class: ProviderErrorClass::InsufficientCredits,
        http_status: Some(400),
        error: Some(TopLevelError {
            code: Some("BAD_REQUEST".into()),
            error_type: Some("invalid_request_error".into()),
            message: Some("You have insufficient credits".into()),
        }),
    }
}

#[test]
fn builtin_goat_credits_is_temporary_unavailable_credential_model() {
    let snapshot = EffectivePolicySnapshot::builtin();
    let decision = evaluate(&snapshot, "dest-a", &goat_credits())
        .into_iter()
        .next()
        .expect("goat credits");
    assert_eq!(decision.action, PolicyAction::TemporaryUnavailable);
    assert_eq!(decision.scope, RestrictionScope::CredentialModel);
    assert_eq!(decision.source.owner, BUILTIN_OWNER);
    assert_eq!(decision.source.rule_id, GOAT_CREDITS_REJECTION_RULE);
    assert_eq!(decision.backoff.initial_secs, 30);
    assert_eq!(decision.backoff.max_secs, 300);
}

#[test]
fn builtin_does_not_reread_error_text_or_match_other_classes() {
    for class in [
        ProviderErrorClass::ClientError,
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::CommandCodeGoat,
        },
        ProviderErrorClass::ServerError,
        ProviderErrorClass::UnauthorizedPassthrough,
    ] {
        assert_eq!(
            evaluate(
                &EffectivePolicySnapshot::builtin(),
                "dest-a",
                &PolicyInput {
                    adapter: ProviderAdapterKind::CommandCodeGoat,
                    class,
                    http_status: Some(400),
                    error: goat_credits().error.clone(),
                }
            )
            .len(),
            0,
            "{class:?}"
        );
    }
}

#[test]
fn custom_status_only_matches_without_body_and_timeout_has_no_message() {
    let rule = ConfiguredRule::Custom {
        id: "status-500".into(),
        destination_id: None,
        enabled: true,
        scope: RestrictionScope::Credential,
        matcher: CustomMatch {
            status_codes: vec![500],
            error_codes: Vec::new(),
            error_types: Vec::new(),
            message_contains: Vec::new(),
        },
        backoff: PolicyBackoff::default(),
    };
    let snapshot = compile_snapshot(&[rule], &EffectivePolicySnapshot::builtin(), 2);
    let hit = PolicyInput {
        adapter: ProviderAdapterKind::CommandCodeGoat,
        class: ProviderErrorClass::ServerError,
        http_status: Some(500),
        error: None,
    };
    assert!(
        evaluate(&snapshot, "dest-a", &hit)
            .iter()
            .any(|d| d.source.rule_id == "status-500")
    );
    let timeout = PolicyInput {
        adapter: ProviderAdapterKind::CommandCodeGoat,
        class: ProviderErrorClass::ServerError,
        http_status: None,
        error: Some(TopLevelError {
            code: None,
            error_type: None,
            message: Some("timed out".into()),
        }),
    };
    assert!(
        evaluate(&snapshot, "dest-a", &timeout)
            .iter()
            .all(|d| d.source.rule_id != "status-500")
    );
}

#[test]
fn extract_top_level_error_ignores_nested_echoes() {
    let body = r#"{"error":{"code":"X","type":"Y","message":"hello"},"request":{"error":{"code":"NOPE"}}}"#;
    let error = extract_top_level_error(body).unwrap();
    assert_eq!(error.code.as_deref(), Some("X"));
    assert_eq!(error.error_type.as_deref(), Some("Y"));
    assert_eq!(error.message.as_deref(), Some("hello"));
    assert!(extract_top_level_error("not json").is_none());
}

#[test]
fn destination_override_keeps_global_generation_for_other_dest() {
    let previous = EffectivePolicySnapshot::builtin();
    let global_gen = previous.layers[0].source.rule_generation;
    let override_rule = ConfiguredRule::BuiltinOverride {
        id: GOAT_CREDITS_REJECTION_RULE.into(),
        destination_id: Some("dest-a".into()),
        enabled: false,
        backoff: None,
    };
    let snapshot = compile_snapshot(&[override_rule], &previous, 3);
    assert!(
        snapshot.effective_for("dest-a").is_empty()
            || snapshot
                .effective_for("dest-a")
                .iter()
                .all(|rule| rule.source.rule_id != GOAT_CREDITS_REJECTION_RULE || !rule.enabled)
    );
    let for_b = snapshot.effective_for("dest-b");
    let builtin = for_b
        .iter()
        .find(|rule| rule.source.rule_id == GOAT_CREDITS_REJECTION_RULE)
        .unwrap();
    assert_eq!(builtin.source.rule_generation, global_gen);
    assert!(snapshot.is_live(&builtin.source, "dest-b"));
    assert!(!snapshot.is_live(&builtin.source, "dest-a"));
}

#[test]
fn same_id_recreation_gets_a_new_generation() {
    let first = compile_snapshot(&[], &EffectivePolicySnapshot::builtin(), 1);
    let deleted = compile_snapshot(
        &[ConfiguredRule::Custom {
            id: "r1".into(),
            destination_id: None,
            enabled: true,
            scope: RestrictionScope::Credential,
            matcher: CustomMatch {
                status_codes: vec![500],
                error_codes: Vec::new(),
                error_types: Vec::new(),
                message_contains: Vec::new(),
            },
            backoff: PolicyBackoff::default(),
        }],
        &first,
        2,
    );
    let generation = deleted
        .layers
        .iter()
        .find(|layer| layer.source.rule_id == "r1")
        .unwrap()
        .source
        .rule_generation;
    let removed = compile_snapshot(&[], &deleted, 3);
    let recreated = compile_snapshot(
        &[ConfiguredRule::Custom {
            id: "r1".into(),
            destination_id: None,
            enabled: true,
            scope: RestrictionScope::Credential,
            matcher: CustomMatch {
                status_codes: vec![500],
                error_codes: Vec::new(),
                error_types: Vec::new(),
                message_contains: Vec::new(),
            },
            backoff: PolicyBackoff::default(),
        }],
        &removed,
        4,
    );
    let again = recreated
        .layers
        .iter()
        .find(|layer| layer.source.rule_id == "r1")
        .unwrap()
        .source
        .rule_generation;
    assert_ne!(generation, again);
}

#[test]
fn persisted_document_round_trip_is_camel_case() {
    let rules = [ConfiguredRule::Custom {
        id: "status-500".into(),
        destination_id: None,
        enabled: true,
        scope: RestrictionScope::Credential,
        matcher: CustomMatch {
            status_codes: vec![500],
            error_codes: Vec::new(),
            error_types: Vec::new(),
            message_contains: Vec::new(),
        },
        backoff: PolicyBackoff {
            initial_secs: 12,
            max_secs: 40,
        },
    }];
    let encoded = encode_policy_document(&rules).unwrap();
    assert!(encoded.contains("\"initialSeconds\":12"));
    assert!(encoded.contains("\"statusCodes\""));
    let parsed = parse_policy_document(&encoded).unwrap();
    assert_eq!(parsed, rules);
    assert!(parse_policy_document("{}").is_err());
}
