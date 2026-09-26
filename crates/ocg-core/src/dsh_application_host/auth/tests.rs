use super::*;
use std::fs;
use std::path::Path;

const FIXTURE_SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn temp_home(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ocg-dsh-grant-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_grant(home: &Path, yaml: &str) {
    fs::write(home.join(".credentials.yaml"), yaml).unwrap();
}

fn valid_document(extra_records: &str) -> String {
    format!(
        "version: 1\nrecords:\n{extra_records}  {GRANT_KEY}:\n    kind: grant\n    payload:\n      version: 1\n      secret: {FIXTURE_SECRET}\n"
    )
}

#[test]
fn reads_only_the_browser_grant_and_redacts_the_secret() {
    let home = temp_home("valid");
    write_grant(
        &home,
        &valid_document("  refs/other:\n    kind: ref\n    payload: {}\n"),
    );
    let grant = read_browser_session_grant(&home).expect("grant");
    let debug = format!("{grant:?}");
    assert_eq!(debug, "BrowserSessionSecret([redacted])");
    assert!(!debug.contains(FIXTURE_SECRET));
    assert_ne!(grant.digest(), [0u8; 32]);
    let origin = DshRuntimeOrigin::parse("http://127.0.0.1:3080").unwrap();
    let cookie = grant.mint_cookie(&origin).expect("cookie");
    assert!(cookie.value.starts_with("v1."));
    assert!(!format!("{cookie:?}").contains(FIXTURE_SECRET));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn missing_file_or_grant_is_missing() {
    let home = temp_home("missing");
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Missing
    );
    write_grant(
        &home,
        "version: 1\nrecords:\n  refs/other:\n    kind: ref\n    payload: {}\n",
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Missing
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn top_level_document_version_must_be_one() {
    let home = temp_home("doc-version");
    write_grant(
        &home,
        &format!(
            "records:\n  {GRANT_KEY}:\n    kind: grant\n    payload:\n      version: 1\n      secret: {FIXTURE_SECRET}\n"
        ),
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Unsupported
    );
    write_grant(
        &home,
        &format!(
            "version: 2\nrecords:\n  {GRANT_KEY}:\n    kind: grant\n    payload:\n      version: 1\n      secret: {FIXTURE_SECRET}\n"
        ),
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Unsupported
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn duplicate_or_unknown_grant_format_is_unsupported() {
    let home = temp_home("bad");
    write_grant(
        &home,
        &format!(
            "version: 1\nrecords:\n  {GRANT_KEY}: {{kind: grant, payload: {{version: 1, secret: {FIXTURE_SECRET}}}}}\n  {GRANT_KEY}: {{kind: grant, payload: {{version: 1, secret: {FIXTURE_SECRET}}}}}\n"
        ),
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Unsupported
    );
    write_grant(
        &home,
        &format!(
            "version: 1\nrecords:\n  {GRANT_KEY}:\n    kind: grant\n    payload:\n      version: 2\n      secret: {FIXTURE_SECRET}\n"
        ),
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Unsupported
    );
    write_grant(
        &home,
        &format!(
            "version: 1\nrecords:\n  {GRANT_KEY}:\n    kind: token\n    payload:\n      version: 1\n      secret: {FIXTURE_SECRET}\n"
        ),
    );
    assert_eq!(
        read_browser_session_grant(&home).unwrap_err(),
        BrowserGrantError::Unsupported
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn grant_error_messages_are_secret_free() {
    for error in [BrowserGrantError::Missing, BrowserGrantError::Unsupported] {
        let message = error.message();
        assert!(!message.contains("secret"));
        assert!(!message.contains("cookie"));
        assert!(!message.contains(FIXTURE_SECRET));
    }
}
