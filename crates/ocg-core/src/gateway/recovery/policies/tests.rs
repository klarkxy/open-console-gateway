use super::*;
use crate::routing_snapshot::ExecutionCredential;
use crate::temporary_policy::{TemporaryRuleScope, builtin_rule};
use ocg_domain::{
    credential::ModelScope,
    destination::{Cooldowns, Grants},
};

fn account(id: &str) -> ExecutionCredential {
    ExecutionCredential {
        id: id.into(),
        credential_id: format!("credential-{id}"),
        destination_id: "connection".into(),
        provider_id: "command-code".into(),
        official_pricing_kind: None,
        name: id.into(),
        key_cipher: "synthetic".into(),
        enabled: true,
        ready: true,
        auth_error: None,
        cooldowns: Cooldowns {
            generic_until: None,
            five_hour_until: None,
            week_until: None,
            month_until: None,
            free_until: None,
        },
        binding_id: format!("binding-{id}"),
        binding_enabled: true,
        credential_version: 1,
        authorization_connection_id: "connection".into(),
        scope: ModelScope::All,
        grants: Grants {
            allowed_endpoint_ids: vec![],
            allowed_origins: vec![],
        },
        quota_recovery: None,
        quota_probe: false,
    }
}

fn resources(id: &str, generation: u8, model: u8, saved: &SavedRules) -> ResourceSet {
    let mut resources =
        ResourceSet::fixture(7, 9, model, &["a", "b"], false).with_credential(generation, id);
    resources.add_policies(
        saved,
        &account(id),
        "exact-route",
        &format!("model-{model}"),
    );
    resources
}

fn reject(permit: &mut RecoveryPermit, current: &ResourceSet, mono: Instant) {
    assert!(
        !permit
            .observe_policies(
                current,
                400,
                ProviderErrorClass::InsufficientCredits,
                None,
                None,
                mono
            )
            .is_empty()
    );
}

#[test]
fn local_credit_wait_is_per_credential_model_even_in_a_shared_pool() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = (Utc::now(), Instant::now());
    let saved = SavedRules::default();
    let a = resources("a", 1, 1, &saved);
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    reject(&mut permit, &a, mono);
    drop(permit);
    assert!(runtime.acquire(a.clone(), wall, mono).is_err());
    assert!(
        runtime
            .acquire(resources("b", 2, 1, &saved), wall, mono)
            .is_ok()
    );
    assert!(
        runtime
            .acquire(resources("a", 1, 2, &saved), wall, mono)
            .is_ok()
    );
    assert!(
        runtime
            .acquire(resources("a", 3, 1, &saved), wall, mono)
            .is_ok()
    );
}

#[test]
fn due_probe_is_singleflight_and_ordinary_success_cannot_clear_a_later_rejection() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = (Utc::now(), Instant::now());
    let a = resources("a", 1, 1, &SavedRules::default());
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut concurrent = runtime.acquire(a.clone(), wall, mono).unwrap();
    reject(&mut first, &a, mono);
    concurrent.confirm_success();
    drop((first, concurrent));
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    for _ in 0..16 {
        assert!(runtime.acquire(a.clone(), wall, later).is_err());
    }
    probe.confirm_success();
    drop(probe);
    assert!(runtime.acquire(a.clone(), wall, later).is_ok());
    assert!(runtime.acquire(a, wall, later).is_ok());
}

#[test]
fn changed_rule_version_fences_late_errors_without_resetting_unchanged_rules() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = (Utc::now(), Instant::now());
    let mut saved = SavedRules::default();
    let mut other = builtin_rule();
    other.id = "operator.also-credits".into();
    saved.rules.push(VersionedRule {
        rule: other,
        revision: 1,
    });
    let a = resources("a", 1, 1, &saved);
    let mut old = runtime.acquire(a.clone(), wall, mono).unwrap();
    reject(&mut old, &a, mono);
    saved.rules[0].rule.enabled = false;
    saved.rules[0].revision = 2;
    runtime.reconcile_policies(&saved);
    let current = resources("a", 1, 1, &saved);
    let receipts = old.observe_policies(
        &current,
        400,
        ProviderErrorClass::InsufficientCredits,
        None,
        None,
        mono,
    );
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].rule_id, "operator.also-credits");
    drop(old);
    assert!(runtime.acquire(current, wall, mono).is_err());
}

#[test]
fn operator_reset_is_rule_specific_and_does_not_shorten_retry_after() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = (Utc::now(), Instant::now());
    let a = resources("a", 1, 1, &SavedRules::default());
    let mut old = runtime.acquire(a.clone(), wall, mono).unwrap();
    let hint = Some(RetryHint::Until(wall + chrono::Duration::seconds(600)));
    old.observe_policies(
        &a,
        400,
        ProviderErrorClass::InsufficientCredits,
        None,
        hint,
        mono,
    );
    let id = {
        let inner = runtime.inner.lock();
        let slot = inner
            .slots
            .values()
            .find(|slot| slot.policy.is_some() && slot.restricted())
            .unwrap();
        format!("{}:{}", slot.policy_id, slot.revision)
    };
    assert!(runtime.reset_policy_wait(&id));
    assert!(!runtime.reset_policy_wait(&id));
    assert!(
        old.observe_policies(
            &a,
            400,
            ProviderErrorClass::InsufficientCredits,
            None,
            None,
            mono
        )
        .is_empty()
    );
    drop(old);
    assert!(
        runtime
            .acquire(
                a.clone(),
                wall + chrono::Duration::seconds(40),
                mono + Duration::from_secs(40)
            )
            .is_err()
    );
    assert!(
        runtime
            .acquire(
                a,
                wall + chrono::Duration::seconds(601),
                mono + Duration::from_secs(601)
            )
            .is_ok()
    );
}

#[test]
fn cancellation_never_certifies_recovery_and_backoff_is_bounded() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mut mono) = (Utc::now(), Instant::now());
    let a = resources("a", 1, 1, &SavedRules::default());
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    reject(&mut permit, &a, mono);
    drop(permit);
    mono += Duration::from_secs(40);
    drop(runtime.acquire(a.clone(), wall, mono).unwrap());
    assert!(runtime.acquire(a.clone(), wall, mono).is_err());
    for _ in 0..10 {
        mono += Duration::from_secs(310);
        let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
        reject(&mut permit, &a, mono);
        drop(permit);
        let wait = runtime.acquire(a.clone(), wall, mono).err().unwrap();
        assert!(wait.next_probe_in_seconds.unwrap() <= 301);
        assert!(wait.upstream_not_before.is_none());
    }
}

#[test]
fn model_unknown_does_not_widen_but_explicit_credential_scope_does() {
    let mut model_only = ResourceSet::fixture(1, 1, 1, &["a"], false);
    model_only.add_policies(&SavedRules::default(), &account("a"), "route", "");
    assert!(model_only.policies.is_empty());
    let mut saved = SavedRules::default();
    saved.rules[0].rule.scope = TemporaryRuleScope::Credential;
    model_only.add_policies(&saved, &account("a"), "route", "");
    assert_eq!(model_only.policies.len(), 1);
}
