use super::*;

#[test]
fn minimax_application_errors_remain_errors_in_json_and_sse() {
    for (code, expected) in [
        (0, MiniMaxEnvelope::Success),
        (1002, MiniMaxEnvelope::Temporary),
        (1008, MiniMaxEnvelope::OtherError(1008)),
        (2056, MiniMaxEnvelope::OtherError(2056)),
    ] {
        let body = format!(r#"{{"base_resp":{{"status_code":{code}}}}}"#);
        assert_eq!(minimax_envelope(&body), Some(expected));
        assert_eq!(
            minimax_envelope(&format!("data: {body}\n\n")),
            Some(expected)
        );
    }
    assert_eq!(
        minimax_envelope(r#"{"content":"weekly usage limit"}"#),
        None
    );
}
