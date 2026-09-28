use super::Level;
use super::{render_console_block, split_console_lines};

/// A fixed wall clock keeps the rendered shape deterministic.
fn stamp() -> chrono::DateTime<chrono::Local> {
    chrono::Local::now()
        .with_time(chrono::NaiveTime::from_hms_opt(9, 5, 0).unwrap())
        .unwrap()
}

#[test]
fn console_block_stamps_the_message_and_aligns_continuations() {
    let rendered = render_console_block(Level::Warn, &["head", "tail"], stamp());
    assert_eq!(rendered[0], "09:05:00 WARN  head");
    let column = rendered[0].find("head").unwrap();
    assert!(
        rendered[1].starts_with(&" ".repeat(column)),
        "continuations line up under the message column: {rendered:?}"
    );
    assert!(rendered[1].ends_with("tail"));
}

#[test]
fn console_lines_drop_blanks_and_truncate_on_a_character_boundary() {
    let long = "é".repeat(200);
    let lines = split_console_lines(&format!("\n  {long}  \n\n short "));
    assert_eq!(lines[1], "short");
    let truncated = &lines[0];
    assert!(
        truncated.ends_with('…'),
        "an over-long line is marked: {truncated}"
    );
    let body = &truncated[..truncated.len() - '…'.len_utf8()];
    assert!(
        body.len() < 240,
        "the body respects the console bound: {body}"
    );
    assert!(
        long.starts_with(body),
        "truncation keeps a prefix of the message"
    );
}

#[test]
fn severity_order_and_normalization() {
    assert!(Level::Trace < Level::Debug);
    assert!(Level::Debug < Level::Info);
    assert!(Level::Info < Level::Warn);
    assert!(Level::Warn < Level::Error);
    assert_eq!(Level::parse(" WARNING "), Some(Level::Warn));
    assert_eq!(Level::parse("invalid"), None);
}

#[test]
fn persisted_threshold_and_exact_filters_precede_limit() {
    let dir = std::env::temp_dir().join(format!("ocg-level-test-{}", uuid::Uuid::new_v4()));
    let mut db = crate::db::Database::open(dir.clone()).unwrap();
    db.log_level = Level::Warn;
    db.log_gateway("debug", "routing", "skipped").unwrap();
    db.log_gateway_diagnostic(
        "error",
        "request",
        "old-error",
        Some("ocg-match"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    db.log_gateway("warn", "gateway", "new-warning").unwrap();
    assert!(db.log_gateway("invalid", "gateway", "nope").is_err());
    let rows = db
        .query_gateway_logs_filtered(1, Some("ocg-match"), Some("error"), Some("request"))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message, "old-error");
    assert_eq!(db.list_gateway_logs(100).unwrap().len(), 2);
    db.log_level = Level::Trace;
    db.log_gateway("trace", "routing", "visible").unwrap();
    assert_eq!(db.list_gateway_logs(100).unwrap().len(), 3);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
