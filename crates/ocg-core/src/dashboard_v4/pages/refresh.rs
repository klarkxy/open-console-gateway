//! Demand-driven account observations. GET reads never enter this module.
use super::*;
use crate::dashboard_v3::{check_expectation, parse_mutation_json};
use axum::body::Bytes;
use std::hash::{Hash, Hasher};

#[derive(Default)]
pub(crate) struct AutomaticRefreshCache {
    attempts: HashMap<String, DateTime<Utc>>,
}
impl AutomaticRefreshCache {
    pub(super) fn admit(&mut self, key: String, now: DateTime<Utc>) -> bool {
        self.attempts
            .retain(|_, at| *at <= now && *at + Duration::minutes(5) > now);
        if self.attempts.contains_key(&key) {
            return false;
        }
        if self.attempts.len() >= 256
            && let Some(oldest) = self
                .attempts
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
        {
            self.attempts.remove(&oldest);
        }
        self.attempts.insert(key, now);
        true
    }
}
#[derive(Clone, Copy)]
pub(crate) enum RefreshOutcome {
    Refreshed,
    Fresh,
    Unavailable,
    Throttled,
}
impl RefreshOutcome {
    fn code(self) -> &'static str {
        match self {
            Self::Refreshed => "refreshed",
            Self::Fresh => "fresh",
            Self::Unavailable => "unavailable",
            Self::Throttled => "throttled",
        }
    }
}
enum Execution {
    Unavailable,
    Platform(String),
    OfficialApi,
    ProviderUsage,
    GoUsage,
}
struct Prepared {
    binding: u64,
    execution: Execution,
    billing: Option<crate::billing_types::BillingStatus>,
    account: dashboard_v3::Account,
    refresh: AccountRefreshFact,
    observation_errors: Vec<PageReadIssue>,
    key: String,
}
fn binding_facts(db: &crate::db::Database, id: &str) -> Result<(u64, bool), V3ApiError> {
    let snapshot =
        crate::routing_snapshot::RoutingSnapshot::load(db).map_err(V3ApiError::internal)?;
    let c = snapshot
        .credentials
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| V3ApiError::internal("account binding missing"))?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (
        &c.credential_id,
        c.credential_version,
        &c.key_cipher,
        &c.provider_id,
        &c.destination_id,
        &c.binding_id,
        c.enabled,
        c.binding_enabled,
        c.ready,
        &c.authorization_connection_id,
    )
        .hash(&mut hash);
    serde_json::to_string(&c.scope)
        .map_err(V3ApiError::internal)?
        .hash(&mut hash);
    serde_json::to_string(&c.grants)
        .map_err(V3ApiError::internal)?
        .hash(&mut hash);
    if let Some(d) = snapshot
        .projection
        .destinations
        .iter()
        .find(|d| d.id == c.destination_id)
    {
        d.base_url.hash(&mut hash);
        serde_json::to_string(&d.protocol_routes)
            .map_err(V3ApiError::internal)?
            .hash(&mut hash);
    }
    Ok((hash.finish(), c.enabled && c.binding_enabled && c.ready))
}
fn prepare(
    state: &CoreState,
    id: &str,
    input: &AccountPageRefreshRequest,
) -> Result<Prepared, V3ApiError> {
    let _settings = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let db = state.db.lock();
    let raw = db
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    let dynamic = db
        .list_control_plane_dynamic_providers()
        .map_err(V3ApiError::internal)?;
    let account = dashboard_v3::accounts::account_from_db(state, &db, raw.clone(), &dynamic)?;
    let (binding, available) = binding_facts(&db, id)?;
    let billing = super::super::billing::cached_status(state, &db, id).ok();
    let link = db
        .list_platform_links()
        .map_err(V3ApiError::internal)?
        .into_iter()
        .find(|l| l.account_id == id);
    let refresh = accounts::refresh_fact(
        Some(&AccountSummary::from(&account)),
        billing.as_ref(),
        link.as_ref(),
        state.sample_gateway_clock().0,
    );
    let execution = if account.setup_step != dashboard_v3::AccountSetupStep::Ready
        || (matches!(input.mode, AccountPageRefreshMode::Automatic) && !available)
        || !refresh.supported
    {
        Execution::Unavailable
    } else if let Some(link) = &link {
        Execution::Platform(link.platform_account_id.clone())
    } else if dynamic
        .iter()
        .find(|p| p.id == account.provider_id)
        .and_then(crate::official_api::kind_for_runtime)
        .is_some_and(|kind| kind.balance_available())
    {
        Execution::OfficialApi
    } else if account.provider_id == crate::provider::OPENCODE_PROVIDER_ID {
        Execution::GoUsage
    } else {
        Execution::ProviderUsage
    };
    let mut key = format!("management-auto:{id}:{binding}");
    // Observation versions change on refresh; target and parent material define
    // the binding, so a completed refresh does not reset the automatic throttle.
    if let Some(link) = &link {
        key.push_str(&format!("{}:{:?}", link.platform_account_id, link.group));
        let parent = db
            .platform_account(&link.platform_account_id)
            .map_err(V3ApiError::internal)?;
        if let Some(parent) = parent {
            key.push_str(&format!("{}:{:?}", parent.base_url, parent.kind));
        }
    }
    Ok(Prepared {
        binding,
        execution,
        billing,
        account,
        refresh,
        observation_errors: link
            .as_ref()
            .and_then(|l| l.snapshot.as_ref())
            .into_iter()
            .flat_map(|snapshot| &snapshot.errors)
            .map(|code| PageReadIssue {
                resource: "platform".into(),
                id: Some(id.into()),
                code: code.clone(),
            })
            .collect(),
        key,
    })
}
fn response(state: &CoreState, prepared: Prepared, outcome: &str) -> AccountPageRefresh {
    let observed = outcome == "refreshed";
    let partial = observed && !prepared.observation_errors.is_empty();
    AccountPageRefresh {
        revision: ControlRevision::from_state(state),
        outcome: if partial { "partial" } else { outcome }.into(),
        billing: prepared.billing,
        account: Some(prepared.account),
        refresh: prepared.refresh,
        errors: if observed {
            prepared.observation_errors
        } else {
            Vec::new()
        },
    }
}
pub(crate) async fn refresh_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountPageRefresh>, V3ApiError> {
    let input = parse_mutation_json::<AccountPageRefreshRequest>(&body)?;
    let prepared = prepare_async(state.clone(), id.clone(), input.clone()).await?;
    let now = state.sample_gateway_clock().0;
    let expected =
        Bytes::from(serde_json::to_vec(&input.expectation).map_err(V3ApiError::internal)?);
    let worker_state = state.clone();
    let worker_id = id.clone();
    let worker_input = input.clone();
    let throttle_key = prepared.key.clone();
    let flight_key = format!(
        "page:{}:{}:{}:{:?}",
        prepared.key,
        input.expectation.expected_revision,
        input.expectation.process_generation,
        input.mode
    );
    let result = state
        .management_page_refresh
        .run(flight_key, move || async move {
            let state = worker_state;
            let id = worker_id;
            let input = worker_input;
            if matches!(prepared.execution, Execution::Unavailable) {
                return Ok(RefreshOutcome::Unavailable);
            }
            if matches!(input.mode, AccountPageRefreshMode::Automatic) {
                if prepared
                    .refresh
                    .fresh_until
                    .as_deref()
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .is_some_and(|at| at > now)
                {
                    return Ok(RefreshOutcome::Fresh);
                }
                if prepared.refresh.next_allowed_at.is_some()
                    || !state
                        .management_auto_refresh
                        .lock()
                        .admit(throttle_key, state.sample_gateway_clock().0)
                {
                    return Ok(RefreshOutcome::Throttled);
                }
            }
            let result = match prepared.execution {
                Execution::Unavailable => unreachable!(),
                Execution::Platform(parent) => {
                    let worker_state = state.clone();
                    let worker_id = id.clone();
                    let expectation = input.expectation.clone();
                    let automatic = matches!(input.mode, AccountPageRefreshMode::Automatic);
                    crate::dashboard_v3::usage::coalesce_balance(
                        &state,
                        &id,
                        &input.expectation,
                        if automatic {
                            "management-platform-automatic"
                        } else {
                            "management-platform-manual"
                        },
                        move || async move {
                            crate::dashboard_v3::platforms::refresh_for_page(
                                worker_state,
                                parent,
                                worker_id,
                                expectation,
                                automatic,
                            )
                            .await
                        },
                    )
                    .await
                }
                Execution::OfficialApi => super::super::official_api::refresh_balance(
                    State(state.clone()),
                    Path(id.clone()),
                    expected,
                )
                .await
                .map(|_| ()),
                Execution::GoUsage => {
                    let _permit = state
                        .provider_usage_refresh
                        .exclusive(format!("page-go:{id}"))
                        .await;
                    crate::dashboard_v3::usage::refresh_provider_usage(
                        State(state.clone()),
                        Path(id.clone()),
                        expected,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| match error {
                        crate::dashboard_v3::usage_refresh::RefreshApiError::Api(error) => error,
                        crate::dashboard_v3::usage_refresh::RefreshApiError::Throttled {
                            ..
                        } => V3ApiError::throttled_at(&state, "account refresh is throttled"),
                    })
                }
                Execution::ProviderUsage => crate::dashboard_v3::usage::refresh_provider_usage(
                    State(state.clone()),
                    Path(id.clone()),
                    expected,
                )
                .await
                .map(|_| ())
                .map_err(|error| match error {
                    crate::dashboard_v3::usage_refresh::RefreshApiError::Api(error) => error,
                    crate::dashboard_v3::usage_refresh::RefreshApiError::Throttled { .. } => {
                        V3ApiError::throttled_at(&state, "account refresh is throttled")
                    }
                }),
            };
            match result {
                Ok(()) => Ok(RefreshOutcome::Refreshed),
                Err(error) if error.operation_reason() == "throttled" => {
                    Ok(RefreshOutcome::Throttled)
                }
                Err(error) => Err(error),
            }
        })
        .await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        if binding_facts(&state.db.lock(), &id)?.0 != prepared.binding {
            return Err(V3ApiError::conflict_at(
                &state,
                "account binding changed during refresh",
            ));
        }
    }
    let outcome = result?.code();
    let current = prepare_async(state.clone(), id, input).await?;
    Ok(Json(response(&state, current, outcome)))
}
async fn prepare_async(
    state: CoreState,
    id: String,
    input: AccountPageRefreshRequest,
) -> Result<Prepared, V3ApiError> {
    tokio::task::spawn_blocking(move || prepare(&state, &id, &input))
        .await
        .map_err(V3ApiError::internal)?
}

#[cfg(test)]
pub(super) fn binding_key_for_test(state: &CoreState, id: &str) -> String {
    let input = AccountPageRefreshRequest {
        expectation: crate::dashboard_v3::MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
        mode: AccountPageRefreshMode::Automatic,
    };
    prepare(state, id, &input).unwrap().key
}
