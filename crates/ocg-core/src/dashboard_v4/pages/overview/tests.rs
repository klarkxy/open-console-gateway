use super::*;

fn now() -> DateTime<Utc> {
    "2026-10-07T16:00:00Z".parse().unwrap()
}
fn cooldowns() -> CredentialCooldownsDto {
    CredentialCooldownsDto {
        generic_until: None,
        five_hour_until: None,
        week_until: None,
        month_until: None,
        free_until: None,
    }
}
fn facts(c: &CredentialCooldownsDto) -> AttentionFacts<'_> {
    AttentionFacts {
        ready: true,
        enabled: true,
        auth_error: false,
        has_expiry: true,
        expires_on: "2026-10-06",
        free_only: false,
        cooldowns: c,
    }
}

#[test]
fn attention_prioritizes_auth_then_expiry_then_cooldown_and_respects_disabled_onboarding() {
    let mut c = cooldowns();
    c.week_until = Some((now() + Duration::hours(1)).to_rfc3339());
    let mut f = facts(&c);
    f.auth_error = true;
    assert_eq!(
        attention_reason(&f, now(), 0),
        Some((DashboardAttentionReason::AuthError, None))
    );
    f.auth_error = false;
    assert_eq!(
        attention_reason(&f, now(), 0),
        Some((DashboardAttentionReason::Expired, Some(1)))
    );
    f.expires_on = "2026-10-07";
    assert_eq!(
        attention_reason(&f, now(), 0),
        Some((DashboardAttentionReason::Cooling, None))
    );
    f.enabled = false;
    f.auth_error = true;
    assert_eq!(attention_reason(&f, now(), 0), None);
    f.ready = false;
    assert_eq!(
        attention_reason(&f, now(), 0),
        Some((DashboardAttentionReason::SetupIncomplete, None))
    );
}

#[test]
fn expiry_uses_declared_cadence_valid_dates_and_browser_calendar_boundary() {
    let c = cooldowns();
    let mut f = facts(&c);
    f.expires_on = "2026-10-07";
    assert_eq!(attention_reason(&f, now(), 0), None);
    assert_eq!(
        attention_reason(&f, now(), 480),
        Some((DashboardAttentionReason::Expired, Some(1)))
    );
    assert_eq!(
        attention_reason(&f, now() - Duration::milliseconds(1), 480),
        None
    );
    f.has_expiry = false;
    assert_eq!(attention_reason(&f, now(), 480), None);
    f.has_expiry = true;
    for date in ["", "invalid", "2026-02-30", "2026-2-3"] {
        f.expires_on = date;
        assert_eq!(attention_reason(&f, now(), 480), None, "{date}");
    }
}

#[test]
fn cooldown_excludes_elapsed_invalid_and_unrelated_windows_on_free_only_plans() {
    let mut c = cooldowns();
    c.week_until = Some((now() + Duration::hours(1)).to_rfc3339());
    c.generic_until = Some((now() + Duration::hours(2)).to_rfc3339());
    let check = |c: &CredentialCooldownsDto| {
        attention_reason(
            &AttentionFacts {
                has_expiry: false,
                free_only: true,
                ..facts(c)
            },
            now(),
            0,
        )
    };
    assert_eq!(check(&c), None);
    c.free_until = Some(now().to_rfc3339());
    assert_eq!(check(&c), None);
    c.free_until = Some("invalid".into());
    assert_eq!(check(&c), None);
    c.free_until = Some((now() + Duration::milliseconds(1)).to_rfc3339());
    assert_eq!(check(&c), Some((DashboardAttentionReason::Cooling, None)));
    assert_eq!(
        cooldown_deadline(&facts(&c), now()),
        Some(now() + Duration::hours(2))
    );
}

#[test]
fn observation_expires_at_midnight_or_fifteen_seconds() {
    assert_eq!(
        observation_deadline(now(), 480),
        now() + Duration::seconds(15)
    );
    let before = now() - Duration::milliseconds(1);
    assert_eq!(observation_deadline(before, 480), now());
    let utc_boundary: DateTime<Utc> = "2026-10-08T00:00:00Z".parse().unwrap();
    assert_eq!(
        observation_deadline(utc_boundary - Duration::milliseconds(1), 480),
        utc_boundary
    );
}

#[test]
fn attention_is_bounded_after_priority_sort_and_reports_every_match() {
    let mut items: Vec<_> = (0..70)
        .map(|i| DashboardAttentionItem {
            account_id: format!("{i:03}"),
            account_name: format!("Account {i:03}"),
            reason: DashboardAttentionReason::SetupIncomplete,
            expired_days: None,
        })
        .collect();
    items.push(DashboardAttentionItem {
        account_id: "auth".into(),
        account_name: "Z".into(),
        reason: DashboardAttentionReason::AuthError,
        expired_days: None,
    });
    items.push(DashboardAttentionItem {
        account_id: "expired".into(),
        account_name: "Z".into(),
        reason: DashboardAttentionReason::Expired,
        expired_days: Some(3),
    });
    let (items, total) = bound_attention(items);
    assert_eq!(total, 72);
    assert_eq!(items.len(), ATTENTION_LIMIT);
    assert_eq!(items[0].account_id, "auth");
    assert_eq!(items[1].account_id, "expired");
    assert_eq!(items[2].account_id, "000");
}

#[test]
fn model_totals_aggregate_all_days_sort_ties_and_round_daily_average() {
    let rows: Vec<_> = [
        ("2026-10-06", "b", 20),
        ("2026-10-07", "a", 31),
        ("2026-10-07", "b", 11),
    ]
    .into_iter()
    .map(|(date, model, tokens)| DashboardTokenRow {
        date: date.into(),
        model: model.into(),
        tokens,
    })
    .collect();
    let (models, total, average) = token_totals(&rows);
    assert_eq!(
        models
            .iter()
            .map(|m| (m.model.as_str(), m.tokens))
            .collect::<Vec<_>>(),
        vec![("a", 31), ("b", 31)]
    );
    assert_eq!((total, average), (62, 2));
    assert_eq!(token_totals(&[]).1, 0);
    assert_eq!(token_totals(&[]).2, 0);
}

#[test]
fn series_pads_thirty_utc_days_and_keeps_daily_model_totals_in_descending_order() {
    let rows = vec![
        DashboardTokenRow {
            date: "2026-10-07".into(),
            model: "a".into(),
            tokens: 10,
        },
        DashboardTokenRow {
            date: "2026-10-07".into(),
            model: "b".into(),
            tokens: 20,
        },
        DashboardTokenRow {
            date: "2026-10-07".into(),
            model: "a".into(),
            tokens: 15,
        },
    ];
    let series = chart_series(&rows, now());
    assert_eq!(series.len(), 30);
    assert_eq!(series[0].date, "2026-09-08");
    assert_eq!(series[0].total_tokens, 0);
    assert!(series[0].models.is_empty());
    assert_eq!(series[29].date, "2026-10-07");
    assert_eq!(series[29].total_tokens, 45);
    assert_eq!(
        series[29]
            .models
            .iter()
            .map(|r| (r.model.as_str(), r.tokens))
            .collect::<Vec<_>>(),
        vec![("a", 25), ("b", 20)]
    );
    assert_eq!(
        chart_series(&[], now() + Duration::hours(8))[29].date,
        "2026-10-08"
    );
}
