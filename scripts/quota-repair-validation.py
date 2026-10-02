from pathlib import Path


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    if new in text:
        return
    assert text.count(old) == 1, (path, old[:100], text.count(old))
    p.write_text(text.replace(old, new))


replace('crates/ocg-core/src/dashboard_v3/mod.rs',
        'fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, V3ApiError> {',
        'pub(crate) fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, V3ApiError> {')
replace('crates/ocg-core/src/dashboard_v4/billing.rs',
        'parse_mutation_json::<crate::billing_types::BillingSnapshotRequest>(&body)?',
        'crate::dashboard_v3::parse_json::<crate::billing_types::BillingSnapshotRequest>(&body)?')

p = Path('crates/ocg-core/src/dashboard_v4/billing/tests.rs')
s = p.read_text()
for name in ['cached_reads_see_usage_writes_without_a_settings_revision_and_external_writes',
             'cache_expires_when_a_not_yet_visible_credit_bucket_becomes_active']:
    a = s.index('async fn ' + name)
    b = s.find('\n#[', a)
    if b == -1:
        b = len(s)
    chunk = s[a:b]
    if '    let _ = configure(' not in chunk:
        assert chunk.count('    configure(') == 1
        chunk = chunk.replace('    configure(', '    let _ = configure(')
        s = s[:a] + chunk + s[b:]
p.write_text(s)

# The batch body remains strict and session-authenticated, but is read-only.
p = Path('crates/ocg-core/src/dashboard_v4/billing/tests.rs')
s = p.read_text()
if 'malformed_batch_bodies_do_not_mutate_control_or_billing_state' not in s:
    p.write_text(s + r'''

#[tokio::test]
async fn malformed_batch_bodies_do_not_mutate_control_or_billing_state() {
    let (dir, state) = state();
    let revision = state.settings_revision();
    for input in [r#"null"#, r#"{}"#, r#"{"accountIds":[null]}"#,
                  r#"{"accountIds":[],"unknown":true}"#] {
        let error = snapshots(State(state.clone()), Bytes::from(input.to_string())).await.unwrap_err();
        assert_eq!(error.envelope().code, "invalidJson");
        assert_eq!(state.settings_revision(), revision);
    }
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
''')

print('Applied verified read-only parser and strict batch-body regression fixes.')
