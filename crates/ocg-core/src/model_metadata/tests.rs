use super::*;
use PublishedUpstreamProtocol::{ChatCompletions, Messages, Responses};
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Grants, HttpProtocolRoute, LegacyDestinationRef, ModelResolution,
    sealed_capabilities,
};

fn metadata(context: u64, output: u64, inputs: &[&str], efforts: &[(&str, &str)]) -> ModelMetadata {
    ModelMetadata {
        context_window: Some(context),
        max_output_tokens: Some(output),
        input_modalities: Some(inputs.iter().map(|s| s.to_string()).collect()),
        reasoning: Some(true),
        reasoning_efforts: Some(
            efforts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ),
        ..Default::default()
    }
}

#[test]
fn catalog_facts_are_normalized_without_model_name_guessing() {
    let rows = parse_catalog(br#"{"data":[{"id":"private-model","name":"Local model","context_length":262144,"max_output_tokens":32768,"input":["text","image"],"reasoning":true,"reasoningEfforts":{"low":"low","xhigh":"max"}}]}"#);
    let row = &rows["private-model"];
    assert_eq!(row.context_window, Some(262144));
    assert_eq!(row.max_output_tokens, Some(32768));
    assert_eq!(row.reasoning_efforts.as_ref().unwrap()["xhigh"], "max");
    assert_eq!(row.input_modalities.as_ref().unwrap(), &["text", "image"]);
}

#[test]
fn unknown_is_not_a_fabricated_capacity_or_effort_list() {
    let rows =
        parse_catalog(br#"{"data":[{"id":"gpt-private"},{"id":"thinking","reasoning":true}]}"#);
    assert_eq!(rows["gpt-private"], ModelMetadata::default());
    assert_eq!(rows["thinking"].reasoning, Some(true));
    assert_eq!(rows["thinking"].reasoning_efforts, None);
}

#[test]
fn fallback_alias_uses_minimum_limits_and_capability_intersection() {
    let a = metadata(
        262144,
        32768,
        &["text", "image"],
        &[("low", "low"), ("high", "high"), ("xhigh", "max")],
    );
    let b = metadata(
        131072,
        16384,
        &["text"],
        &[("low", "low"), ("high", "default")],
    );
    let result = common(&[a, b]);
    assert_eq!(result.context_window, Some(131072));
    assert_eq!(result.max_output_tokens, Some(16384));
    assert_eq!(result.input_modalities, Some(vec!["text".into()]));
    assert_eq!(
        result.reasoning_efforts,
        Some(BTreeMap::from([("low".into(), "low".into())]))
    );
}

#[test]
fn unknown_fallback_blocks_positive_claims() {
    let result = common(&[
        metadata(262144, 32768, &["text", "image"], &[("high", "high")]),
        ModelMetadata::default(),
    ]);
    assert_eq!(result.context_window, None);
    assert_eq!(result.input_modalities, None);
    assert_eq!(result.reasoning, None);
    assert_eq!(result.reasoning_efforts, None);
}

#[test]
fn invalid_metadata_is_rejected() {
    for value in [
        json!({"contextWindow":0}),
        json!({"contextWindow":100,"maxOutputTokens":200}),
        json!({"reasoningEfforts":{"ultra":"ultra"}}),
        json!({"reasoning":false,"reasoningEfforts":{"high":"high"}}),
        json!({"inputModalities":["text","text"]}),
        json!({"reasoningEfforts":{"high":"bad\nvalue"}}),
        json!({"toolCalling":false,"parallelToolCalls":true}),
    ] {
        let metadata: ModelMetadata = serde_json::from_value(value).unwrap();
        assert!(metadata.validate().is_err());
    }
}

#[test]
fn raw_secrets_and_unrecognized_fields_are_never_copied() {
    let rows = parse_catalog(br#"{"data":[{"id":"a","contextWindow":8000,"api_key":"secret","headers":{"Authorization":"secret"},"ocg":{"schemaVersion":1,"contextWindow":9000,"arbitrary":"secret"}}]}"#);
    let encoded = serde_json::to_string(&rows).unwrap();
    assert!(!encoded.contains("secret"));
    assert_eq!(rows["a"].context_window, Some(9000));
}

#[test]
fn exported_schema_version_2_metadata_round_trips_without_the_protocol_profile() {
    let exported = serde_json::to_vec(&json!({
        "data": [{
            "id": "upstream-v2",
            "name": "Root name",
            "contextWindow": 1000,
            "ocg": {
                "schemaVersion": 2,
                "name": "Declared name",
                "contextWindow": 32000,
                "maxOutputTokens": 4096,
                "inputModalities": ["text", "image"],
                "outputModalities": ["text"],
                "reasoning": true,
                "reasoningEfforts": {"low": "low", "high": "high"},
                "toolCalling": true,
                "parallelToolCalls": false,
                "protocols": {
                    "preferred": "messages",
                    "supported": ["chat_completions", "messages"]
                },
                "clientProtocol": "openai-completions",
                "messagesControls": {"effort": "max"},
                "arbitrary": "raw-secret"
            }
        }]
    }))
    .unwrap();
    let parsed = parse_catalog(&exported);
    let metadata = &parsed["upstream-v2"];
    assert_eq!(metadata.name.as_deref(), Some("Declared name"));
    assert_eq!(metadata.context_window, Some(32_000));
    assert_eq!(metadata.max_output_tokens, Some(4096));
    assert_eq!(
        metadata.input_modalities,
        Some(vec!["text".into(), "image".into()])
    );
    assert_eq!(metadata.output_modalities, Some(vec!["text".into()]));
    assert_eq!(metadata.reasoning, Some(true));
    assert_eq!(metadata.tool_calling, Some(true));
    assert_eq!(metadata.parallel_tool_calls, Some(false));
    assert_eq!(
        metadata.reasoning_efforts,
        Some(BTreeMap::from([
            ("high".into(), "high".into()),
            ("low".into(), "low".into()),
        ]))
    );

    let ignored = parse_catalog(
        &serde_json::to_vec(&json!({
            "data": [{
                "id": "upstream-v3",
                "contextWindow": 1000,
                "ocg": {
                    "schemaVersion": 3,
                    "contextWindow": 9,
                    "inputModalities": ["image"],
                    "reasoningEfforts": {"max": "max"},
                    "toolCalling": true,
                    "protocols": {
                        "preferred": "chat_completions",
                        "supported": ["chat_completions"]
                    }
                }
            }]
        }))
        .unwrap(),
    );
    assert_eq!(ignored["upstream-v3"].context_window, Some(1000));
    assert_eq!(ignored["upstream-v3"].input_modalities, None);
    assert_eq!(ignored["upstream-v3"].reasoning_efforts, None);
    assert_eq!(ignored["upstream-v3"].tool_calling, None);

    let dir =
        std::env::temp_dir().join(format!("ocg-metadata-v2-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let mut destination = route_fixture();
    destination.catalog[0].public_model = "public-v2".into();
    destination.catalog[0].upstream_model = "upstream-v2".into();
    observe(&db, &destination, &parsed).unwrap();
    let stored = load(&db).unwrap();
    let observed = stored
        .iter()
        .find(|record| record.public_model == "public-v2")
        .and_then(|record| record.observed.as_ref())
        .unwrap();
    assert_eq!(observed, metadata);
    let persisted = serde_json::to_value(observed).unwrap();
    let keys: BTreeSet<_> = persisted.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "contextWindow".to_string(),
            "inputModalities".to_string(),
            "maxOutputTokens".to_string(),
            "name".to_string(),
            "outputModalities".to_string(),
            "parallelToolCalls".to_string(),
            "reasoning".to_string(),
            "reasoningEfforts".to_string(),
            "toolCalling".to_string(),
        ])
    );
    let encoded = serde_json::to_string(observed).unwrap();
    assert!(!encoded.contains("protocols"));
    assert!(!encoded.contains("messagesControls"));
    assert!(!encoded.contains("clientProtocol"));
    assert!(!encoded.contains("raw-secret"));
    assert!(!encoded.contains("openai-completions"));
    drop(db);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn duplicate_rows_only_keep_common_guarantees() {
    let rows = parse_catalog(
        br#"{"data":[{"id":"a","contextWindow":8000},{"id":"a","contextWindow":4000}]}"#,
    );
    assert_eq!(rows["a"].context_window, Some(4000));
}

#[test]
fn invalid_reported_metadata_withdraws_the_fact_instead_of_preserving_a_stale_record() {
    let rows = parse_catalog(br#"{"data":[{"id":"a","contextWindow":100,"maxTokens":200}]}"#);
    assert_eq!(rows["a"], ModelMetadata::default());
}

#[test]
fn secret_echoes_become_unknown_without_deleting_the_model_identity() {
    let mut facts = ModelMetadata {
        name: Some("echo private-key".into()),
        context_window: Some(8000),
        ..Default::default()
    };
    facts.redact_secret("private-key");
    assert_eq!(facts, ModelMetadata::default());
    let mut facts = ModelMetadata {
        context_window: Some(8000),
        ..Default::default()
    };
    facts.redact_secret("");
    assert_eq!(facts.context_window, Some(8000));
}

#[test]
fn disjoint_known_modalities_do_not_turn_into_an_unknown_text_fallback() {
    let result = common(&[
        metadata(8000, 1000, &["text"], &[]),
        metadata(8000, 1000, &["image"], &[]),
    ]);
    assert_eq!(result.input_modalities, Some(vec![]));
}

fn route_fixture() -> Destination {
    use ocg_domain::destination::*;
    Destination {
        id: "route-one".into(),
        legacy: LegacyDestinationRef::Dynamic("test".into()),
        adapter: AdapterKind::Http,
        name: "test".into(),
        brand_family: None,
        base_url: Some("https://example.test/v1".into()),
        protocols: vec![Protocol::ChatCompletions],
        protocol_routes: vec![],
        auth_scheme: AuthScheme::Bearer,
        model_resolution: ModelResolution::PublicAndUpstream,
        catalog: vec![CatalogModel {
            public_model: "public".into(),
            upstream_model: "upstream".into(),
            protocols: vec![Protocol::ChatCompletions],
            preferred: Some(Protocol::ChatCompletions),
            enabled: true,
            upstream_override: None,
        }],
        capabilities: sealed_capabilities(AdapterKind::Http),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

#[test]
fn declarations_are_bound_to_the_exact_destination_route_and_model_mapping() {
    let destination = route_fixture();
    let model = &destination.catalog[0];
    let mut records = vec![];
    let record = record_for(&mut records, &destination, model);
    record.observed = Some(ModelMetadata {
        context_window: Some(8000),
        ..Default::default()
    });
    record.declared = Some(ModelMetadata {
        context_window: Some(16000),
        ..Default::default()
    });
    assert_eq!(
        effective(&records, &destination, model).0.context_window,
        Some(16000)
    );
    assert_eq!(effective(&records, &destination, model).1, "operator");
    let mut changed = destination.clone();
    changed.base_url = Some("https://other.test/v1".into());
    assert_eq!(effective(&records, &changed, model).1, "unknown");
    let record = record_for(&mut records, &changed, model);
    assert!(record.observed.is_none() && record.declared.is_none());
    let record = record_for(&mut records, &destination, model);
    record.observed = Some(ModelMetadata {
        context_window: Some(8000),
        ..Default::default()
    });
    let mut changed_model = model.clone();
    changed_model.upstream_model = "different".into();
    assert_eq!(
        effective(&records, &destination, &changed_model).1,
        "unknown"
    );
}

#[test]
fn modelsdev_fills_only_fields_the_route_never_learned() {
    let destination = route_fixture();
    let model = &destination.catalog[0];
    let mut catalog = crate::modelsdev::ModelsDevCatalog::default();
    catalog.models.insert(
        "upstream".into(),
        ModelMetadata {
            context_window: Some(64000),
            input_modalities: Some(vec!["text".into(), "image".into()]),
            ..Default::default()
        },
    );

    // No record: models.dev supplies the facts.
    let records = vec![];
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "modelsdev");
    assert!(filled);
    assert_eq!(metadata.context_window, Some(64000));

    // An upstream observation outranks the public catalog per field: known
    // fields stay route-specific, fields it never learned are filled.
    let mut records = vec![];
    record_for(&mut records, &destination, model).observed = Some(ModelMetadata {
        context_window: Some(8000),
        ..Default::default()
    });
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "upstream");
    assert!(filled);
    assert_eq!(metadata.context_window, Some(8000));
    assert_eq!(
        metadata.input_modalities,
        Some(vec!["text".into(), "image".into()])
    );

    // An observation that already knows everything leaves nothing to fill.
    record_for(&mut records, &destination, model).observed = Some(ModelMetadata {
        context_window: Some(8000),
        input_modalities: Some(vec!["text".into()]),
        ..Default::default()
    });
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "upstream");
    assert!(!filled);
    assert_eq!(metadata.input_modalities, Some(vec!["text".into()]));

    // An operator declaration outranks both and stays untouched.
    record_for(&mut records, &destination, model).declared = Some(ModelMetadata {
        context_window: Some(16000),
        ..Default::default()
    });
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "operator");
    assert!(!filled);
    assert_eq!(metadata.context_window, Some(16000));
    assert_eq!(metadata.input_modalities, None);

    // A route change invalidates the record; models.dev still applies.
    let mut changed = destination.clone();
    changed.base_url = Some("https://other.test/v1".into());
    let (metadata, source, _) = effective_with_catalog(&records, &catalog, &changed, model);
    assert_eq!(source, "modelsdev");
    assert_eq!(metadata.context_window, Some(64000));

    // No catalog entry and no route-specific record: stays unknown.
    let empty = crate::modelsdev::ModelsDevCatalog::default();
    let (_, source, filled) = effective_with_catalog(&[], &empty, &destination, model);
    assert_eq!(source, "unknown");
    assert!(!filled);
}

#[test]
fn parsed_offerings_keep_operator_replacement_and_empty_upstream_tiers() {
    let destination = route_fixture();
    let model = &destination.catalog[0];
    let catalog = crate::modelsdev::parse_api(
        br#"{
        "lab": {"models": {"upstream": {
            "limit": {"context": 64000},
            "modalities": {"input": ["text", "image"]},
            "reasoning": true,
            "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]
        }}}
    }"#,
    );

    let (metadata, source, filled) = effective_with_catalog(&[], &catalog, &destination, model);
    assert_eq!(source, "modelsdev");
    assert!(filled);
    assert_eq!(
        metadata
            .reasoning_efforts
            .as_ref()
            .unwrap()
            .get("high")
            .map(String::as_str),
        Some("high")
    );

    let mut records = vec![];
    record_for(&mut records, &destination, model).observed = Some(ModelMetadata {
        reasoning: Some(true),
        reasoning_efforts: Some(BTreeMap::new()),
        ..Default::default()
    });
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "upstream");
    assert!(filled);
    assert_eq!(metadata.context_window, Some(64000));
    assert_eq!(metadata.reasoning_efforts, Some(BTreeMap::new()));

    record_for(&mut records, &destination, model).declared = Some(ModelMetadata {
        context_window: Some(16000),
        ..Default::default()
    });
    let (metadata, source, filled) =
        effective_with_catalog(&records, &catalog, &destination, model);
    assert_eq!(source, "operator");
    assert!(!filled);
    assert_eq!(metadata.context_window, Some(16000));
    assert_eq!(metadata.reasoning_efforts, None);
    assert_eq!(metadata.input_modalities, None);
}

fn no_cooldowns() -> ocg_domain::destination::Cooldowns {
    ocg_domain::destination::Cooldowns {
        generic_until: None,
        five_hour_until: None,
        week_until: None,
        month_until: None,
        free_until: None,
    }
}

fn open_destination(
    id: &str,
    public_model: &str,
    upstream_model: &str,
    protocols: &[Protocol],
    preferred: Option<Protocol>,
) -> Destination {
    Destination {
        id: id.into(),
        legacy: LegacyDestinationRef::Dynamic(id.into()),
        adapter: AdapterKind::Http,
        name: id.into(),
        brand_family: None,
        base_url: Some("https://models.example/v1".into()),
        protocols: protocols.to_vec(),
        protocol_routes: Vec::new(),
        auth_scheme: AuthScheme::None,
        model_resolution: ModelResolution::PublicAndUpstream,
        catalog: vec![CatalogModel {
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            protocols: protocols.to_vec(),
            preferred,
            enabled: true,
            upstream_override: None,
        }],
        capabilities: sealed_capabilities(AdapterKind::Http),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

fn bearer_destination(
    id: &str,
    public_model: &str,
    upstream_model: &str,
    protocols: &[Protocol],
    preferred: Option<Protocol>,
) -> Destination {
    let mut destination = open_destination(id, public_model, upstream_model, protocols, preferred);
    destination.auth_scheme = AuthScheme::Bearer;
    destination.protocol_routes = protocols
        .iter()
        .map(|protocol| HttpProtocolRoute {
            protocol: *protocol,
            endpoint_url: format!("https://models.example/v1/{}", protocol.as_str()),
            auth_scheme: AuthScheme::Bearer,
        })
        .collect();
    destination.base_url = destination
        .protocol_routes
        .first()
        .map(|route| route.endpoint_url.clone());
    destination
}

fn ranked_credential(
    destination_id: &str,
    rank: u32,
    scope: ModelScope,
    binding_enabled: bool,
    key_cipher: &str,
) -> (
    ocg_domain::destination::Credential,
    crate::routing_snapshot::ExecutionCredential,
) {
    let credential_id = format!("cred-{destination_id}-{rank}");
    let legacy_id = format!("acct-{destination_id}-{rank}");
    let name = format!("key-{rank}");
    let domain = ocg_domain::destination::Credential {
        id: credential_id.clone(),
        legacy_account_id: legacy_id.clone(),
        destination_id: destination_id.to_string(),
        name: name.clone(),
        notes: None,
        has_secret: !key_cipher.is_empty(),
        enabled: true,
        routing_rank: rank,
        scope: scope.clone(),
        grants: Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        },
        auth_state: ocg_domain::credential::AuthState::Unknown,
        last_error: None,
        cooldowns: no_cooldowns(),
        quota_pool_id: None,
        onboarding_task: None,
        purchase_date: None,
    };
    let execution = crate::routing_snapshot::ExecutionCredential {
        id: legacy_id,
        credential_id,
        destination_id: destination_id.to_string(),
        provider_id: "test".into(),
        name,
        key_cipher: key_cipher.to_string(),
        enabled: true,
        ready: true,
        auth_error: None,
        cooldowns: no_cooldowns(),
        binding_id: format!("binding-{destination_id}-{rank}"),
        binding_enabled,
        credential_version: 1,
        authorization_connection_id: format!("connection-{destination_id}-{rank}"),
        scope,
        grants: Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        },
        quota_recovery: None,
        quota_probe: false,
        goat_plan: crate::goat_plan_cooldowns::GoatPlanCooldowns::default(),
    };
    (domain, execution)
}

fn grant_protocols(
    destination: &Destination,
    credential: &mut crate::routing_snapshot::ExecutionCredential,
    allowed: &[Protocol],
) {
    use ocg_domain::connection::ConnectionId;
    use ocg_domain::credential::{assigned_endpoints_for_routes, safe_default_grants};
    use ocg_domain::destination::http_configured_routes;
    let connection: ConnectionId = serde_json::from_value(serde_json::Value::String(
        credential.authorization_connection_id.clone(),
    ))
    .unwrap();
    let routes = http_configured_routes(destination);
    let assigned = assigned_endpoints_for_routes(&connection, &routes);
    let kept: Vec<_> = routes
        .iter()
        .zip(assigned)
        .filter(|(route, _)| allowed.contains(&Protocol::from(route.operation)))
        .map(|(_, endpoint)| endpoint)
        .collect();
    let (ids, origins) = safe_default_grants(&kept);
    credential.grants.allowed_endpoint_ids = ids;
    credential.grants.allowed_origins = origins;
}

fn profile(
    preferred: PublishedUpstreamProtocol,
    supported: &[PublishedUpstreamProtocol],
) -> PublishedModelProtocolProfile {
    PublishedModelProtocolProfile {
        preferred,
        supported: supported.to_vec(),
    }
}

fn publish(
    destinations: Vec<Destination>,
    pairs: Vec<(
        ocg_domain::destination::Credential,
        crate::routing_snapshot::ExecutionCredential,
    )>,
    records: &[Record],
    requested: &str,
) -> Option<PublishedModelFacts> {
    let mut domain = Vec::with_capacity(pairs.len());
    let mut execution = Vec::with_capacity(pairs.len());
    for (row, runtime) in pairs {
        domain.push(row);
        execution.push(runtime);
    }
    let snapshot = crate::gateway::handler::RuntimeCatalogSnapshot::from_routing(
        crate::routing_snapshot::RoutingSnapshot {
            projection: crate::destination_projection::DestinationProjection {
                destinations,
                credentials: domain,
            },
            credentials: execution,
            ollama_pinned: Vec::new(),
        },
        chrono::Utc::now(),
    );
    published_model_facts(
        records,
        &crate::modelsdev::ModelsDevCatalog::default(),
        &snapshot,
        requested,
    )
}

fn assert_no_protocol(facts: Option<PublishedModelFacts>) {
    assert!(
        facts
            .as_ref()
            .and_then(|facts| facts.protocols.as_ref())
            .is_none()
    );
}

#[test]
fn saved_preferred_is_published_when_that_protocol_is_authorized() {
    let destination = open_destination(
        "saved",
        "shared-model",
        "upstream-saved",
        &[Protocol::Messages, Protocol::ChatCompletions],
        Some(Protocol::Messages),
    );
    let facts = publish(
        vec![destination],
        vec![ranked_credential(
            "saved",
            0,
            ModelScope::All,
            true,
            "cipher",
        )],
        &[],
        "shared-model",
    )
    .unwrap();
    assert_eq!(
        facts.protocols,
        Some(profile(Messages, &[ChatCompletions, Messages]))
    );
    let wire = serde_json::to_value(facts.protocols.unwrap()).unwrap();
    assert_eq!(wire["preferred"], json!("messages"));
    assert_eq!(wire["supported"], json!(["chat_completions", "messages"]));
}

#[test]
fn unavailable_preferred_uses_the_first_saved_authorized_protocol() {
    let mut disabled = open_destination(
        "disabled-preferred",
        "shared-model",
        "upstream-disabled",
        &[Protocol::ChatCompletions, Protocol::Messages],
        Some(Protocol::ChatCompletions),
    );
    disabled.catalog[0].protocols = vec![Protocol::Messages];
    let disabled_facts = publish(
        vec![disabled],
        vec![ranked_credential(
            "disabled-preferred",
            0,
            ModelScope::All,
            true,
            "cipher",
        )],
        &[],
        "shared-model",
    )
    .unwrap();
    assert_eq!(
        disabled_facts.protocols,
        Some(profile(Messages, &[Messages]))
    );

    let ungranted = bearer_destination(
        "ungranted-preferred",
        "shared-model",
        "upstream-ungranted",
        &[Protocol::Responses, Protocol::Messages],
        Some(Protocol::Responses),
    );
    let (domain, mut execution) =
        ranked_credential("ungranted-preferred", 0, ModelScope::All, true, "cipher");
    grant_protocols(&ungranted, &mut execution, &[Protocol::Messages]);
    let ungranted_facts = publish(
        vec![ungranted],
        vec![(domain, execution)],
        &[],
        "shared-model",
    )
    .unwrap();
    assert_eq!(
        ungranted_facts.protocols,
        Some(profile(Messages, &[Messages]))
    );
}

#[test]
fn lower_rank_credential_does_not_hide_a_protocol_another_key_can_carry() {
    let destination = bearer_destination(
        "union",
        "shared-model",
        "upstream-union",
        &[Protocol::ChatCompletions, Protocol::Messages],
        Some(Protocol::Messages),
    );
    let (first_domain, mut first) = ranked_credential("union", 0, ModelScope::All, true, "cipher");
    let (second_domain, mut second) =
        ranked_credential("union", 1, ModelScope::All, true, "cipher");
    grant_protocols(&destination, &mut first, &[Protocol::ChatCompletions]);
    grant_protocols(&destination, &mut second, &[Protocol::Messages]);
    let facts = publish(
        vec![destination],
        vec![(first_domain, first), (second_domain, second)],
        &[],
        "shared-model",
    )
    .unwrap();
    assert_eq!(
        facts.protocols,
        Some(profile(Messages, &[ChatCompletions, Messages]))
    );
}

#[test]
fn scope_binding_grant_key_and_enablement_leave_protocols_unpublished() {
    let open = open_destination(
        "open",
        "shared-model",
        "upstream-open",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    assert_no_protocol(publish(
        vec![open.clone()],
        vec![ranked_credential(
            "open",
            0,
            ModelScope::Only {
                models: vec!["other-model".into()],
            },
            true,
            "cipher",
        )],
        &[],
        "shared-model",
    ));
    assert_no_protocol(publish(
        vec![open.clone()],
        vec![ranked_credential(
            "open",
            0,
            ModelScope::All,
            false,
            "cipher",
        )],
        &[],
        "shared-model",
    ));
    let (domain, mut not_ready) = ranked_credential("open", 0, ModelScope::All, true, "cipher");
    not_ready.ready = false;
    assert_no_protocol(publish(
        vec![open],
        vec![(domain, not_ready)],
        &[],
        "shared-model",
    ));

    let locked = bearer_destination(
        "locked",
        "shared-model",
        "upstream-locked",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    let (keyed, mut keyless) = ranked_credential("locked", 0, ModelScope::All, true, "");
    grant_protocols(&locked, &mut keyless, &[Protocol::ChatCompletions]);
    assert_no_protocol(publish(
        vec![locked.clone()],
        vec![(keyed, keyless)],
        &[],
        "shared-model",
    ));
    assert_no_protocol(publish(
        vec![locked.clone()],
        vec![ranked_credential(
            "locked",
            0,
            ModelScope::All,
            true,
            "cipher",
        )],
        &[],
        "shared-model",
    ));

    let mut disabled_model = locked;
    disabled_model.catalog[0].enabled = false;
    let mut records = Vec::new();
    record_for(&mut records, &disabled_model, &disabled_model.catalog[0]).declared =
        Some(ModelMetadata {
            context_window: Some(999),
            ..Default::default()
        });
    let (domain, mut execution) = ranked_credential("locked", 0, ModelScope::All, true, "cipher");
    grant_protocols(
        &disabled_model,
        &mut execution,
        &[Protocol::ChatCompletions],
    );
    let facts = publish(
        vec![disabled_model],
        vec![(domain, execution)],
        &records,
        "shared-model",
    )
    .unwrap();
    assert!(facts.protocols.is_none());
    assert_eq!(facts.metadata.context_window, None);

    let mut disabled_destination = bearer_destination(
        "closed",
        "shared-model",
        "upstream-closed",
        &[Protocol::Messages],
        Some(Protocol::Messages),
    );
    disabled_destination.enabled = false;
    assert_no_protocol(publish(
        vec![disabled_destination],
        vec![ranked_credential(
            "closed",
            0,
            ModelScope::All,
            true,
            "cipher",
        )],
        &[],
        "shared-model",
    ));
}

#[test]
fn routing_rank_then_destination_id_selects_preferred_and_supported_is_the_union() {
    let routes = |zeta_rank: u32, alpha_rank: u32| {
        let zeta = open_destination(
            "zeta",
            "shared-model",
            "upstream-zeta",
            &[Protocol::Responses],
            Some(Protocol::Responses),
        );
        let alpha = open_destination(
            "alpha",
            "shared-model",
            "upstream-alpha",
            &[Protocol::Messages, Protocol::ChatCompletions],
            Some(Protocol::Messages),
        );
        publish(
            vec![zeta, alpha],
            vec![
                ranked_credential("zeta", zeta_rank, ModelScope::All, true, "cipher"),
                ranked_credential("alpha", alpha_rank, ModelScope::All, true, "cipher"),
            ],
            &[],
            "shared-model",
        )
        .unwrap()
        .protocols
    };
    let union = vec![ChatCompletions, Responses, Messages];
    assert_eq!(routes(0, 1), Some(profile(Responses, &union)));
    assert_eq!(routes(2, 1), Some(profile(Messages, &union)));
    assert_eq!(routes(5, 5), Some(profile(Messages, &union)));
}

#[test]
fn routing_rank_uses_the_projection_credential_id_only() {
    let real = open_destination(
        "real",
        "shared-model",
        "upstream-real",
        &[Protocol::Responses],
        Some(Protocol::Responses),
    );
    let lookalike = open_destination(
        "lookalike",
        "shared-model",
        "upstream-lookalike",
        &[Protocol::Messages],
        Some(Protocol::Messages),
    );
    let (real_domain, real_execution) =
        ranked_credential("real", 5, ModelScope::All, true, "cipher");
    let (decoy, mut execution) = ranked_credential("lookalike", 0, ModelScope::All, true, "cipher");
    assert_eq!(decoy.legacy_account_id, execution.id);
    assert_ne!(decoy.id, "cred-lookalike-unmatched");
    execution.credential_id = "cred-lookalike-unmatched".into();
    let facts = publish(
        vec![real, lookalike],
        vec![(real_domain, real_execution), (decoy, execution)],
        &[],
        "shared-model",
    )
    .unwrap();
    assert_eq!(
        facts.protocols,
        Some(profile(Responses, &[Responses, Messages]))
    );
}

#[test]
fn cooldown_probe_and_auth_error_do_not_change_the_protocol_profile() {
    let snapshot = |dirty: bool| {
        let destination = open_destination(
            "cooled",
            "shared-model",
            "upstream-cooled",
            &[Protocol::Messages, Protocol::ChatCompletions],
            Some(Protocol::Messages),
        );
        let (domain, mut execution) =
            ranked_credential("cooled", 0, ModelScope::All, true, "cipher");
        if dirty {
            let until = chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
            execution.auth_error = Some("upstream rejected".into());
            execution.quota_probe = true;
            execution.cooldowns.generic_until = Some(until);
            execution.cooldowns.five_hour_until = Some(until);
            execution.cooldowns.week_until = Some(until);
            execution.cooldowns.month_until = Some(until);
            execution.cooldowns.free_until = Some(until);
        }
        publish(
            vec![destination],
            vec![(domain, execution)],
            &[],
            "shared-model",
        )
        .unwrap()
        .protocols
    };
    let expected = Some(profile(Messages, &[ChatCompletions, Messages]));
    assert_eq!(snapshot(false), expected);
    assert_eq!(snapshot(true), expected);
}

#[test]
fn qualified_routes_keep_the_conservative_capability_intersection() {
    let alpha = open_destination(
        "alpha",
        "shared-model",
        "upstream-alpha",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    let beta = open_destination(
        "beta",
        "shared-model",
        "upstream-beta",
        &[Protocol::Messages],
        Some(Protocol::Messages),
    );
    let scoped = open_destination(
        "scoped",
        "shared-model",
        "upstream-scoped",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    let mut disabled_model = open_destination(
        "disabled-model",
        "shared-model",
        "upstream-disabled",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    disabled_model.catalog[0].enabled = false;
    let mut disabled_destination = open_destination(
        "disabled-destination",
        "shared-model",
        "upstream-closed",
        &[Protocol::ChatCompletions],
        Some(Protocol::ChatCompletions),
    );
    disabled_destination.enabled = false;
    let poison = ModelMetadata {
        context_window: Some(1),
        tool_calling: Some(false),
        reasoning: Some(true),
        ..Default::default()
    };
    let mut records = Vec::new();
    record_for(&mut records, &alpha, &alpha.catalog[0]).declared = Some(ModelMetadata {
        context_window: Some(8000),
        tool_calling: Some(true),
        ..Default::default()
    });
    record_for(&mut records, &beta, &beta.catalog[0]).declared = Some(ModelMetadata {
        context_window: Some(4000),
        tool_calling: Some(true),
        ..Default::default()
    });
    for destination in [&scoped, &disabled_model, &disabled_destination] {
        record_for(&mut records, destination, &destination.catalog[0]).declared =
            Some(poison.clone());
    }
    let facts = publish(
        vec![alpha, beta, scoped, disabled_model, disabled_destination],
        vec![
            ranked_credential("alpha", 0, ModelScope::All, true, "cipher"),
            ranked_credential("beta", 1, ModelScope::All, true, "cipher"),
            ranked_credential(
                "scoped",
                2,
                ModelScope::Only {
                    models: vec!["other-model".into()],
                },
                true,
                "cipher",
            ),
            ranked_credential("disabled-model", 3, ModelScope::All, true, "cipher"),
            ranked_credential("disabled-destination", 4, ModelScope::All, true, "cipher"),
        ],
        &records,
        "shared-model",
    )
    .unwrap();
    assert_eq!(facts.metadata.context_window, Some(4000));
    assert_eq!(facts.metadata.tool_calling, Some(true));
    assert_eq!(facts.metadata.reasoning, None);
    assert_eq!(
        facts.protocols,
        Some(profile(ChatCompletions, &[ChatCompletions, Messages]))
    );
}

#[test]
fn public_name_and_raw_upstream_publish_the_same_profile() {
    let destination = open_destination(
        "custom",
        "ocg-pub-name",
        "org/ocg-raw",
        &[Protocol::Responses, Protocol::ChatCompletions],
        Some(Protocol::Responses),
    );
    let expected = Some(profile(Responses, &[ChatCompletions, Responses]));
    for requested in ["ocg-pub-name", "OCG-Pub-Name", "org/ocg-raw"] {
        let facts = publish(
            vec![destination.clone()],
            vec![ranked_credential(
                "custom",
                0,
                ModelScope::All,
                true,
                "cipher",
            )],
            &[],
            requested,
        );
        assert_eq!(facts.unwrap().protocols, expected, "{requested}");
    }
    assert_no_protocol(publish(
        vec![destination],
        vec![ranked_credential(
            "custom",
            0,
            ModelScope::All,
            true,
            "cipher",
        )],
        &[],
        "org/someone-else",
    ));
    let mut public_only = open_destination(
        "public-only",
        "visible-name",
        "hidden-upstream",
        &[Protocol::Messages],
        Some(Protocol::Messages),
    );
    public_only.model_resolution = ModelResolution::PublicOnly;
    let pair = ranked_credential("public-only", 0, ModelScope::All, true, "cipher");
    let visible = Some(profile(Messages, &[Messages]));
    assert_eq!(
        publish(
            vec![public_only.clone()],
            vec![pair.clone()],
            &[],
            "visible-name"
        )
        .unwrap()
        .protocols,
        visible
    );
    assert_eq!(
        publish(
            vec![public_only.clone()],
            vec![pair.clone()],
            &[],
            "Visible-Name"
        )
        .unwrap()
        .protocols,
        visible
    );
    assert_no_protocol(publish(
        vec![public_only],
        vec![pair],
        &[],
        "hidden-upstream",
    ));
    assert_no_protocol(publish(Vec::new(), Vec::new(), &[], "gpt-4o"));
}

#[test]
fn absent_or_illegal_protocol_json_does_not_become_chat() {
    assert_eq!(
        read_published_protocol_profile(None),
        Err(PublishedProtocolProfileError::Unknown)
    );
    assert_eq!(
        read_published_protocol_profile(Some(&Value::Null)),
        Err(PublishedProtocolProfileError::Unknown)
    );
    assert_eq!(
        read_published_protocol_profile(json!({"schemaVersion": 2}).get("protocols")),
        Err(PublishedProtocolProfileError::Unknown)
    );
    for value in [
        json!({}),
        json!("chat_completions"),
        json!({"preferred": "openai-completions", "supported": ["chat_completions"]}),
        json!({"preferred": "chat_completions", "supported": ["messages"]}),
        json!({"preferred": "chat_completions", "supported": []}),
        json!({
            "preferred": "chat_completions",
            "supported": ["chat_completions", "chat_completions"]
        }),
        json!({"supported": ["chat_completions"]}),
        json!({
            "schemaVersion": 2,
            "protocols": {"preferred": "messages", "supported": ["messages"]}
        }),
    ] {
        assert_eq!(
            read_published_protocol_profile(Some(&value)),
            Err(PublishedProtocolProfileError::Invalid)
        );
    }
    let ocg = json!({
        "schemaVersion": 1,
        "status": "unknown",
        "protocols": {"preferred": "responses", "supported": ["chat_completions", "responses"]}
    });
    assert_eq!(
        read_published_protocol_profile(ocg.get("protocols")).unwrap(),
        profile(Responses, &[ChatCompletions, Responses])
    );
}
