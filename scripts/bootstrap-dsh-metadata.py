"""One-shot exact-anchor feature-branch edit, removed before the final PR."""
from pathlib import Path

def replace(path, old, new, count=1):
    p = Path(path)
    s = p.read_text()
    if s.count(old) != count:
        raise RuntimeError(f'{path}: expected {count} anchors, found {s.count(old)}: {old[:100]}')
    p.write_text(s.replace(old, new))

p = 'integrations/dsh-plugin/index.js'
replace(p, 'import { randomBytes }', 'import { parseModelCatalog, describeOcgModel } from "./model-catalog.js";\nimport { randomBytes }')
s = Path(p).read_text()
a = s.index('function modelDefinition(id) {')
b = s.index('const HANDOFF_CLAIM_MARKER', a)
Path(p).write_text(s[:a] + s[b:])
replace(p, '      const models = parseModelCatalog(await response.json());', '''      let catalog;
      try {
        catalog = parseModelCatalog(await response.json(), { providerId, baseUrl });
      } catch {
        throw new LlmError("Open Console Gateway returned an invalid /v1/models payload", "INVALID_CONFIG");
      }
      const { models, modelErrors, metadata } = catalog;''')
replace(p, '            modelErrors: new Map(),', '            modelErrors,\n            ocgMetadata: metadata,')
replace(p, '      return super.listModels(provider);', '''      const metadata = profiles.get(provider)?.ocgMetadata;
      return (await super.listModels(provider)).map((info) => describeOcgModel(info, metadata?.get(info.id)));''')
replace(p, '      return super.resolveModel(provider, model, signal);', '''      const metadata = profiles.get(provider)?.ocgMetadata.get(model);
      return describeOcgModel(await super.resolveModel(provider, model, signal), metadata);''')
replace(p, '      return super.prepareCall(provider, model, signal);', '''      // Capture metadata before the await, just like PiAiAdapter captures its provider.
      const metadata = profiles.get(provider)?.ocgMetadata.get(model);
      const prepared = await super.prepareCall(provider, model, signal);
      return { ...prepared, model: describeOcgModel(prepared.model, metadata) };''')
replace('integrations/dsh-plugin/package.json', '    "index.js",', '    "index.js",\n    "model-catalog.js",')
replace('crates/ocg-core/src/dsh_application_host.rs', '''    (
        "cordis.patch.yml",''', '''    (
        "model-catalog.js",
        include_str!("../../../integrations/dsh-plugin/model-catalog.js"),
    ),
    (
        "cordis.patch.yml",''')
p = 'scripts/dsh-plugin-package.test.mjs'
replace(p, '  await writeFile(plugin, rendered);', '''  await writeFile(plugin, rendered);
  await writeFile(join(root, "model-catalog.js"), await readFile(new URL("model-catalog.js", sourceRoot)));''')
replace(p, '    assert.deepEqual(result.prepared.model, {', '''    assert.equal(result.prepared.model.ocg.status, "legacy");
    const { ocg: _metadata, ...preparedModel } = result.prepared.model;
    assert.deepEqual(preparedModel, {''')
replace('crates/ocg-core/src/lib.rs', 'pub mod models;', 'pub(crate) mod model_metadata;\npub mod models;')
p = 'crates/ocg-core/src/gateway/handler.rs'
replace(p, '''    axum::Json(serde_json::json!({
        "object": "list",
        "data": data
    }))''', '''    if crate::model_metadata::enrich(&state.db.lock(), &snapshot, &mut data).is_err() {
        return protocol_error_response(
            ApiFormat::ChatCompletions,
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to load model metadata",
            None,
        );
    }
    axum::Json(serde_json::json!({
        "object": "list",
        "data": data
    }))''')
p = 'crates/ocg-core/src/dashboard_v4/mod.rs'
replace(p, 'mod identities;', 'mod identities;\nmod model_metadata;')
replace(p, '''        .route(
            "/destinations/{id}/model-tests",''', '''        .route(
            "/destinations/{id}/model-metadata",
            get(model_metadata::get).put(model_metadata::put),
        )
        .route(
            "/destinations/{id}/model-tests",''')
p = 'crates/ocg-core/src/dashboard_v4/types.rs'
replace(p, 'pub use crate::db::routing_cards::RoutingCard;', '''pub use crate::db::routing_cards::RoutingCard;
pub use crate::model_metadata::ModelMetadata;
pub use super::model_metadata::{DestinationModelMetadata, DestinationModelMetadataEntry, DestinationModelMetadataUpdate};''')
replace(p, '    "DestinationCatalogUpdate",', '''    "DestinationCatalogUpdate",
    "ModelMetadata",
    "DestinationModelMetadata",
    "DestinationModelMetadataEntry",
    "DestinationModelMetadataUpdate",''')
replace(p, '    include_type::<DestinationList>(&mut serialize);', '''    include_type::<DestinationList>(&mut serialize);
    include_type::<ModelMetadata>(&mut serialize);
    include_type::<DestinationModelMetadata>(&mut serialize);
    include_type::<DestinationModelMetadataEntry>(&mut serialize);''')
replace(p, '    include_type::<DestinationCatalogUpdate>(&mut deserialize);', '''    include_type::<DestinationCatalogUpdate>(&mut deserialize);
    include_type::<DestinationModelMetadataUpdate>(&mut deserialize);''')
p = 'crates/ocg-core/src/custom.rs'
s = Path(p).read_text()
a = s.index('pub(crate) async fn discover_models_with_auth(')
b = s.index('async fn discover_custom_models_inner(', a)
old = s[a:b]
new = old.replace('pub(crate) async fn discover_models_with_auth(', 'pub(crate) async fn discover_models_with_metadata(', 1)
new = new.replace(') -> Result<CustomModelDiscoveryResult, CustomModelDiscoveryFailure> {', ''') -> Result<(CustomModelDiscoveryResult, std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>), CustomModelDiscoveryFailure> {
    let mut metadata = std::collections::BTreeMap::new();''', 1)
new = new.replace('    tokio::time::timeout(', '    let result = tokio::time::timeout(', 1)
new = new.replace('discover_custom_models_inner(config, input, auth, api_key)', 'discover_custom_models_inner(config, input, auth, api_key, &mut metadata)')
new = new.replace('    })?\n}', '''    })??;
    metadata.retain(|id, value| result.models.contains(id)
        && (api_key.is_empty() || (!id.contains(api_key)
            && !serde_json::to_string(value).unwrap_or_default().contains(api_key))));
    Ok((result, metadata))
}''')
wrapper = '''pub(crate) async fn discover_models_with_auth(
    config: &AppConfig,
    input: &AccountCustomConfigInput,
    auth: Option<crate::provider::UpstreamAuthScheme>,
    api_key: &str,
) -> Result<CustomModelDiscoveryResult, CustomModelDiscoveryFailure> {
    discover_models_with_metadata(config, input, auth, api_key).await.map(|(result, _)| result)
}

'''
s = s[:a] + wrapper + new + s[b:]
a = s.index('async fn discover_custom_models_inner(')
b = s.index('fn model_discovery_request_timeout', a)
section = s[a:b].replace('    api_key: &str,\n)', '    api_key: &str,\n    metadata: &mut std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,\n)', 1)
section = section.replace('        let page_result = parse_model_discovery_page(&body)?;', '''        let page_result = parse_model_discovery_page(&body)?;
        let page_metadata = crate::model_metadata::parse_catalog(&body);''')
section = section.replace('                models.push(model);', '''                if let Some(facts) = page_metadata.get(&model) {
                    metadata.insert(model.clone(), facts.clone());
                }
                models.push(model);''', 1)
Path(p).write_text(s[:a] + section + s[b:])
p = 'crates/ocg-core/src/dashboard_v4/destination_catalog.rs'
replace(p, '    let discovered = crate::custom::discover_models_with_auth(&config, &input, auth, &key)', '    let (discovered, metadata) = crate::custom::discover_models_with_metadata(&config, &input, auth, &key)')
replace(p, '''                &destination,
                &catalog,
            )
        })''', '''                &destination,
                &catalog,
            )?;
            let mut updated = destination.clone();
            updated.catalog = catalog.clone();
            crate::model_metadata::observe(db, &updated, &metadata)
        })''')
p = 'crates/ocg-core/src/goat.rs'
replace(p, '    pub protocol_baseline: OfficialProtocolBaseline,', '''    pub protocol_baseline: OfficialProtocolBaseline,
    pub(crate) metadata: std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,''')
replace(p, '''    Ok(ProviderCatalogDiscovery {
        models,
        protocol_baseline,
    })''', '''    Ok(ProviderCatalogDiscovery {
        models,
        protocol_baseline,
        metadata: crate::model_metadata::parse_catalog(bytes),
    })''')
p = 'crates/ocg-core/src/dashboard_v3/providers.rs'
s = Path(p).read_text()
a = s.index('async fn refresh_go_or_command_catalog(')
b = s.index('pub(super) async fn refresh_provider_models(', a)
section = s[a:b]
old = '    state.routing.reset();'
assert section.count(old) == 1
section = section.replace(old, '''    {
        let db = state.db.lock();
        let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).map_err(V3ApiError::internal)?;
        let adapter = ocg_domain::destination::adapter_kind_for_builtin(provider_id);
        for destination in &snapshot.projection.destinations {
            if Some(destination.adapter) == adapter {
                crate::model_metadata::observe(&db, destination, &discovery.metadata).map_err(V3ApiError::internal)?;
            }
        }
    }
    state.routing.reset();''')
Path(p).write_text(s[:a]+section+s[b:])
for suffix, text in [('', '\n## Model details in DSH\n\nSee [model metadata and reasoning tiers](model-metadata.md) for discovery, route-specific declarations, and upgrading the installed OCG plugin.\n'), ('.zh-CN', '\n## DSH 中的模型信息\n\n参见[模型元数据与推理档位](model-metadata.zh-CN.md)，了解目录刷新、按连接声明参数，以及升级已安装的 OCG 插件。\n')]:
    p = Path('docs/user/applications'+suffix+'.md')
    p.write_text(p.read_text()+text)
print('Applied route metadata, DSH translation and packaging changes.')
