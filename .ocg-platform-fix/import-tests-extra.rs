
#[test]
fn pagination_advances_even_when_the_first_fifty_tokens_are_disabled() {
    let rows: Vec<Value> = (1..=50).map(|id| json!({"id": id, "status": 2})).collect();
    let data = json!({"items": rows, "total": 100});
    assert!(parse_token_list(&data).iter().all(|token| !token.enabled));
    assert_eq!(next_page(1, token_rows(&data).len(), Some(100)).unwrap(), Some(2));
    assert_eq!(next_page(2, 50, Some(100)).unwrap(), None);
}

#[test]
fn pagination_uses_raw_rows_not_the_number_of_valid_or_enabled_tokens() {
    let rows: Vec<Value> = (1..=50).map(|_| json!({"name": "malformed"})).collect();
    let data = json!({"items": rows, "total": 51});
    assert!(parse_token_list(&data).is_empty());
    assert_eq!(next_page(1, token_rows(&data).len(), Some(51)).unwrap(), Some(2));
}

#[test]
fn pagination_without_a_total_requires_another_page_only_when_full() {
    assert_eq!(next_page(1, 50, None).unwrap(), Some(2));
    assert_eq!(next_page(2, 0, None).unwrap(), None);
    assert_eq!(next_page(2, 7, None).unwrap(), None);
}

#[test]
fn inconsistent_or_unbounded_pages_never_claim_completion() {
    assert!(next_page(0, 50, Some(100)).is_err());
    assert!(next_page(1, 51, Some(100)).is_err());
    assert!(next_page(1, 0, Some(100)).is_err());
    assert!(next_page(MAX_PAGE, 50, None).is_err());
    assert!(next_page(1, 1, Some(-1)).is_err());
}

#[test]
fn remote_token_ids_cannot_change_the_full_key_request_path() {
    assert!(token_id(&json!({"id": "../user/self"})).is_none());
    assert!(token_id(&json!({"id": -1})).is_none());
}
