//! Explicit user-operation receipts. Diagnostics and requests have separate writers.
//!
//! Constructing this value only captures intent in memory. Synchronous actions
//! write their one final receipt after the business result is known. An async
//! action explicitly calls `accepted` and carries this same value to completion.
//! Recording never changes the caller's business response or repeats its work.

use crate::log_types::{
    OperationFinish, OperationLog, OperationMetadata, OperationOutcome, OperationSource,
};
use crate::state::CoreState;

pub struct UserOperation {
    state: std::sync::Weak<crate::state::CoreStateInner>,
    record: OperationLog,
    pending: bool,
}

impl UserOperation {
    pub fn operation_id(&self) -> &str {
        &self.record.operation_id
    }

    /// Reuse an existing authoritative business-operation identity on replay.
    pub(crate) fn use_operation_id(&mut self, id: uuid::Uuid) {
        self.record.operation_id = id.to_string();
    }
    pub fn new(
        state: &CoreState,
        source: OperationSource,
        action: &'static str,
        subject_type: &'static str,
        subject_id: Option<String>,
    ) -> Self {
        Self {
            state: std::sync::Arc::downgrade(state),
            record: OperationLog {
                operation_id: uuid::Uuid::new_v4().to_string(),
                started_at: chrono::Utc::now(),
                completed_at: None,
                action: action.into(),
                source,
                actor_id: None,
                subject_type: Some(subject_type.into()),
                subject_id: subject_id
                    .as_deref()
                    .and_then(crate::log_types::operation_subject),
                outcome: OperationOutcome::Pending,
                reason_code: None,
                metadata: OperationMetadata::default(),
            },
            pending: false,
        }
    }

    pub fn dashboard(
        state: &CoreState,
        action: &'static str,
        subject_type: &'static str,
        subject_id: Option<String>,
    ) -> Self {
        Self::new(
            state,
            OperationSource::Dashboard,
            action,
            subject_type,
            subject_id,
        )
    }

    pub fn subject(&mut self, id: impl Into<String>) {
        self.record.subject_id = crate::log_types::operation_subject(&id.into());
    }

    /// Call only after a job was actually accepted; move the value into the job.
    pub fn accepted(&mut self, metadata: OperationMetadata) {
        self.record.metadata = metadata;
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let result = state.db.lock().begin_operation(&self.record);
        self.pending = result.is_ok();
        if let Err(error) = result {
            self.record_error(&error);
        }
    }

    pub fn complete(
        mut self,
        outcome: OperationOutcome,
        reason_code: Option<&str>,
        metadata: OperationMetadata,
    ) {
        self.record.completed_at = Some(chrono::Utc::now());
        self.record.outcome = outcome;
        self.record.reason_code = reason_code.map(str::to_owned);
        self.record.metadata = metadata;
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let result = if self.pending {
            state.db.lock().finish_operation(
                &self.record.operation_id,
                &OperationFinish {
                    completed_at: self.record.completed_at.unwrap(),
                    outcome,
                    reason_code: self.record.reason_code.clone(),
                    metadata: self.record.metadata.clone(),
                },
            )
        } else {
            state.db.lock().record_operation(&self.record)
        };
        if let Err(error) = result {
            self.record_error(&error);
        }
    }

    /// Atomic business actions use their semantic error code, never HTTP status.
    /// Composite actions must supply their actual partial/compensated receipt.
    pub(crate) fn result<T>(self, result: &Result<T, crate::dashboard_v3::V3ApiError>) {
        use OperationOutcome::{Failed, Rejected, Success};
        match result {
            Ok(_) => self.complete(Success, None, OperationMetadata::default()),
            Err(error) => {
                let reason = error.operation_reason();
                let outcome = match reason {
                    "unauthorized"
                    | "invalidJson"
                    | "missingExpectedRevision"
                    | "revisionConflict"
                    | "invalidRequest"
                    | "notFound"
                    | "conflict"
                    | "preconditionFailed"
                    | "notImplemented"
                    | "forbidden"
                    | "gone"
                    | "throttled"
                    | "builtinProviderImmutable"
                    | "operationPayloadMismatch" => Rejected,
                    _ => Failed,
                };
                self.complete(outcome, Some(reason), OperationMetadata::default());
            }
        }
    }

    fn record_error(&self, error: &anyhow::Error) {
        tracing::warn!(operation_id = %self.record.operation_id, action = %self.record.action,
            error = %error, "user operation receipt could not be recorded");
    }
}

#[cfg(test)]
mod tests;
