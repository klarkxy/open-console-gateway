from pathlib import Path
p=Path('crates/ocg-core/src/model_metadata.rs')
s=p.read_text()
a=s.index('        if metadata.validate().is_ok() {')
b=s.index('\n    }\n    result',a)
body='\n'.join(line[4:] for line in s[a:b].splitlines()[1:-1])
s=s[:a]+'''        if metadata.validate().is_err() {
            // A malformed new declaration withdraws old facts for this ID.
            metadata = ModelMetadata::default();
        }
'''+body+s[b:]
s=s.replace('            value.validate().map_err(anyhow::Error::msg)?;\n','')
a=s.index('    pub(crate) fn validate(&self)')
s=s[:a]+'''    pub(crate) fn redact_secret(&mut self, secret: &str) {
        if !secret.is_empty()
            && serde_json::to_string(self).is_ok_and(|encoded| encoded.contains(secret))
        {
            *self = Self::default();
        }
    }

'''+s[a:]
p.write_text(s)
p=Path('crates/ocg-core/src/custom.rs')
s=p.read_text()
a=s.index('    metadata.retain(')
b=s.index('    Ok((result, metadata))',a)
s=s[:a]+'''    metadata.retain(|id, value| {
        value.redact_secret(api_key);
        result.models.contains(id) && (api_key.is_empty() || !id.contains(api_key))
    });
'''+s[b:]
p.write_text(s)
p=Path('integrations/dsh-plugin/model-catalog.js')
s=p.read_text().replace('return { enabled: Object.values(map).some((v) => v !== null), map, declared };','return { enabled: Object.values(map).some((v) => v !== null), map, declared, hasEfforts: raw !== undefined && raw !== null };')
s=s.replace('    reasoningEfforts: Object.fromEntries(Object.entries(thinking.map).filter(([, v]) => v !== null)),','''    ...(thinking.hasEfforts ? { reasoningEfforts: Object.fromEntries(Object.entries(thinking.map).filter(([, v]) => v !== null)) } : {}),
    ...(Array.isArray(source.sources) ? { sources: source.sources.filter((value) => ["operator", "upstream", "unknown"].includes(value)) } : {}),''')
p.write_text(s)
p=Path('scripts/dsh-application-install-smoke.mjs')
s=p.read_text().replace('function dshBin() {','''function dshBin() {
  if (process.env.OCG_DSH_SMOKE_BIN) return resolve(process.env.OCG_DSH_SMOKE_BIN);''')
s=s.replace('data: [{ id: "smoke-model-a" }, { id: "org/smoke-model-b" }],','''data: [{ id: "smoke-model-a", ocg: {
          schemaVersion: 1, contextWindow: 262144, maxOutputTokens: 32768,
          inputModalities: ["text", "image"], reasoning: true,
          reasoningEfforts: { low: "low", high: "high", xhigh: "max" },
        } }, { id: "org/smoke-model-b" }],''')
s=s.replace('      assert.equal(body.stream, true);','      assert.equal(body.stream, true);\n      assert.equal(body.reasoning_effort, "max");')
s=s.replace('        const streamChunks = [];','''        if (prepared.model.context.contextWindow !== 262144) throw new Error("context metadata did not reach DSH");
        if (JSON.stringify(prepared.model.reasoning.efforts.map((effort) => effort.id)) !== JSON.stringify(["low", "high", "xhigh"])) throw new Error("reasoning tiers did not reach DSH");
        const streamChunks = [];''')
s=s.replace('          model: "smoke-model-a",\n          messages:', '          model: "smoke-model-a",\n          reasoningEffort: "xhigh",\n          messages:')
s=s.replace('      chatStreamCompleted: true,','      chatStreamCompleted: true,\n      contextAndReasoningTiersVerified: true,\n      reasoningWireMappingVerified: true,')
p.write_text(s)
p=Path('crates/ocg-core/src/model_metadata/tests.rs')
p.write_text(p.read_text()+'''
#[test]
fn invalid_reported_metadata_withdraws_the_fact_instead_of_preserving_a_stale_record() {
    let rows = parse_catalog(br#"{"data":[{"id":"a","contextWindow":100,"maxTokens":200}]}"#);
    assert_eq!(rows["a"], ModelMetadata::default());
}

#[test]
fn secret_echoes_become_unknown_without_deleting_the_model_identity() {
    let mut facts = ModelMetadata { name: Some("echo private-key".into()), context_window: Some(8000), ..Default::default() };
    facts.redact_secret("private-key");
    assert_eq!(facts, ModelMetadata::default());
    let mut facts = ModelMetadata { context_window: Some(8000), ..Default::default() };
    facts.redact_secret("");
    assert_eq!(facts.context_window, Some(8000));
}

#[test]
fn disjoint_known_modalities_do_not_turn_into_an_unknown_text_fallback() {
    let result = common(&[metadata(8000, 1000, &["text"], &[]), metadata(8000, 1000, &["image"], &[])]);
    assert_eq!(result.input_modalities, Some(vec![]));
}

fn route_fixture() -> Destination {
    use ocg_domain::destination::*;
    Destination {
        id: "route-one".into(), legacy: LegacyDestinationRef::Dynamic("test".into()),
        adapter: AdapterKind::Http, name: "test".into(), brand_family: None,
        base_url: Some("https://example.test/v1".into()),
        protocols: vec![Protocol::ChatCompletions], protocol_routes: vec![],
        auth_scheme: AuthScheme::Bearer, model_resolution: ModelResolution::PublicAndUpstream,
        catalog: vec![CatalogModel { public_model: "public".into(), upstream_model: "upstream".into(),
            protocols: vec![Protocol::ChatCompletions], preferred: Some(Protocol::ChatCompletions),
            enabled: true, upstream_override: None }],
        capabilities: sealed_capabilities(AdapterKind::Http), plan: None,
        max_credentials: None, observer_credential_id: None, enabled: true,
    }
}

#[test]
fn declarations_are_bound_to_the_exact_destination_route_and_model_mapping() {
    let destination = route_fixture();
    let model = &destination.catalog[0];
    let mut records = vec![];
    let record = record_for(&mut records, &destination, model);
    record.observed = Some(ModelMetadata { context_window: Some(8000), ..Default::default() });
    record.declared = Some(ModelMetadata { context_window: Some(16000), ..Default::default() });
    assert_eq!(effective(&records, &destination, model).0.context_window, Some(16000));
    assert_eq!(effective(&records, &destination, model).1, "operator");
    let mut changed = destination.clone();
    changed.base_url = Some("https://other.test/v1".into());
    assert_eq!(effective(&records, &changed, model).1, "unknown");
    let record = record_for(&mut records, &changed, model);
    assert!(record.observed.is_none() && record.declared.is_none());
    let record = record_for(&mut records, &destination, model);
    record.observed = Some(ModelMetadata { context_window: Some(8000), ..Default::default() });
    let mut changed_model = model.clone();
    changed_model.upstream_model = "different".into();
    assert_eq!(effective(&records, &destination, &changed_model).1, "unknown");
}
''')
p=Path('scripts/dsh-model-catalog.test.mjs')
p.write_text(p.read_text()+'''

test("unknown reasoning controls stay distinguishable from an explicit empty offer", () => {
  assert.equal(parse({ id: "legacy" }).metadata.get("legacy").reasoningEfforts, undefined);
  assert.deepEqual(parse(declared({ reasoningEfforts: {} })).metadata.get("private-alias").reasoningEfforts, {});
});

test("disjoint alias modalities cannot silently acquire text support", () => {
  assert.ok(parse(declared({ inputModalities: [] })).modelErrors.has("private-alias"));
});

test("an output-only limit is preserved without a contradictory internal context", () => {
  const { models: [model], metadata } = parse({ id: "output-only", maxTokens: 262144 });
  assert.equal(model.maxTokens, 262144);
  assert.ok(model.contextWindow >= model.maxTokens);
  assert.equal(metadata.get("output-only").contextWindow, undefined);
});
''')
print('Hardened unknown facts, route binding, secret-echo handling, and the real DSH wire smoke.')
