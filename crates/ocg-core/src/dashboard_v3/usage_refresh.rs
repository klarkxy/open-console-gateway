//! Official OpenCode Go usage refresh.
//!
//! `POST /accounts/{id}/usage/refresh` is a persistent operational mutation.
//! It requires `expectedRevision` and `processGeneration`, validates those
//! tokens before any outbound work, and rechecks them before caller-owned
//! side effects. Calibration, throttle, dedupe, last-known-good preservation,
//! and official-failure classification stay in the shared usage coordinator
//! and do not bump `settings_revision`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};

use crate::go_usage::GoUsageError;
use crate::models::UsageWindow as ModelUsageWindow;
use crate::state::CoreState;
use crate::usage_sync::{
    OfficialUsageRefreshError, OfficialUsageRefreshSuccess, UsageSyncCommitAuthorization,
    UsageSyncTrigger, refresh_official_usage_with_authorization,
};

use super::types::{
    ERROR_THROTTLED, UsageRefresh, UsageRefreshThrottleError, UsageRefreshUpdate, UsageWindow,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

pub(super) enum RefreshApiError {
    Api(V3ApiError),
    Throttled {
        body: UsageRefreshThrottleError,
        retry_after_secs: u64,
    },
}

impl From<V3ApiError> for RefreshApiError {
    fn from(error: V3ApiError) -> Self {
        Self::Api(error)
    }
}

impl IntoResponse for RefreshApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::Throttled {
                body,
                retry_after_secs,
            } => {
                let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
                if let Ok(header_value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                    response
                        .headers_mut()
                        .insert(header::RETRY_AFTER, header_value);
                }
                response
            }
        }
    }
}

pub(super) async fn refresh_account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<UsageRefresh>, RefreshApiError> {
    let input = parse_mutation_json::<UsageRefreshUpdate>(&body)?;
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
    }

    let authorization = UsageSyncCommitAuthorization::control_revision(
        input.expectation.expected_revision,
        input.expectation.process_generation,
    );
    let observation = refresh_official_usage_with_authorization(
        &state,
        &id,
        UsageSyncTrigger::Manual,
        authorization,
    )
    .await;
    if observation.owner_authorization != authorization {
        // Followers of a V2/background-owned in-flight refresh retain that
        // leader's persistence semantics. This caller still must not report an
        // outcome against stale V3 CAS tokens, including upstream failures.
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
    }

    match observation.result {
        Ok(success) => Ok(Json(usage_refresh_from_success(&state, success))),
        Err(error) => Err(map_refresh_error(&state, error)),
    }
}

fn usage_refresh_from_success(
    state: &CoreState,
    success: OfficialUsageRefreshSuccess,
) -> UsageRefresh {
    let pricing_revision = Some(state.pricing_snapshot().revision.clone());
    UsageRefresh {
        usage: usage_window_from_model(state, success.usage, pricing_revision),
        source: success.source.to_string(),
        last_success_at: success.last_success_at,
        next_allowed_at: success.next_allowed_at,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn usage_window_from_model(
    state: &CoreState,
    usage: ModelUsageWindow,
    pricing_revision: Option<String>,
) -> UsageWindow {
    UsageWindow {
        account_id: usage.account_id,
        window_5h: usage.window_5h,
        window_week: usage.window_week,
        window_month: usage.window_month,
        resets_in_5h: rfc3339_opt(usage.resets_in_5h),
        resets_in_week: rfc3339_opt(usage.resets_in_week),
        resets_in_month: rfc3339_opt(usage.resets_in_month),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision,
    }
}

fn rfc3339_opt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|value| value.to_rfc3339())
}

fn map_refresh_error(state: &CoreState, error: OfficialUsageRefreshError) -> RefreshApiError {
    match error {
        OfficialUsageRefreshError::NotFound => V3ApiError::not_found(state).into(),
        OfficialUsageRefreshError::NotEligible(message) => {
            V3ApiError::invalid_request_at(state, message).into()
        }
        OfficialUsageRefreshError::Conflict(message) => {
            V3ApiError::conflict_at(state, message).into()
        }
        OfficialUsageRefreshError::CommitAuthorizationRejected => {
            V3ApiError::revision_conflict(state).into()
        }
        OfficialUsageRefreshError::Throttled {
            next_allowed_at,
            retry_after_secs,
        } => {
            let retry_after_secs = retry_after_secs.max(1);
            RefreshApiError::Throttled {
                body: UsageRefreshThrottleError {
                    code: ERROR_THROTTLED.to_string(),
                    message: format!(
                        "official Go usage refresh is temporarily throttled; retry after {retry_after_secs}s"
                    ),
                    current_revision: Some(state.settings_revision()),
                    process_generation: Some(state.process_generation()),
                    next_allowed_at: next_allowed_at.to_rfc3339(),
                },
                retry_after_secs,
            }
        }
        OfficialUsageRefreshError::Upstream(GoUsageError::Unauthorized)
        | OfficialUsageRefreshError::Upstream(GoUsageError::Forbidden) => {
            V3ApiError::invalid_request_at(state, "official Go usage rejected this account key")
                .into()
        }
        OfficialUsageRefreshError::Upstream(upstream) => {
            V3ApiError::outbound_failed(state, upstream.to_string()).into()
        }
        OfficialUsageRefreshError::Internal(message) => V3ApiError::internal(message).into(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn production_source_reuses_the_shared_coordinator_without_a_release_fetch_seam() {
        let production = include_str!("usage_refresh.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(
            production.contains("refresh_official_usage_with_authorization"),
            "V3 refresh must reuse the shared usage coordinator"
        );
        assert!(production.contains("UsageSyncTrigger::Manual"));
        assert!(
            !production.contains("bump_settings_revision"),
            "official usage refresh must not bump the control revision"
        );
        assert!(
            !production.contains("crate::dashboard::"),
            "V3 must not call into the V2 dashboard adapter"
        );
        assert!(
            !production.contains("crate::go_usage::fetch_go_usage"),
            "V3 must not bypass the shared coordinator with a direct official fetch"
        );
        for needle in [
            "OnceLock",
            "OFFICIAL_FETCH",
            "install_official",
            "set_fetch_for_test",
            "dyn Fn",
        ] {
            assert!(
                !production.contains(needle),
                "usage_refresh.rs must not introduce a release fetch seam containing `{needle}`"
            );
        }
    }
}
