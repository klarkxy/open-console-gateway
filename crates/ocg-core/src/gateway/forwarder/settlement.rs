//! Quota, credit, and stream-drop settlement guards.

use super::attempt_record::{AttemptSink, finalize_logged_forward};
use super::*;

#[cfg(test)]
mod tests;

// Guard drop timing is part of the behavior contract: each Drop impl below is the
// original body, moved verbatim. Callers still construct, disarm, and drop these
// guards at the same points in `forward_request_with_deadline`.

#[derive(Clone)]
pub(super) struct QuotaObservation {
    pub(super) selection: LiveSendSelection,
    pub(super) fence: crate::gateway::recovery::RecoveryObservation,
}
impl QuotaObservation {
    pub(super) fn is_current(&self, db: &Database) -> Result<bool> {
        Ok(
            self.fence.is_current()
                && live_send::selection_allows_observation(db, &self.selection)?,
        )
    }
}

pub(super) struct QuotaTrialGuard {
    pub(super) state: CoreState,
    pub(super) episode: crate::quota_recovery::QuotaEpisode,
    pub(super) observation: Option<QuotaObservation>,
    pub(super) settled: bool,
}

impl QuotaTrialGuard {
    pub(super) fn new(state: CoreState, episode: crate::quota_recovery::QuotaEpisode) -> Self {
        Self {
            state,
            episode,
            observation: None,
            settled: false,
        }
    }

    pub(super) fn with_observation(mut self, observation: QuotaObservation) -> Self {
        self.observation = Some(observation);
        self
    }

    pub(super) fn succeed(&mut self) {
        if self.settled {
            return;
        }
        persist_quota_write(
            &self.state,
            Some(&self.episode),
            |db| {
                if let Some(observation) = &self.observation
                    && !observation.is_current(db)?
                {
                    return Ok(false);
                }
                crate::db::quota_recovery::clear_matching_on(&db.conn, &self.episode)
            },
            "quota recovery clear",
        );
        self.settled = true;
    }

    #[allow(dead_code)]
    pub(super) fn fail_quota(
        &mut self,
        account: &ExecutionCredential,
        evidence: &ocg_gateway::quota::QuotaEvidence,
        now: chrono::DateTime<Utc>,
    ) {
        if self.settled {
            return;
        }
        let recorded = persist_quota_write(
            &self.state,
            Some(&self.episode),
            |db| {
                if let Some(observation) = &self.observation
                    && !observation.is_current(db)?
                {
                    return Ok(false);
                }
                crate::db::quota_recovery::record_evidence_on(
                    &db.conn,
                    account,
                    evidence,
                    Some(&self.episode),
                    now,
                )
            },
            "quota recovery evidence",
        );
        if recorded {
            self.settled = true;
        }
    }

    pub(super) fn fail_nonquota(&mut self, now: chrono::DateTime<Utc>) {
        if self.settled {
            return;
        }
        persist_quota_write(
            &self.state,
            Some(&self.episode),
            |db| {
                if let Some(observation) = &self.observation
                    && !observation.is_current(db)?
                {
                    return Ok(false);
                }
                crate::db::quota_recovery::release_nonquota_trial_on(&db.conn, &self.episode, now)
            },
            "quota trial nonquota release",
        );
        self.settled = true;
    }
}

impl Drop for QuotaTrialGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let now = self.state.sample_gateway_clock().0;
        self.fail_nonquota(now);
    }
}

/// Protocol-supported explicit application error on HTTP 200. Root `error`
/// object or `type=error` / `response.failed` only. Nested assistant content
/// and `error: null` are not exhaustion and are not a completed trial.
pub(super) fn explicit_nonquota_application_error(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    if matches!(
        root.get("type").and_then(Value::as_str),
        Some("error" | "response.failed")
    ) {
        return true;
    }
    root.get("error").is_some_and(Value::is_object)
}

fn persist_quota_write(
    state: &CoreState,
    episode: Option<&crate::quota_recovery::QuotaEpisode>,
    write: impl FnOnce(&Database) -> anyhow::Result<bool>,
    what: &str,
) -> bool {
    state.with_settings_update(|| {
        let db = state.db.lock();
        let persisted = match write(&db) {
            Ok(changed) => changed,
            Err(error) => {
                let _ = db.log_gateway(
                    "warn",
                    "forwarder",
                    &format!("failed to persist {what}: {error}"),
                );
                false
            }
        };
        let released = episode.is_some_and(|episode| {
            let mut probes = state.quota_probes.lock();
            if probes.get(&episode.credential_id) == Some(episode) {
                probes.remove(&episode.credential_id);
                true
            } else {
                false
            }
        });
        if persisted || released {
            state.bump_settings_revision();
        }
        persisted
    })
}

pub(super) struct CreditRequestGuard {
    pub(super) state: CoreState,
    pub(super) context: ForwardAttemptContext,
    pub(super) pricing: RequestPricingSnapshot,
    pub(super) service_tier: Option<String>,
    pub(super) armed: bool,
}

impl Drop for CreditRequestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(id) = self.context.credit_log_id else {
            return;
        };
        let db = self.state.db.lock();
        if let Err(error) = finalize_logged_forward(
            &db,
            id,
            "outcome_unknown",
            None,
            metadata_metrics(
                &self.pricing,
                self.service_tier.as_deref(),
                "outcome_unknown",
            ),
            Some("request ended before its upstream usage was confirmed"),
            None,
            &self.context,
        ) {
            let _ = db.log_gateway(
                "warn",
                "forwarder",
                &format!("credit request {id} finalization failed: {error}"),
            );
        }
    }
}

pub(super) struct StreamOutcomeGuard {
    pub(super) state: CoreState,
    pub(super) log_id: i64,
    pub(super) stream_state: Arc<Mutex<StreamState>>,
    pub(super) model: String,
    pub(super) pricing: RequestPricingSnapshot,
    pub(super) service_tier: Option<String>,
    pub(super) attempt_context: ForwardAttemptContext,
    pub(super) upstream_status: u16,
    pub(super) upstream_wait_ms: u64,
    pub(super) armed: bool,
    pub(super) recovery: Option<RecoveryPermit>,
    pub(super) quota_trial: Arc<Mutex<Option<QuotaTrialGuard>>>,
}

impl StreamOutcomeGuard {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        state: CoreState,
        log_id: i64,
        stream_state: Arc<Mutex<StreamState>>,
        model: String,
        pricing: impl Into<RequestPricingSnapshot>,
        service_tier: Option<String>,
        attempt_context: ForwardAttemptContext,
        upstream_status: u16,
        upstream_wait_ms: u64,
        quota_trial: Arc<Mutex<Option<QuotaTrialGuard>>>,
    ) -> Self {
        Self {
            state,
            log_id,
            stream_state,
            model,
            pricing: pricing.into(),
            service_tier,
            attempt_context,
            upstream_status,
            upstream_wait_ms,
            armed: true,
            recovery: None,
            quota_trial,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(super) fn settle_quota(&mut self, success: bool) {
        let mut trial = self.quota_trial.lock();
        if let Some(trial) = trial.as_mut() {
            if success {
                trial.succeed();
            } else {
                trial.fail_nonquota(self.state.sample_gateway_clock().0);
            }
        }
    }
}

impl Drop for StreamOutcomeGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let (status, metrics, error_message, failure) = {
            let stream = self.stream_state.lock();
            if !stream.terminal && !stream.error {
                let message = outcome_unknown_message(
                    "downstream disconnected before the upstream stream outcome was confirmed",
                );
                let failure = self.attempt_context.failure(FailureSpec {
                    error_source: "downstream",
                    error_stage: "downstream_disconnect",
                    downstream_status: Some(self.upstream_status),
                    upstream_status: Some(self.upstream_status),
                    upstream_wait_ms: Some(self.upstream_wait_ms),
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&message),
                    request_body: None,
                });
                (
                    "outcome_unknown",
                    metadata_metrics(
                        &self.pricing,
                        self.service_tier.as_deref(),
                        "outcome_unknown",
                    ),
                    Some(message),
                    Some(failure),
                )
            } else if stream.error {
                let status = if stream.outcome_unknown {
                    "outcome_unknown"
                } else {
                    "error"
                };
                let failure = (!stream.diagnostic_recorded).then(|| {
                    let error = stream
                        .error_message
                        .as_deref()
                        .unwrap_or("upstream stream error");
                    self.attempt_context.failure(FailureSpec {
                        error_source: "upstream",
                        error_stage: "stream",
                        downstream_status: Some(self.upstream_status),
                        upstream_status: Some(self.upstream_status),
                        upstream_wait_ms: Some(self.upstream_wait_ms),
                        retry_action: Some("return"),
                        upstream_headers: None,
                        upstream_error: Some(error),
                        request_body: None,
                    })
                });
                (
                    status,
                    metadata_metrics(
                        &self.pricing,
                        self.service_tier.as_deref(),
                        if stream.outcome_unknown {
                            "outcome_unknown"
                        } else {
                            "not_applicable"
                        },
                    ),
                    stream.error_message.clone(),
                    failure,
                )
            } else if stream.has_usage {
                let (prompt, completion, cached, cache_creation) = token_counts(stream.usage);
                let metrics = pricing_metrics(
                    &self.pricing,
                    &self.model,
                    prompt,
                    completion,
                    cached,
                    cache_creation,
                    self.service_tier.as_deref(),
                );
                (
                    success_status_for_cost(metrics.cost_state),
                    metrics,
                    None,
                    None,
                )
            } else {
                (
                    "success_no_usage",
                    metadata_metrics(&self.pricing, self.service_tier.as_deref(), "usage_missing"),
                    None,
                    None,
                )
            }
        };

        let diagnostic = failure.as_ref().map(FailureRecord::update);
        let persisted_error = error_message
            .as_deref()
            .map(|message| self.attempt_context.redact_known_secret(message));
        let db = self.state.db.lock();
        if let Err(error) = DbAttemptSink::new(&db).finalize(
            self.log_id,
            status,
            None,
            metrics,
            persisted_error.as_deref(),
            diagnostic.as_ref(),
            &self.attempt_context,
        ) {
            let _ = db.log_gateway(
                "warn",
                "forwarder",
                &format!(
                    "failed to finalize dropped streaming row {}: {}",
                    self.log_id, error
                ),
            );
        }
        drop(db);
        let success = status.starts_with("success");
        self.settle_quota(success);
    }
}

// `unfold` with an Init/Done state runs the normal finalizer once. The guard
// handles the complementary path where Hyper drops the body because the
// downstream client disconnected before polling that finalizer.
pub(super) enum FinalizerState {
    Init {
        db_h: CoreState,
        st_f: Arc<Mutex<StreamState>>,
        converter_f: Arc<Mutex<StreamConverter>>,
        mdl: String,
        initial_id: i64,
        guard: Box<StreamOutcomeGuard>,
    },
    Done,
}

pub(super) enum StreamRead {
    Chunk(bytes::Bytes),
    Failed(reqwest::Error),
    IdleTimeout,
}

pub(super) enum PreOutputFailure {
    Retry(ForwardResult),
    Return(Vec<bytes::Bytes>),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_pre_output_stream_failure(
    state: &CoreState,
    stream_state: &Arc<Mutex<StreamState>>,
    converter: &Arc<Mutex<StreamConverter>>,
    log_id: i64,
    pricing: &RequestPricingSnapshot,
    attempt: &ForwardAttemptContext,
    plan: &RequestPlan,
    upstream_status: StatusCode,
    upstream_wait_ms: u64,
    failure_status: StatusCode,
    error_source: &'static str,
    error_stage: &'static str,
    detail: &str,
    stream_input: StreamClassifyInput,
    allow_retry: bool,
) -> PreOutputFailure {
    let class = classify_stream(stream_input);
    let action = forward_action_for_class(class, allow_retry, None);
    let retry = matches!(action, ForwardAction::RetrySameAccount);
    let message = if retry {
        outcome_unknown_retry_message(detail)
    } else {
        outcome_unknown_message(detail)
    };
    {
        let mut stream = stream_state.lock();
        stream.error = true;
        stream.outcome_unknown = true;
        stream.error_message = Some(message.clone());
        stream.diagnostic_recorded = true;
    }
    let chunks = converter.lock().outcome_unknown_event(&message);
    let failure = attempt.failure(FailureSpec {
        error_source,
        error_stage,
        downstream_status: Some(upstream_status.as_u16()),
        upstream_status: Some(upstream_status.as_u16()),
        upstream_wait_ms: Some(upstream_wait_ms),
        retry_action: Some(retry_action_name(action)),
        upstream_headers: None,
        upstream_error: Some(detail),
        request_body: None,
    });
    let diagnostic = failure.update();
    let db = state.db.lock();
    if let Err(error) = DbAttemptSink::new(&db).finalize(
        log_id,
        "outcome_unknown",
        None,
        metadata_metrics(pricing, plan.service_tier.as_deref(), "outcome_unknown"),
        Some(&message),
        Some(&diagnostic),
        attempt,
    ) {
        let _ = db.log_gateway(
            "warn",
            "forwarder",
            &format!("failed to update streaming row {log_id}: {error}"),
        );
    }
    if retry {
        PreOutputFailure::Retry(ForwardResult {
            response: outcome_unknown_response_with_message(plan.client, failure_status, &message),
            action,
            error_message: Some(message),
            sent: true,
        })
    } else {
        PreOutputFailure::Return(chunks)
    }
}
