use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::ControlRevision;
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
use crate::quota_recovery::{PersistedQuotaRecovery, QuotaAcquire, QuotaPresentationStatus};
use crate::state::CoreStateInner;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
use std::sync::Arc;

fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir =
        std::env::temp_dir().join(format!("ocg-quota-acquire-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(db, dir.clone(), Arc::new(StaticKeyCipher::new(tag))).unwrap(),
    );
    (dir, state)
}

fn insert_go(state: &crate::state::CoreState, id: &str) {
    let now = chrono::Utc::now();
    state
        .db
        .lock()
        .create_account(&Account {
            id: id.into(),
            provider_id: OPENCODE_PROVIDER_ID.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
}

fn persist_due(state: &crate::state::CoreState, account_id: &str) {
    let now = chrono::Utc::now();
    let mut recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        now - chrono::Duration::minutes(30),
        None,
    );
    recovery.next_retry_at = now - chrono::Duration::minutes(1);
    let (id, version, key_cipher, _) =
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, account_id)
            .unwrap()
            .unwrap();
    let episode = QuotaEpisode {
        credential_id: id,
        account_id: account_id.into(),
        credential_version: version,
        epoch: recovery.epoch,
        key_cipher,
    };
    crate::db::quota_recovery::save_on(&state.db.lock().conn, &episode, &recovery).unwrap();
}

#[test]
fn trial_acquisition_bumps_revision_and_duplicate_stays_probing() {
    let (dir, state) = state("rev");
    insert_go(&state, "acct-a");
    persist_due(&state, "acct-a");
    let ready_revision = ControlRevision::from_state(&state);
    let after_trial = {
        let _settings_update = state.settings_update.lock();
        let db = state.db.lock();
        let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
        let credential = snapshot
            .credentials
            .iter()
            .find(|credential| credential.id == "acct-a")
            .unwrap();
        let view = credential
            .quota_recovery
            .as_ref()
            .unwrap()
            .present(state.sample_gateway_clock().0, false);
        assert_eq!(view.status, QuotaPresentationStatus::Ready);
        assert_eq!(ControlRevision::from_state(&state), ready_revision);
        let first =
            acquire_quota_trial_locked(&state, &db, credential, state.sample_gateway_clock().0)
                .unwrap();
        assert!(matches!(first, QuotaAcquire::Trial(_)));
        let probing = ControlRevision::from_state(&state);
        assert!(probing.revision > ready_revision.revision);
        let mut live = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
        live.apply_quota_probes(&state.quota_probes.lock());
        let credential = live
            .credentials
            .iter()
            .find(|credential| credential.id == "acct-a")
            .unwrap();
        assert!(credential.quota_probe);
        let view = credential
            .quota_recovery
            .as_ref()
            .unwrap()
            .present(state.sample_gateway_clock().0, credential.quota_probe);
        assert_eq!(view.status, QuotaPresentationStatus::Probing);
        probing
    };
    {
        let _settings_update = state.settings_update.lock();
        let db = state.db.lock();
        let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
        let credential = snapshot
            .credentials
            .iter()
            .find(|credential| credential.id == "acct-a")
            .unwrap();
        let second =
            acquire_quota_trial_locked(&state, &db, credential, state.sample_gateway_clock().0)
                .unwrap();
        assert!(matches!(second, QuotaAcquire::SkipProbing));
        assert_eq!(ControlRevision::from_state(&state), after_trial);
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn not_in_recovery_does_not_bump_revision() {
    let (dir, state) = state("idle");
    insert_go(&state, "acct-a");
    let before = state.settings_revision();
    {
        let _settings_update = state.settings_update.lock();
        let db = state.db.lock();
        let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
        let credential = snapshot
            .credentials
            .iter()
            .find(|credential| credential.id == "acct-a")
            .unwrap();
        let result =
            acquire_quota_trial_locked(&state, &db, credential, state.sample_gateway_clock().0)
                .unwrap();
        assert!(matches!(result, QuotaAcquire::NotInRecovery));
        assert_eq!(state.settings_revision(), before);
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
