use super::*;

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
