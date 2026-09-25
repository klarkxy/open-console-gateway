use super::*;
use serde_json::json;

fn custom() -> TemporaryRule {
    TemporaryRule {
        id: "operator.credits".into(),
        destination_id: None,
        enabled: true,
        scope: TemporaryRuleScope::CredentialModel,
        initial_seconds: 30,
        max_seconds: 300,
        matcher: TemporaryRuleMatch::HttpError {
            statuses: vec![400, 402],
            fields: vec![
                ErrorFieldMatch {
                    pointer: "/error/code".into(),
                    mode: ErrorFieldMatchMode::Exact,
                    values: vec!["BAD_REQUEST".into(), "NO_CREDIT".into()],
                },
                ErrorFieldMatch {
                    pointer: "/error/message".into(),
                    mode: ErrorFieldMatchMode::Contains,
                    values: vec!["insufficient credits".into()],
                },
            ],
            body_contains: vec![],
        },
    }
}

#[test]
fn sealed_classification_is_reused_without_reinterpreting_other_errors() {
    let rule = builtin_rule();
    assert!(rule.matches(400, ProviderErrorClass::InsufficientCredits, None));
    for status in [200, 401, 413, 429, 500] {
        assert!(!rule.matches(status, ProviderErrorClass::InsufficientCredits, None));
    }
    assert!(!rule.matches(
        400,
        ProviderErrorClass::ClientError,
        Some("insufficient credits")
    ));
}

#[test]
fn fields_are_and_values_are_or_and_contains_is_case_insensitive() {
    let rule = custom();
    let yes = json!({"error":{"code":"NO_CREDIT","message":"You have INSUFFICIENT CREDITS."}})
        .to_string();
    assert!(rule.matches(402, ProviderErrorClass::ClientError, Some(&yes)));
    for body in [
        r#"{"error":{"code":"OTHER","message":"insufficient credits"}}"#,
        r#"{"error":{"code":"NO_CREDIT","message":"invalid model"}}"#,
        r#"{"echo":{"error":{"code":"NO_CREDIT","message":"insufficient credits"}}}"#,
        "not JSON",
    ] {
        assert!(!rule.matches(400, ProviderErrorClass::ClientError, Some(body)));
    }
    assert!(!rule.matches(200, ProviderErrorClass::ClientError, Some(&yes)));
    assert!(!rule.matches(413, ProviderErrorClass::ClientError, Some(&yes)));
    assert!(!rule.matches(400, ProviderErrorClass::ClientError, None));
    let huge = format!("{}{}", " ".repeat(MAX_MATCH_BYTES), yes);
    assert!(!rule.matches(400, ProviderErrorClass::ClientError, Some(&huge)));
}

#[test]
fn raw_body_is_explicit_and_failed_body_reads_cannot_match_text() {
    let mut rule = custom();
    rule.matcher = TemporaryRuleMatch::HttpError {
        statuses: vec![503],
        fields: vec![],
        body_contains: vec!["maintenance".into()],
    };
    assert!(rule.matches(503, ProviderErrorClass::ServerError, Some("MAINTENANCE")));
    assert!(!rule.matches(503, ProviderErrorClass::ServerError, None));
    rule.matcher = TemporaryRuleMatch::HttpError {
        statuses: vec![503],
        fields: vec![],
        body_contains: vec![],
    };
    assert!(rule.matches(503, ProviderErrorClass::ServerError, None));
    rule.enabled = false;
    assert!(!rule.matches(503, ProviderErrorClass::ServerError, None));
}

#[test]
fn validation_bounds_complexity_and_refuses_implicit_scope_or_unsafe_fields() {
    assert!(validate_rules(&[builtin_rule(), custom()]).is_ok());
    assert!(validate_rules(&[]).is_err());
    assert!(validate_rules(&[builtin_rule(), builtin_rule()]).is_err());
    let mut rule = custom();
    rule.initial_seconds = 0;
    assert!(validate_rules(&[builtin_rule(), rule.clone()]).is_err());
    rule.initial_seconds = 30;
    rule.max_seconds = 29;
    assert!(validate_rules(&[builtin_rule(), rule.clone()]).is_err());
    rule.max_seconds = 300;
    rule.matcher = TemporaryRuleMatch::HttpError {
        statuses: vec![200],
        fields: vec![],
        body_contains: vec![],
    };
    assert!(validate_rules(&[builtin_rule(), rule.clone()]).is_err());
    rule.matcher = TemporaryRuleMatch::HttpError {
        statuses: vec![400],
        fields: vec![ErrorFieldMatch {
            pointer: "/request/echo".into(),
            mode: ErrorFieldMatchMode::Contains,
            values: vec!["credits".into()],
        }],
        body_contains: vec![],
    };
    assert!(validate_rules(&[builtin_rule(), rule]).is_err());
    assert!(serde_json::from_value::<TemporaryRule>(json!({"unknown":true})).is_err());
}
