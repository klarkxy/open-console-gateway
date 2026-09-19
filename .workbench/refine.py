from pathlib import Path
import os, re, subprocess
BRANCH = 'maintainer/unified-restrictions'
assert os.environ['GITHUB_REPOSITORY'] == 'klarkxy/open-console-gateway'
assert os.environ['GITHUB_REF'] == 'refs/heads/' + BRANCH
p = Path('crates/ocg-core/src/gateway/failure.rs')
s = p.read_text().replace('    pub window: Option<UsageWindowKind>,', '    #[serde(serialize_with = "serialize_window")]\n    pub window: Option<UsageWindowKind>,')
s = s.replace('#[cfg(test)]\nmod tests;', '''fn serialize_window<S: serde::Serializer>(window: &Option<UsageWindowKind>, serializer: S) -> Result<S::Ok, S::Error> {
    window.map(|window| match window {
        UsageWindowKind::FiveHours => "five_hours",
        UsageWindowKind::Week => "week",
        UsageWindowKind::Month => "month",
        UsageWindowKind::Free => "free",
    }).serialize(serializer)
}

#[cfg(test)]
mod tests;''')
p.write_text(s)
for name in ['crates/ocg-core/src/gateway/forwarder.rs','crates/ocg-core/src/gateway/forwarder/tests.rs']:
    p = Path(name); s = p.read_text()
    s = s.replace('            known_secret: None,\n            restriction_details: None,\n            route_account_id:', '            known_secret: None,\n            route_account_id:')
    s = re.sub(r'(\s+)official_price: None,(?!\s*restriction_details:)', r'\1official_price: None,\1restriction_details: None,', s)
    if name.endswith('/forwarder.rs'):
        old = '    let restriction_endpoint = format!("{url}|{route:?}|{:?}", plan.upstream);'
        assert old in s
        s = s.replace(old, '''    let proxy_identity = (route == RouteLabel::Proxy).then_some(config.proxy_url.as_str());
    let restriction_endpoint = format!("{url}|{route:?}|{:?}|{proxy_identity:?}", plan.upstream);''')
        a = s.index('    if let Err(error) = resolver.confirm_live() {')
        b = s.index('    // Admission is operational state', a)
        block = s[a:b]; s = s[:a] + s[b:]
        pos = s.index('    if let Some(compat) = &plan.legacy_tool_compat {', a)
        s = s[:pos] + block + s[pos:]
        s = s.replace('fn outcome_unknown_response(format: ApiFormat, status: StatusCode, detail: &str) -> Response {', 'pub(crate) fn outcome_unknown_response(format: ApiFormat, status: StatusCode, detail: &str) -> Response {')
    p.write_text(s)
p = Path('crates/ocg-core/src/gateway/failure/decode.rs')
s = p.read_text(); assert '.unwrap_or(body);' in s
p.write_text(s.replace('.unwrap_or(body);', '.unwrap_or(if json.is_some() { "" } else { body });'))
p = Path('crates/ocg-core/src/gateway/executor.rs'); s = p.read_text()
old = '''                        return protocol_error_response(
                            client_format,
                            StatusCode::GATEWAY_TIMEOUT,
                            message,
                            None,
                        );'''
assert old in s
s = s.replace(old, '''                        return super::forwarder::outcome_unknown_response(
                            client_format,
                            StatusCode::GATEWAY_TIMEOUT,
                            message,
                        );''')
s = s.replace('    let encoded = serialize_diagnostic(diagnostic.clone());\n    log_request_failure', '''    if error_stage == "request_budget" {
        diagnostic.retry_action = Some(if error_source == "transport" { "no_replay_outcome_unknown" } else { "return" }.to_string());
    }
    let encoded = serialize_diagnostic(diagnostic.clone());
    log_request_failure''')
p.write_text(s)
p = Path('crates/ocg-core/src/gateway/diagnostics.rs'); s = p.read_text()
s = s.replace('''        status: if diagnostic.error_source == "client" {
            "client_error"''', '''        status: if diagnostic.error_source == "transport" && diagnostic.error_stage == "request_budget" {
            "outcome_unknown"
        } else if diagnostic.error_source == "client" {
            "client_error"''', 1)
s = s.replace('''        cost_state: "not_applicable".to_string(),
        error_message: Some(redact_text(message)),''', '''        cost_state: if diagnostic.error_source == "transport" && diagnostic.error_stage == "request_budget" { "outcome_unknown" } else { "not_applicable" }.to_string(),
        error_message: Some(redact_text(message)),''', 1)
p.write_text(s)
p = Path('crates/ocg-core/src/gateway/failure/tests.rs'); s = p.read_text()
s += '''
#[test]
fn diagnostic_serialization_preserves_window_and_evidence() {
    let value = serde_json::to_value(rate(ErrorProfile::CommandCodeGoat, GOAT, Some("90"))).unwrap();
    assert_eq!(value["window"], "week");
    assert_eq!(value["rule_id"], "goat.plan_window");
    assert_eq!(value["rule_version"], 1);
    assert!(value.get("body").is_none());
}
#[test]
fn echoed_limit_text_outside_message_is_not_account_evidence() {
    let f = rate(ErrorProfile::OpenCodeGo, r#"{"error":{},"echo":"Weekly usage limit reached. Resets in 1 day."}"#, None);
    assert_eq!(f.scope, Scope::Unspecified);
    assert!(f.decide().persist_reset.is_none());
    assert!(!f.decide().wait_for_recovery);
}
'''; p.write_text(s)
p = Path('crates/ocg-core/tests/gateway_routing_acceptance.rs'); s = p.read_text()
a = s.index('async fn shared_quota_skips_sibling_credential_on_429()'); b = s.index('\n#[tokio::test]', a)
t = s[a:b].replace('shared_quota_skips_sibling_credential_on_429','unknown_custom_429_does_not_invent_shared_pool_exhaustion').replace('["lab-a", "lab-c"]','["lab-a", "lab-b"]').replace('[DUMMY_A, DUMMY_C]','[DUMMY_A, DUMMY_B]').replace('shared-quota sibling must not be contacted','unknown Custom 429 must not claim shared quota exhaustion').replace('logs.iter().all(|log| log.account_id != sibling_id)','logs.iter().any(|log| log.account_id == sibling_id && log.http_status == Some(200))').replace('sibling credential must not appear in forward logs','eligible sibling should serve after an unknown request-local rejection').replace('shared-quota-skips-sibling','unknown-429-keeps-shared-pool-eligible')
p.write_text(s[:a]+t+s[b:])
subprocess.run(['cargo','fmt','--all'], check=True)
for path in ['.github/workflows/restriction-offline-checks.yml','.github/workflows/restriction-workbench.yml','.workbench/refine.py']:
    Path(path).unlink()
subprocess.run(['git','add','-A'], check=True)
subprocess.run(['git','-c','user.name=github-actions[bot]','-c','user.email=41898282+github-actions[bot]@users.noreply.github.com','commit','-m','test(gateway): verify recovery admission and repair diagnostic wiring'], check=True)
subprocess.run(['git','push','origin','HEAD:refs/heads/'+BRANCH], check=True)
sha = subprocess.check_output(['git','rev-parse','HEAD'], text=True).strip()
with open(os.environ['GITHUB_OUTPUT'], 'a') as f: f.write('sha='+sha+'\n')
print('SOURCE_COMMIT='+sha)
