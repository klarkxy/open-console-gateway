use super::*;

#[test]
fn copilot_discovery_prefers_stable_then_existing_insiders() {
    let root = std::env::temp_dir().join(format!("ocg-copilot-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("Code - Insiders/User")).unwrap();
    assert_eq!(
        copilot_in_config_base(&root).path,
        root.join("Code - Insiders/User/chatLanguageModels.json")
    );
    std::fs::create_dir_all(root.join("Code/User")).unwrap();
    assert_eq!(
        copilot_in_config_base(&root).path,
        root.join("Code/User/chatLanguageModels.json")
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn copilot_target_requires_language_model_file() {
    assert!(constrain_filename(ByokClient::Copilot, Path::new("chatLanguageModels.json")).is_ok());
    for file in [
        "settings.json",
        "keybindings.json",
        "chatLanguageModels.jsonc",
        "config.toml",
    ] {
        assert!(constrain_filename(ByokClient::Copilot, Path::new(file)).is_err());
    }
}
