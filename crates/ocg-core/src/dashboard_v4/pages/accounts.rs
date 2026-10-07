use super::super::types::*;
use super::*;
use ocg_domain::credential::{AuthState, ModelScope};

impl From<&crate::platform::PlatformSnapshot> for PlatformSnapshotSummary {
    fn from(s: &crate::platform::PlatformSnapshot) -> Self {
        use crate::platform::{PlatformQuota, PlatformQuotaKind};
        let primary = |kind: fn(&PlatformQuota) -> bool| {
            s.quotas
                .iter()
                .find(|q| kind(q) && q.period.as_deref().is_none_or(str::is_empty))
                .or_else(|| s.quotas.iter().find(|q| kind(q)))
        };
        let wallet = primary(|q| matches!(q.kind, PlatformQuotaKind::Wallet));
        let month = s.quotas.iter().find(|q| {
            matches!(q.kind, PlatformQuotaKind::Wallet) && q.period.as_deref() == Some("month")
        });
        let key = primary(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit));
        let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
        Self {
            wallet: wallet.or(month).map(|base| PlatformWalletSummary {
                unit: base.unit.clone(),
                remaining: wallet
                    .filter(|q| !q.unlimited)
                    .and_then(|q| finite(q.remaining)),
                remaining_unlimited: wallet.is_some_and(|q| q.unlimited),
                history_used: wallet.filter(|q| !q.unlimited).and_then(|q| finite(q.used)),
                month_used: month
                    .filter(|q| q.unit == base.unit)
                    .and_then(|q| finite(q.used)),
                observed_at: (s.observed_at > 0).then_some(s.observed_at),
            }),
            key_remaining: key.filter(|q| !q.unlimited).and_then(|q| {
                finite(q.remaining).map(|amount| PlatformAmountSummary {
                    amount,
                    unit: q.unit.clone(),
                })
            }),
            key_name: s
                .quotas
                .iter()
                .find(|q| {
                    matches!(q.kind, PlatformQuotaKind::KeyLimit)
                        && !q.scope_id.trim().is_empty()
                        && q.scope_id.trim() != "key"
                })
                .map(|q| q.scope_id.trim().to_string()),
            observed_at: s.observed_at,
            stale: s.stale,
            errors: s.errors.clone(),
            quotas: s.quotas.clone(),
            billing_preference: s.billing_preference.clone(),
            wallet_overflow: s.wallet_overflow,
            model_count: count(s.models.len()),
            price_count: count(s.prices.len()),
            group_count: count(s.groups.len()),
        }
    }
}
impl From<&crate::platform::PlatformAccount> for PlatformSummary {
    fn from(p: &crate::platform::PlatformAccount) -> Self {
        Self {
            id: p.id.clone(),
            kind: p.kind,
            name: p.name.clone(),
            base_url: p.base_url.clone(),
            has_user_credential: p.has_user_credential,
            version: p.version,
            snapshot: p.snapshot.as_ref().map(Into::into),
        }
    }
}
impl From<&crate::platform::PlatformLink> for PlatformLinkSummary {
    fn from(p: &crate::platform::PlatformLink) -> Self {
        Self {
            account_id: p.account_id.clone(),
            platform_account_id: p.platform_account_id.clone(),
            group: p.group.clone(),
            snapshot: p.snapshot.as_ref().map(Into::into),
        }
    }
}
impl From<&DestinationCredentialDto> for AccountCredentialSummary {
    fn from(c: &DestinationCredentialDto) -> Self {
        Self {
            id: c.id.clone(),
            legacy_account_id: c.legacy_account_id.clone(),
            destination_id: c.destination_id.clone(),
            name: c.name.clone(),
            notes: c.notes.clone(),
            has_secret: c.has_secret,
            key_preview: c.has_secret.then(|| "••••".into()),
            enabled: c.enabled,
            routing_rank: c.routing_rank,
            scope: CredentialScopeSummary {
                kind: match c.scope {
                    ModelScope::All => "all",
                    ModelScope::Only { .. } => "only",
                }
                .into(),
                model_count: match &c.scope {
                    ModelScope::All => 0,
                    ModelScope::Only { models } => count(models.len()),
                },
                single_model: match &c.scope {
                    ModelScope::Only { models } if models.len() == 1 => Some(models[0].clone()),
                    _ => None,
                },
            },
            grants: c.grants.clone(),
            auth_state: c.auth_state,
            last_error: c.last_error.clone(),
            cooldowns: c.cooldowns.clone(),
            quota_pool_id: c.quota_pool_id.clone(),
            onboarding_task: c.onboarding_task.clone(),
            purchase_date: c.purchase_date.clone(),
            quota_recovery: c.quota_recovery.clone(),
        }
    }
}

pub(super) fn refresh_fact(
    account: Option<&AccountSummary>,
    billing: Option<&crate::billing_types::BillingStatus>,
    link: Option<&crate::platform::PlatformLink>,
    now: DateTime<Utc>,
) -> AccountRefreshFact {
    let observed = link
        .and_then(|l| l.snapshot.as_ref())
        .filter(|s| !s.stale)
        .and_then(|s| DateTime::<Utc>::from_timestamp(s.observed_at, 0))
        .map(|t| t.to_rfc3339())
        .or_else(|| account.and_then(|a| a.usage_sync_last_success_at.clone()));
    let fresh_until = observed
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| (t + Duration::minutes(5)).to_rfc3339());
    let supported = billing.is_none_or(|b| b.model != crate::billing_types::BillingModel::Credits)
        && (link.is_some() || billing.is_some_and(|b| b.official_refresh));
    let next = account
        .and_then(|a| a.usage_sync_next_allowed_at.clone())
        .filter(|t| DateTime::parse_from_rfc3339(t).is_ok_and(|t| t > now));
    AccountRefreshFact {
        supported,
        observed_at: observed,
        fresh_until,
        next_allowed_at: next,
    }
}
fn row(s: &PageSnapshot, d: &DestinationDto, c: &DestinationCredentialDto) -> AccountPageRow {
    let account = s.accounts.iter().find(|a| a.id == c.legacy_account_id);
    let link = s.links.iter().find(|l| l.account_id == c.legacy_account_id);
    let billing = s.billing.get(&c.legacy_account_id);
    let refresh = refresh_fact(account, billing, link, s.as_of);
    let cooling = active_cooldown(d, c, s.as_of);
    let ready = account.is_some_and(|a| a.setup_step == dashboard_v3::AccountSetupStep::Ready);
    let status = if account.is_none() {
        "unknown"
    } else if !ready {
        "registering"
    } else if !c.enabled {
        "disabled"
    } else if c.auth_state == AuthState::Invalid {
        "unavailable"
    } else if cooling {
        "cooling"
    } else if let Some(recovery) = &c.quota_recovery {
        match recovery.status {
            QuotaRecoveryStatus::Waiting => "quota_waiting",
            QuotaRecoveryStatus::Ready => "quota_ready",
            QuotaRecoveryStatus::Probing => "quota_probing",
        }
    } else if !d.enabled || account.is_some_and(|a| !a.plan_routable) {
        "draft"
    } else {
        "enabled"
    };
    let external = d.capabilities.external_integration;
    let has_account = account.is_some();
    let mut actions = Vec::new();
    if has_account {
        actions.push(action("toggle", ready));
    }
    if refresh.supported {
        actions.push(action("refresh-usage", ready));
    }
    if billing.is_some_and(|b| b.manual_calibration) {
        actions.push(action("manual-usage", ready));
    }
    if d.capabilities.testable && !external {
        actions.push(action("test-connection", has_account && ready));
    }
    if !external
        && link.is_none()
        && (d.capabilities.discoverable_models
            || providers::scope(s, d)
                .is_some_and(|v| v.0.card.catalog_refresh || v.0.card.fetch_zen_models))
    {
        actions.push(action("refresh-models", has_account && ready));
    }
    let writable = has_account
        && ready
        && !external
        && d.account_controls.toggle_write != AccountToggleWriteDto::ProviderSettings
        && account
            .is_some_and(|a| a.credential_kind == dashboard_v3::AccountCredentialKind::ApiKey);
    if writable {
        actions.push(action("rotate-key", true));
        actions.push(action("edit-binding", true));
        if providers::connection_for(s, d)
            .is_some_and(|connection| connection.credential_create.allowed)
        {
            actions.push(action("add-key", true));
        }
    }
    if let Some(recovery) = &c.quota_recovery
        && recovery.status == QuotaRecoveryStatus::Waiting
    {
        actions.push(action("retry-quota", true));
    }
    if d.capabilities.testable && !external {
        actions.push(action("open-models", has_account));
    }
    if external {
        actions.push(action("open-cpa", true));
    } else if link.is_some() {
        actions.push(action("fetch-models", true));
        actions.push(action("edit-key", has_account));
        actions.push(action("unlink", true));
        actions.push(action("delete", has_account));
    } else if d.account_controls.toggle_write != AccountToggleWriteDto::ProviderSettings {
        if d.capabilities.managed_signup && !ready {
            actions.push(action("continue-setup", has_account));
        }
        if d.account_controls.browser_profile {
            actions.push(action("reset-profile", has_account));
        }
        if ready && d.account_controls.console_link == Some(AccountConsoleLinkDto::Opencode) {
            actions.push(action("open-console", true));
        }
        if ready && d.account_controls.console_link == Some(AccountConsoleLinkDto::Ollama) {
            actions.push(action("open-site", true));
        }
        if has_account {
            actions.push(action("edit", true));
            actions.push(action("delete", true));
        }
        if has_account && cooling {
            actions.push(action("reset", true));
        }
    }
    if has_account && ready && d.plan.as_ref().is_some_and(|p| p.expiry_cadence.is_some()) {
        actions.push(action("purchase-date", true));
    }
    if let Some(card) = s
        .cards
        .iter()
        .find(|card| card.credential_ids.contains(&c.id))
        && let Some(position) = card.credential_ids.iter().position(|id| id == &c.id)
    {
        actions.push(action("move-up", position > 0));
        actions.push(action(
            "move-down",
            position + 1 < card.credential_ids.len(),
        ));
        actions.push(action("move-to-card", true));
    }
    let model_count = match &c.scope {
        ModelScope::All => count(d.catalog.len()),
        ModelScope::Only { models } => count(models.len()),
    };
    AccountPageRow {
        credential: c.into(),
        account: account.cloned(),
        platform_link: link.map(Into::into),
        status: status.into(),
        route_available: full_route_available(s, d, c),
        model_count,
        inference_endpoint_url: d.base_url.clone(),
        actions,
        billing: billing.cloned(),
        refresh,
        tags: s
            .tags
            .get(&c.legacy_account_id)
            .cloned()
            .unwrap_or(AccountPageTags {
                credential_count: 1,
                binding_disabled: false,
                quota_share_name: None,
                quota_share_count: 0,
                duplicate_name: false,
            }),
    }
}
fn active_cooldown(d: &DestinationDto, c: &DestinationCredentialDto, now: DateTime<Utc>) -> bool {
    let free = d.plan.as_ref().is_some_and(|p| {
        !p.windows.is_empty() && p.windows.iter().all(|w| w.kind == PlanWindowKindDto::Free)
    });
    let windows = if free {
        vec![&c.cooldowns.generic_until, &c.cooldowns.free_until]
    } else {
        vec![
            &c.cooldowns.generic_until,
            &c.cooldowns.five_hour_until,
            &c.cooldowns.week_until,
            &c.cooldowns.month_until,
        ]
    };
    windows.iter().any(|at| {
        at.as_deref()
            .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            .is_some_and(|at| at > now)
    })
}
fn route_available(d: &DestinationDto, c: &DestinationCredentialDto, now: DateTime<Utc>) -> bool {
    d.enabled
        && c.enabled
        && c.auth_state != AuthState::Invalid
        && c.quota_recovery.is_none()
        && (d.adapter == AdapterKindDto::Cpa
            || d.auth_scheme == AuthSchemeDto::None
            || c.has_secret)
        && c.onboarding_task
            .as_ref()
            .is_none_or(|t| t.state != ocg_domain::credential::OnboardingTaskState::InProgress)
        && match &c.scope {
            ModelScope::All => true,
            ModelScope::Only { models } => !models.is_empty(),
        }
        && !active_cooldown(d, c, now)
}
fn full_route_available(
    s: &PageSnapshot,
    d: &DestinationDto,
    c: &DestinationCredentialDto,
) -> bool {
    !s.tags
        .get(&c.legacy_account_id)
        .is_some_and(|tags| tags.binding_disabled)
        && (!d.capabilities.external_integration
            || s.cpa_status
                .as_deref()
                .is_some_and(|status| status == "running" || status == "external"))
        && route_available(d, c, s.as_of)
        && s.accounts
            .iter()
            .find(|a| a.id == c.legacy_account_id)
            .is_some_and(|a| {
                a.setup_step == dashboard_v3::AccountSetupStep::Ready && a.plan_routable
            })
}
fn plan_key(d: &DestinationDto) -> String {
    if d.legacy.kind == LegacyDestinationKindDto::CustomAccount {
        "custom".into()
    } else {
        d.legacy.id.clone()
    }
}
fn card_rows(s: &PageSnapshot, card: &RoutingCard, q: &PageQuery) -> Vec<AccountPageRow> {
    let Some(d) = s.destinations.iter().find(|d| d.id == card.destination_id) else {
        return vec![];
    };
    let needle = q.search();
    card.credential_ids
        .iter()
        .filter_map(|id| s.credentials.iter().find(|c| &c.id == id))
        .map(|c| row(s, d, c))
        .filter(|r| {
            let status = q.status.as_deref().unwrap_or_default();
            let status_match = status.is_empty()
                || status == "all"
                || r.status == status
                || (status == "enabled" && r.credential.enabled)
                || (status == "disabled" && !r.credential.enabled)
                || (status == "available" && r.route_available)
                || (status == "auth-error"
                    && (r.account.as_ref().is_some_and(|a| a.auth_error.is_some())
                        || r.credential.auth_state == AuthState::Invalid));
            status_match
                && (needle.is_empty()
                    || [
                        r.credential.name.as_str(),
                        r.credential.legacy_account_id.as_str(),
                        d.name.as_str(),
                        d.brand_family.as_deref().unwrap_or_default(),
                        r.account
                            .as_ref()
                            .and_then(|a| a.username.as_deref())
                            .unwrap_or_default(),
                    ]
                    .iter()
                    .any(|v| v.to_lowercase().contains(&needle)))
        })
        .collect()
}
fn card(
    s: &PageSnapshot,
    layout: &RoutingCard,
    position: usize,
    rows: Vec<AccountPageRow>,
    matched: usize,
    rows_offset: usize,
) -> Option<AccountCardPageItem> {
    let d = s
        .destinations
        .iter()
        .find(|d| d.id == layout.destination_id)?;
    let platform = if d.legacy.kind == LegacyDestinationKindDto::PlatformParent {
        s.platforms.iter().find(|p| p.id == d.legacy.id)
    } else {
        layout.credential_ids.iter().find_map(|id| {
            s.credentials
                .iter()
                .find(|c| c.id == *id)
                .and_then(|c| s.links.iter().find(|l| l.account_id == c.legacy_account_id))
                .and_then(|l| s.platforms.iter().find(|p| p.id == l.platform_account_id))
        })
    };
    let connection = providers::connection_for(s, d);
    let create = if platform.is_some() {
        Some(CredentialCreateCapabilityDto {
            allowed: true,
            material_kinds: vec![ocg_domain::credential::MaterialKind::ApiKey],
            reason: None,
        })
    } else {
        connection.map(|c| c.credential_create.clone())
    };
    let members = s
        .credentials
        .iter()
        .filter(|c| layout.credential_ids.contains(&c.id))
        .collect::<Vec<_>>();
    let availability = if members.is_empty() {
        "no_keys"
    } else if members.iter().all(|c| c.quota_recovery.is_some()) {
        "quota_exhausted"
    } else if members.iter().any(|c| full_route_available(s, d, c)) {
        "available"
    } else {
        "no_available_keys"
    };
    let destination_count = s
        .credentials
        .iter()
        .filter(|c| c.destination_id == d.id)
        .count();
    let mut actions = vec![
        action("move-card-up", position > 0),
        action("move-card-top", position > 0),
        action("move-card-down", position + 1 < s.cards.len()),
        action("move-card-bottom", position + 1 < s.cards.len()),
        action("add-card", true),
    ];
    if layout.credential_ids.is_empty()
        && s.cards
            .iter()
            .filter(|c| c.destination_id == layout.destination_id)
            .count()
            > 1
    {
        actions.push(action("remove-empty-card", true));
    }
    if d.adapter == AdapterKindDto::Http && !d.capabilities.observer && destination_count == 0 {
        actions.push(action("delete-group", true));
    }
    if create.as_ref().is_some_and(|c| c.allowed) {
        actions.push(action("add-key", true));
    }
    if let Some(parent) = platform {
        actions.push(action("edit", true));
        if destination_count == 0 {
            actions.push(action("delete", true));
        }
        if parent.has_user_credential {
            actions.push(action("refresh-parent", true));
        }
        if destination_count > 0 {
            actions.push(action("fetch-all-models", true));
        }
        if parent.kind == crate::platform::PlatformKind::NewApi && parent.has_user_credential {
            actions.push(action("import-keys", true));
        }
        actions.push(action("link-existing", true));
    } else if d.capabilities.discoverable_models {
        actions.push(action("fetch-models", true));
        actions.push(action("fetch-all-models", true));
    }
    let rows_len = rows.len();
    Some(AccountCardPageItem {
        card_id: layout.id.clone(),
        position: count(position),
        destination: d.into(),
        platform: platform.map(Into::into),
        total_credentials: count(layout.credential_ids.len()),
        matched_credentials: count(matched),
        rows,
        credential_create: create,
        actions,
        availability: availability.into(),
        rows_offset: count(rows_offset),
        rows_has_more: rows_offset + rows_len < matched,
        cpa_status: if d.capabilities.external_integration {
            s.cpa_status.clone()
        } else {
            None
        },
    })
}
pub(super) fn project(s: &PageSnapshot, q: &PageQuery) -> AccountsPage {
    let (offset, limit) = q.bounds();
    let mut filtered = Vec::new();
    let mut plans = Vec::<AccountPlanFilter>::new();
    let mut matched_credentials = 0;
    let needle = q.search();
    for (position, layout) in s.cards.iter().enumerate() {
        let Some(d) = s
            .destinations
            .iter()
            .find(|d| d.id == layout.destination_id)
        else {
            continue;
        };
        let key = plan_key(d);
        if let Some(p) = plans.iter_mut().find(|p| p.value == key) {
            p.card_count += 1;
            p.credential_count += count(layout.credential_ids.len());
        } else {
            plans.push(AccountPlanFilter {
                value: key.clone(),
                label: d.name.clone(),
                card_count: 1,
                credential_count: count(layout.credential_ids.len()),
            });
        }
        if q.plan
            .as_deref()
            .is_some_and(|p| !p.is_empty() && p != "all" && p != key && p != d.id)
        {
            continue;
        }
        let rows = card_rows(s, layout, q);
        let empty_match = layout.credential_ids.is_empty()
            && q.status
                .as_deref()
                .is_none_or(|st| st.is_empty() || st == "all" || st == "empty")
            && (needle.is_empty() || d.name.to_lowercase().contains(&needle));
        if rows.is_empty() && !empty_match {
            continue;
        }
        matched_credentials += rows.len();
        filtered.push((position, layout, rows));
    }
    let matched_cards = filtered.len();
    let units: usize = filtered.iter().map(|(_, _, r)| r.len().max(1)).sum();
    let mut cursor = 0;
    let mut cards = Vec::new();
    for (position, layout, rows) in filtered {
        let length = rows.len().max(1);
        let start = offset.saturating_sub(cursor).min(length);
        let end = (offset + limit).saturating_sub(cursor).min(length);
        if end > start {
            let matched = rows.len();
            let page = rows.into_iter().skip(start).take(end - start).collect();
            if let Some(card) = card(s, layout, position, page, matched, start) {
                cards.push(card);
            }
        }
        cursor += length;
    }
    AccountsPage {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        as_of: s.as_of.to_rfc3339(),
        valid_until: Some(s.valid_until.to_rfc3339()),
        total_cards: count(s.cards.len()),
        total_credentials: count(s.credentials.len()),
        matched_cards: count(matched_cards),
        matched_credentials: count(matched_credentials),
        cards,
        offset: count(offset),
        limit: count(limit),
        has_more: offset + limit < units,
        errors: s.errors.clone(),
        plan_options: plans,
        routing_mode: s.routing_mode,
        conversation_sticky: s.conversation_sticky,
    }
}
pub(super) fn credentials(
    state: &CoreState,
    s: &PageSnapshot,
    id: &str,
    q: &PageQuery,
) -> Result<AccountCardCredentialsPage, V3ApiError> {
    let card = s
        .cards
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "card not found"))?;
    let rows = card_rows(s, card, q);
    let filtered = rows.len();
    let (offset, limit) = q.bounds();
    Ok(AccountCardCredentialsPage {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        as_of: s.as_of.to_rfc3339(),
        valid_until: Some(s.valid_until.to_rfc3339()),
        card_id: id.into(),
        total: count(card.credential_ids.len()),
        filtered_total: count(filtered),
        rows: rows.into_iter().skip(offset).take(limit).collect(),
        offset: count(offset),
        limit: count(limit),
        has_more: offset + limit < filtered,
        errors: s.errors.clone(),
    })
}
pub(super) fn identities(state: &CoreState) -> Result<Vec<IdentitySummary>, V3ApiError> {
    let (snapshot, dynamic, custom, routing, plans) = {
        let db = state.db.lock();
        (
            db.list_identity_model().map_err(V3ApiError::internal)?,
            db.list_control_plane_dynamic_providers()
                .map_err(V3ApiError::internal)?,
            db.list_custom_account_runtimes()
                .map_err(V3ApiError::internal)?,
            super::super::identities::CurrentHttpRoutingFacts::load(&db)?,
            crate::goat_plan_cooldowns::load_all_on(&db.conn).map_err(V3ApiError::internal)?,
        )
    };
    super::super::identities::project_identities(
        state,
        snapshot,
        &dynamic,
        &custom,
        &routing,
        &plans,
        state.sample_gateway_clock().0,
    )
}
pub(super) fn detail(
    state: &CoreState,
    s: &PageSnapshot,
    id: &str,
) -> Result<AccountPageDetail, V3ApiError> {
    let account = {
        let db = state.db.lock();
        let raw = db
            .get_account(id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
        let dynamic = db
            .list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?;
        dashboard_v3::accounts::account_from_db(state, &db, raw, &dynamic)?
    };
    let credential = s
        .credentials
        .iter()
        .find(|c| c.legacy_account_id == id)
        .cloned();
    let destination = credential
        .as_ref()
        .and_then(|c| s.destinations.iter().find(|d| d.id == c.destination_id))
        .cloned();
    let identity = identities(state)?
        .into_iter()
        .find(|i| i.legacy.id == id || i.credentials.iter().any(|c| c.legacy.id == id));
    if cache::ReadVersion::capture(state)?.token() != s.read_version {
        return Err(V3ApiError::conflict_at(
            state,
            "account changed during detail read",
        ));
    }
    let connection = destination
        .as_ref()
        .and_then(|d| providers::connection_for(s, d))
        .cloned();
    let platform_link = s.links.iter().find(|l| l.account_id == id).cloned();
    let platform = platform_link
        .as_ref()
        .and_then(|l| s.platforms.iter().find(|p| p.id == l.platform_account_id))
        .cloned();
    Ok(AccountPageDetail {
        operations: operations::detail(s, &account, destination.as_ref(), identity.as_ref()),
        revision: s.revision.clone(),
        account,
        destination,
        credential,
        identity,
        connection,
        platform,
        platform_link,
    })
}

impl From<&dashboard_v3::Account> for AccountSummary {
    fn from(a: &dashboard_v3::Account) -> Self {
        Self {
            id: a.id.clone(),
            provider_id: a.provider_id.clone(),
            credential_kind: a.credential_kind,
            quota_scope: a.quota_scope,
            name: a.name.clone(),
            username: a.username.clone(),
            enabled: a.enabled,
            account_type: a.account_type,
            setup_step: a.setup_step,
            purchase_date: a.purchase_date.clone(),
            expires_on: a.expires_on.clone(),
            cooldown_until: a.cooldown_until.clone(),
            cooldown_generic_until: a.cooldown_generic_until.clone(),
            cooldown_5h_until: a.cooldown_5h_until.clone(),
            cooldown_week_until: a.cooldown_week_until.clone(),
            cooldown_month_until: a.cooldown_month_until.clone(),
            cooldown_free_until: a.cooldown_free_until.clone(),
            last_error: a.last_error.clone(),
            auth_error: a.auth_error.clone(),
            notes: a.notes.clone(),
            usage_sync_last_success_at: a.usage_sync_last_success_at.clone(),
            usage_sync_next_allowed_at: a.usage_sync_next_allowed_at.clone(),
            created_at: a.created_at.clone(),
            updated_at: a.updated_at.clone(),
            revision: a.revision,
            process_generation: a.process_generation,
            verification_status: a.verification_status,
            connection_verified_at: a.connection_verified_at.clone(),
            verification_error: a.verification_error.clone(),
            plan_routable: a.plan_routable,
            ollama_billing_tier: a.ollama_billing_tier,
            model_capability_count: count(a.model_capabilities.len()),
        }
    }
}

pub(super) fn summary_from_db(
    state: &CoreState,
    db: &crate::db::Database,
    account: crate::models::Account,
    dynamic: &[crate::dynamic::DynamicProviderRuntime],
    model_capability_count: u32,
) -> Result<AccountSummary, V3ApiError> {
    let (
        (usage_sync_last_success_at, usage_sync_next_allowed_at),
        verification,
        ollama_billing,
        goat_plan,
    ) = {
        let sync = db
            .account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?;
        let verification = db
            .account_verification_state(&account.id)
            .map_err(V3ApiError::internal)?
            .unwrap_or_default();
        let ollama_billing = if account.provider_id == crate::provider::OLLAMA_PROVIDER_ID {
            db.ollama_cloud_billing_tier(&account.id)
                .map_err(V3ApiError::internal)?
        } else {
            None
        };
        let goat_plan = crate::goat_plan_cooldowns::load_for_legacy_on(&db.conn, &account.id)
            .map_err(V3ApiError::internal)?;
        (
            crate::usage_sync::dashboard_sync_fields(sync.as_ref(), state.usage_sync.now()),
            verification,
            ollama_billing,
            goat_plan,
        )
    };
    let known_secret = if account.last_error.is_some()
        || account.auth_error.is_some()
        || verification.verification_error.is_some()
    {
        if account.key_cipher.is_empty() {
            Some(String::new())
        } else {
            state.decrypt_key(&account.key_cipher).ok()
        }
    } else {
        None
    };
    let sanitize_persisted_error = |error: Option<String>| {
        error.and_then(|error| {
            known_secret
                .as_deref()
                .map(|secret| crate::redaction::redact_known_secret(&error, secret))
        })
    };
    let plan = crate::provider::builtin_provider(&account.provider_id);
    // A stored legacy purchase anchor is not evidence of a dynamic Provider's
    // billing cadence or credential expiry. Keep storage intact, but do not
    // publish invented subscription dates for these account-owned Keys.
    let has_builtin_lifecycle = plan.is_some();
    let (cooldown_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until) =
        crate::goat_plan_cooldowns::overlay_account_deadlines(
            account.cooldown_until,
            account.cooldown_5h_until,
            account.cooldown_week_until,
            account.cooldown_month_until,
            goat_plan.as_ref(),
        );
    Ok(AccountSummary {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),

        credential_kind: account.credential_kind.into(),
        quota_scope: account.quota_scope.into(),
        name: account.name,
        username: account.username,
        enabled: account.enabled,
        account_type: account.account_type.into(),
        setup_step: account.setup_step.into(),
        purchase_date: if has_builtin_lifecycle {
            account.purchase_date
        } else {
            String::new()
        },
        expires_on: if has_builtin_lifecycle {
            account.expires_on
        } else {
            String::new()
        },
        cooldown_until,
        cooldown_generic_until: account.cooldown_generic_until.map(|t| t.to_rfc3339()),
        cooldown_5h_until,
        cooldown_week_until,
        cooldown_month_until,
        cooldown_free_until: account.cooldown_free_until.map(|t| t.to_rfc3339()),
        last_error: sanitize_persisted_error(account.last_error),
        auth_error: sanitize_persisted_error(account.auth_error),
        notes: account.notes,
        usage_sync_last_success_at,
        usage_sync_next_allowed_at,
        created_at: account.created_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        verification_status: verification.status.into(),
        connection_verified_at: verification
            .connection_verified_at
            .map(|value| value.to_rfc3339()),
        verification_error: sanitize_persisted_error(verification.verification_error),
        plan_routable: plan.is_some_and(|plan| plan.routable)
            || crate::dynamic::find_runtime(dynamic, &account.provider_id).is_some(),
        model_capability_count,
        ollama_billing_tier: ollama_billing.map(dashboard_v3::OllamaBillingTier::from),
    })
}
