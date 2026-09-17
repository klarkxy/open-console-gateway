"""Prepare a maintainer-owned merge of PR #60 without changing main or the fork."""
from pathlib import Path
import os
import subprocess

HEAD = os.environ['PR60_HEAD_SHA']
BASE = '7697783d330e701f7e442f07fe0ffe0dcca58cb7'

def git(*args):
    return subprocess.check_output(['git', *args], text=True)

def read(path):
    return Path(path).read_text()

def write(path, text):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(text)

def replace(path, old, new, count=1):
    text = read(path)
    actual = text.count(old)
    if actual != count:
        raise RuntimeError(f'{path}: expected {count} occurrences, got {actual}: {old[:120]!r}')
    write(path, text.replace(old, new))

def extract(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]

def original(path):
    return git('show', f'{HEAD}:{path}')

def apply_pr(paths):
    patch = git('diff', BASE, HEAD, '--', *paths)
    subprocess.run(['git', 'apply', '--3way', '--index'], input=patch, text=True, check=True)

apply_pr([
    'crates/ocg-core/src/provider.rs',
    'crates/ocg-core/src/usage_sync.rs',
    'crates/ocg-core/src/usage_sync/provider_adapter.rs',
    'crates/ocg-domain/src/provider.rs',
    'crates/ocg-domain/src/provider/tests.rs',
    'crates/ocg-core/tests/dashboard_v3_usage_refresh.rs',
    'crates/ocg-core/tests/gateway_fallback.rs',
])
for path in [
    'crates/ocg-core/src/command_code_rate_limit.rs',
    'crates/ocg-core/src/command_code_rate_limit/tests.rs',
    'crates/ocg-core/src/command_code_usage.rs',
    'crates/ocg-core/src/command_code_usage/tests.rs',
    'crates/ocg-core/src/dashboard_v3/command_code_usage_refresh.rs',
    'crates/ocg-core/tests/dashboard_v3_command_code_usage_refresh.rs',
]:
    if Path(path).exists():
        raise RuntimeError(f'Unexpected existing port target: {path}')
    write(path, original(path))
replace('crates/ocg-core/src/lib.rs', 'pub mod browser;\n',
        'pub mod browser;\npub(crate) mod command_code_rate_limit;\npub(crate) mod command_code_usage;\n')
replace('crates/ocg-core/src/dashboard_v3/mod.rs', 'mod connection;\n',
        'mod command_code_usage_refresh;\nmod connection;\n')
replace('crates/ocg-core/src/dashboard_v3/mod.rs', 'pub use crate::official_protocols::OfficialProtocolBaseline;',
        '#[cfg(debug_assertions)]\npub use crate::command_code_usage::{CommandCodeUsageTargetGuard, install_command_code_usage_target_for_tests};\n\npub use crate::official_protocols::OfficialProtocolBaseline;')

# Change only deadline calculation, preserving main's credential/replay policy.
path = 'crates/ocg-core/src/gateway/classify.rs'
replace(path, 'use chrono::Duration;',
        'use chrono::{DateTime, Duration, Utc};\nuse crate::provider::COMMAND_CODE_PROVIDER_ID;')
function = extract(original(path), 'pub(crate) fn rate_limit_window_and_deadline(', 'pub(crate) fn rate_limit_fallback(')
replace(path, 'pub(crate) fn rate_limit_fallback(', function + 'pub(crate) fn rate_limit_fallback(')
test = extract(original(path), '    #[test]\n    fn command_code_plan_window_uses_the_exact_provider_deadline()', '    #[test]\n    fn go_429_free_wording_still_exhausts_the_free_window()')
replace(path, 'mod tests {\n    use super::*;', 'mod tests {\n    use super::*;\n\n' + test)
path = 'crates/ocg-core/src/gateway/forwarder.rs'
replace(path, 'rate_limit_fallback, rate_limit_window_and_cooldown, schedule_go_usage_sync,',
        'rate_limit_fallback, rate_limit_window_and_deadline, schedule_go_usage_sync,')
replace(path, '''                let (window, cooldown) = rate_limit_window_and_cooldown(policy, &text);
                let until = Utc::now() + cooldown;''', '''                let observed_at = Utc::now();
                let (window, until) = rate_limit_window_and_deadline(
                    &account.provider_id, policy, &text, observed_at,
                );
                let cooldown = until.signed_duration_since(observed_at);''')

# Both HTTP shapes delegate to the same GOAT service.
path = 'crates/ocg-core/src/dashboard_v3/usage_refresh.rs'
replace(path, '''    let authorization = UsageSyncCommitAuthorization::control_revision(''', '''    let provider_id = {
        let db = state.db.lock();
        db.get_account(&id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found(&state))?
            .provider_id
    };
    if provider_id == crate::provider::COMMAND_CODE_PROVIDER_ID {
        return super::command_code_usage_refresh::refresh(&state, &id, &input.expectation)
            .await.map(Json);
    }

    let authorization = UsageSyncCommitAuthorization::control_revision(''')
replace(path, 'fn usage_window_from_model(', 'pub(super) fn usage_window_from_model(')
write(path, read(path) + '\n' + extract(original(path), 'impl RefreshApiError {', '\n}') + '\n}\n')
path = 'crates/ocg-core/src/dashboard_v3/usage.rs'
replace(path, '''        ProviderAdapterKind::MiniMaxCn | ProviderAdapterKind::KimiCn => {}''', '''        ProviderAdapterKind::CommandCodeGoat => {
            super::command_code_usage_refresh::refresh(&state, &id, &expectation).await?;
            return provider_usage_locked(&state, &id).map(Json).map_err(RefreshApiError::from);
        }
        ProviderAdapterKind::MiniMaxCn | ProviderAdapterKind::KimiCn => {}''')
replace(path, '''        (
            db.live_local_quota_windows(&account.id, &limits, "command-code-goat-local")
                .map_err(V3ApiError::internal)?,
            None,
        )''', '''        let observed_at = db.account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?.and_then(|sync| sync.last_success_at);
        let mut windows = db.live_local_quota_windows(&account.id, &limits, "command-code-goat-local")
            .map_err(V3ApiError::internal)?;
        for window in &mut windows {
            window.observed_at = observed_at;
        }
        (windows, None)''')

# Current UI uses catalog availability, not a hardcoded provider list.
path = 'crates/ocg-domain/src/provider.rs'
text = read(path)
start = text.index('provider_id: COMMAND_CODE_PROVIDER_ID,', text.index('pub const BUILTIN_PROVIDERS'))
end = text.index('    BuiltinProvider {', start)
block = text[start:end]
assert block.count('usage_availability: "local_state"') == 1
write(path, text[:start] + block.replace('usage_availability: "local_state"', 'usage_availability: "available"') + text[end:])
path = 'crates/ocg-domain/src/provider/tests.rs'
replace(path, 'assert_eq!(goat.usage.contract, UsageContractKind::Authoritative);',
        'assert_eq!(goat.usage.contract, UsageContractKind::Authoritative);\n    assert_eq!(goat.usage.catalog_availability, "available");')

# Sample the injectable clock after reading the body; retain that same instant.
path = 'crates/ocg-core/src/command_code_usage.rs'
replace(path, 'pub struct CommandCodeUsageSnapshot {\n',
        'pub struct CommandCodeUsageSnapshot {\n    pub observed_at: DateTime<Utc>,\n')
replace(path, '    process_generation: u64,\n) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {',
        '    process_generation: u64,\n    now: impl FnOnce() -> DateTime<Utc>,\n) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {')
replace(path, 'fetch_command_code_usage_from(config, api_key, endpoint).await',
        'fetch_command_code_usage_from(config, api_key, endpoint, now).await')
replace(path, '    endpoint: &str,\n) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {',
        '    endpoint: &str,\n    now: impl FnOnce() -> DateTime<Utc>,\n) -> Result<CommandCodeUsageSnapshot, CommandCodeUsageError> {')
replace(path, 'parse_command_code_usage_body(&body, Utc::now())', 'parse_command_code_usage_body(&body, now())')
replace(path, '    Ok(CommandCodeUsageSnapshot {\n', '    Ok(CommandCodeUsageSnapshot {\n        observed_at: now,\n')
path = 'crates/ocg-core/src/command_code_usage/tests.rs'
replace(path, 'fetch_command_code_usage_from(&AppConfig::default(), TEST_KEY, &url)',
        'fetch_command_code_usage_from(&AppConfig::default(), TEST_KEY, &url, Utc::now)', count=2)
path = 'crates/ocg-core/src/dashboard_v3/command_code_usage_refresh.rs'
replace(path, 'fetch_command_code_usage(&config, &key, state.process_generation()).await',
        'fetch_command_code_usage(&config, &key, state.process_generation(), || state.usage_sync.now()).await')
replace(path, 'record_failed_attempt_if_current(state, id, expectation, &account_snapshot, now)?;',
        'record_failed_attempt_if_current(state, id, expectation, &account_snapshot, state.usage_sync.now())?;')
replace(path, '    let next_allowed_at = now + MANUAL_THROTTLE;',
        '    let now = snapshot.observed_at;\n    let next_allowed_at = now + MANUAL_THROTTLE;')

# Keep action placement capability-based, including simultaneous calibration.
path = 'src/components/AccountCard.vue'
replace(path, '<div class="account-actions">',
        '<div class="account-actions" :class="{ \'account-actions--calibratable\': usageRefreshAvailable && manualUsageCalibration }">')
replace(path, '''          v-if="manualUsageCalibration && accountIsReady(account) && edits"
          class="account-action account-action--secondary"''', '''          v-if="manualUsageCalibration && accountIsReady(account) && edits"
          class="account-action account-action--secondary account-action--calibration"''')
replace(path, '</style>', '''.account-actions--calibratable {
  grid-template-columns: repeat(5, 40px);
}
.account-actions--calibratable .account-action--calibration { grid-column: 3; }
.account-actions--calibratable .account-action--test { grid-column: 4; }
.account-actions--calibratable .account-action--menu { grid-column: 5; }
</style>''')

# Add real HTTP regression coverage for the current endpoint and response clock.
path = 'crates/ocg-core/tests/dashboard_v3_command_code_usage_refresh.rs'
text = read(path)
text = text.replace('async fn serve_usage_once(\n', 'async fn serve_usage_controlled(\n', 1)
text = text.replace('    body: Value,\n) -> (String, tokio::task::JoinHandle<Option<String>>) {',
                    '    body: Value,\n    barrier: Option<(std::sync::Arc<tokio::sync::Notify>, std::sync::Arc<tokio::sync::Notify>)>,\n) -> (String, tokio::task::JoinHandle<Option<String>>) {', 1)
needle = '        let response = format!(\n'
assert text.count(needle) == 1
text = text.replace(needle, '''        if let Some((entered, release)) = barrier {
            entered.notify_one();
            release.notified().await;
        }
''' + needle)
text += '''
async fn serve_usage_once(status: u16, body: Value) -> (String, tokio::task::JoinHandle<Option<String>>) {
    serve_usage_controlled(status, body, None).await
}

#[tokio::test]
async fn provider_usage_refresh_uses_response_time_and_shares_legacy_throttle() {
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;
    let harness = start_loopback("goat-provider-refresh-clock").await;
    let account_id = create_goat(&harness).await;
    let started_at = Utc::now();
    let observed_at = started_at + Duration::minutes(2);
    let clock = Arc::new(Mutex::new(started_at));
    let captured_clock = clock.clone();
    harness.state.usage_sync.set_clock_for_test(move || *captured_clock.lock().unwrap());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (url, authorization) = serve_usage_controlled(
        200, usage_body(started_at), Some((entered.clone(), release.clone())),
    ).await;
    let _target = install_command_code_usage_target_for_tests(harness.state.process_generation(), url);
    let client = harness.client.clone();
    let endpoint = format!("{}/accounts/{}/provider-usage", harness.v3_base, account_id);
    let expectation = cas(&harness);
    let pending = tokio::spawn(async move {
        client.post(endpoint).json(&expectation).send().await.unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified()).await.unwrap();
    *clock.lock().unwrap() = observed_at;
    release.notify_one();
    let response = pending.await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body);
    assert_eq!(body["availability"], "available");
    let windows = body["quotaWindows"].as_array().unwrap();
    assert_eq!(windows.len(), 3, "{body}");
    let rolling = windows.iter().find(|window| window["windowKind"] == "five_hours").unwrap();
    let reset = DateTime::parse_from_rfc3339(rolling["resetsAt"].as_str().unwrap()).unwrap().with_timezone(&Utc);
    assert_eq!(reset, started_at + Duration::hours(3));
    assert_eq!(rolling["observedAt"], observed_at.to_rfc3339());
    assert_eq!(body["syncState"]["lastSuccessAt"], observed_at.to_rfc3339());
    assert!((rolling["used"].as_f64().unwrap() - 7.0).abs() < 0.000001);
    assert_no_inference_cooldown(&harness, &account_id);
    assert_eq!(authorization.await.unwrap().as_deref(), Some("Bearer user-goat-refresh-secret"));
    let (status, body) = send_json(&harness, Method::POST, &refresh_path(&account_id), &cas(&harness)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], ERROR_THROTTLED);
    harness.stop();
}
'''
write(path, text)

# Preserve current generated contracts and manually rewritten main paragraphs.
doc_paths = git('diff', '--name-only', BASE, HEAD, '--', 'docs/').splitlines()
unresolved_docs = []
for path in doc_paths:
    current = read(path)
    old = git('show', f'{BASE}:{path}')
    incoming = original(path)
    import difflib
    old_lines, new_lines = old.splitlines(keepends=True), incoming.splitlines(keepends=True)
    for tag, a, b, c, d in difflib.SequenceMatcher(None, old_lines, new_lines, autojunk=False).get_opcodes():
        if tag == 'equal':
            continue
        before, after = ''.join(old_lines[a:b]), ''.join(new_lines[c:d])
        if before and current.count(before) == 1:
            current = current.replace(before, after, 1)
        elif not before:
            context = ''.join(old_lines[max(0, a-2):a])
            if context and current.count(context) == 1:
                current = current.replace(context, context + after, 1)
            else:
                unresolved_docs.append((path, before[:240]))
        elif before.strip() not in current:
            unresolved_docs.append((path, before[:240]))
        else:
            raise RuntimeError(f'Ambiguous documentation context: {path}')
    write(path, current)
print('Documentation contexts superseded by main:', unresolved_docs)
if unresolved_docs:
    raise RuntimeError('Review updated main documentation contexts before publishing')

subprocess.run(['cargo', 'fmt', '--all'], check=True)
subprocess.run(['git', 'add', '-A'], check=True)
subprocess.run(['git', 'diff', '--cached', '--check'], check=True)
subprocess.run(['git', 'commit', '-m', 'feat(goat): integrate PR #60 on current main',
                '-m', 'Maintainer integration of the original GOAT usage and rate-limit contribution. '
                      'Preserve original parents and current pricing; adapt provider-usage routing, '
                      'catalog-driven actions, observation time, and regression coverage.'], check=True)
print('INTEGRATION_COMMIT=' + git('rev-parse', 'HEAD').strip())
print(git('diff', '--stat', 'HEAD^1', 'HEAD'))
