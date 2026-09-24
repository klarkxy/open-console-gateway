use super::*;

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE destinations (id TEXT PRIMARY KEY);
         CREATE TABLE credentials (
             id TEXT PRIMARY KEY, destination_id TEXT, routing_rank INTEGER,
             created_at TEXT DEFAULT '', legacy_account_id TEXT DEFAULT '',
             credential_purpose TEXT DEFAULT 'inference', preserved TEXT DEFAULT 'unchanged'
         );
         INSERT INTO destinations VALUES ('a'), ('b');
         INSERT INTO credentials (id,destination_id,routing_rank) VALUES
             ('a1','a',0), ('b1','b',1), ('a2','a',2);",
    )
    .unwrap();
    conn
}

fn card(id: &str, destination: &str, credentials: &[&str]) -> RoutingCard {
    RoutingCard {
        id: id.into(),
        destination_id: destination.into(),
        credential_ids: credentials.iter().map(|id| (*id).into()).collect(),
    }
}

fn ranked(conn: &Connection) -> Vec<String> {
    conn.prepare(
        "SELECT id FROM credentials WHERE credential_purpose = 'inference' ORDER BY routing_rank",
    )
    .unwrap()
    .query_map([], |row| row.get(0))
    .unwrap()
    .collect::<rusqlite::Result<Vec<_>>>()
    .unwrap()
}

#[test]
fn initial_layout_preserves_interleaving_and_get_does_not_write() {
    let conn = connection();
    let first = load_on(&conn).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|row| row.credential_ids.clone())
            .collect::<Vec<_>>(),
        [vec!["a1"], vec!["b1"], vec!["a2"]]
    );
    assert_ne!(first[0].id, first[2].id);
    assert_eq!(load_on(&conn).unwrap(), first);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM settings", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(ranked(&conn), ["a1", "b1", "a2"]);
}

#[test]
fn intentional_adjacent_and_empty_cards_survive_reconciliation() {
    let conn = connection();
    let cards = vec![
        card("a-first", "a", &["a1"]),
        card("empty", "a", &[]),
        card("a-second", "a", &["a2"]),
        card("b", "b", &["b1"]),
    ];
    let tx = conn.unchecked_transaction().unwrap();
    save_on(&tx, &cards).unwrap();
    tx.commit().unwrap();
    assert_eq!(ranked(&conn), ["a1", "a2", "b1"]);
    reconcile_on(&conn).unwrap();
    assert_eq!(load_on(&conn).unwrap(), cards);
}

#[test]
fn external_reorder_splits_one_card_deterministically_without_rank_writes() {
    let conn = connection();
    let cards = vec![card("a", "a", &["a1", "a2"]), card("b", "b", &["b1"])];
    save_on(&conn, &cards).unwrap();
    conn.execute("UPDATE credentials SET routing_rank = CASE id WHEN 'a1' THEN 0 WHEN 'b1' THEN 1 ELSE 2 END", []).unwrap();
    let first = load_on(&conn).unwrap();
    assert_eq!(first[0].id, "a");
    assert_eq!(first[1].id, "b");
    assert_ne!(first[2].id, "a");
    assert_eq!(first[2].credential_ids, ["a2"]);
    assert_eq!(load_on(&conn).unwrap(), first);
    assert_eq!(ranked(&conn), ["a1", "b1", "a2"]);
    reconcile_on(&conn).unwrap();
    assert_eq!(load_on(&conn).unwrap(), first);
}

#[test]
fn new_key_joins_only_a_neighbor_and_deleted_key_leaves_an_empty_card() {
    let conn = connection();
    let cards = vec![card("a", "a", &["a1", "a2"]), card("b", "b", &["b1"])];
    save_on(&conn, &cards).unwrap();
    conn.execute(
        "INSERT INTO credentials (id,destination_id,routing_rank) VALUES ('a3','a',3)",
        [],
    )
    .unwrap();
    let new = load_on(&conn).unwrap();
    assert_eq!(new[0].credential_ids, ["a1", "a2"]);
    assert_eq!(new[2].credential_ids, ["a3"]);
    reconcile_on(&conn).unwrap();
    conn.execute("DELETE FROM credentials WHERE id = 'b1'", [])
        .unwrap();
    let deleted = load_on(&conn).unwrap();
    assert_eq!(deleted[1], card("b", "b", &[]));
    assert_eq!(deleted[0].id, "a");
    assert_ne!(deleted[0].id, deleted[2].id);
    conn.execute("DELETE FROM destinations WHERE id = 'b'", [])
        .unwrap();
    assert!(
        load_on(&conn)
            .unwrap()
            .iter()
            .all(|card| card.destination_id != "b")
    );
}

#[test]
fn new_key_before_an_existing_card_reuses_its_identity() {
    let conn = connection();
    save_on(
        &conn,
        &[card("a", "a", &["a1", "a2"]), card("b", "b", &["b1"])],
    )
    .unwrap();
    conn.execute("UPDATE credentials SET routing_rank = routing_rank + 1", [])
        .unwrap();
    conn.execute(
        "INSERT INTO credentials (id,destination_id,routing_rank) VALUES ('a0','a',0)",
        [],
    )
    .unwrap();
    let cards = load_on(&conn).unwrap();
    assert_eq!(cards[0], card("a", "a", &["a0", "a1", "a2"]));
}

#[test]
fn rejected_layouts_and_transaction_failure_leave_ranks_and_metadata_unchanged() {
    let conn = connection();
    reconcile_on(&conn).unwrap();
    let before = load_on(&conn).unwrap();
    let raw = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTING_KEY],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    for bad in [
        vec![card("x", "a", &["a1", "a2"])],
        vec![card("x", "a", &["a1", "a2"]), card("x", "b", &["b1"])],
        vec![card("x", "a", &["a1", "a2", "a1"]), card("y", "b", &["b1"])],
        vec![card("x", "b", &["a1", "b1"]), card("y", "a", &["a2"])],
    ] {
        assert!(save_on(&conn, &bad).is_err());
        assert_eq!(load_on(&conn).unwrap(), before);
    }
    {
        let tx = conn.unchecked_transaction().unwrap();
        save_on(
            &tx,
            &[card("b", "b", &["b1"]), card("a", "a", &["a2", "a1"])],
        )
        .unwrap();
        // Simulates failure in the caller's runtime preflight before commit.
    }
    assert_eq!(load_on(&conn).unwrap(), before);
    assert_eq!(
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTING_KEY],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        raw
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM credentials WHERE preserved <> 'unchanged'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn observers_are_never_routing_card_members_and_corrupt_json_is_not_rewritten() {
    let conn = connection();
    conn.execute("INSERT INTO credentials (id,destination_id,routing_rank,credential_purpose) VALUES ('observer','a',0,'platform_observer')",[]).unwrap();
    let mut cards = load_on(&conn).unwrap();
    assert_eq!(
        cards
            .iter()
            .map(|card| card.credential_ids.len())
            .sum::<usize>(),
        3
    );
    cards[0].credential_ids.push("observer".into());
    assert!(save_on(&conn, &cards).is_err());
    conn.execute("INSERT INTO settings VALUES (?1, 'corrupt')", [SETTING_KEY])
        .unwrap();
    assert!(load_on(&conn).is_err());
    assert!(reconcile_on(&conn).is_err());
    assert_eq!(
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTING_KEY],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "corrupt"
    );
}

#[test]
fn automatic_cards_cannot_persist_an_unreadable_oversized_layout() {
    let conn = connection();
    conn.execute("INSERT INTO destinations VALUES ('empty')", [])
        .unwrap();
    reconcile_on(&conn).unwrap();
    let before = load_on(&conn).unwrap();
    let mut cards = vec![card("a", "a", &["a2", "a1"]), card("b", "b", &["b1"])];
    cards.extend((2..MAX_CARDS).map(|index| card(&format!("empty-{index}"), "a", &[])));
    {
        let tx = conn.unchecked_transaction().unwrap();
        save_on(&tx, &cards).unwrap();
        // The missing destination requires one more automatic empty card.
        assert!(reconcile_on(&tx).is_err());
    }
    assert_eq!(load_on(&conn).unwrap(), before);
    assert_eq!(ranked(&conn), ["a1", "b1", "a2"]);
}

#[test]
fn restoring_membership_never_reorders_the_merged_credential_sequence() {
    let conn = connection();
    let proposed = vec![
        card("b", "b", &["b1"]),
        card("a", "a", &["a1", "a2"]),
        card("empty", "a", &[]),
    ];
    restore_on(&conn, &proposed).unwrap();
    assert_eq!(ranked(&conn), ["a1", "b1", "a2"]);
    let restored = load_on(&conn).unwrap();
    assert_eq!(
        restored
            .iter()
            .flat_map(|card| card.credential_ids.iter())
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a1", "b1", "a2"]
    );
    assert!(
        restored
            .iter()
            .any(|card| card.id == "empty" && card.credential_ids.is_empty())
    );
    assert_eq!(load_on(&conn).unwrap(), restored);
}
