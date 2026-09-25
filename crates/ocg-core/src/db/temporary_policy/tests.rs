use super::*;

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE destinations(id TEXT PRIMARY KEY); INSERT INTO destinations VALUES ('one'),('two');").unwrap();
    conn
}

#[test]
fn defaults_are_read_only_and_rules_roundtrip_with_stable_versions() {
    let conn = db();
    let original = load_on(&conn).unwrap();
    assert_eq!(original.rules.len(), 1);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM settings", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut rules = vec![builtin_rule()];
    let first = save_on(&conn, &rules).unwrap();
    assert_eq!(first.rules[0].revision, 0);
    rules[0].initial_seconds = 60;
    let second = save_on(&conn, &rules).unwrap();
    assert!(second.rules[0].revision > first.rules[0].revision);
    assert_eq!(load_on(&conn).unwrap().rules[0].rule, rules[0]);
    assert_eq!(
        save_on(&conn, &rules).unwrap().rules[0].revision,
        second.rules[0].revision
    );
}

#[test]
fn destination_override_can_disable_global_and_remove_readd_never_reuses_version() {
    let conn = db();
    let global = builtin_rule();
    let mut local = global.clone();
    local.destination_id = Some("one".into());
    local.enabled = false;
    let saved = save_on(&conn, &[global.clone(), local.clone()]).unwrap();
    let old_version = saved.rules[1].revision;
    assert!(saved.effective("one").is_empty());
    assert_eq!(saved.effective("two").len(), 1);
    save_on(&conn, std::slice::from_ref(&global)).unwrap();
    let readded = save_on(&conn, &[global, local]).unwrap();
    assert!(readded.rules[1].revision > old_version);
}

#[test]
fn invalid_write_does_not_replace_existing_configuration() {
    let conn = db();
    let before = save_on(&conn, &[builtin_rule()]).unwrap();
    let mut local = builtin_rule();
    local.destination_id = Some("missing".into());
    assert!(save_on(&conn, &[builtin_rule(), local]).is_err());
    assert_eq!(load_on(&conn).unwrap().rules[0].rule, before.rules[0].rule);
    conn.execute("UPDATE settings SET value='{}'", []).unwrap();
    assert!(
        load_on(&conn).is_err(),
        "corrupt configuration cannot silently enable defaults"
    );
}
