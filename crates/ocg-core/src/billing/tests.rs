use super::*;
use ocg_domain::billing::BillingTokens;

#[test]
fn initial_monthly_credit_expiry_precedes_a_later_topup() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = CreditMeterState::new(
        "meter".into(),
        "cred".into(),
        "dest".into(),
        "https://example.test/v1".into(),
        config(
            Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
            Vec::new(),
        ),
        vec![bucket(
            "month",
            CreditBucketKind::Monthly,
            400.0,
            100.0,
            "2026-09-21T00:00:00Z",
            None,
        )],
        now,
    )
    .unwrap();
    assert_eq!(
        state.buckets[0].expires_at,
        Some(at("2026-09-30T16:00:00Z"))
    );
    state
        .add_grant("topup".into(), 400.0, Some(at("2026-10-21T00:00:00Z")), now)
        .unwrap();
    state.deduct(80.0, now).unwrap();
    assert_eq!(remaining_of(&state, "month"), 20.0);
    assert_eq!(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.kind == CreditBucketKind::TopUp)
            .unwrap()
            .remaining,
        400.0
    );
}

#[test]
fn spent_permanent_grants_do_not_exhaust_future_grant_capacity() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(Vec::new(), None, Vec::new());
    for index in 0..100 {
        state
            .add_grant(format!("grant-{index}"), 1.0, None, now)
            .unwrap();
        state.deduct(1.0, now).unwrap();
    }
    state.add_grant("next".into(), 10.0, None, now).unwrap();
    assert_eq!(state.project(now, 0).remaining, 10.0);
    assert!(state.buckets.len() <= MAX_BUCKETS);
}

#[test]
fn exhausted_active_grant_remains_available_for_balance_correction() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(Vec::new(), None, Vec::new());
    let id = state.add_grant("active".into(), 10.0, None, now).unwrap();
    state.deduct(10.0, now).unwrap();
    state.advance(now).unwrap();
    assert_eq!(state.project(now, 0).buckets.len(), 1);
    state
        .calibrate(
            &[CreditBalanceCorrection {
                bucket_id: id,
                remaining: 5.0,
            }],
            now,
        )
        .unwrap();
    assert_eq!(state.project(now, 0).remaining, 5.0);
}

#[test]
fn renewal_prunes_history_and_keeps_funded_grants() {
    let now = at("2026-09-21T00:00:00Z");
    let boundary = "2026-09-30T16:00:00Z";
    let mut buckets = vec![bucket(
        "month",
        CreditBucketKind::Monthly,
        400.0,
        30.0,
        "2026-09-01T00:00:00Z",
        Some(boundary),
    )];
    for index in 0..63 {
        buckets.push(bucket(
            &format!("manual-{index}"),
            CreditBucketKind::Manual,
            1.0,
            1.0,
            "2026-09-01T00:00:00Z",
            None,
        ));
    }
    let mut state = meter(buckets, Some(monthly(400.0, boundary, 480)), Vec::new());
    assert!(state.add_grant("no-room".into(), 1.0, None, now).is_err());
    state.advance(at(boundary)).unwrap();
    assert_eq!(state.project(at(boundary), 0).remaining, 463.0);
    state.advance(at(boundary)).unwrap();
    assert_eq!(state.project(at(boundary), 0).remaining, 463.0);
}

#[test]
fn calibration_cannot_consume_the_reserved_monthly_slot() {
    let now = at("2026-09-21T00:00:00Z");
    let mut buckets: Vec<_> = (0..63)
        .map(|index| {
            bucket(
                &format!("funded-{index}"),
                CreditBucketKind::Manual,
                1.0,
                1.0,
                "2026-09-01T00:00:00Z",
                None,
            )
        })
        .collect();
    buckets.push(bucket(
        "empty",
        CreditBucketKind::Manual,
        1.0,
        0.0,
        "2026-09-01T00:00:00Z",
        None,
    ));
    let mut state = meter(
        buckets,
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        Vec::new(),
    );
    assert!(
        state
            .calibrate(
                &[CreditBalanceCorrection {
                    bucket_id: "empty".into(),
                    remaining: 1.0
                }],
                now
            )
            .is_err()
    );
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn rate(
    model: &str,
    input: f64,
    cache_read: Option<f64>,
    output: f64,
    cache_write: Option<f64>,
) -> CreditRate {
    CreditRate {
        model: model.to_string(),
        input_per_million: input,
        output_per_million: output,
        cache_read_per_million: cache_read,
        cache_write_per_million: cache_write,
    }
}

fn monthly(amount: f64, next_reset: &str, offset: i32) -> MonthlyCredits {
    MonthlyCredits {
        amount,
        next_reset_at: at(next_reset),
        timezone_offset_minutes: offset,
        renewal_ends_at: None,
    }
}

fn config(monthly: Option<MonthlyCredits>, rates: Vec<CreditRate>) -> CreditConfiguration {
    CreditConfiguration {
        name: "Mini".to_string(),
        currency: "CNY".to_string(),
        credits_per_currency: 1_000_000.0,
        rates,
        monthly,
        source_url: Some(STEPFUN_PRICING_URL.to_string()),
    }
}

fn bucket(
    id: &str,
    kind: CreditBucketKind,
    granted: f64,
    remaining: f64,
    starts: &str,
    expires: Option<&str>,
) -> CreditBucket {
    CreditBucket {
        id: id.to_string(),
        kind,
        label: id.to_string(),
        granted,
        remaining,
        starts_at: at(starts),
        expires_at: expires.map(at),
    }
}

fn remaining_of(state: &CreditMeterState, id: &str) -> f64 {
    state
        .buckets
        .iter()
        .find(|bucket| bucket.id == id)
        .unwrap()
        .remaining
}

fn meter(
    buckets: Vec<CreditBucket>,
    monthly: Option<MonthlyCredits>,
    rates: Vec<CreditRate>,
) -> CreditMeterState {
    CreditMeterState::new(
        "meter-1".into(),
        "cred-1".into(),
        "dest-1".into(),
        "https://api.stepfun.com/v1/chat/completions".into(),
        config(monthly, rates),
        buckets,
        at("2026-01-01T00:00:00Z"),
    )
    .unwrap()
}

#[test]
fn charge_weights_four_groups_and_converts_to_credits() {
    let attempt = CreditAttempt {
        credential_id: "cred-1".into(),
        meter_id: "meter-1".into(),
        destination_id: "dest-1".into(),
        endpoint: "https://api.stepfun.com/v1".into(),
        account_id: "acc-1".into(),
        model: "step-5-preview".into(),
        currency: "CNY".into(),
        credits_per_currency: 1_000_000.0,
        rate: Some(rate("step-5-preview", 2.0, Some(0.5), 3.0, Some(4.0))),
        at: at("2026-09-21T00:00:00Z"),
    };
    let tokens = BillingTokens::new(100, 5, 20, 10);
    // uncached 70*2 + output 5*3 + read 20*0.5 + write 10*4 = 205 currency units / 1e6 * 1e6
    assert_eq!(attempt.charge(tokens), Some(205.0));
}

#[test]
fn charge_unknown_or_malformed_is_none_never_zero() {
    let mut attempt = CreditAttempt {
        credential_id: "cred-1".into(),
        meter_id: "meter-1".into(),
        destination_id: "dest-1".into(),
        endpoint: "https://api.stepfun.com/v1".into(),
        account_id: "acc-1".into(),
        model: "router".into(),
        currency: "CNY".into(),
        credits_per_currency: 1_000_000.0,
        rate: None,
        at: at("2026-09-21T00:00:00Z"),
    };
    let tokens = BillingTokens::new(10, 2, 0, 0);
    assert_eq!(attempt.charge(tokens), None);

    attempt.rate = Some(rate("step-5-preview", 2.0, Some(0.5), 3.0, None));
    assert_eq!(attempt.charge(BillingTokens::new(10, 0, 0, 5)), None);
    assert_eq!(attempt.charge(BillingTokens::new(-1, 0, 0, 0)), None);

    attempt.rate = Some(rate("step-5-preview", 2.0, Some(0.5), 3.0, Some(4.0)));
    assert_eq!(attempt.charge(BillingTokens::new(0, 0, 0, 0)), Some(0.0));
}

#[test]
fn deduct_uses_earliest_expiry_and_keeps_month_pool_independent() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(
        vec![
            bucket(
                "monthly",
                CreditBucketKind::Monthly,
                400.0,
                400.0,
                "2026-09-01T16:00:00Z",
                Some("2026-09-30T16:00:00Z"),
            ),
            bucket(
                "top-late",
                CreditBucketKind::TopUp,
                100.0,
                100.0,
                "2026-09-10T00:00:00Z",
                Some("2026-10-10T00:00:00Z"),
            ),
            bucket(
                "top-soon",
                CreditBucketKind::TopUp,
                50.0,
                50.0,
                "2026-09-12T00:00:00Z",
                Some("2026-09-25T00:00:00Z"),
            ),
            bucket(
                "manual",
                CreditBucketKind::Manual,
                20.0,
                20.0,
                "2026-09-01T00:00:00Z",
                None,
            ),
        ],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );

    state.deduct(80.0, now).unwrap();
    assert_eq!(remaining_of(&state, "top-soon"), 0.0);
    assert_eq!(remaining_of(&state, "top-late"), 100.0);
    assert_eq!(remaining_of(&state, "monthly"), 370.0);
    assert_eq!(remaining_of(&state, "manual"), 20.0);

    state.deduct(470.0, now).unwrap();
    assert_eq!(remaining_of(&state, "top-late"), 0.0);
    assert_eq!(remaining_of(&state, "monthly"), 0.0);
    assert_eq!(remaining_of(&state, "manual"), 20.0);
    assert_eq!(state.overdrawn, 0.0);
}

#[test]
fn deduct_stores_overflow_as_overdrawn() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            10.0,
            10.0,
            "2026-09-01T00:00:00Z",
            None,
        )],
        None,
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    state.deduct(25.0, now).unwrap();
    assert_eq!(state.buckets[0].remaining, 0.0);
    assert_eq!(state.overdrawn, 15.0);
    assert_eq!(state.spent_since_calibration, 25.0);
    let view = state.project(now, 2);
    assert_eq!(view.remaining, 0.0);
    assert_eq!(view.pending_requests, 2);
}

#[test]
fn missed_cycles_grant_only_the_current_month() {
    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            400.0,
            50.0,
            "2025-12-31T16:00:00Z",
            Some("2026-01-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-01-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    let now = at("2026-03-30T16:00:00Z");
    state.advance(now).unwrap();
    let monthly_buckets: Vec<_> = state
        .buckets
        .iter()
        .filter(|bucket| bucket.kind == CreditBucketKind::Monthly)
        .collect();
    let current = monthly_buckets
        .iter()
        .find(|bucket| bucket_active(bucket, now))
        .expect("current cycle");
    assert_eq!(current.granted, 400.0);
    assert_eq!(current.remaining, 400.0);
    assert_eq!(current.starts_at, at("2026-03-30T16:00:00Z"));
    assert_eq!(current.expires_at, Some(at("2026-04-29T16:00:00Z")));
    assert_eq!(
        monthly_buckets
            .iter()
            .filter(|bucket| bucket_active(bucket, now))
            .count(),
        1
    );
}

#[test]
fn january_31_anchor_clamps_february_then_returns_to_march_31() {
    let mut state = meter(
        vec![bucket(
            "seed",
            CreditBucketKind::Monthly,
            400.0,
            400.0,
            "2025-12-31T16:00:00Z",
            Some("2026-01-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-01-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );

    state.advance(at("2026-01-30T16:00:00Z")).unwrap();
    let jan = state
        .buckets
        .iter()
        .find(|bucket| bucket_active(bucket, at("2026-01-30T16:00:00Z")))
        .unwrap();
    assert_eq!(jan.starts_at, at("2026-01-30T16:00:00Z"));
    assert_eq!(jan.expires_at, Some(at("2026-02-27T16:00:00Z")));

    state.advance(at("2026-02-27T16:00:00Z")).unwrap();
    let feb = state
        .buckets
        .iter()
        .find(|bucket| bucket_active(bucket, at("2026-02-27T16:00:00Z")))
        .unwrap();
    assert_eq!(feb.starts_at, at("2026-02-27T16:00:00Z"));
    assert_eq!(feb.expires_at, Some(at("2026-03-30T16:00:00Z")));

    state.advance(at("2026-03-30T16:00:00Z")).unwrap();
    let mar = state
        .buckets
        .iter()
        .find(|bucket| bucket_active(bucket, at("2026-03-30T16:00:00Z")))
        .unwrap();
    assert_eq!(mar.starts_at, at("2026-03-30T16:00:00Z"));
    assert_eq!(mar.expires_at, Some(at("2026-04-29T16:00:00Z")));
}

#[test]
fn china_first_of_month_stays_on_utc_plus_8_midnight() {
    let mut state = meter(
        vec![bucket(
            "seed",
            CreditBucketKind::Monthly,
            400.0,
            400.0,
            "2026-09-01T16:00:00Z",
            Some("2026-09-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    state.advance(at("2026-09-30T16:00:00Z")).unwrap();
    state.advance(at("2026-10-31T16:00:00Z")).unwrap();
    state.advance(at("2026-11-30T16:00:00Z")).unwrap();
    let current = state
        .buckets
        .iter()
        .find(|bucket| bucket_active(bucket, at("2026-11-30T16:00:00Z")))
        .unwrap();
    assert_eq!(current.starts_at, at("2026-11-30T16:00:00Z"));
    assert_eq!(current.expires_at, Some(at("2026-12-31T16:00:00Z")));
}

#[test]
fn frozen_attempt_rate_survives_configuration_edits() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            400.0,
            250.0,
            "2026-09-01T16:00:00Z",
            Some("2026-09-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    let attempt = state.capture_attempt("acc-1".into(), "step-5-preview", now);
    let mut edited = state.configuration.clone();
    edited.rates = vec![rate("step-5-preview", 1.0, Some(1.0), 1.0, Some(1.0))];
    edited.monthly = Some(monthly(1_600_000_000.0, "2026-09-30T16:00:00Z", 480));
    state.configure(edited, now).unwrap();
    assert_eq!(state.meter_id, "meter-1");
    assert_eq!(state.project(now, 0).remaining, 250.0);
    assert_eq!(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.id == "monthly")
            .unwrap()
            .granted,
        400.0
    );
    let tokens = BillingTokens::new(1_000_000, 0, 0, 0);
    assert_eq!(attempt.charge(tokens), Some(7.0 * 1_000_000.0));
    let live = state.capture_attempt("acc-1".into(), "step-5-preview", now);
    assert_eq!(live.charge(tokens), Some(1.0 * 1_000_000.0));
}

#[test]
fn calibration_corrects_known_buckets_and_resets_unknown_counters() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(
        vec![
            bucket(
                "monthly",
                CreditBucketKind::Monthly,
                400.0,
                10.0,
                "2026-09-01T16:00:00Z",
                Some("2026-09-30T16:00:00Z"),
            ),
            bucket(
                "topup",
                CreditBucketKind::TopUp,
                80.0,
                80.0,
                "2026-09-10T00:00:00Z",
                Some("2026-10-10T00:00:00Z"),
            ),
        ],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    state.overdrawn = 9.0;
    state.unpriced_requests = 4;
    state.spent_since_calibration = 30.0;
    state
        .calibrate(
            &[CreditBalanceCorrection {
                bucket_id: "monthly".into(),
                remaining: 320.0,
            }],
            now,
        )
        .unwrap();
    assert_eq!(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.id == "monthly")
            .unwrap()
            .remaining,
        320.0
    );
    assert_eq!(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.id == "topup")
            .unwrap()
            .remaining,
        80.0
    );
    assert_eq!(state.overdrawn, 0.0);
    assert_eq!(state.unpriced_requests, 0);
    assert_eq!(state.spent_since_calibration, 0.0);
    assert_eq!(state.last_calibration_at, Some(now));
    assert_eq!(state.project(now, 7).pending_requests, 7);

    let err = state
        .calibrate(
            &[CreditBalanceCorrection {
                bucket_id: "ghost".into(),
                remaining: 1.0,
            }],
            now,
        )
        .unwrap_err();
    assert!(err.to_string().contains("unknown bucket id"));
    let err = state
        .calibrate(
            &[CreditBalanceCorrection {
                bucket_id: "monthly".into(),
                remaining: 401.0,
            }],
            now,
        )
        .unwrap_err();
    assert!(err.to_string().contains("exceeds granted"));
}

#[test]
fn setup_does_not_fabricate_monthly_usage_and_overflow_is_rejected() {
    let err = CreditMeterState::new(
        "meter-1".into(),
        "cred-1".into(),
        "dest-1".into(),
        "https://api.stepfun.com/v1".into(),
        config(
            Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
            vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
        ),
        Vec::new(),
        at("2026-09-21T00:00:00Z"),
    )
    .and_then(|mut state| {
        assert!(state.buckets.is_empty());
        state.advance(at("2026-09-21T00:00:00Z"))?;
        assert!(
            state
                .project(at("2026-09-21T00:00:00Z"), 0)
                .buckets
                .is_empty()
        );
        state.add_grant("bonus".into(), 1e15 + 1.0, None, at("2026-09-21T00:00:00Z"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("invalid grant"));

    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            10.0,
            10.0,
            "2026-09-01T00:00:00Z",
            None,
        )],
        None,
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    assert!(state.deduct(f64::NAN, at("2026-09-21T00:00:00Z")).is_err());
    assert!(
        state
            .deduct(f64::INFINITY, at("2026-09-21T00:00:00Z"))
            .is_err()
    );
}

#[test]
fn billing_model_classifier_and_stepfun_plan_detector() {
    assert_eq!(
        billing_model_for_adapter(AdapterKind::OpencodeGo),
        BillingModel::Quota
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Goat),
        BillingModel::Quota
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Minimax),
        BillingModel::Quota
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Kimi),
        BillingModel::Quota
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Zen),
        BillingModel::Quota
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Ollama),
        BillingModel::Credits
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Http),
        BillingModel::Cash
    );
    assert_eq!(
        billing_model_for_adapter(AdapterKind::Cpa),
        BillingModel::Cash
    );

    assert!(is_stepfun_plan_endpoint(
        "https://api.stepfun.com/step_plan"
    ));
    assert!(is_stepfun_plan_endpoint(
        "https://api.stepfun.com:443/step_plan"
    ));
    assert!(is_stepfun_plan_endpoint(
        "https://api.stepfun.com/step_plan/v1/chat/completions"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com/v1/chat/completions"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com:443/v1/chat/completions"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com/step_planet"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com/step_planning"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "http://api.stepfun.com/step_plan"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com:444/step_plan"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://user:pass@api.stepfun.com/step_plan"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://api.stepfun.com/step_plan?q=1"
    ));
    assert!(!is_stepfun_plan_endpoint(
        "https://evil.api.stepfun.com/step_plan"
    ));
    assert_eq!(
        billing_model_for_destination(
            AdapterKind::Http,
            "https://api.stepfun.com/v1/chat/completions"
        ),
        BillingModel::Cash
    );
    assert_eq!(
        billing_model_for_destination(AdapterKind::Http, "https://api.stepfun.com/step_plan"),
        BillingModel::Credits
    );
}

#[test]
fn stepfun_presets_use_published_cny_rates_and_next_china_month() {
    let at_now = at("2026-09-21T07:00:00Z");
    let presets = stepfun_plan_credits("https://api.stepfun.com/step_plan", at_now).unwrap();
    assert_eq!(presets.len(), 4);
    assert_eq!(presets[0].id, "mini");
    assert_eq!(presets[0].initial_grant, 400_000_000.0);
    assert_eq!(presets[1].initial_grant, 1_600_000_000.0);
    assert_eq!(presets[2].initial_grant, 8_000_000_000.0);
    assert_eq!(presets[3].initial_grant, 40_000_000_000.0);
    let monthly = presets[0].configuration.monthly.as_ref().unwrap();
    assert_eq!(monthly.next_reset_at, at("2026-09-30T16:00:00Z"));
    assert_eq!(monthly.timezone_offset_minutes, 480);
    assert_eq!(presets[0].configuration.credits_per_currency, 1_000_000.0);
    assert_eq!(
        presets[0].configuration.source_url.as_deref(),
        Some(STEPFUN_PRICING_URL)
    );

    let by_model = |model: &str| {
        presets[0]
            .configuration
            .rates
            .iter()
            .find(|rate| rate.model == model)
            .unwrap()
    };
    let preview = by_model("step-5-preview");
    assert_eq!(preview.input_per_million, 7.0);
    assert_eq!(preview.cache_read_per_million, Some(0.35));
    assert_eq!(preview.output_per_million, 20.0);
    assert_eq!(preview.cache_write_per_million, Some(7.0));
    let flash = by_model("step-3.7-flash");
    assert_eq!(flash.input_per_million, 1.35);
    assert_eq!(flash.cache_read_per_million, Some(0.27));
    assert_eq!(flash.output_per_million, 8.1);
    assert_eq!(flash.cache_write_per_million, Some(1.35));
    for model in ["step-3.5-flash", "step-3.5-flash-2603"] {
        let row = by_model(model);
        assert_eq!(row.input_per_million, 0.7);
        assert_eq!(row.cache_read_per_million, Some(0.14));
        assert_eq!(row.output_per_million, 2.1);
        assert_eq!(row.cache_write_per_million, Some(0.7));
    }
    assert!(
        presets[0]
            .configuration
            .rates
            .iter()
            .all(|rate| !rate.model.contains("router") && !rate.model.contains("audio"))
    );
    assert!(stepfun_plan_credits("https://api.stepfun.com/v1/chat/completions", at_now).is_none());
}

#[test]
fn add_grant_appends_topup_without_touching_month_remaining() {
    let now = at("2026-09-21T00:00:00Z");
    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            400.0,
            250.0,
            "2026-09-01T16:00:00Z",
            Some("2026-09-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))],
    );
    let id = state
        .add_grant("pack".into(), 80.0, Some(at("2026-10-21T00:00:00Z")), now)
        .unwrap();
    let topup = state.buckets.iter().find(|bucket| bucket.id == id).unwrap();
    assert_eq!(topup.kind, CreditBucketKind::TopUp);
    assert_eq!(topup.remaining, 80.0);
    assert_eq!(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.id == "monthly")
            .unwrap()
            .remaining,
        250.0
    );
    let view = state.project(now, 0);
    assert_eq!(view.remaining, 330.0);
    assert_eq!(view.active_granted, 480.0);
}

#[test]
fn credit_rate_lookup_is_exact_upstream_model_only() {
    let rates = vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))];
    assert_eq!(
        credit_rate_for_model(&rates, "step-5-preview").map(|rate| rate.input_per_million),
        Some(7.0)
    );
    assert!(credit_rate_for_model(&rates, "step_5_preview").is_none());
    assert!(credit_rate_for_model(&rates, "step-5-preview-alias").is_none());
    assert!(credit_rate_for_model(&rates, "router").is_none());
    assert!(
        CreditMeterState::new(
            "meter-1".into(),
            "cred-1".into(),
            "dest-1".into(),
            "https://api.stepfun.com/v1".into(),
            config(
                None,
                vec![
                    rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0)),
                    rate("step-5-preview", 1.0, Some(1.0), 1.0, Some(1.0)),
                ],
            ),
            Vec::new(),
            at("2026-09-21T00:00:00Z"),
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate rate model")
    );
}

#[test]
fn validate_model_accepts_runtime_upstream_ids() {
    for model in ["anthropic/claude-sonnet-4", "llama3:latest"] {
        CreditMeterState::new(
            "meter-1".into(),
            "cred-1".into(),
            "dest-1".into(),
            "https://api.stepfun.com/v1".into(),
            config(None, vec![rate(model, 7.0, Some(0.35), 20.0, Some(7.0))]),
            Vec::new(),
            at("2026-09-21T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            credit_rate_for_model(&[rate(model, 7.0, Some(0.35), 20.0, Some(7.0))], model)
                .map(|rate| rate.model.as_str()),
            Some(model)
        );
    }
    let reject = |model: &str| {
        CreditMeterState::new(
            "meter-1".into(),
            "cred-1".into(),
            "dest-1".into(),
            "https://api.stepfun.com/v1".into(),
            config(None, vec![rate(model, 7.0, Some(0.35), 20.0, Some(7.0))]),
            Vec::new(),
            at("2026-09-21T00:00:00Z"),
        )
        .unwrap_err()
        .to_string()
        .contains("invalid model name")
    };
    assert!(reject(""));
    assert!(reject("   "));
    assert!(reject("llama3:\nlatest"));
    assert!(reject("llama3:\0latest"));
    assert!(reject(&"a".repeat(201)));
}

#[test]
fn configure_preserves_current_grant_until_next_real_boundary() {
    let now = at("2026-09-21T00:00:00Z");
    let rates = vec![rate("step-5-preview", 7.0, Some(0.35), 20.0, Some(7.0))];
    let mut state = meter(
        vec![bucket(
            "monthly",
            CreditBucketKind::Monthly,
            400.0,
            250.0,
            "2026-09-01T16:00:00Z",
            Some("2026-09-30T16:00:00Z"),
        )],
        Some(monthly(400.0, "2026-09-30T16:00:00Z", 480)),
        rates.clone(),
    );
    assert_eq!(state.last_calibration_at, Some(state.created_at));

    let mut same_rates = state.configuration.clone();
    same_rates.name = "Mini".to_string();
    state.configure(same_rates, now).unwrap();
    assert_eq!(remaining_of(&state, "monthly"), 250.0);
    assert_eq!(state.meter_id, "meter-1");

    let mut moved = state.configuration.clone();
    moved.monthly = Some(monthly(1_600.0, "2026-09-21T00:00:00Z", 480));
    state.configure(moved, now).unwrap();
    state.advance(now).unwrap();
    assert_eq!(remaining_of(&state, "monthly"), 250.0);
    assert_eq!(
        state
            .buckets
            .iter()
            .filter(|bucket| bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, now))
            .count(),
        1
    );
    assert_eq!(state.project(now, 0).remaining, 250.0);

    let next = at("2026-10-21T00:00:00Z");
    state.advance(next).unwrap();
    {
        let active: Vec<_> = state
            .buckets
            .iter()
            .filter(|bucket| {
                bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, next)
            })
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].granted, 1_600.0);
        assert_eq!(active[0].remaining, 1_600.0);
        assert_eq!(active[0].starts_at, next);
    }
    assert_eq!(remaining_of(&state, "monthly"), 250.0);
    assert!(!bucket_active(
        state
            .buckets
            .iter()
            .find(|bucket| bucket.id == "monthly")
            .unwrap(),
        next
    ));

    let mut off = state.configuration.clone();
    off.monthly = None;
    state.configure(off, next).unwrap();
    assert_eq!(active_monthly_remaining(&state, next), 1_600.0);

    let mut on = state.configuration.clone();
    on.monthly = Some(monthly(400.0, "2026-10-21T00:00:00Z", 480));
    state.configure(on, next).unwrap();
    state.advance(next).unwrap();
    assert_eq!(active_monthly_remaining(&state, next), 1_600.0);
    assert_eq!(
        state
            .buckets
            .iter()
            .filter(|bucket| bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, next))
            .count(),
        1
    );

    let later = at("2026-11-21T00:00:00Z");
    state.advance(later).unwrap();
    let granted: Vec<_> = state
        .buckets
        .iter()
        .filter(|bucket| bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, later))
        .cloned()
        .collect();
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].granted, 400.0);
    assert_eq!(granted[0].remaining, 400.0);
}

fn active_monthly_remaining(state: &CreditMeterState, now: DateTime<Utc>) -> f64 {
    state
        .buckets
        .iter()
        .filter(|bucket| bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, now))
        .map(|bucket| bucket.remaining)
        .sum()
}
