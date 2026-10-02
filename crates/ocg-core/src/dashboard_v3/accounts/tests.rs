use super::{AccountUpdate, MutationExpectation, update_account_locked};
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::goat_plan_cooldowns::{self, GoatPlanCooldowns};
use crate::models::{Account, AccountSetupStep, AccountType, UsageWindowKind};
use crate::provider::{COMMAND_CODE_PROVIDER_ID, CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
use crate::state::CoreStateInner;
use chrono::{TimeZone, Utc};
use std::sync::Arc;

const SAME_KEY: &str = "goat-plan-same-key";
const NEXT_KEY: &str = "goat-plan-next-key";

fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir =
        std::env::temp_dir().join(format!("ocg-goat-key-patch-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let opened = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("goat-key-patch")),
        )
        .unwrap(),
    );
    (dir, opened)
}

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

fn instant(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 3, hour, 0, 0).unwrap()
}

fn seeded_windows() -> GoatPlanCooldowns {
    GoatPlanCooldowns {
        five_hours: Some(instant(1)),
        week: Some(instant(2)),
        month: Some(instant(3)),
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

fn seed_windows(state: &crate::state::CoreState, account_id: &str) -> GoatPlanCooldowns {
    let stored = stored_key(state, account_id);
    let windows = seeded_windows();
    let db = state.db.lock();
    for (window, reset) in [
        (UsageWindowKind::FiveHours, windows.five_hours.unwrap()),
        (UsageWindowKind::Week, windows.week.unwrap()),
        (UsageWindowKind::Month, windows.month.unwrap()),
    ] {
        assert!(
            goat_plan_cooldowns::record_window_on(
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

fn plan(state: &crate::state::CoreState, account_id: &str) -> Option<GoatPlanCooldowns> {
    goat_plan_cooldowns::load_for_legacy_on(&state.db.lock().conn, account_id).unwrap()
}

fn patch_key(state: &crate::state::CoreState, id: &str, key: &str) {
    update_account_locked(
        state,
        id,
        AccountUpdate {
            expectation: MutationExpectation {
                expected_revision: state.settings_revision(),
                process_generation: state.process_generation(),
            },
            name: None,
            username: None,
            password: None,
            key: Some(key.into()),
            enabled: None,
            referral_code: None,
            purchase_date: None,
            notes: None,
            ollama_billing_tier: None,
        },
    )
    .unwrap();
}

#[test]
fn same_goat_key_patch_keeps_every_plan_window_and_ciphertext() {
    let (dir, state) = state("same");
    insert_keyed(&state, "goat-same", COMMAND_CODE_PROVIDER_ID, SAME_KEY);
    let windows = seed_windows(&state, "goat-same");
    let before = stored_key(&state, "goat-same");

    patch_key(&state, "goat-same", &format!("  {SAME_KEY}  "));

    let after = stored_key(&state, "goat-same");
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), SAME_KEY);
    assert_eq!(plan(&state, "goat-same"), Some(windows));
    assert_eq!(after.version, before.version);
    assert_eq!(after.auth_state_version, before.auth_state_version);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn changed_goat_key_patch_clears_every_plan_window() {
    let (dir, state) = state("changed");
    insert_keyed(&state, "goat-next", COMMAND_CODE_PROVIDER_ID, SAME_KEY);
    seed_windows(&state, "goat-next");
    let before = stored_key(&state, "goat-next");

    patch_key(&state, "goat-next", NEXT_KEY);

    let after = stored_key(&state, "goat-next");
    assert_ne!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), NEXT_KEY);
    assert_eq!(plan(&state, "goat-next"), None);
    assert_eq!(after.version, before.version);
    assert_eq!(after.auth_state_version, before.auth_state_version);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn same_non_goat_key_patch_still_reencrypts_and_clears_a_plan_map() {
    let (dir, state) = state("other");
    insert_keyed(&state, "go-same", OPENCODE_PROVIDER_ID, SAME_KEY);
    seed_windows(&state, "go-same");
    let before = stored_key(&state, "go-same");

    patch_key(&state, "go-same", SAME_KEY);

    let after = stored_key(&state, "go-same");
    assert_ne!(after.key_cipher, before.key_cipher);
    assert_eq!(state.decrypt_key(&after.key_cipher).unwrap(), SAME_KEY);
    assert_eq!(plan(&state, "go-same"), None);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
