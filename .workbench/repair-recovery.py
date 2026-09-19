"""Apply reviewed PR #73 regression fixes on its isolated branch only."""
from pathlib import Path
import os
import subprocess

BRANCH = 'maintainer/unified-restrictions'
assert os.environ['GITHUB_REPOSITORY'] == 'klarkxy/open-console-gateway'
assert os.environ['GITHUB_REF'] == 'refs/heads/' + BRANCH
assert subprocess.check_output(['git', 'rev-parse', 'HEAD^'], text=True).strip() == '7cd953599562acd786e56bce46f84f2dbafd30d0'

def once(text, old, new):
    assert text.count(old) == 1, (old[:160], text.count(old))
    return text.replace(old, new, 1)

def test_span(text, name):
    start = text.index('#[tokio::test]\nasync fn ' + name + '(')
    end = text.find('\n#[tokio::test]', start + 1)
    return start, len(text) if end < 0 else end

p = Path('crates/ocg-core/src/gateway/executor.rs')
s = p.read_text()
s = once(s, 'ForwardAction, LiveSendSelection, forward_request, rate_limited_response,', 'ForwardAction, LiveSendSelection, forward_request_with_deadline, rate_limited_response,')
s = once(s, '''        // Applies through headers / stream pre-output only. Once a stream is
        // handed off, its existing idle timeout and no-replay rules take over.
        let request_deadline = tokio::time::Instant::now()
            + Duration::from_secs(snapshots.config.non_stream_timeout_secs.max(1));
''', '')
s = once(s, '''        loop {
            let (decision_wall, decision_mono) = state.sample_gateway_clock();''', '''        // A stream has its own pre-output budget; non-stream settings must not
        // truncate it. After handoff, only the existing stream idle timer applies.
        let request_deadline = tokio::time::Instant::now()
            + request_budget_duration(&snapshots.config, requested_plan.stream);
        loop {
            let (decision_wall, decision_mono) = state.sample_gateway_clock();''')
a = s.index('                let forwarded = tokio::time::timeout_at(')
b = s.index('                match forwarded {', a)
s = s[:a] + '''                // The attempt owns timeout finalization so a known HTTP status
                // and the selected account cannot be lost to outer cancellation.
                let forwarded = forward_request_with_deadline(
                    client,
                    route,
                    &state,
                    &account,
                    &snapshots.config,
                    &active_plan,
                    &trace,
                    &client_body,
                    loop_state.attempt,
                    !retried_same_account,
                    headers.clone(),
                    snapshots.pricing.clone(),
                    client_key_id.as_deref(),
                    &snapshots.dynamics,
                    &selection,
                    Some(request_deadline),
                ).await;
''' + s[b:]
s = once(s, 'const MAX_REQUEST_ATTEMPTS: u32 = 32;', '''const MAX_REQUEST_ATTEMPTS: u32 = 32;

fn request_budget_duration(config: &AppConfig, stream: bool) -> Duration {
    Duration::from_secs(if stream {
        config.stream_idle_timeout_secs
    } else {
        config.non_stream_timeout_secs
    }.max(1))
}''')
s = once(s, 'mod tests {\n', '''mod tests {
    #[test]
    fn request_budgets_keep_stream_and_non_stream_settings_independent() {
        let config = crate::models::AppConfig {
            non_stream_timeout_secs: 1,
            stream_idle_timeout_secs: 5,
            ..Default::default()
        };
        assert_eq!(super::request_budget_duration(&config, false).as_secs(), 1);
        assert_eq!(super::request_budget_duration(&config, true).as_secs(), 5);
    }
''')
p.write_text(s)

p = Path('crates/ocg-core/src/gateway/forwarder.rs')
s = p.read_text()
s = once(s, '#[allow(clippy::too_many_arguments)]\npub(crate) async fn forward_request(', '''// Isolated attempt tests do not own a logical-request budget.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_request(''')
s = once(s, '    forward_request_impl(\n', '    forward_request_with_deadline(\n')
s = once(s, '''        dynamics,
        selection,
    )
    .await
}''', '''        dynamics,
        selection,
        None,
    )
    .await
}''')
s = once(s, 'async fn forward_request_impl(', 'pub(crate) async fn forward_request_with_deadline(')
a = s.index('pub(crate) async fn forward_request_with_deadline(')
b = s.index('    let mut attempt_context', a)
head = s[a:b]
head = once(head, '    selection: &LiveSendSelection,\n', '    selection: &LiveSendSelection,\n    request_deadline: Option<tokio::time::Instant>,\n')
s = s[:a] + head + s[b:]
s = once(s, '    let sent = forward_once(\n', '''    let mut timeouts = AttemptTimeouts::from_secs(
        config.non_stream_timeout_secs,
        config.stream_idle_timeout_secs,
    );
    if let Some(deadline) = request_deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let message = "Gateway request deadline exceeded before send; no upstream request sent";
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: "request_budget",
                downstream_status: Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
                upstream_status: None,
                upstream_wait_ms: None,
                retry_action: Some("return"),
                upstream_headers: None,
                upstream_error: None,
                request_body: Some(client_body),
            });
            DbAttemptSink::new(&state.db.lock()).insert(
                account, &model, "error", None,
                metadata_metrics(&pricing_snapshot, plan.service_tier.as_deref(), "not_applicable"),
                Some(message), &attempt_context, Some(failure),
            )?;
            return Ok(ForwardResult {
                response: protocol_status_error_response(plan.client, StatusCode::SERVICE_UNAVAILABLE, message, None),
                action: ForwardAction::Return,
                error_message: Some(message.into()),
            });
        }
        timeouts.non_stream = timeouts.non_stream.min(remaining);
        timeouts.stream_header = timeouts.stream_header.min(remaining);
    }
    let sent = forward_once(
''')
s = once(s, '''        AttemptTimeouts::from_secs(
            config.non_stream_timeout_secs,
            config.stream_idle_timeout_secs,
        ),''', '        timeouts,')
s = once(s, 'upstream did not return response headers within {}s', 'upstream response header timeout after {}s')
s = once(s, '''    let body_timeout = plan
        .stream
        .then(|| StdDuration::from_secs(config.stream_idle_timeout_secs));''', '''    let body_timeout = plan
        .stream
        .then(|| StdDuration::from_secs(config.stream_idle_timeout_secs));
    let body_timeout = match request_deadline {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            Some(body_timeout.map_or(remaining, |timeout| timeout.min(remaining)))
        }
        None => body_timeout,
    };''')
s = once(s, '''            let preflight = tokio::time::timeout(stream_idle_timeout, upstream_stream.next()).await;''', '''            // Heartbeats or partial frames must not restart the logical request
            // budget while no usable downstream output has been produced.
            let read_timeout = request_deadline.map_or(stream_idle_timeout, |deadline| {
                stream_idle_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now()))
            });
            let preflight = tokio::time::timeout(read_timeout, upstream_stream.next()).await;''')
a = s.index('        let (initial_chunks, upstream_finished) = loop {')
b = s.index('        let stream = futures_util::stream::unfold(', a)
part = s[a:b]
part = once(part, '''                    let detail =
                        format!("upstream stream idle timeout after {stream_idle_timeout_secs}s");''', '''                    let budget_expired = request_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline);
                    let detail = if budget_expired {
                        "Gateway request deadline exceeded before stream output (timeout)".to_string()
                    } else {
                        format!("upstream stream idle timeout after {stream_idle_timeout_secs}s")
                    };''')
part = once(part, '''                        "stream",
                        &detail,
                        StreamClassifyInput::IdleTimeoutBeforeOutput,
                        allow_same_account_retry,''', '''                        if budget_expired { "request_budget" } else { "stream" },
                        &detail,
                        StreamClassifyInput::IdleTimeoutBeforeOutput,
                        allow_same_account_retry && !budget_expired,''')
s = s[:a] + part + s[b:]
p.write_text(s)

p = Path('crates/ocg-core/tests/custom_trusted_admin.rs')
s = p.read_text()
a, b = test_span(s, 'custom_429_is_generic_and_does_not_parse_go_windows')
t = s[a:b]
t = once(t, 'custom_429_is_generic_and_does_not_parse_go_windows', 'unknown_custom_429_does_not_invent_cooldown_or_parse_go_windows')
t = once(t, '''                    body: "5-hour usage limit reached. Resets in 13min.",
                },''', '''                    body: "5-hour usage limit reached. Resets in 13min.",
                },
                FakeReply {
                    status: 200,
                    body: SUCCESS_CHAT_BODY,
                },''')
t = once(t, '''        after["cooldownGenericUntil"].as_str().is_some(),
        "Custom 429 must persist a generic cooldown: {after}"''', '''        after["cooldownGenericUntil"].is_null() && after["cooldownUntil"].is_null(),
        "an unknown Custom 429 must not invent account cooldown: {after}"''')
t = once(t, '''    assert_ne!(
        again_status,
        StatusCode::OK,
        "selector must skip the cooling Custom account: {again_body}"
    );''', '''    assert_eq!(
        again_status,
        StatusCode::OK,
        "the next request must still be allowed to use this account: {again_body}"
    );
    assert_eq!(harness.fake_calls().len(), 3, "one verification and two inference calls");
    assert!(after["authError"].is_null());
    assert!(after["lastError"].is_null());''')
p.write_text(s[:a] + t + s[b:])

p = Path('crates/ocg-core/tests/gateway_resource_recovery.rs')
s = p.read_text()
a, b = test_span(s, 'all_waiting_returns_without_resending_and_recovers_on_demand')
t = s[a:b]
t = once(t, '    let h = p.bind().await;', '''    // The ordinary account is only a template for the GOAT fixture; it must
    // not remain a hidden, healthy fallback in an all-resources-waiting test.
    p.state.db.lock().delete_account("acct-1").unwrap();
    let h = p.bind().await;''')
p.write_text(s[:a] + t + s[b:])

p = Path('crates/ocg-core/tests/v3_runtime_invariants.rs')
s = p.read_text()
a, b = test_span(s, 'outer_fallback_resamples_injected_wall_for_cooldown')
t = s[a:b]
t = once(t, '    let wall_calls = Arc::new(AtomicUsize::new(0));\n    let mono_calls = Arc::new(AtomicUsize::new(0));\n', '')
t = once(t, '''            let wall_calls = wall_calls.clone();
            move || {
                wall_calls.fetch_add(1, Ordering::SeqCst);
                *wall.lock().unwrap()
            }''', '''            move || *wall.lock().unwrap()''')
t = once(t, '''        {
            let mono_calls = mono_calls.clone();
            move || {
                mono_calls.fetch_add(1, Ordering::SeqCst);
                t0
            }
        },''', '        move || t0,')
x = t.index('    assert_eq!(\n        wall_calls.load(')
y = t.index('    let mut logs = ', x)
t = t[:x] + '''    // Recovery admission/observation also samples the clock. Assert the
    // externally visible selection at the changed wall instant, not an internal
    // call count that cannot distinguish selection from recovery reads.
''' + t[y:]
s = s[:a] + t + s[b:]
a = s.index('#[tokio::test]\nasync fn same_account_retry_does_not_resample_or_reselect()')
b = s.index('\nasync fn dashboard_json(', a)
s = s[:a] + '''#[tokio::test]
async fn same_account_retry_does_not_reselect_or_advance_round_robin() {
    let frozen = Utc::now();
    let t0 = Instant::now();
    let (state, dir) = go_state_with_keys_and_clock(
        &["key-1", "key-2"], move || frozen, move || t0,
    );
    let mut config = state.config();
    config.upstream_base_url = closed_upstream_url();
    config.connect_timeout_secs = 1;
    config.routing_mode = RoutingMode::RoundRobin;
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;
    for (expected_account, total) in [("acct-1", 2), ("acct-2", 4)] {
        let (status, _) = chat(port, GO_MODEL).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let logs = state.db.lock().list_forward_logs(10).unwrap();
        assert_eq!(logs.len(), total, "{logs:?}");
        let mut current: Vec<_> = logs.iter().filter(|log| log.account_id == expected_account).collect();
        current.sort_by_key(|log| log.attempt);
        assert_eq!(current.len(), 2, "safe retry must stay on the selected account: {logs:?}");
        assert_eq!(current[0].attempt, Some(1));
        assert_eq!(current[1].attempt, Some(2));
        assert_eq!(current[0].request_id, current[1].request_id);
        assert_eq!(current[0].error_stage.as_deref(), Some("connect"));
        assert_eq!(current[1].error_stage.as_deref(), Some("connect"));
    }
    // The second logical request choosing acct-2 proves the first retry did
    // not advance round-robin selection, regardless of recovery clock reads.
    gateway::stop_gateway(gateway_handle);
    let _ = std::fs::remove_dir_all(dir);
}
''' + s[b:]
p.write_text(s)

subprocess.run(['cargo', 'fmt', '--all'], check=True)
subprocess.run(['git', 'diff', '--check'], check=True)
for path in ['.workbench/repair-recovery.py', '.github/workflows/recovery-validation.yml']:
    Path(path).unlink()
subprocess.run(['git', 'add', '-A'], check=True)
subprocess.run(['git', 'diff', '--cached', '--stat'], check=True)
subprocess.run(['git', '-c', 'user.name=github-actions[bot]', '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com', 'commit', '-m', 'fix(gateway): preserve stream budgets and phase-aware timeout evidence'], check=True)
subprocess.run(['git', 'push', 'origin', 'HEAD:refs/heads/' + BRANCH], check=True)
sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
with open(os.environ['GITHUB_OUTPUT'], 'a') as f:
    f.write('sha=' + sha + '\n')
print('SOURCE_COMMIT=' + sha)
