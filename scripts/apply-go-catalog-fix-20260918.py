from pathlib import Path
import subprocess

ROOT = Path.cwd()
EXPECTED = '352dabaa8a682a4661ef38b177713c91543e67cc'
if subprocess.check_output(['git', 'merge-base', 'HEAD', EXPECTED], text=True).strip() != EXPECTED:
    raise RuntimeError('Unexpected branch ancestry; refusing to patch')
if subprocess.check_output(['git', 'diff', '--name-only', EXPECTED, 'HEAD', '--', 'crates', 'src', 'docs'], text=True).strip():
    raise RuntimeError('Source changed after review; refusing to overwrite')
changes = {}

def read(path):
    return changes.get(path, (ROOT / path).read_text())

def replace(path, old, new, count=1):
    text = read(path)
    if text.count(old) != count:
        raise RuntimeError(f'{path}: expected {count} source matches for {old[:100]!r}, got {text.count(old)}')
    changes[path] = text.replace(old, new)

def section(path, start, end, new):
    text = read(path)
    if text.count(start) != 1 or text.count(end) != 1:
        raise RuntimeError(f'{path}: ambiguous section boundary {start!r}')
    a = text.index(start)
    b = text.index(end, a)
    changes[path] = text[:a] + new + text[b:]

# Public catalogs are not account validation or quota probes.
goat = 'crates/ocg-core/src/goat.rs'
replace(goat, '''pub async fn probe_opencode_go_models(
    config: &AppConfig,
    api_key: &str,
    base_url: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    let url = opencode_go_models_url_for_base(base_url);
    probe_provider_models_at_url(config, api_key, &url, "OpenCode Go").await
}''', '''/// Read the public Go directory without sending an account credential.
pub async fn refresh_opencode_go_models(
    config: &AppConfig,
    base_url: &str,
) -> Result<Vec<String>, GoatVerifyFailure> {
    let url = opencode_go_models_url_for_base(base_url);
    probe_public_provider_models_at_url(config, &url, "OpenCode Go").await
}''')

providers = 'crates/ocg-core/src/dashboard_v3/providers.rs'
replace(providers, '''enum GoCommandCatalogAccount<'a> {
    Explicit { account_id: &'a str },
    Eligible,
    None,
}

''', '')
replace(providers, "    account_selection: GoCommandCatalogAccount<'_>,\n", '')
replace(providers, 'let (account, config, key, base_url, source_url, previous_models) = {',
        'let (config, base_url, source_url, previous_models) = {')
text = read(providers)
a = text.index('        let account = match (provider_id, &account_selection) {')
b = text.index('        let config = state.config();', a)
changes[providers] = text[:a] + text[b:]
replace(providers, '(account, config, key, base_url, source_url, previous_models)',
        '(config, base_url, source_url, previous_models)')
replace(providers, '''        goat::probe_opencode_go_models(
            &config,
            key.as_deref().expect("OpenCode refresh prepared a Key"),
            &base_url,
        )
        .await''', '        goat::refresh_opencode_go_models(&config, &base_url).await')
replace(providers, '''    if let Some(account) = account.as_ref() {
        let current = load_model_account(state, &account.id)?;
        if current.updated_at != account.updated_at || current.key_cipher != account.key_cipher {
            return Err(V3ApiError::conflict_at(
                state,
                "the selected OpenCode Go account changed while models were refreshing",
            ));
        }
    }
''', '')
replace(providers, '        account_id: account.map(|account| account.id),', '        account_id: None,')
text = read(providers)
a = text.index('    let account_selection = if provider_id == OPENCODE_PROVIDER_ID {')
b = text.index('    Ok(Json(ProviderModels {', a)
changes[providers] = text[:a] + '''    // The optional legacy accountId is accepted but is not used for public discovery.
    let refreshed =
        refresh_go_or_command_catalog(&state, &provider_id, &input.expectation).await?;
''' + text[b:]
replace(providers, '''        let account_selection = if scope_id == COMMAND_CODE_PROVIDER_ID {
            GoCommandCatalogAccount::None
        } else {
            GoCommandCatalogAccount::Eligible
        };
        refresh_go_or_command_catalog(&state, &scope_id, &expectation, account_selection).await?;''',
'''        refresh_go_or_command_catalog(&state, &scope_id, &expectation).await?;''')

# Shared aliases stay curated. Resolve exact Go pins with all conflict checks.
alias = 'crates/ocg-gateway/src/alias.rs'
new_publication = '''/// Client-visible names: curated aliases plus uniquely resolved exact Go IDs.
/// Discovering a model does not create a shared cross-provider alias. Hosts
/// still apply protocol enablement and the operator publication switch.
pub fn published_routeable_models_with_runtime_catalogs(
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<PublishedAlias> {
    let mut published = published_routeable_aliases_with_runtime_catalogs(catalogs);
    for id in catalogs.go {
        if is_free_model(id) || published.iter().any(|item| item.alias == *id) {
            continue;
        }
        if matches!(
            resolve_with_runtime_catalogs(id, catalogs),
            Ok(ResolvedModel::PinnedRaw { mapping, .. })
                if mapping.routeable && mapping.is_opencode_go() && mapping.upstream_model == *id
        ) {
            published.push(PublishedAlias {
                alias: id.clone(),
                owned_by: OPENCODE_PROVIDER_ID.to_string(),
            });
        }
    }
    published.sort_by(|left, right| left.alias.cmp(&right.alias));
    published
}

/// Public names that resolve to this provider, independent of first-wins ownership.
pub fn routeable_models_for_with_runtime_catalogs(
    provider_id: &str,
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<String> {
    published_routeable_models_with_runtime_catalogs(catalogs)
        .into_iter()
        .filter(|item| {
            resolve_with_runtime_catalogs(&item.alias, catalogs).is_ok_and(|resolved| {
                resolved.routeable_mappings().iter().any(|mapping| mapping.provider_id == provider_id)
            })
        })
        .map(|item| item.alias)
        .collect()
}

'''
replace(alias, 'fn go_catalog_alias(model_id: &str) -> String {', new_publication + 'fn go_catalog_alias(model_id: &str) -> String {')
facade = 'crates/ocg-core/src/alias.rs'
replace(facade, '    routeable_aliases_for, routeable_aliases_for_with_runtime_catalogs,',
'''    routeable_aliases_for, routeable_aliases_for_with_runtime_catalogs,
    published_routeable_models_with_runtime_catalogs, routeable_models_for_with_runtime_catalogs,''')
handler = 'crates/ocg-core/src/gateway/handler.rs'
replace(handler, 'crate::alias::published_routeable_aliases_with_runtime_catalogs(catalogs)',
        'crate::alias::published_routeable_models_with_runtime_catalogs(catalogs)')
replace(handler, '''/// GET /v1/models —authenticated local Alias registry list.
///
/// Returns OpenAI list JSON for routeable code-owned aliases, then eligible
/// Custom capability IDs, de-duplicated and in deterministic order. Refreshed
/// built-in catalogs can activate sealed names or add an exact raw pin, but
/// cannot create arbitrary aliases. It never calls upstream.''',
'''/// GET /v1/models: authenticated, local public model inventory.
///
/// Includes curated aliases, exact Go catalog pins, and eligible Custom,
/// CPA and dynamic names. Protocol and publication switches still apply.
/// Catalog discovery never creates arbitrary shared aliases. No upstream I/O.''')

# The Go summary and application picker also use runtime catalog identities.
replace(providers, '''    let mut entries: Vec<ProviderCatalogEntry> = BUILTIN_PROVIDERS''',
'''    let go_models = contracts.providers.get(OPENCODE_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice()).unwrap_or_default();
    let public_catalogs = alias::RuntimeCatalogs {
        go: go_models,
        zen_free: &zen_catalog.models,
        command_code: goat_models,
        minimax: minimax_models,
        kimi: kimi_models,
        ollama: ollama_models,
        ollama_pinned: &ollama_pinned_models,
        ..alias::RuntimeCatalogs::default()
    };
    let mut entries: Vec<ProviderCatalogEntry> = BUILTIN_PROVIDERS''')
replace(providers, '''            catalog_entry(
                plan,
                &zen_catalog.models,
                goat_models,
                minimax_models,
                kimi_models,
                ollama_models,
                &ollama_pinned_models,
            )''',
'''            let mut entry = catalog_entry(
                plan,
                &zen_catalog.models,
                goat_models,
                minimax_models,
                kimi_models,
                ollama_models,
                &ollama_pinned_models,
            );
            if plan.provider_id == OPENCODE_PROVIDER_ID {
                entry.model_aliases = alias::routeable_models_for_with_runtime_catalogs(
                    plan.provider_id, public_catalogs,
                );
            }
            entry''')
replace(providers, '    Json(model_capabilities())', '    Json(model_capabilities(&state.provider_contracts()))')
replace(providers, 'use crate::kernel::protocol::supported_model_protocol_profiles;\n', '')
section(providers, 'fn model_capabilities()', 'fn zen_free_settings_from_state(', '''fn model_capabilities(contracts: &EffectiveContractSet) -> Vec<ProviderModelCapability> {
    contracts.providers.get(OPENCODE_PROVIDER_ID).into_iter()
        .flat_map(|scope| scope.models.values())
        .map(|model| ProviderModelCapability {
            model_id: model.model_id.clone(),
            provider_id: OPENCODE_PROVIDER_ID.to_string(),
            preferred_protocol: AccountUpstreamProtocol::from(model.preferred_protocol),
            supported_protocols: model.protocols.values()
                .filter(|row| row.available)
                .map(|row| AccountUpstreamProtocol::from(row.protocol))
                .collect(),
        })
        .collect()
}

''')
obs = 'crates/ocg-core/src/control/observability.rs'
section(obs, 'pub(crate) fn application_models_from_snapshot(', 'pub(crate) fn dashboard_summary(', '''pub(crate) fn application_models_from_snapshot(
    _snapshot: &PricingSnapshot,
    contracts: Option<&EffectiveContractSet>,
) -> Vec<String> {
    let Some(contracts) = contracts else { return Vec::new(); };
    let Some(scope) = contracts.providers.get(crate::provider::OPENCODE_PROVIDER_ID)
        .filter(|scope| scope.catalog.source == crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS)
    else { return Vec::new(); };
    let catalogs = alias::RuntimeCatalogs {
        go: &scope.catalog.models,
        ..alias::RuntimeCatalogs::default()
    };
    alias::routeable_models_for_with_runtime_catalogs(crate::provider::OPENCODE_PROVIDER_ID, catalogs)
        .into_iter()
        .filter(|name| {
            alias::resolve_with_runtime_catalogs(name, catalogs).is_ok_and(|resolved| {
                resolved.routeable_mappings().iter().any(|mapping| {
                    mapping.is_opencode_go() && contracts.mapping_has_enabled_protocol(mapping)
                })
            })
        })
        .collect()
}

''')
ui = 'src/domain/provider-aliases.ts'
replace(ui, '''      if (!model.alias) continue;
      rows.push({
        provider_id: providerId,
        key: `${scope.key}:${model.alias}:${model.model_id}`,
        public_model: model.alias,''',
'''      const publicModel = model.alias || (providerId === "opencode" ? model.model_id : "");
      if (!publicModel) continue;
      rows.push({
        provider_id: providerId,
        key: `${scope.key}:${publicModel}:${model.model_id}`,
        public_model: publicModel,''')

# Rename the unavailable result and its test seam throughout Rust callers.
for file in (ROOT / 'crates').rglob('*.rs'):
    path = str(file.relative_to(ROOT))
    text = read(path)
    updated = text.replace('FallbackChat', 'Unavailable')
    updated = updated.replace('install_official_protocol_fetch_fallback_chat_for_tests',
                              'install_official_protocol_fetch_unavailable_for_tests')
    if updated != text:
        changes[path] = updated
protocols = 'crates/ocg-core/src/official_protocols.rs'
replace(protocols, '''//! A failed fetch or a model the document does not list defaults to Chat
//! Completions. Zen Free reuses the Go endpoint table and looks up the''',
'''//! A failed fetch or omitted model supplies no new protocol evidence.
//! Existing evidence survives. Zen Free reuses the Go endpoint table and looks up the''')
replace(protocols, '    /// Fetch or parse failed; every model defaults to Chat.',
        '    /// Fetch or parse failed; preserve saved evidence instead of guessing a protocol.')
section(protocols, '    /// `None` keeps a known unsupported id', '\npub fn uses_official_docs_protocol_baseline(', '''    /// Only protocols explicitly described by this document are evidence.
    pub fn protocol_for(&self, provider_id: &str, model_id: &str) -> Option<UpstreamProtocolKind> {
        if model_id.trim().is_empty()
            || (provider_id == COMMAND_CODE_PROVIDER_ID && model_id.eq_ignore_ascii_case("stealth/ox-alpha"))
        {
            return None;
        }
        match self {
            Self::Mapped(map) => lookup_mapped(map, model_id).or_else(|| {
                (provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID)
                    .then(|| lookup_mapped(map, &strip_zen_free_suffix(model_id))).flatten()
            }),
            Self::FamilyRule if provider_id == COMMAND_CODE_PROVIDER_ID =>
                ocg_domain::protocol::command_code_preferred_format(model_id).map(api_to_upstream),
            Self::FamilyRule | Self::Unavailable => None,
        }
    }
}

/// A reviewed per-model offline default, never a model-directory whitelist.
/// Unknown IDs have no default. A known explicitly unsupported row stays unknown.
pub(crate) fn known_opencode_default(model_id: &str, zen_free: bool) -> Option<UpstreamProtocolKind> {
    if model_id.trim().is_empty() || (zen_free && !crate::kernel::ids::is_free_model(model_id)) {
        return None;
    }
    let paid_id = if zen_free { strip_zen_free_suffix(model_id) } else { model_id.to_string() };
    let profile = crate::kernel::protocol::model_protocol(model_id)
        .or_else(|| crate::kernel::protocol::model_protocol(&paid_id))?;
    if profile.supported.is_empty() { return None; }
    Some(api_to_upstream(profile.preferred))
}
''')
replace(protocols, '''    parse_endpoint_protocol_rows(endpoint_table, id_index)
}''', '''    let map = parse_endpoint_protocol_rows(endpoint_table, id_index)?;
    if map.is_empty() {
        bail!("OpenCode Go endpoint table contains no recognized protocols");
    }
    Ok(map)
}''')
contracts = 'crates/ocg-core/src/provider_contracts.rs'
replace(contracts, '''        ProviderAdapterKind::OpenCodeGo | ProviderAdapterKind::ZenFree => {
            UpstreamProtocolKind::ChatCompletions
        }''', '''        ProviderAdapterKind::OpenCodeGo | ProviderAdapterKind::ZenFree => {
            // The V3 preferred field is non-null; this placeholder does not
            // confer support when an unknown model has no admitted evidence.
            provider_default_protocol(adapter, model_id)
                .unwrap_or(UpstreamProtocolKind::ChatCompletions)
        }''')
section(contracts, '/// Chat Completions is the documented miss/fallback', '#[allow(clippy::too_many_arguments)]\nfn merge_model_contract(', '''/// Known per-model defaults only. Directory discovery cannot assert Chat support.
fn provider_default_protocol(
    adapter: ProviderAdapterKind,
    model_id: &str,
) -> Option<UpstreamProtocolKind> {
    match adapter {
        ProviderAdapterKind::OpenCodeGo => crate::official_protocols::known_opencode_default(model_id, false),
        ProviderAdapterKind::ZenFree => crate::official_protocols::known_opencode_default(model_id, true),
        _ => None,
    }
}

''')

# Only replace explicitly documented models. Missing entries preserve state.
db = 'crates/ocg-core/src/db.rs'
section(db, 'fn apply_official_protocol_baseline_on(', 'fn insert_default_off_override_on(', '''fn apply_official_protocol_baseline_on(
    conn: &Connection,
    scope: &ContractScope,
    current_models: &[String],
    baseline: &crate::official_protocols::OfficialProtocolBaseline,
    now: DateTime<Utc>,
    force_off_extras: bool,
) -> Result<()> {
    let evidence = load_scope_evidence_on(conn, scope)?;
    let mut preferences = Vec::new();
    for model_id in current_models {
        let Some(protocol) = baseline.protocol_for(scope.id(), model_id) else {
            continue;
        };
        // Old static declarations may also carry independent probe history.
        // Demote those declarations, keeping their diagnostics, before pruning.
        conn.execute(
            "UPDATE provider_contract_model_protocols SET source = 'probe_observed'
             WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3
               AND source = 'static' AND protocol <> ?4
               AND (verified_at IS NOT NULL OR observed_at IS NOT NULL
                    OR last_probe_result IS NOT NULL OR last_probe_at IS NOT NULL
                    OR last_probe_error IS NOT NULL)",
            params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
        )?;
        conn.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3
               AND source = 'static' AND protocol <> ?4",
            params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
        )?;
        let mut row = evidence.iter().find(|row| row.model_id == *model_id && row.protocol == protocol)
            .cloned().unwrap_or(PersistedModelProtocol {
                scope: scope.clone(), model_id: model_id.clone(), protocol,
                source: ContractEvidenceSource::Static,
                verified_at: None, observed_at: None, last_probe_result: None,
                last_probe_at: None, last_probe_error: None,
            });
        row.source = ContractEvidenceSource::Static;
        upsert_model_protocol_row_on(conn, &row)?;
        let has_preference: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_model_protocol_preferences
             WHERE provider_id = ?1 AND model_id = ?2)",
            params![scope.id(), model_id.trim().to_ascii_lowercase()],
            |row| row.get(0),
        )?;
        if (force_off_extras || !has_preference)
            && preference_protocol_allowed(conn, scope, model_id, protocol)?
        {
            preferences.push((model_id.clone(), protocol));
        }
        if force_off_extras {
            for extra in UpstreamProtocolKind::ALL {
                if extra != protocol {
                    set_model_protocol_override_on(conn, scope, model_id, extra,
                        ProtocolOverrideState::ForceOff, now)?;
                }
            }
        }
    }
    if !preferences.is_empty() {
        set_model_protocol_preferences_on(conn, scope, &preferences)?;
    }
    Ok(())
}

''')
replace(db, '''        clear_provider_protocol_judgments_on(&tx, scope)?;
        apply_official_protocol_baseline_on(&tx, scope, current_models, baseline, now, true)?;''',
'''        anyhow::ensure!(
            !matches!(baseline, crate::official_protocols::OfficialProtocolBaseline::Unavailable),
            "cannot reset protocol configuration without an official document"
        );
        clear_provider_protocol_judgments_on(&tx, scope)?;
        apply_official_protocol_baseline_on(&tx, scope, current_models, baseline, now, true)?;''')

# Update obsolete expectations and add regressions, without executing tests.
pt = 'crates/ocg-core/src/official_protocols/tests.rs'
section(pt, '#[test]\nfn go_placeholder_endpoints_are_skipped_so_apply_defaults_to_chat()',
        '#[test]\nfn command_code_provider_docs_without_model_table_use_family_rule()', '''#[test]
fn go_placeholder_endpoints_are_not_protocol_evidence() {
    let html = include_str!("../../tests/fixtures/opencode-go.html");
    assert!(parse_go_official_protocols(html).is_err());
}

''')
section(pt, '#[test]\nfn mapped_miss_and_failed_fetch_default_to_chat()',
        '#[test]\nfn endpoint_url_parser_accepts_official_go_and_command_paths()', '''#[test]
fn omitted_models_and_failed_fetch_supply_no_protocol_evidence() {
    let mapped = OfficialProtocolBaseline::mapped([("grok-4.6", UpstreamProtocolKind::Responses)]);
    assert_eq!(mapped.protocol_for("opencode", "grok-4.6"), Some(UpstreamProtocolKind::Responses));
    assert_eq!(mapped.protocol_for("opencode", "future-go-model"), None);
    assert_eq!(mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "grok-4.6-free"), Some(UpstreamProtocolKind::Responses));
    assert_eq!(mapped.protocol_for(OPENCODE_ZEN_FREE_PROVIDER_ID, "future-free"), None);
    assert_eq!(OfficialProtocolBaseline::Unavailable.protocol_for("opencode", "grok-4.6"), None);
    assert_eq!(OfficialProtocolBaseline::Unavailable.protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"), None);
    assert_eq!(OfficialProtocolBaseline::FamilyRule.protocol_for(COMMAND_CODE_PROVIDER_ID, "claude-fable-5"), Some(UpstreamProtocolKind::Messages));
    assert_eq!(OfficialProtocolBaseline::FamilyRule.protocol_for(COMMAND_CODE_PROVIDER_ID, "stealth/ox-alpha"), None);
    assert_eq!(super::known_opencode_default("grok-4.6", false), Some(UpstreamProtocolKind::Responses));
    assert_eq!(super::known_opencode_default("future-go-model", false), None);
}

''')
at = 'crates/ocg-gateway/src/alias/tests.rs'
changes[at] = read(at) + '''
#[test]
fn public_go_catalog_names_include_new_pins_without_creating_shared_aliases() {
    let go = vec!["future-go-model".to_string(), "vendor/model-x".to_string()];
    let catalogs = RuntimeCatalogs { go: &go, ..RuntimeCatalogs::default() };
    let published = published_routeable_models_with_runtime_catalogs(catalogs);
    for id in &go {
        assert!(published.iter().any(|item| item.alias == *id && item.owned_by == OPENCODE_PROVIDER_ID));
        assert!(matches!(resolve_with_runtime_catalogs(id, catalogs), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.is_opencode_go()));
        assert!(!published_routeable_aliases_with_runtime_catalogs(catalogs).iter().any(|item| item.alias == *id));
    }
    let custom = vec!["future-go-model".to_string()];
    let conflicting = RuntimeCatalogs { custom: &custom, ..catalogs };
    assert!(!published_routeable_models_with_runtime_catalogs(conflicting).iter().any(|item| item.alias == "future-go-model"));
    assert!(resolve_with_runtime_catalogs("not-in-catalog", catalogs).is_err());
}
'''

# Paired documentation; preserve unrelated sections.
for path, zh in [('docs/maintainer/runtime-invariants.md', False),
                 ('docs/maintainer/runtime-invariants.zh-CN.md', True)]:
    text = read(path)
    if not zh:
        old1 = next(line for line in text.splitlines() if line.startswith('- Authenticated `GET /v1/models`'))
        old2 = next(line for line in text.splitlines() if line.startswith('- Go published aliases still come from'))
        new1 = '- Authenticated `GET /v1/models` lists enabled curated aliases, uniquely resolved exact Go catalog IDs, and eligible Custom/CPA/user-defined names. New Go IDs stay pinned to Go; discovery does not make them shared aliases. Ambiguous identities are not advertised. The Aliases publication switch hides a name without changing routing. `application-models` contains Go-resolvable names with an enabled protocol from the saved catalog, independent of price coverage; an unknown price remains unpriced. Both GET paths are local reads. Explicit Go directory refresh is public and keyless, independent of stored accounts, credential validity, quota, or routing cooldown. MiniMax/Kimi authenticated directory refresh still uses a ready stored Key. Directory discovery does not verify an account or enable a new model.'
        new2 = '- `MODEL_PROTOCOLS` is a per-model offline protocol fallback and curated shared-alias seed, not the Go model inventory. Go/Zen/Command refresh reads official protocol documentation; only a recognized documented endpoint or documented Command family rule contributes new evidence. A failed fetch or omitted model preserves existing evidence and saved preferences. Without saved evidence, Go/Zen use a known per-model offline default; unknown models have no inferred available protocol and require an explicit selection or probe. Newly discovered Go IDs remain default-off. Enabled client protocols pass through; otherwise conversion follows the saved preferred protocol and enabled adapter fallback order. No request-time protocol probing or paid discovery occurs. Catalog removal remains local and a later refresh may rediscover default-off entries.'
        text = text.replace(old1, new1).replace(old2, new2)
    else:
        lines = text.splitlines()
        candidates = [i for i, line in enumerate(lines) if line.startswith('- ') and 'GET /v1/models' in line and 'application-models' in line and ('交集' in line or '∩' in line)]
        if len(candidates) != 1:
            raise RuntimeError('Chinese model-list invariant was not uniquely found')
        lines[candidates[0]] = '- 已鉴权的 `GET /v1/models` 发布已启用的既有别名、可唯一解析的 Go 目录原始 ID，以及符合条件的 Custom、CPA 和用户定义供应商名称。Go 新 ID 保持绑定 Go，不会自动变为跨供应商别名；有歧义的名称不发布。Aliases 开关仅影响发布，不改变路由。`application-models` 来自已保存 Go 目录中可解析且已启用协议的名称，不再要求价格表覆盖；价格未知仍为未定价。两个 GET 都只读本地数据。Go 公开目录刷新不携带 Key，不依赖账号、凭据有效性、额度或冷却状态；MiniMax/Kimi 的鉴权目录刷新仍使用已保存 Key。刷新目录不验证账号，也不会自动启用新模型。'
        candidates = [i for i, line in enumerate(lines) if line.startswith('- ') and 'MODEL_PROTOCOLS' in line]
        if len(candidates) != 1:
            raise RuntimeError('Chinese protocol invariant was not uniquely found')
        lines[candidates[0]] = '- `MODEL_PROTOCOLS` 只提供已知模型的离线协议默认值和既有共享别名，不再充当 Go 模型目录白名单。Go、Zen、Command 刷新时读取官方协议文档；只有明确端点或 Command 的官方模型家族规则提供新证据。文档读取失败或漏列模型时，保留原有证据和已保存首选协议。没有历史证据时，Go/Zen 仅使用明确的逐模型离线默认值，完全未知模型不推断为 Chat，需要用户显式选择协议或测试。Go 新模型仍默认关闭。已启用客户端协议直通，否则按已保存首选协议和适配器顺序转换。请求路径不探测协议，也不发送付费发现请求。本地删除目录项后，后续刷新可重新发现该模型并默认关闭。'
        text = '\n'.join(lines) + '\n'
    changes[path] = text

changes['docs/user/model-catalog-refresh.md'] = '''[简体中文](model-catalog-refresh.zh-CN.md)

# Go model catalog refresh

Refresh the model catalog on Providers to fetch the public OpenCode Go directory. This request sends no account Key and can run without a configured Go account. It does not test credentials, make an inference request, or change account quota/cooldown state.

New entries appear in the saved directory and start disabled. Select and enable an upstream protocol before using them. Known offline defaults and recognized official documentation provide protocol hints. If a document cannot be read or omits a model, existing evidence and saved preferences survive; an entirely unknown model is not assumed to support Chat Completions. A model's mandatory preferred-protocol field alone is not proof of support; inspect its available/enabled protocols.

After enablement, new exact Go IDs can appear in the gateway model list, application picker, and Aliases page without a release. They remain provider-pinned names, not automatically shared aliases. Conflicting raw names are withheld from the client list and rejected as ambiguous. Aliases publication switches only hide names; they do not disable routing. Gateway and inference authentication are unchanged.

Price coverage does not control model discovery or selection. Missing prices remain unknown/unpriced, never zero. Refresh pricing separately when needed.
'''
changes['docs/user/model-catalog-refresh.zh-CN.md'] = '''[English](model-catalog-refresh.md)

# Go 模型目录刷新

在供应商页面刷新模型目录即可读取 OpenCode Go 公开目录。请求不携带账号 Key，也不要求已经配置 Go 账号。它不会验证凭据、发送推理请求或修改账号额度与冷却状态。

新条目会进入已保存目录，默认关闭。使用前需要选择并启用上游协议。已知模型的离线默认值和官方文档中的明确端点提供协议参考。文档读取失败或漏列模型时，保留原有证据和已保存首选协议；完全未知模型不会被假定支持 Chat Completions。接口中必填的首选协议字段本身不代表已知支持，应检查可用和已启用协议。

启用后，新的精确 Go ID 无需发版即可进入网关模型列表、应用模型选择器和 Aliases 页面。它们仍绑定 Go，不会自动变成跨供应商共享别名。存在歧义的原始名称不向客户端发布，请求会明确返回名称歧义。Aliases 发布开关只隐藏名称，不禁用路由。网关及推理请求的鉴权保持不变。

价格覆盖不再决定模型能否被发现或选择。缺失价格保持未知、未定价，而不是零。需要时单独刷新价格。
'''

# Validate every edit before writing any source file.
for path, text in sorted(changes.items()):
    dest = ROOT / path
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text)
    print(path)
print(f'Updated {len(changes)} source/documentation files. No tests were run.')
