use super::super::types::*;
pub(super) use super::model_rows::{enabled_protocol, http_model};
use super::*;
use dashboard_v3::{ContractScopeKind, EffectiveModelContract};
use ocg_domain::connection::{
    AuthorizationState, ConnectionOrigin, EligibilityReason, EligibilityState, LegacyConnectionKind,
};

pub(super) fn connection_for<'a>(
    s: &'a PageSnapshot,
    d: &DestinationDto,
) -> Option<&'a ConnectionSummary> {
    let kind = match d.legacy.kind {
        LegacyDestinationKindDto::Builtin => LegacyConnectionKind::BuiltinProvider,
        LegacyDestinationKindDto::Dynamic => LegacyConnectionKind::DynamicProvider,
        LegacyDestinationKindDto::CustomAccount => LegacyConnectionKind::CustomAccount,
        LegacyDestinationKindDto::PlatformParent => return None,
    };
    s.connections
        .iter()
        .find(|c| c.legacy.kind == kind && c.legacy.id == d.legacy.id)
}
fn provider_id(d: &DestinationDto) -> String {
    if d.legacy.kind == LegacyDestinationKindDto::CustomAccount {
        "custom".into()
    } else {
        d.legacy.id.clone()
    }
}
fn destination_item(s: &PageSnapshot, d: &DestinationDto) -> ProviderPageItem {
    let connection = connection_for(s, d);
    let definition = s.definitions.iter().find(|p| p.id == d.legacy.id);
    let credentials = s
        .credentials
        .iter()
        .filter(|c| c.destination_id == d.id)
        .collect::<Vec<_>>();
    let fallback_legacy = LegacyIdentity {
        kind: match d.legacy.kind {
            LegacyDestinationKindDto::Dynamic => LegacyConnectionKind::DynamicProvider,
            LegacyDestinationKindDto::CustomAccount | LegacyDestinationKindDto::PlatformParent => {
                LegacyConnectionKind::CustomAccount
            }
            _ => LegacyConnectionKind::BuiltinProvider,
        },
        id: d.legacy.id.clone(),
    };
    let allowed = !d.capabilities.external_integration
        && d.auth_scheme != AuthSchemeDto::None
        && d.max_credentials != Some(1);
    ProviderPageItem {
        rail_key: format!("d:{}", d.id),
        destination_id: Some(d.id.clone()),
        connection_id: connection.map(|c| c.id.clone()),
        provider_id: Some(provider_id(d)),
        legacy: connection
            .map(|c| c.legacy.clone())
            .unwrap_or(fallback_legacy),
        name: d.name.clone(),
        brand_family: d.brand_family.clone(),
        preset_id: definition.and_then(|p| p.preset_id.clone()),
        origin: connection
            .map(|c| c.origin)
            .unwrap_or(ConnectionOrigin::Builtin),
        lifecycle: connection.map(|c| c.lifecycle).unwrap_or(if d.enabled {
            ConnectionLifecycle::Configured
        } else {
            ConnectionLifecycle::Disabled
        }),
        authorization: connection
            .map(|c| c.authorization)
            .unwrap_or(AuthorizationState::NotRequired),
        eligibility: connection
            .map(|c| c.eligibility.clone())
            .unwrap_or(Eligibility {
                state: if d.enabled {
                    EligibilityState::Eligible
                } else {
                    EligibilityState::Ineligible
                },
                reason: if d.enabled {
                    EligibilityReason::None
                } else {
                    EligibilityReason::ConnectionDisabled
                },
            }),
        credential_create: connection.map(|c| c.credential_create.clone()).unwrap_or(
            CredentialCreateCapabilityDto {
                allowed,
                material_kinds: vec![],
                reason: (!allowed).then_some(CredentialCreateUnavailableReasonDto::Unavailable),
            },
        ),
        credential_count: count(credentials.len()),
        enabled_credential_count: count(credentials.iter().filter(|c| c.enabled).count()),
        catalog_count: count(d.catalog.len()),
    }
}
fn items(s: &PageSnapshot) -> Vec<ProviderPageItem> {
    let mut items = s
        .destinations
        .iter()
        .filter(|d| d.legacy.kind != LegacyDestinationKindDto::PlatformParent)
        .map(|d| destination_item(s, d))
        .collect::<Vec<_>>();
    for c in &s.connections {
        if items
            .iter()
            .any(|i| i.connection_id.as_deref() == Some(&c.id))
        {
            continue;
        }
        if c.lifecycle != ConnectionLifecycle::Draft {
            continue;
        }
        let provider_id = match c.legacy.kind {
            LegacyConnectionKind::CustomAccount => Some("custom".into()),
            _ => Some(c.legacy.id.clone()),
        };
        let preset_id = s
            .definitions
            .iter()
            .find(|p| p.id == c.legacy.id)
            .and_then(|p| p.preset_id.clone());
        items.push(ProviderPageItem {
            rail_key: format!("c:{}", c.id),
            destination_id: None,
            connection_id: Some(c.id.clone()),
            provider_id,
            legacy: c.legacy.clone(),
            name: c.name.clone(),
            brand_family: c.display_family.clone(),
            preset_id,
            origin: c.origin,
            lifecycle: c.lifecycle,
            authorization: c.authorization,
            eligibility: c.eligibility.clone(),
            credential_create: c.credential_create.clone(),
            credential_count: c.credential_count,
            enabled_credential_count: c.enabled_credential_count,
            catalog_count: c.target_count,
        });
    }
    items
}
pub(super) fn project(s: &PageSnapshot, q: &PageQuery) -> ProvidersPage {
    let mut rows = items(s);
    let total = rows.len();
    let needle = q.search();
    rows.retain(|r| {
        needle.is_empty()
            || [
                r.name.as_str(),
                r.brand_family.as_deref().unwrap_or_default(),
                r.provider_id.as_deref().unwrap_or_default(),
                r.destination_id
                    .as_ref()
                    .and_then(|id| s.destinations.iter().find(|d| &d.id == id))
                    .and_then(|d| d.base_url.as_deref())
                    .unwrap_or_default(),
            ]
            .iter()
            .any(|v| v.to_lowercase().contains(&needle))
    });
    rows.sort_by(|a, b| natural_cmp(&a.name, &b.name).then_with(|| a.rail_key.cmp(&b.rail_key)));
    if q.sort.as_deref() == Some("name_desc") {
        rows.reverse();
    }
    let filtered = rows.len();
    let (offset, limit) = q.bounds();
    ProvidersPage {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        as_of: s.as_of.to_rfc3339(),
        valid_until: Some(s.valid_until.to_rfc3339()),
        total: count(total),
        filtered_total: count(filtered),
        items: rows.into_iter().skip(offset).take(limit).collect(),
        offset: count(offset),
        limit: count(limit),
        has_more: offset + limit < filtered,
        errors: s.errors.clone(),
    }
}
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(ac), Some(bc)) if ac.is_ascii_digit() && bc.is_ascii_digit() => {
                let mut an = String::new();
                let mut bn = String::new();
                while ai.peek().is_some_and(|c| c.is_ascii_digit()) {
                    an.push(ai.next().unwrap());
                }
                while bi.peek().is_some_and(|c| c.is_ascii_digit()) {
                    bn.push(bi.next().unwrap());
                }
                let av = an.trim_start_matches('0');
                let bv = bn.trim_start_matches('0');
                let cmp = av.len().cmp(&bv.len()).then_with(|| av.cmp(bv));
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            (Some(ac), Some(bc)) => {
                let cmp = ac.cmp(&bc);
                ai.next();
                bi.next();
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
        }
    }
}
fn selected<'a>(
    state: &CoreState,
    s: &'a PageSnapshot,
    id: &str,
) -> Result<
    (
        ProviderPageItem,
        Option<&'a DestinationDto>,
        Option<&'a ConnectionSummary>,
    ),
    V3ApiError,
> {
    let item = items(s)
        .into_iter()
        .find(|i| {
            i.rail_key == id
                || i.destination_id.as_deref() == Some(id)
                || i.provider_id.as_deref() == Some(id.strip_prefix("p:").unwrap_or(id))
                || i.connection_id.as_deref() == Some(id.strip_prefix("c:").unwrap_or(id))
        })
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider page item not found"))?;
    let d = item
        .destination_id
        .as_ref()
        .and_then(|id| s.destinations.iter().find(|d| &d.id == id));
    let c = item
        .connection_id
        .as_ref()
        .and_then(|id| s.connections.iter().find(|c| &c.id == id));
    Ok((item, d, c))
}
fn catalog_summary(c: &dashboard_v3::EffectiveCatalog) -> ProviderCatalogSummary {
    ProviderCatalogSummary {
        source: c.source.clone(),
        source_url: c.source_url.clone(),
        refreshed_at: c.refreshed_at.clone(),
        model_count: count(c.models.len()),
        refresh_supported: c.refresh_supported,
    }
}
pub(super) fn scope(
    s: &PageSnapshot,
    d: &DestinationDto,
) -> Option<(ProviderScopeSummary, Vec<EffectiveModelContract>)> {
    if d.legacy.kind == LegacyDestinationKindDto::Builtin {
        let p = s
            .contracts
            .providers
            .iter()
            .find(|p| p.provider_id == d.legacy.id)?;
        let models = p.models.clone();
        return Some((
            ProviderScopeSummary {
                key: format!("provider:{}", p.scope_id),
                scope_kind: p.scope_kind,
                scope_id: p.scope_id.clone(),
                provider_id: p.provider_id.clone(),
                label: d.name.clone(),
                static_protocol_snapshot_date: p.static_protocol_snapshot_date.clone(),
                account_count: count(p.accounts.len()),
                catalog: catalog_summary(&p.catalog),
                usage: p.usage.clone(),
                card: p.card,
                catalog_routable: p.catalog_routable,
                production_inference: p.production_inference,
                disabled_reasons: p.disabled_reasons.clone(),
                revision: p.revision,
                all_disabled: models.iter().all(|m| enabled_protocol(m).is_none()),
            },
            models,
        ));
    }
    let models = d
        .catalog
        .iter()
        .map(|m| http_model(d, m))
        .collect::<Vec<_>>();
    let account_count = count(
        s.credentials
            .iter()
            .filter(|c| c.destination_id == d.id)
            .filter_map(|c| s.accounts.iter().find(|a| a.id == c.legacy_account_id))
            .count(),
    );
    let dynamic = s.definitions.iter().find(|p| p.id == d.legacy.id);
    Some((
        ProviderScopeSummary {
            key: format!("custom_endpoint:{}", d.id),
            scope_kind: ContractScopeKind::CustomEndpoint,
            scope_id: d.id.clone(),
            provider_id: provider_id(d),
            label: d.name.clone(),
            static_protocol_snapshot_date: None,
            account_count,
            catalog: ProviderCatalogSummary {
                source: if dynamic.is_some_and(|p| p.preset_id.is_some()) {
                    "preset"
                } else {
                    "static"
                }
                .into(),
                source_url: String::new(),
                refreshed_at: None,
                model_count: count(models.len()),
                refresh_supported: d.capabilities.discoverable_models,
            },
            usage: dashboard_v3::CapabilitySummary {
                availability: "not_applicable".into(),
            },
            card: dashboard_v3::CardCapabilitySummary {
                fetch_zen_models: false,
                discover_models: d.capabilities.discoverable_models,
                protocol_probe: d.capabilities.testable,
                catalog_refresh: d.capabilities.discoverable_models,
            },
            catalog_routable: d.enabled,
            production_inference: d.enabled,
            disabled_reasons: if d.enabled {
                vec![]
            } else {
                vec!["destination_disabled".into()]
            },
            revision: s.revision.revision,
            all_disabled: models.iter().all(|m| enabled_protocol(m).is_none()),
        },
        models,
    ))
}
pub(super) fn model_rows(s: &PageSnapshot, d: &DestinationDto) -> Vec<ProviderModelPageRow> {
    let presentation = if d.legacy.kind == LegacyDestinationKindDto::Builtin {
        s.contracts
            .providers
            .iter()
            .find(|p| p.provider_id == d.legacy.id)
            .and_then(|p| p.presentation.as_ref())
    } else {
        d.presentation.as_ref()
    };
    presentation
        .into_iter()
        .flat_map(|p| p.models.iter())
        .map(|m| {
            let metadata = s.metadata.get(&d.id).and_then(|rows| {
                rows.iter()
                    .find(|r| r.public_model == m.public_model)
                    .or_else(|| rows.iter().find(|r| r.upstream_model == m.upstream_model))
            });
            ProviderModelPageRow {
                public_model: m.public_model.clone(),
                upstream_model: m.upstream_model.clone(),
                contract: m.contract.clone(),
                upstream_override: m.upstream_override.clone(),
                metadata: metadata.map(|v| v.metadata.clone()),
                metadata_source: metadata.map(|v| v.source.clone()),
                target_protocol: m.target_protocol,
                test_protocol: m.test_protocol,
                writable_protocols: m.writable_protocols.clone(),
                effective_on: m.effective_on,
                actions: m
                    .actions
                    .iter()
                    .map(|a| PageAction {
                        key: a.key.clone(),
                        allowed: a.allowed,
                        reason: a.reason.clone(),
                    })
                    .collect(),
            }
        })
        .collect()
}
pub(super) fn detail(
    state: &CoreState,
    s: &PageSnapshot,
    id: &str,
) -> Result<ProviderPageDetail, V3ApiError> {
    let (item, d, c) = selected(state, s, id)?;
    let catalog_entry = item
        .provider_id
        .as_ref()
        .and_then(|p| s.catalog.iter().find(|e| &e.provider_id == p))
        .map(Into::into);
    Ok(ProviderPageDetail {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        item,
        destination: d.map(Into::into),
        endpoints: c.map(|c| c.endpoints.clone()).unwrap_or_default(),
        catalog_entry,
        scope: d.and_then(|d| scope(s, d).map(|v| v.0)),
        model_write_target: d.filter(|d| !d.capabilities.external_integration).map(|d| {
            ProviderModelWriteTarget {
                kind: if d.legacy.kind == LegacyDestinationKindDto::Builtin {
                    "provider"
                } else {
                    "destination"
                }
                .into(),
                id: if d.legacy.kind == LegacyDestinationKindDto::Builtin {
                    d.legacy.id.clone()
                } else {
                    d.id.clone()
                },
            }
        }),
        actions: vec![
            action(
                "modelEditable",
                d.is_some_and(|d| !d.capabilities.external_integration),
            ),
            action(
                "metadataEditable",
                d.is_some_and(|d| !d.capabilities.external_integration),
            ),
            action(
                "edit",
                d.is_some_and(|d| d.adapter == AdapterKindDto::Http)
                    || c.is_some_and(|c| c.lifecycle == ConnectionLifecycle::Draft),
            ),
            action(
                "delete",
                d.is_some_and(|d| d.adapter == AdapterKindDto::Http && !d.capabilities.observer)
                    && !s
                        .credentials
                        .iter()
                        .any(|cr| d.is_some_and(|d| cr.destination_id == d.id)),
            ),
            action(
                "refreshCatalog",
                d.is_some_and(|d| {
                    !d.capabilities.external_integration
                        && (d.capabilities.discoverable_models
                            || scope(s, d).is_some_and(|v| v.0.catalog.refresh_supported))
                }),
            ),
        ],
    })
}
pub(super) fn models(
    state: &CoreState,
    s: &PageSnapshot,
    id: &str,
    q: &PageQuery,
) -> Result<ProviderModelsPage, V3ApiError> {
    let (_, d, _) = selected(state, s, id)?;
    let mut models = d.map(|d| model_rows(s, d)).unwrap_or_default();
    let total = models.len();
    let all_disabled = models.iter().all(|m| !m.effective_on);
    let needle = q.search();
    models.retain(|m| {
        (!q.enabled_only.unwrap_or(false) || m.effective_on)
            && (needle.is_empty()
                || m.public_model.to_lowercase().contains(&needle)
                || m.upstream_model.to_lowercase().contains(&needle))
    });
    models.sort_by(|a, b| {
        natural_cmp(&a.public_model, &b.public_model)
            .then_with(|| a.upstream_model.cmp(&b.upstream_model))
    });
    let filtered = models.len();
    let (mut offset, limit) = q.bounds();
    if let Some(target) = &q.model
        && let Some(index) = models.iter().position(|m| {
            &m.public_model == target
                || &m.upstream_model == target
                || &m.contract.model_id == target
        })
    {
        offset = (index / limit) * limit;
    }
    Ok(ProviderModelsPage {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        total: count(total),
        filtered_total: count(filtered),
        all_disabled,
        models: models.into_iter().skip(offset).take(limit).collect(),
        offset: count(offset),
        limit: count(limit),
        has_more: offset + limit < filtered,
    })
}
pub(super) fn edit_detail(
    state: &CoreState,
    s: &PageSnapshot,
    id: &str,
) -> Result<ProviderEditDetail, V3ApiError> {
    let (item, d, c) = selected(state, s, id)?;
    let destination = d.cloned();
    let definition = item
        .provider_id
        .as_ref()
        .and_then(|p| s.definitions.iter().find(|d| &d.id == p))
        .cloned();
    let credentials = s
        .credentials
        .iter()
        .filter(|credential| d.is_some_and(|d| credential.destination_id == d.id))
        .cloned()
        .collect::<Vec<_>>();
    let accounts = {
        let db = state.db.lock();
        let dynamic = db
            .list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?;
        s.accounts
            .iter()
            .filter(|a| {
                credentials.iter().any(|c| c.legacy_account_id == a.id)
                    || (d.is_none() && item.provider_id.as_deref() == Some(&a.provider_id))
            })
            .filter_map(|a| db.get_account(&a.id).transpose())
            .map(|a| {
                a.map_err(V3ApiError::internal)
                    .and_then(|a| dashboard_v3::accounts::account_from_db(state, &db, a, &dynamic))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let identities = accounts::identities(state)?
        .into_iter()
        .filter(|i| {
            i.credentials
                .iter()
                .any(|c| accounts.iter().any(|a| a.id == c.legacy.id))
        })
        .collect();
    if cache::ReadVersion::capture(state)?.token() != s.read_version {
        return Err(V3ApiError::conflict_at(
            state,
            "provider changed during detail read",
        ));
    }
    let related_contracts = dashboard_v3::ProviderContracts {
        providers: s
            .contracts
            .providers
            .iter()
            .filter(|p| item.provider_id.as_deref() == Some(&p.provider_id))
            .cloned()
            .collect(),
        custom_endpoints: s
            .contracts
            .custom_endpoints
            .iter()
            .filter(|p| accounts.iter().any(|a| a.id == p.scope_id))
            .cloned()
            .collect(),
        revision: s.revision.revision,
        process_generation: s.revision.process_generation,
        pricing_revision: s.revision.pricing_revision.clone(),
    };
    let catalog_entry = item
        .provider_id
        .as_ref()
        .and_then(|id| s.catalog.iter().find(|e| &e.provider_id == id))
        .cloned();
    Ok(ProviderEditDetail {
        scope: d.and_then(|d| {
            scope(s, d).map(|(summary, _)| ProviderEditScope {
                summary,
                models: model_rows(s, d),
                accounts: s
                    .contracts
                    .providers
                    .iter()
                    .find(|p| {
                        d.legacy.kind == LegacyDestinationKindDto::Builtin
                            && p.provider_id == d.legacy.id
                    })
                    .map(|p| p.accounts.clone())
                    .unwrap_or_default(),
            })
        }),
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        item,
        destination,
        definition,
        related_contracts,
        catalog_entry,
        credentials,
        accounts,
        identities,
        connection: c.cloned(),
    })
}

impl From<&dashboard_v3::ProviderCatalogEntry> for ProviderCatalogEntrySummary {
    fn from(e: &dashboard_v3::ProviderCatalogEntry) -> Self {
        Self {
            provider_id: e.provider_id.clone(),
            origin: e.origin,
            editable: e.editable,
            deletable: e.deletable,
            offering: e.offering.clone(),
            display_name: e.display_name.clone(),
            display_family: e.display_family.clone(),
            credential_kind: e.credential_kind,
            quota_scope: e.quota_scope,
            singleton: e.singleton,
            creation_availability: e.creation_availability.clone(),
            creation_unavailable_reason: e.creation_unavailable_reason.clone(),
            verification_policy: e.verification_policy.clone(),
            verification_runtime_availability: e.verification_runtime_availability.clone(),
            routable: e.routable,
            managed_registration: e.managed_registration,
            pricing_availability: e.pricing_availability.clone(),
            usage_availability: e.usage_availability.clone(),
            manual_usage_calibration: e.manual_usage_calibration,
            quota_unit: e.quota_unit.clone(),
            model_source: e.model_source.clone(),
            key_prefix: e.key_prefix.clone(),
            auth_schemes: e.auth_schemes.clone(),
            upstream_protocols: e.upstream_protocols.clone(),
            form_fields: e.form_fields.clone(),
            model_alias_count: count(e.model_aliases.len()),
        }
    }
}
