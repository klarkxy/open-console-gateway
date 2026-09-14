use super::{is_downstream_visible, normalize_public_model_key};
use std::collections::HashSet;

#[test]
fn normalize_trims_and_folds_ascii_case() {
    assert_eq!(
        normalize_public_model_key("  DeepSeek-V4-FlashNH  ").unwrap(),
        "deepseek-v4-flashnh"
    );
}

#[test]
fn normalize_rejects_blank_overlong_and_control_characters() {
    assert!(normalize_public_model_key("   ").is_err());
    assert!(normalize_public_model_key(&"a".repeat(201)).is_err());
    assert!(normalize_public_model_key("ok\u{0007}").is_err());
    assert_eq!(
        normalize_public_model_key(&"a".repeat(200)).unwrap(),
        "a".repeat(200)
    );
}

#[test]
fn hidden_names_are_invisible_after_case_fold() {
    let unpublished = HashSet::from(["deepseek-v4-flashnh".into()]);
    assert!(!is_downstream_visible("DeepSeek-V4-FlashNH", &unpublished));
    assert!(is_downstream_visible("glm-5.1", &unpublished));
}
