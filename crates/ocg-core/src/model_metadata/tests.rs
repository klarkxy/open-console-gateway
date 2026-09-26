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
