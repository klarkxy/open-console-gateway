//! Read-only shadow planner for inference attempts.
//!
//! [`plan_shadow_attempts`] reuses [`super::materialize::materialize_account_routes`]
//! and [`super::provider_adapter::resolve_route_with_dynamics`]. It never decrypts
//! credentials, never builds an HTTP client, and never calls Host send. Live
//! `forward_once` remains the single outbound path.
//!
//! Compare is opt-in via a thread-local flag that defaults off. When enabled,
//! the live path may log [`ShadowMismatch`] values after materialize/resolve
//! and before send; it must not change the live [`AttemptSpec`] or send count.

use crate::alias::ResolvedModel;
use crate::custom::CustomAccountRuntime;
use crate::dynamic::DynamicProviderRuntime;
use crate::gateway::attempt::{AttemptSpec, CredentialHandle};
use crate::gateway::materialize::{
    InferenceBindingIndex, MaterializedCandidate, MaterializedRouteSet,
    materialize_account_routes_with_bindings,
};
use crate::gateway::protocol::{ParsedClientRequest, ProtocolError, RequestPlan};
use crate::gateway::provider_adapter;
use crate::goat::GoatAccountRuntime;
use crate::kernel::protocol::ApiFormat;
use crate::models::{Account, AppConfig};
use crate::provider::ProviderAdapterKind;
use crate::provider_contracts::EffectiveContractSet;
use bytes::Bytes;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    static SHADOW_COMPARE_ENABLED: Cell<bool> = const { Cell::new(false) };
}

static SHADOW_MISMATCH_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static SHADOW_OUTBOUND_SENDS: AtomicU64 = AtomicU64::new(0);

/// Same snapshots [`materialize_account_routes`] already consumes.
pub(crate) struct ShadowPlanInput<'a> {
    pub accounts: &'a [Account],
    pub config: &'a AppConfig,
    pub parsed: &'a ParsedClientRequest,
    pub resolved: &'a ResolvedModel,
    pub client_model: &'a str,
    pub routing_model: &'a str,
    pub client_body: &'a Bytes,
    pub free_available: bool,
    pub custom_runtimes: &'a HashMap<String, CustomAccountRuntime>,
    pub goat_runtimes: &'a HashMap<String, GoatAccountRuntime>,
    pub cpa_base_url: Option<&'a str>,
    pub contracts: &'a EffectiveContractSet,
    pub dynamics: &'a [DynamicProviderRuntime],
    pub bindings: &'a InferenceBindingIndex,
}

/// Comparable, secret-free view of one planned attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShadowAttempt {
    pub public_name: String,
    pub upstream_model: String,
    pub adapter_kind: ProviderAdapterKind,
    pub endpoint: Option<String>,
    pub protocol: ApiFormat,
    pub credential_handle: CredentialHandle,
    pub account_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShadowPlan {
    pub attempts: Vec<ShadowAttempt>,
    pub rejects: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadowField {
    PublicName,
    UpstreamModel,
    AdapterKind,
    Endpoint,
    Protocol,
    CredentialHandle,
    AccountId,
    CandidateOrder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShadowMismatch {
    Count {
        live: usize,
        shadow: usize,
    },
    Field {
        index: usize,
        account_id: String,
        field: ShadowField,
        live: String,
        shadow: String,
    },
    MissingOnShadow {
        account_id: String,
    },
    MissingOnLive {
        account_id: String,
    },
}

impl ShadowMismatch {
    fn account_id(&self) -> Option<&str> {
        match self {
            Self::Count { .. } => None,
            Self::Field { account_id, .. }
            | Self::MissingOnShadow { account_id }
            | Self::MissingOnLive { account_id } => Some(account_id.as_str()),
        }
    }

    fn field_name(&self) -> &'static str {
        match self {
            Self::Count { .. } => "count",
            Self::Field { field, .. } => match field {
                ShadowField::PublicName => "public_name",
                ShadowField::UpstreamModel => "upstream_model",
                ShadowField::AdapterKind => "adapter_kind",
                ShadowField::Endpoint => "endpoint",
                ShadowField::Protocol => "protocol",
                ShadowField::CredentialHandle => "credential_handle",
                ShadowField::AccountId => "account_id",
                ShadowField::CandidateOrder => "candidate_order",
            },
            Self::MissingOnShadow { .. } => "missing_on_shadow",
            Self::MissingOnLive { .. } => "missing_on_live",
        }
    }
}

/// RAII enable for the default-off compare hook. Production never constructs this.
#[cfg(test)]
pub(crate) struct ShadowCompareGuard {
    previous: bool,
}

#[cfg(test)]
impl ShadowCompareGuard {
    pub(crate) fn enable() -> Self {
        let previous = SHADOW_COMPARE_ENABLED.with(|flag| {
            let previous = flag.get();
            flag.set(true);
            previous
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for ShadowCompareGuard {
    fn drop(&mut self) {
        SHADOW_COMPARE_ENABLED.with(|flag| flag.set(self.previous));
    }
}

pub(crate) fn shadow_compare_enabled() -> bool {
    SHADOW_COMPARE_ENABLED.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn shadow_mismatch_count() -> u64 {
    SHADOW_MISMATCH_COUNT.load(Ordering::Relaxed)
}

/// Host send counter owned by this module. The planner never increments it.
#[cfg(test)]
pub(crate) fn shadow_recorded_outbound_sends() -> u64 {
    SHADOW_OUTBOUND_SENDS.load(Ordering::Relaxed)
}

/// Plan the AttemptSpecs the live path would send, without Host send or decrypt.
pub(crate) fn plan_shadow_attempts(
    input: &ShadowPlanInput<'_>,
) -> Result<ShadowPlan, ProtocolError> {
    let set = materialize_account_routes_with_bindings(
        input.accounts,
        input.config,
        input.parsed,
        input.resolved,
        input.client_model,
        input.routing_model,
        input.client_body,
        input.free_available,
        input.custom_runtimes,
        input.goat_runtimes,
        input.cpa_base_url,
        input.contracts,
        input.dynamics,
        input.bindings,
    )?;
    Ok(shadow_plan_from_materialized(
        &set,
        input.config,
        input.dynamics,
    ))
}

fn shadow_plan_from_materialized(
    set: &MaterializedRouteSet,
    config: &AppConfig,
    dynamics: &[DynamicProviderRuntime],
) -> ShadowPlan {
    let (attempts, resolve_rejects) = attempts_from_materialized(&set.routes, config, dynamics);
    let mut rejects = set.rejected.clone();
    rejects.extend(resolve_rejects);
    ShadowPlan { attempts, rejects }
}

pub(crate) fn live_shadow_attempts(
    routes: &[MaterializedCandidate],
    config: &AppConfig,
    dynamics: &[DynamicProviderRuntime],
) -> Vec<ShadowAttempt> {
    attempts_from_materialized(routes, config, dynamics).0
}

fn attempts_from_materialized(
    routes: &[MaterializedCandidate],
    config: &AppConfig,
    dynamics: &[DynamicProviderRuntime],
) -> (Vec<ShadowAttempt>, Vec<String>) {
    let mut attempts = Vec::new();
    let mut rejects = Vec::new();
    for route in routes {
        match provider_adapter::resolve_route_with_dynamics(
            &route.routing.account,
            config,
            &route.plan,
            dynamics,
        ) {
            Ok(spec) => attempts.push(shadow_attempt_from_live(
                &route.routing.account,
                &route.plan,
                &spec,
                dynamics,
            )),
            Err(error) => rejects.push(format!(
                "{}/{} account `{}`: {error}",
                route.routing.account.provider_id,
                route.routing.account.provider_id,
                route.routing.account.name
            )),
        }
    }
    (attempts, rejects)
}

pub(crate) fn shadow_attempt_from_live(
    account: &Account,
    plan: &RequestPlan,
    spec: &AttemptSpec,
    dynamics: &[DynamicProviderRuntime],
) -> ShadowAttempt {
    let adapter_kind = crate::dynamic::adapter_kind_for(&account.provider_id, dynamics)
        .unwrap_or(ProviderAdapterKind::ConfigurableHttp);
    ShadowAttempt {
        public_name: plan
            .resolved_alias
            .as_deref()
            .filter(|alias| !alias.is_empty())
            .unwrap_or(&plan.client_model)
            .to_string(),
        upstream_model: plan.model.clone(),
        adapter_kind,
        endpoint: spec.request_url().ok(),
        protocol: spec.upstream,
        credential_handle: spec.credential.clone(),
        account_id: account.id.clone(),
    }
}

/// Compare live and shadow attempt lists. Does not mutate either side.
pub(crate) fn shadow_diff(live: &[ShadowAttempt], shadow: &[ShadowAttempt]) -> Vec<ShadowMismatch> {
    let mut mismatches = Vec::new();
    if live.len() != shadow.len() {
        mismatches.push(ShadowMismatch::Count {
            live: live.len(),
            shadow: shadow.len(),
        });
    }
    let n = live.len().min(shadow.len());
    for index in 0..n {
        let left = &live[index];
        let right = &shadow[index];
        if left.account_id != right.account_id {
            mismatches.push(ShadowMismatch::Field {
                index,
                account_id: left.account_id.clone(),
                field: ShadowField::CandidateOrder,
                live: left.account_id.clone(),
                shadow: right.account_id.clone(),
            });
        }
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::PublicName,
            &left.public_name,
            &right.public_name,
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::UpstreamModel,
            &left.upstream_model,
            &right.upstream_model,
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::AdapterKind,
            &format!("{:?}", left.adapter_kind),
            &format!("{:?}", right.adapter_kind),
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::Endpoint,
            &display_endpoint(left.endpoint.as_deref()),
            &display_endpoint(right.endpoint.as_deref()),
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::Protocol,
            &format!("{:?}", left.protocol),
            &format!("{:?}", right.protocol),
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::CredentialHandle,
            &credential_identity(&left.credential_handle),
            &credential_identity(&right.credential_handle),
        );
        push_field_mismatch(
            &mut mismatches,
            index,
            &left.account_id,
            ShadowField::AccountId,
            &left.account_id,
            &right.account_id,
        );
    }
    if live.len() > shadow.len() {
        for attempt in &live[n..] {
            mismatches.push(ShadowMismatch::MissingOnShadow {
                account_id: attempt.account_id.clone(),
            });
        }
    } else if shadow.len() > live.len() {
        for attempt in &shadow[n..] {
            mismatches.push(ShadowMismatch::MissingOnLive {
                account_id: attempt.account_id.clone(),
            });
        }
    }
    mismatches
}

fn push_field_mismatch(
    mismatches: &mut Vec<ShadowMismatch>,
    index: usize,
    account_id: &str,
    field: ShadowField,
    live: &str,
    shadow: &str,
) {
    if live != shadow {
        mismatches.push(ShadowMismatch::Field {
            index,
            account_id: account_id.to_string(),
            field,
            live: live.to_string(),
            shadow: shadow.to_string(),
        });
    }
}

fn display_endpoint(endpoint: Option<&str>) -> String {
    endpoint.unwrap_or("").to_string()
}

fn credential_identity(handle: &CredentialHandle) -> String {
    match handle {
        CredentialHandle::None => "none".to_string(),
        CredentialHandle::Account { id } => format!("account:{id}"),
    }
}

fn record_mismatches(mismatches: &[ShadowMismatch]) {
    if mismatches.is_empty() {
        return;
    }
    SHADOW_MISMATCH_COUNT.fetch_add(mismatches.len() as u64, Ordering::Relaxed);
    for mismatch in mismatches {
        eprintln!(
            "OCG_SHADOW_MISMATCH {}",
            serde_json::json!({
                "account_id": mismatch.account_id(),
                "field": mismatch.field_name(),
                "detail": format!("{mismatch:?}"),
            })
        );
    }
}

/// Live-path hook. Default off. When on, compare and log only — never send.
pub(crate) fn maybe_compare_live_routes(input: &ShadowPlanInput<'_>, live: &MaterializedRouteSet) {
    if !shadow_compare_enabled() {
        return;
    }
    let live_attempts = live_shadow_attempts(&live.routes, input.config, input.dynamics);
    match plan_shadow_attempts(input) {
        Ok(shadow) => {
            let mismatches = shadow_diff(&live_attempts, &shadow.attempts);
            record_mismatches(&mismatches);
        }
        Err(error) => {
            SHADOW_MISMATCH_COUNT.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "OCG_SHADOW_MISMATCH {}",
                serde_json::json!({
                    "account_id": serde_json::Value::Null,
                    "field": "plan",
                    "detail": error.message,
                })
            );
        }
    }
}

#[cfg(test)]
mod tests;
