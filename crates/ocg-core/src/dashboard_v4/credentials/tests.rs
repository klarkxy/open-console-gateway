use super::super::destinations::DestinationsError;
use super::*;
use crate::crypto::StaticKeyCipher;
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
use crate::quota_recovery::PersistedQuotaRecovery;
use crate::quota_recovery::QuotaEpisode;
use crate::state::CoreStateInner;
use chrono::{TimeZone, Utc};
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
use std::sync::Arc;

fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-quota-retry-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("quota-retry")),
        )
        .unwrap(),
    );
    (dir, state)
}

fn insert_go(state: &crate::state::CoreState, id: &str) {
    let now = Utc::now();
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

#[test]
fn quota_retry_is_cas_idempotent_when_ready_and_does_not_send() {
    let (dir, state) = state("retry");
    insert_go(&state, "acct-a");
    let now = Utc.with_ymd_and_hms(2026, 9, 20, 0, 0, 0).unwrap();
    let recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        now,
        None,
    );
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let (id, version, key_cipher, _) =
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, "acct-a")
            .unwrap()
            .unwrap();
    let episode = QuotaEpisode {
        credential_id: id,
        account_id: "acct-a".into(),
        credential_version: version,
        epoch: recovery.epoch,
        key_cipher,
    };
    crate::db::quota_recovery::save_on(&state.db.lock().conn, &episode, &recovery).unwrap();

    let waiting = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(
        waiting.credential.quota_recovery.as_ref().unwrap().status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Ready
    );
    let revision = waiting.revision.revision;
    let again = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(again.revision.revision, revision);
    assert_eq!(
        again.credential.quota_recovery.as_ref().unwrap().status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Ready
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn quota_retry_rejects_stale_cas() {
    let (dir, state) = state("cas");
    insert_go(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let err = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision() + 1,
            process_generation: state.process_generation(),
        },
    )
    .unwrap_err();
    match err {
        DestinationsError::Api(_) => {}
        DestinationsError::Refused(_) => panic!("expected CAS conflict"),
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn persist_unknown(state: &crate::state::CoreState, account_id: &str) -> QuotaEpisode {
    persist_recovery_at(
        state,
        account_id,
        Utc.with_ymd_and_hms(2026, 9, 20, 0, 0, 0).unwrap(),
    )
}

fn persist_waiting(state: &crate::state::CoreState, account_id: &str) -> QuotaEpisode {
    let now = Utc::now();
    persist_recovery_at(state, account_id, now + chrono::Duration::hours(1))
}

fn persist_recovery_at(
    state: &crate::state::CoreState,
    account_id: &str,
    next_retry_at: chrono::DateTime<Utc>,
) -> QuotaEpisode {
    let observed = next_retry_at - chrono::Duration::minutes(15);
    let mut recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        observed,
        None,
    );
    recovery.next_retry_at = next_retry_at;
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
    episode
}

#[test]
fn quota_retry_matching_probe_is_idempotent_and_stale_probe_does_not_block() {
    let (dir, state) = state("probe-id");
    insert_go(&state, "acct-a");
    let episode = persist_waiting(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), episode.clone());
    let probing = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(
        probing.credential.quota_recovery.as_ref().unwrap().status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Probing
    );
    let revision = probing.revision.revision;
    let again = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(again.revision.revision, revision);

    let mut stale = episode.clone();
    stale.key_cipher = "rotated-cipher".into();
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), stale);
    let ready = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_ne!(ready.revision.revision, revision);
    assert_eq!(
        ready.credential.quota_recovery.as_ref().unwrap().status,
        crate::dashboard_v4::types::QuotaRecoveryStatus::Ready
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn quota_retry_stale_ready_receipt_cannot_overwrite_newer_state() {
    let (dir, state) = state("stale-ready");
    insert_go(&state, "acct-a");
    persist_unknown(&state, "acct-a");
    let credential_id = credential_id_for_legacy_account("acct-a").to_string();
    let ready = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    let stale_revision = ready.revision.revision;
    state.bump_settings_revision();
    let err = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: stale_revision,
            process_generation: state.process_generation(),
        },
    )
    .unwrap_err();
    match err {
        DestinationsError::Api(_) => {}
        DestinationsError::Refused(_) => panic!("expected CAS conflict"),
    }
    crate::db::quota_recovery::clear_for_account_on(&state.db.lock().conn, "acct-a").unwrap();
    let current = quota_retry_locked(
        &state,
        &credential_id,
        MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap_err();
    match current {
        DestinationsError::Api(_) => {}
        DestinationsError::Refused(_) => panic!("cleared recovery must not be restored"),
    }
    assert!(
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, "acct-a")
            .unwrap()
            .unwrap()
            .3
            .is_none()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

const GOAT_SAME_KEY: &str = "goat-plan-same-key";
const GOAT_NEXT_KEY: &str = "goat-plan-next-key";

fn insert_keyed(state: &crate::state::CoreState, id: &str, provider_id: &str, secret: &str) {
    let now = Utc::now();
    state
        .db
        .lock()
        .create_account(&Account {
            id: id.into(),
            provider_id: provider_id.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key(secret).unwrap(),
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

fn goat_instant(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 3, hour, 0, 0).unwrap()
}

fn seeded_windows() -> crate::goat_plan_cooldowns::GoatPlanCooldowns {
    crate::goat_plan_cooldowns::GoatPlanCooldowns {
        five_hours: Some(goat_instant(1)),
        week: Some(goat_instant(2)),
        month: Some(goat_instant(3)),
    }
}

struct StoredKey {
    credential_id: String,
    binding_id: String,
    version: u64,
    auth_state_version: u64,
    key_cipher: String,
}

fn stored_key(state: &crate::state::CoreState, account_id: &str) -> StoredKey {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, binding_id, COALESCE(credential_version, 1),
                    COALESCE(auth_state_version, 1), key_cipher
             FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| {
                Ok(StoredKey {
                    credential_id: row.get(0)?,
                    binding_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u64,
                    auth_state_version: row.get::<_, i64>(3)? as u64,
                    key_cipher: row.get(4)?,
                })
            },
        )
        .unwrap()
}

fn seed_windows(
    state: &crate::state::CoreState,
    account_id: &str,
) -> crate::goat_plan_cooldowns::GoatPlanCooldowns {
    let stored = stored_key(state, account_id);
    let windows = seeded_windows();
    let db = state.db.lock();
    for (window, reset) in [
        (
            crate::models::UsageWindowKind::FiveHours,
            windows.five_hours.unwrap(),
        ),
        (crate::models::UsageWindowKind::Week, windows.week.unwrap()),
        (
            crate::models::UsageWindowKind::Month,
            windows.month.unwrap(),
        ),
    ] {
        assert!(
            crate::goat_plan_cooldowns::record_window_on(
                &db.conn,
                &stored.credential_id,
                account_id,
                &stored.binding_id,
                stored.version,
                &stored.key_cipher,
                window,
                reset,
            )
            .unwrap()
        );
    }
    windows
}

fn plan(
    state: &crate::state::CoreState,
    account_id: &str,
) -> Option<crate::goat_plan_cooldowns::GoatPlanCooldowns> {
    crate::goat_plan_cooldowns::load_for_legacy_on(&state.db.lock().conn, account_id).unwrap()
}

fn rotate_secret(
    state: &crate::state::CoreState,
    account_id: &str,
    secret: &str,
) -> CredentialRotateResult {
    let credential_id = credential_id_for_legacy_account(account_id).to_string();
    rotate_locked(
        state,
        &credential_id,
        CredentialRotateRequest {
            expectation: MutationExpectation {
                expected_revision: state.settings_revision(),
                process_generation: state.process_generation(),
            },
            secret_input: secret.into(),
        },
    )
    .unwrap()
}

#[test]
fn same_goat_key_rotate_keeps_every_plan_window_and_bumps_versions() {
    let (dir, state) = state("goat-same");
    insert_keyed(
        &state,
        "goat-same",
        crate::provider::COMMAND_CODE_PROVIDER_ID,
        GOAT_SAME_KEY,
    );
    let windows = seed_windows(&state, "goat-same");
    let before = stored_key(&state, "goat-same");

    let rotated = rotate_secret(&state, "goat-same", &format!("  {GOAT_SAME_KEY}  "));

    let after = stored_key(&state, "goat-same");
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), GOAT_SAME_KEY);
    assert_eq!(plan(&state, "goat-same"), Some(windows.clone()));
    assert_eq!(rotated.version, before.version + 1);
    assert_eq!(rotated.auth_state_version, before.auth_state_version + 1);
    assert_eq!(after.version, rotated.version);
    assert_eq!(after.auth_state_version, rotated.auth_state_version);
    assert_eq!(rotated.credential_id, before.credential_id);
    let stale = crate::goat_plan_cooldowns::record_window_on(
        &state.db.lock().conn,
        &before.credential_id,
        "goat-same",
        &before.binding_id,
        before.version,
        &before.key_cipher,
        crate::models::UsageWindowKind::Week,
        goat_instant(4),
    )
    .unwrap();
    assert!(!stale);
    assert_eq!(plan(&state, "goat-same"), Some(windows));
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn changed_goat_key_rotate_clears_every_plan_window_and_bumps_versions() {
    let (dir, state) = state("goat-next");
    insert_keyed(
        &state,
        "goat-next",
        crate::provider::COMMAND_CODE_PROVIDER_ID,
        GOAT_SAME_KEY,
    );
    seed_windows(&state, "goat-next");
    let before = stored_key(&state, "goat-next");

    let rotated = rotate_secret(&state, "goat-next", GOAT_NEXT_KEY);

    let after = stored_key(&state, "goat-next");
    assert_ne!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), GOAT_NEXT_KEY);
    assert_eq!(plan(&state, "goat-next"), None);
    assert_eq!(rotated.version, before.version + 1);
    assert_eq!(rotated.auth_state_version, before.auth_state_version + 1);
    assert_eq!(after.version, rotated.version);
    assert_eq!(after.auth_state_version, rotated.auth_state_version);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn same_non_goat_key_rotate_still_reencrypts_and_clears_a_plan_map() {
    let (dir, state) = state("goat-other");
    insert_keyed(&state, "go-same", OPENCODE_PROVIDER_ID, GOAT_SAME_KEY);
    seed_windows(&state, "go-same");
    let before = stored_key(&state, "go-same");

    rotate_secret(&state, "go-same", GOAT_SAME_KEY);

    let after = stored_key(&state, "go-same");
    assert_ne!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), GOAT_SAME_KEY);
    assert_eq!(plan(&state, "go-same"), None);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
