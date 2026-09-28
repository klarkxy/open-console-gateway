use super::{
    CodexAdapter, ConfigureInput, FormatAdapter, KimiAdapter, MinimaxAdapter, ZcodeAdapter,
    validate_models,
};
use crate::byok_application::{ByokClient, ByokError, ByokErrorKind, ByokModel, ByokResult};
use crate::byok_application_host::fs::MAX_FILE_BYTES;
use crate::byok_application_host::receipt::{ApplyPlan, FileRole, Receipt};
use crate::model_metadata::ModelMetadata;
use std::path::{Path, PathBuf};

const SECRET: &str = "synthetic-secret";

fn model(id: &str) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            context_window: Some(8192),
            max_output_tokens: Some(1024),
            ..ModelMetadata::default()
        },
    }
}

fn input<'a>(models: &'a [ByokModel], default_model_id: Option<&'a str>) -> ConfigureInput<'a> {
    ConfigureInput {
        gateway_v1_url: "http://127.0.0.1:9/v1",
        secret: SECRET,
        models,
        default_model_id,
    }
}

fn carry(plan: &ApplyPlan) -> Receipt {
    Receipt {
        version: 1,
        client: "test".into(),
        target_path: "test".into(),
        identity: "test".into(),
        created_target: plan.created_target,
        created_catalog: plan.created_catalog,
        baseline_default: plan.baseline_default.clone(),
        last_applied_default: plan.last_applied_default.clone(),
        first_owned: plan.first_owned.clone(),
        last_managed: plan.managed.clone(),
        pending: None,
    }
}

fn role_bytes(plan: &ApplyPlan, role: FileRole) -> Option<Vec<u8>> {
    plan.files
        .iter()
        .find(|file| file.role == role)
        .and_then(|file| file.new_bytes.clone())
}

fn assert_conflict(result: ByokResult<ApplyPlan>) {
    let error = result.expect_err("owned or dangling change must conflict");
    assert_eq!(error.kind, ByokErrorKind::Conflict);
    assert!(!error.message.contains(SECRET));
}

fn assert_invalid(error: ByokError) {
    assert_eq!(error.kind, ByokErrorKind::Invalid);
    assert!(!error.message.contains(SECRET));
}

fn codex_paths() -> (PathBuf, PathBuf) {
    (
        PathBuf::from("/tmp/ocg-codex/config.toml"),
        PathBuf::from("/tmp/ocg-codex/.ocg-byok/model_catalog.json"),
    )
}

fn toml(bytes: &[u8]) -> toml_edit::DocumentMut {
    std::str::from_utf8(bytes)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap()
}

fn json_doc(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

fn yaml_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn codex_prepare_leaves_selection_unowned_until_first_activation() {
    let (target, catalog) = codex_paths();
    let original = "\
# keep-me\n\
model = \"A\"\n\
model_provider = \"openai\"\n\
model_catalog_json = \"/catalogs/original.json\"\n\
keep = \"yes\"\n";
    let models = vec![model("a"), model("b"), model("c")];
    let prepared = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(original.as_bytes()),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let doc = toml(role_bytes(&prepared, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        doc.get("model").and_then(toml_edit::Item::as_str),
        Some("A")
    );
    assert_eq!(
        doc.get("model_provider").and_then(toml_edit::Item::as_str),
        Some("openai")
    );
    assert_eq!(
        doc.get("model_catalog_json")
            .and_then(toml_edit::Item::as_str),
        Some("/catalogs/original.json")
    );
    assert!(doc.get("profile").is_none());
    assert!(doc.get("profiles").is_none());
    assert!(
        doc.get("model_providers")
            .and_then(|item| item.get("ocg"))
            .is_some()
    );
    assert!(prepared.first_owned.is_null());
    assert!(prepared.baseline_default.is_none());
    assert!(prepared.last_applied_default.is_none());
    assert!(prepared.managed.owned.get("model_catalog_json").is_none());
    assert!(role_bytes(&prepared, FileRole::Catalog).is_some());
    let text = String::from_utf8(role_bytes(&prepared, FileRole::Target).unwrap()).unwrap();
    assert!(text.contains("keep-me"));
    assert!(text.contains("keep"));

    let mut edited = text.replace("model = \"A\"", "model = \"B\"");
    edited = edited.replace("/catalogs/original.json", "/catalogs/newer.json");
    let receipt = carry(&prepared);
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(edited.as_bytes()),
            role_bytes(&prepared, FileRole::Catalog).as_deref(),
            Some(&receipt),
            input(&models, Some("c")),
        )
        .unwrap();
    assert_eq!(activated.first_owned["captured"].as_bool(), Some(true));
    assert_eq!(activated.first_owned["model"].as_str(), Some("B"));
    assert_eq!(
        activated.first_owned["model_provider"].as_str(),
        Some("openai")
    );
    assert_eq!(
        activated.first_owned["model_catalog_json"].as_str(),
        Some("/catalogs/newer.json")
    );
    assert_eq!(activated.baseline_default.as_deref(), Some("B"));
    assert_eq!(activated.last_applied_default.as_deref(), Some("c"));
    let active = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        active.get("model").and_then(toml_edit::Item::as_str),
        Some("c")
    );
    assert_eq!(
        active
            .get("model_provider")
            .and_then(toml_edit::Item::as_str),
        Some("ocg")
    );
    assert_eq!(
        active
            .get("model_catalog_json")
            .and_then(toml_edit::Item::as_str),
        Some(catalog.to_string_lossy().as_ref())
    );
    let catalog_json = json_doc(
        role_bytes(&activated, FileRole::Catalog)
            .unwrap()
            .as_slice(),
    );
    let plain = &catalog_json["models"][0];
    assert!(plain["default_reasoning_level"].is_null());
    assert_eq!(
        plain["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let active_receipt = carry(&activated);
    let removed = CodexAdapter
        .remove(
            &target,
            Some(&catalog),
            role_bytes(&activated, FileRole::Target).as_deref(),
            role_bytes(&activated, FileRole::Catalog).as_deref(),
            &active_receipt,
        )
        .unwrap();
    let restored = toml(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        restored.get("model").and_then(toml_edit::Item::as_str),
        Some("B")
    );
    assert_eq!(
        restored
            .get("model_provider")
            .and_then(toml_edit::Item::as_str),
        Some("openai")
    );
    assert_eq!(
        restored
            .get("model_catalog_json")
            .and_then(toml_edit::Item::as_str),
        Some("/catalogs/newer.json")
    );
    assert!(
        restored
            .get("model_providers")
            .and_then(|item| item.get("ocg"))
            .is_none()
    );
    assert!(role_bytes(&removed, FileRole::Catalog).is_none());
    assert!(
        String::from_utf8(role_bytes(&removed, FileRole::Target).unwrap())
            .unwrap()
            .contains("keep")
    );
}

#[test]
fn codex_later_none_does_not_reactivate_a_switched_selection() {
    let (target, catalog) = codex_paths();
    let models = vec![model("c"), model("d")];
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(b"model = \"A\"\nmodel_provider = \"openai\"\n"),
            None,
            None,
            input(&models, Some("c")),
        )
        .unwrap();
    let receipt = carry(&activated);
    let held = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            role_bytes(&activated, FileRole::Target).as_deref(),
            role_bytes(&activated, FileRole::Catalog).as_deref(),
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let held_doc = toml(role_bytes(&held, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        held_doc.get("model").and_then(toml_edit::Item::as_str),
        Some("c")
    );
    assert_eq!(held.first_owned, activated.first_owned);
    assert_eq!(held.baseline_default, activated.baseline_default);
    assert_eq!(held.last_applied_default.as_deref(), Some("c"));

    let switched = String::from_utf8(role_bytes(&activated, FileRole::Target).unwrap())
        .unwrap()
        .replace("model = \"c\"", "model = \"gpt-4\"")
        .replace("model_provider = \"ocg\"", "model_provider = \"openai\"");
    let preserved = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(switched.as_bytes()),
            role_bytes(&activated, FileRole::Catalog).as_deref(),
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let doc = toml(role_bytes(&preserved, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        doc.get("model").and_then(toml_edit::Item::as_str),
        Some("gpt-4")
    );
    assert_eq!(
        doc.get("model_provider").and_then(toml_edit::Item::as_str),
        Some("openai")
    );
    assert_eq!(preserved.first_owned["model"].as_str(), Some("A"));
    assert_eq!(preserved.baseline_default.as_deref(), Some("A"));
    assert_ne!(preserved.last_applied_default.as_deref(), Some("gpt-4"));
}

#[test]
fn codex_remove_rejects_switched_ocg_model_and_live_catalog_reference() {
    let (target, catalog) = codex_paths();
    let models = vec![model("c"), model("d")];
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(b"model = \"A\"\nmodel_provider = \"openai\"\n"),
            None,
            None,
            input(&models, Some("c")),
        )
        .unwrap();
    let receipt = carry(&activated);
    let mut switched = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    switched["model"] = toml_edit::value("d");
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(switched.to_string().as_bytes()),
        role_bytes(&activated, FileRole::Catalog).as_deref(),
        &receipt,
    ));

    let mut referenced = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    referenced["model_catalog_json"] = toml_edit::value(".ocg-byok/model_catalog.json");
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(referenced.to_string().as_bytes()),
        role_bytes(&activated, FileRole::Catalog).as_deref(),
        &receipt,
    ));

    let external_catalog = "/elsewhere/.ocg-byok/model_catalog.json";
    referenced["model_catalog_json"] = toml_edit::value(external_catalog);
    let removed = CodexAdapter
        .remove(
            &target,
            Some(&catalog),
            Some(referenced.to_string().as_bytes()),
            role_bytes(&activated, FileRole::Catalog).as_deref(),
            &receipt,
        )
        .unwrap();
    let preserved = toml(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        preserved
            .get("model_catalog_json")
            .and_then(toml_edit::Item::as_str),
        Some(external_catalog)
    );
    assert!(role_bytes(&removed, FileRole::Catalog).is_none());
    assert!(
        removed
            .files
            .iter()
            .all(|file| file.path != Path::new(external_catalog))
    );
}

#[test]
fn codex_owned_provider_leaf_and_catalog_bytes_conflict_before_rewrite() {
    let (target, catalog) = codex_paths();
    let models = vec![model("a")];
    let source = "keep = \"yes\"\n";
    let first = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(source.as_bytes()),
            None,
            None,
            input(&models, Some("a")),
        )
        .unwrap();
    let receipt = carry(&first);
    let again = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            role_bytes(&first, FileRole::Target).as_deref(),
            role_bytes(&first, FileRole::Catalog).as_deref(),
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    assert_eq!(again.first_owned, first.first_owned);
    let mut provider_doc = toml(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    provider_doc["model_providers"]["ocg"]["request_max_retries"] = toml_edit::value(4);
    let edited = provider_doc.to_string();
    assert_conflict(CodexAdapter.configure(
        &target,
        Some(&catalog),
        Some(edited.as_bytes()),
        role_bytes(&first, FileRole::Catalog).as_deref(),
        Some(&receipt),
        input(&models, None),
    ));
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(edited.as_bytes()),
        role_bytes(&first, FileRole::Catalog).as_deref(),
        &receipt,
    ));
    let mut catalog_bytes = role_bytes(&first, FileRole::Catalog).unwrap();
    catalog_bytes.extend_from_slice(b" ");
    assert_conflict(CodexAdapter.configure(
        &target,
        Some(&catalog),
        role_bytes(&first, FileRole::Target).as_deref(),
        Some(&catalog_bytes),
        Some(&receipt),
        input(&models, None),
    ));
    let kept = String::from_utf8(role_bytes(&first, FileRole::Target).unwrap())
        .unwrap()
        .replace("keep = \"yes\"", "keep = \"no\"");
    let updated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(kept.as_bytes()),
            role_bytes(&first, FileRole::Catalog).as_deref(),
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    assert!(
        String::from_utf8(role_bytes(&updated, FileRole::Target).unwrap())
            .unwrap()
            .contains("keep = \"no\"")
    );
    assert_conflict(CodexAdapter.configure(
        &target,
        Some(&catalog),
        None,
        Some(b"{\"models\":[]}"),
        None,
        input(&models, None),
    ));
}

#[test]
fn every_client_rejects_a_dangling_ocg_default() {
    let codex = dangling_case(
        &CodexAdapter,
        None,
        "model = \"external\"\nmodel_provider = \"openai\"\n",
    );
    assert_conflict(codex);
    let kimi = dangling_case(&KimiAdapter, None, "default_model = \"external\"\n");
    assert_conflict(kimi);
    let minimax = dangling_case(
        &MinimaxAdapter,
        None,
        "defaultModel: external\nlogLevel: debug\n",
    );
    assert_conflict(minimax);
    let zcode = dangling_case(&ZcodeAdapter, None, &zcode_shell(None));
    assert_conflict(zcode);
}

fn dangling_case(
    adapter: &dyn FormatAdapter,
    catalog: Option<&Path>,
    original: &str,
) -> ByokResult<ApplyPlan> {
    let target = PathBuf::from("/tmp/ocg-client/config");
    let catalog_path = PathBuf::from("/tmp/ocg-client/.ocg-byok/model_catalog.json");
    let catalog = catalog.or(Some(catalog_path.as_path()));
    let models = vec![model("a"), model("b")];
    let first = adapter.configure(
        &target,
        catalog,
        Some(original.as_bytes()),
        None,
        None,
        input(&models, Some("a")),
    )?;
    let receipt = carry(&first);
    adapter.configure(
        &target,
        catalog,
        role_bytes(&first, FileRole::Target).as_deref(),
        role_bytes(&first, FileRole::Catalog).as_deref(),
        Some(&receipt),
        input(&[model("b")], None),
    )
}

fn zcode_shell(selection: Option<&str>) -> String {
    let selection = selection
        .map(|value| format!(",\"defaultModelSelection\":{value}"))
        .unwrap_or_default();
    format!(
        "{{\"schemaVersion\":1,\"config\":{{\"providerOrder\":[\"keep\"],\"providerConfigRules\":{{\"providerRules\":[]}},\"modelConfigRules\":{{\"providerModelRules\":[],\"manualProviderModelRules\":[]}},\"extraUser\":true{selection}}}}}"
    )
}

#[test]
fn every_client_remove_rejects_a_user_selected_other_ocg_model() {
    assert_conflict(remove_switched_ocg(
        &CodexAdapter,
        "model = \"external\"\nmodel_provider = \"openai\"\n",
        |text| {
            let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
            doc["model"] = toml_edit::value("b");
            doc.to_string()
        },
    ));
    assert_conflict(remove_switched_ocg(
        &KimiAdapter,
        "default_model = \"external\"\n",
        |text| {
            let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
            doc["default_model"] = toml_edit::value("ocg/b");
            doc.to_string()
        },
    ));
    assert_conflict(remove_switched_ocg(
        &MinimaxAdapter,
        "defaultModel: external\n",
        |text| {
            assert!(text.contains("custom_provider:ocg/a"), "{text}");
            text.replace("custom_provider:ocg/a", "custom_provider:ocg/b")
        },
    ));
    assert_conflict(remove_switched_ocg(
        &ZcodeAdapter,
        &zcode_shell(None),
        |text| {
            let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
            value["config"]["defaultModelSelection"]["modelId"] = serde_json::json!("b");
            serde_json::to_string(&value).unwrap()
        },
    ));
}

fn remove_switched_ocg(
    adapter: &dyn FormatAdapter,
    original: &str,
    switch: impl Fn(String) -> String,
) -> ByokResult<ApplyPlan> {
    let target = PathBuf::from("/tmp/ocg-client/config");
    let catalog = PathBuf::from("/tmp/ocg-client/.ocg-byok/model_catalog.json");
    let models = vec![model("a"), model("b")];
    let first = adapter.configure(
        &target,
        Some(&catalog),
        Some(original.as_bytes()),
        None,
        None,
        input(&models, Some("a")),
    )?;
    let receipt = carry(&first);
    let switched =
        switch(String::from_utf8(role_bytes(&first, FileRole::Target).unwrap()).unwrap());
    adapter.remove(
        &target,
        Some(&catalog),
        Some(switched.as_bytes()),
        role_bytes(&first, FileRole::Catalog).as_deref(),
        &receipt,
    )
}

#[test]
fn prepare_then_user_default_then_activation_restores_that_newer_default() {
    let kimi_target = PathBuf::from("/tmp/ocg-kimi/config.toml");
    let models = vec![model("c")];
    let prepared = KimiAdapter
        .configure(
            &kimi_target,
            None,
            Some(b"default_model = \"A\"\nkeep = \"yes\"\n"),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(prepared.baseline_default.is_none());
    assert!(prepared.last_applied_default.is_none());
    assert!(
        String::from_utf8(role_bytes(&prepared, FileRole::Target).unwrap())
            .unwrap()
            .contains("default_model = \"A\"")
    );
    let edited = String::from_utf8(role_bytes(&prepared, FileRole::Target).unwrap())
        .unwrap()
        .replace("default_model = \"A\"", "default_model = \"B\"");
    let receipt = carry(&prepared);
    let activated = KimiAdapter
        .configure(
            &kimi_target,
            None,
            Some(edited.as_bytes()),
            None,
            Some(&receipt),
            input(&models, Some("c")),
        )
        .unwrap();
    assert_eq!(activated.baseline_default.as_deref(), Some("B"));
    assert_eq!(activated.last_applied_default.as_deref(), Some("ocg/c"));
    let active_receipt = carry(&activated);
    let removed = KimiAdapter
        .remove(
            &kimi_target,
            None,
            role_bytes(&activated, FileRole::Target).as_deref(),
            None,
            &active_receipt,
        )
        .unwrap();
    let text = String::from_utf8(role_bytes(&removed, FileRole::Target).unwrap()).unwrap();
    assert!(text.contains("default_model = \"B\""));
    assert!(text.contains("keep"));
    assert!(!text.contains("ocg/c"));

    let mini_target = PathBuf::from("/tmp/ocg-minimax/config.yaml");
    let prepared = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(b"defaultModel: A\nlogLevel: debug\n"),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(prepared.baseline_default.is_none());
    let prepared_text = yaml_text(&role_bytes(&prepared, FileRole::Target).unwrap());
    let edited = if prepared_text.contains("defaultModel: A") {
        prepared_text.replace("defaultModel: A", "defaultModel: B")
    } else if prepared_text.contains("defaultModel: \"A\"") {
        prepared_text.replace("defaultModel: \"A\"", "defaultModel: \"B\"")
    } else {
        panic!("prepared MiniMax default was not kept: {prepared_text}");
    };
    let receipt = carry(&prepared);
    let activated = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(edited.as_bytes()),
            None,
            Some(&receipt),
            input(&models, Some("c")),
        )
        .unwrap();
    assert_eq!(activated.baseline_default.as_deref(), Some("B"));
    let removed = MinimaxAdapter
        .remove(
            &mini_target,
            None,
            role_bytes(&activated, FileRole::Target).as_deref(),
            None,
            &carry(&activated),
        )
        .unwrap();
    let text = yaml_text(&role_bytes(&removed, FileRole::Target).unwrap());
    assert!(text.contains("defaultModel: B") || text.contains("defaultModel: \"B\""));
    assert!(text.contains("logLevel"));
    assert!(!text.contains("custom_provider:ocg/c"));

    let z_target = PathBuf::from("/tmp/ocg-zcode/provider_config.json");
    let original = zcode_shell(Some(r#"{"providerId":"openai","modelId":"A"}"#));
    let prepared = ZcodeAdapter
        .configure(
            &z_target,
            None,
            Some(original.as_bytes()),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(prepared.baseline_default.is_none());
    let mut edited_value = json_doc(role_bytes(&prepared, FileRole::Target).unwrap().as_slice());
    edited_value["config"]["defaultModelSelection"]["modelId"] = serde_json::json!("B");
    let edited = serde_json::to_vec(&edited_value).unwrap();
    let activated = ZcodeAdapter
        .configure(
            &z_target,
            None,
            Some(edited.as_slice()),
            None,
            Some(&carry(&prepared)),
            input(&models, Some("c")),
        )
        .unwrap();
    assert!(
        activated
            .baseline_default
            .as_deref()
            .unwrap()
            .contains("\"modelId\":\"B\"")
    );
    let removed = ZcodeAdapter
        .remove(
            &z_target,
            None,
            role_bytes(&activated, FileRole::Target).as_deref(),
            None,
            &carry(&activated),
        )
        .unwrap();
    let restored = json_doc(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        restored["config"]["defaultModelSelection"]["providerId"],
        "openai"
    );
    assert_eq!(restored["config"]["defaultModelSelection"]["modelId"], "B");
    assert_eq!(restored["config"]["extraUser"], true);
}

#[test]
fn kimi_alias_collision_and_model_leaf_ownership() {
    let target = PathBuf::from("/tmp/ocg-kimi/config.toml");
    let taken = "[models.\"ocg/taken\"]\nprovider = \"other\"\nmodel = \"taken\"\n";
    assert_conflict(KimiAdapter.configure(
        &target,
        None,
        Some(taken.as_bytes()),
        None,
        None,
        input(&[model("taken")], None),
    ));

    let original = "\
[models.moonshot]\n\
provider = \"kimi\"\n\
model = \"moonshot-v1\"\n";
    let models = vec![model("org/model.v1")];
    let first = KimiAdapter
        .configure(
            &target,
            None,
            Some(original.as_bytes()),
            None,
            None,
            input(&models, Some("org/model.v1")),
        )
        .unwrap();
    let receipt = carry(&first);
    KimiAdapter
        .configure(
            &target,
            None,
            role_bytes(&first, FileRole::Target).as_deref(),
            None,
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let mut doc = toml(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    doc["models"]["ocg/org/model.v1"]["temperature"] = toml_edit::value(0.2);
    let edited = doc.to_string();
    assert_conflict(KimiAdapter.configure(
        &target,
        None,
        Some(edited.as_bytes()),
        None,
        Some(&receipt),
        input(&models, None),
    ));
    assert_conflict(KimiAdapter.remove(&target, None, Some(edited.as_bytes()), None, &receipt));
    let unrelated = String::from_utf8(role_bytes(&first, FileRole::Target).unwrap())
        .unwrap()
        .replace("moonshot-v1", "moonshot-v2");
    let updated = KimiAdapter
        .configure(
            &target,
            None,
            Some(unrelated.as_bytes()),
            None,
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let removed = KimiAdapter
        .remove(
            &target,
            None,
            role_bytes(&updated, FileRole::Target).as_deref(),
            None,
            &carry(&updated),
        )
        .unwrap();
    let text = String::from_utf8(role_bytes(&removed, FileRole::Target).unwrap()).unwrap();
    assert!(text.contains("moonshot-v2"));
    assert!(!text.contains("ocg/org/model.v1"));
}

#[test]
fn minimax_provider_leaf_conflicts_and_root_fields_stay() {
    let target = PathBuf::from("/tmp/ocg-minimax/config.yaml");
    let models = vec![model("m1")];
    let first = MinimaxAdapter
        .configure(
            &target,
            None,
            Some(b"logLevel: debug\nprovider:\n  minimax:\n    name: official\n"),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let receipt = carry(&first);
    MinimaxAdapter
        .configure(
            &target,
            None,
            role_bytes(&first, FileRole::Target).as_deref(),
            None,
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let raw = yaml_text(&role_bytes(&first, FileRole::Target).unwrap());
    assert!(raw.contains("official"));
    let logged = raw.replace("logLevel: debug", "logLevel: info");
    let updated = MinimaxAdapter
        .configure(
            &target,
            None,
            Some(logged.as_bytes()),
            None,
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    assert!(yaml_text(&role_bytes(&updated, FileRole::Target).unwrap()).contains("info"));
    assert!(
        raw.contains("context: 8192"),
        "MiniMax limit was not serialized as context: 8192: {raw}"
    );
    let limits = raw.replace("context: 8192", "context: 1000");
    assert_conflict(MinimaxAdapter.configure(
        &target,
        None,
        Some(limits.as_bytes()),
        None,
        Some(&receipt),
        input(&models, None),
    ));
    let mut root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&first, FileRole::Target).unwrap().as_slice())
            .unwrap();
    root["custom_provider"]["ocg"]["region"] = serde_yaml_ng::Value::String("eu".into());
    let dumped = serde_yaml_ng::to_string(&root).unwrap();
    assert_conflict(MinimaxAdapter.configure(
        &target,
        None,
        Some(dumped.as_bytes()),
        None,
        Some(&receipt),
        input(&models, None),
    ));
    assert_conflict(MinimaxAdapter.remove(&target, None, Some(dumped.as_bytes()), None, &receipt));
}

#[test]
fn zcode_sparse_rules_keep_user_data_and_manual_rules() {
    let target = PathBuf::from("/tmp/ocg-zcode/provider_config.json");
    let models = vec![model("z1")];
    let created = ZcodeAdapter
        .configure(&target, None, None, None, None, input(&models, None))
        .unwrap();
    assert!(created.created_target);
    let removed = ZcodeAdapter
        .remove(
            &target,
            None,
            role_bytes(&created, FileRole::Target).as_deref(),
            None,
            &carry(&created),
        )
        .unwrap();
    assert!(role_bytes(&removed, FileRole::Target).is_none());

    let rule = &json_doc(role_bytes(&created, FileRole::Target).unwrap().as_slice())["config"]["modelConfigRules"]
        ["providerModelRules"][0];
    assert!(rule["config"].get("supportsJsonSchemaOutput").is_none());
    assert!(rule["config"]["properties"].get("inputFormat").is_none());
    assert_eq!(rule["config"]["properties"]["contextWindow"], 8192);
    assert_eq!(
        rule["config"]["optionSpecs"]["maxOutputTokens"]["max"],
        1024
    );

    let shell = zcode_shell(None);
    let first = ZcodeAdapter
        .configure(
            &target,
            None,
            Some(shell.as_bytes()),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let receipt = carry(&first);
    ZcodeAdapter
        .configure(
            &target,
            None,
            role_bytes(&first, FileRole::Target).as_deref(),
            None,
            Some(&receipt),
            input(&models, None),
        )
        .unwrap();
    let mut with_note = json_doc(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    with_note["config"]["modelConfigRules"]["providerModelRules"][0]["config"]["note"] =
        serde_json::json!("user");
    let noted = serde_json::to_vec(&with_note).unwrap();
    assert_conflict(ZcodeAdapter.configure(
        &target,
        None,
        Some(&noted),
        None,
        Some(&receipt),
        input(&models, None),
    ));
    let mut order = json_doc(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    order["config"]["providerOrder"]
        .as_array_mut()
        .unwrap()
        .retain(|item| item.as_str() != Some("ocg"));
    assert_conflict(ZcodeAdapter.configure(
        &target,
        None,
        Some(&serde_json::to_vec(&order).unwrap()),
        None,
        Some(&receipt),
        input(&models, None),
    ));

    let mut manual = json_doc(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    manual["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([
        {"providerId": "ocg", "modelId": "manual-ocg", "config": {"enabled": true}},
        {"providerId": "other", "modelId": "manual-other", "config": {"enabled": true}}
    ]);
    manual["config"]["providerConfigRules"]["extra"] = serde_json::json!(1);
    let manual_bytes = serde_json::to_vec(&manual).unwrap();
    let kept = ZcodeAdapter
        .remove(&target, None, Some(&manual_bytes), None, &receipt)
        .unwrap();
    let value = json_doc(role_bytes(&kept, FileRole::Target).unwrap().as_slice());
    assert_eq!(value["config"]["extraUser"], true);
    assert_eq!(value["config"]["providerConfigRules"]["extra"], 1);
    assert!(
        value["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| rule["providerId"] != "ocg")
    );
    let manual_rules = value["config"]["modelConfigRules"]["manualProviderModelRules"]
        .as_array()
        .unwrap();
    assert_eq!(manual_rules.len(), 2);
    assert!(
        manual_rules
            .iter()
            .any(|rule| rule["modelId"] == "manual-ocg")
    );
    assert!(
        manual_rules
            .iter()
            .any(|rule| rule["modelId"] == "manual-other")
    );
    assert!(
        value["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| rule["providerId"] != "ocg")
    );
}

#[test]
fn minimax_default_keeps_the_public_id_after_the_first_slash() {
    let target = PathBuf::from("/tmp/ocg-minimax/slashed.yaml");
    let id = "vendor/model.name";
    let plan = MinimaxAdapter
        .configure(
            &target,
            None,
            Some(b"logLevel: debug\n"),
            None,
            None,
            input(&[model(id)], Some(id)),
        )
        .unwrap();
    let root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&plan, FileRole::Target).unwrap().as_slice()).unwrap();
    assert_eq!(
        root["defaultModel"].as_str(),
        Some("custom_provider:ocg/vendor/model.name")
    );
    assert_eq!(
        plan.last_applied_default.as_deref(),
        Some("custom_provider:ocg/vendor/model.name")
    );
}

#[test]
fn reasoning_wire_values_are_exact_and_deduplicated() {
    let (codex_target, catalog) = codex_paths();
    let mut mapped = model("reasoner");
    mapped.metadata.reasoning = Some(true);
    mapped.metadata.reasoning_efforts = Some(
        [
            ("off".into(), "off".into()),
            ("xhigh".into(), "max".into()),
            ("high".into(), "max".into()),
        ]
        .into_iter()
        .collect(),
    );
    let plan = CodexAdapter
        .configure(
            &codex_target,
            Some(&catalog),
            None,
            None,
            None,
            input(&[mapped], None),
        )
        .unwrap();
    let catalog_json = json_doc(role_bytes(&plan, FileRole::Catalog).unwrap().as_slice());
    let levels = catalog_json["models"][0]["supported_reasoning_levels"]
        .as_array()
        .unwrap();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0]["effort"], "max");
    assert_eq!(levels[0]["description"], "high");
    assert_eq!(levels[1]["effort"], "off");
    assert_eq!(levels[1]["description"], "off");
    assert_eq!(catalog_json["models"][0]["default_reasoning_level"], "max");

    let z_target = PathBuf::from("/tmp/ocg-zcode/wire.json");
    let mut text_only = model("text-only");
    text_only.metadata.input_modalities = Some(vec!["text".into()]);
    text_only.metadata.reasoning_efforts = Some(
        [
            ("high".into(), "max".into()),
            ("max".into(), "max".into()),
            ("xhigh".into(), "max".into()),
        ]
        .into_iter()
        .collect(),
    );
    let mut unknown = model("unknown-image");
    unknown.metadata.input_modalities = None;
    let written = ZcodeAdapter
        .configure(
            &z_target,
            None,
            None,
            None,
            None,
            input(&[text_only, unknown], None),
        )
        .unwrap();
    let rules = &json_doc(role_bytes(&written, FileRole::Target).unwrap().as_slice())["config"]["modelConfigRules"]
        ["providerModelRules"];
    let text_rule = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["modelId"] == "text-only")
        .unwrap();
    assert_eq!(
        text_rule["config"]["optionSpecs"]["reasoningLevel"]["values"],
        serde_json::json!(["max"])
    );
    assert_eq!(
        text_rule["config"]["optionSpecs"]["reasoningLevel"]["map"],
        "{\"reasoning_effort\": reasoningLevel}"
    );
    assert_eq!(
        text_rule["config"]["properties"]["inputFormat"]["supportsImage"],
        false
    );
    let unknown_rule = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["modelId"] == "unknown-image")
        .unwrap();
    assert!(
        unknown_rule["config"]["properties"]
            .get("inputFormat")
            .is_none()
    );
    assert!(
        unknown_rule["config"]["optionSpecs"]
            .get("reasoningLevel")
            .is_none()
    );
}

#[test]
fn codex_catalog_instructions_template_is_present_and_nonempty() {
    let (target, catalog) = codex_paths();
    let plan = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            None,
            None,
            None,
            input(&[model("vendor/model.name")], None),
        )
        .unwrap();
    let entry = &json_doc(role_bytes(&plan, FileRole::Catalog).unwrap().as_slice())["models"][0];
    let template = entry["model_messages"]["instructions_template"]
        .as_str()
        .unwrap();
    assert!(!template.is_empty());
    assert!(entry.get("base_instructions").is_none());
}

#[test]
fn codex_hundred_model_catalog_stays_within_the_native_file_bound() {
    let (target, catalog) = codex_paths();
    let models: Vec<_> = (0..100)
        .map(|index| model(&format!("model-{index:03}")))
        .collect();
    let plan = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let bytes = role_bytes(&plan, FileRole::Catalog).unwrap();
    assert!(bytes.len() as u64 <= MAX_FILE_BYTES);
    let parsed = json_doc(&bytes);
    let entries = parsed["models"].as_array().unwrap();
    assert_eq!(entries.len(), 100);
    assert!(entries.iter().all(|entry| {
        entry["model_messages"]["instructions_template"]
            .as_str()
            .is_some_and(|template| !template.is_empty())
            && entry.get("base_instructions").is_none()
    }));
}

#[test]
fn requested_default_outside_the_selection_is_invalid() {
    let target = PathBuf::from("/tmp/ocg-kimi/config.toml");
    let error = KimiAdapter
        .configure(
            &target,
            None,
            None,
            None,
            None,
            input(&[model("a")], Some("missing")),
        )
        .unwrap_err();
    assert_invalid(error);
}

#[test]
fn codex_keeps_model_text_when_the_provider_was_switched() {
    let (target, catalog) = codex_paths();
    let models = vec![model("c")];
    let original = "\
model = \"A\"\n\
model_provider = \"openai\"\n\
model_catalog_json = \"/catalogs/original.json\"\n";
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(original.as_bytes()),
            None,
            None,
            input(&models, Some("c")),
        )
        .unwrap();
    let mut doc = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    doc["model_provider"] = toml_edit::value("other");
    let removed = CodexAdapter
        .remove(
            &target,
            Some(&catalog),
            Some(doc.to_string().as_bytes()),
            role_bytes(&activated, FileRole::Catalog).as_deref(),
            &carry(&activated),
        )
        .unwrap();
    let restored = toml(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        restored.get("model").and_then(toml_edit::Item::as_str),
        Some("c")
    );
    assert_eq!(
        restored
            .get("model_provider")
            .and_then(toml_edit::Item::as_str),
        Some("other")
    );
    assert_eq!(
        restored
            .get("model_catalog_json")
            .and_then(toml_edit::Item::as_str),
        Some("/catalogs/original.json")
    );
}

#[test]
fn codex_relative_catalog_reference_blocks_removal() {
    let (target, catalog) = codex_paths();
    let models = vec![model("c")];
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(b"model = \"A\"\nmodel_provider = \"openai\"\n"),
            None,
            None,
            input(&models, Some("c")),
        )
        .unwrap();
    let mut doc = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    doc["model_catalog_json"] = toml_edit::value(".ocg-byok/model_catalog.json");
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(doc.to_string().as_bytes()),
        role_bytes(&activated, FileRole::Catalog).as_deref(),
        &carry(&activated),
    ));
}

#[cfg(windows)]
#[test]
fn codex_windows_catalog_case_variant_blocks_removal() {
    let (target, catalog) = codex_paths();
    let models = vec![model("c")];
    let activated = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(b"model = \"A\"\nmodel_provider = \"openai\"\n"),
            None,
            None,
            input(&models, Some("c")),
        )
        .unwrap();
    let mut doc = toml(role_bytes(&activated, FileRole::Target).unwrap().as_slice());
    let applied = doc
        .get("model_catalog_json")
        .and_then(toml_edit::Item::as_str)
        .unwrap()
        .to_string();
    let cased: String = applied
        .chars()
        .map(|ch| match ch {
            'a'..='z' => ch.to_ascii_uppercase(),
            'A'..='Z' => ch.to_ascii_lowercase(),
            other => other,
        })
        .collect();
    assert_ne!(applied, cased);
    doc["model_catalog_json"] = toml_edit::value(&cased);
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(doc.to_string().as_bytes()),
        role_bytes(&activated, FileRole::Catalog).as_deref(),
        &carry(&activated),
    ));
}

#[test]
fn deleted_provider_with_edited_managed_bytes_conflicts() {
    let models = vec![model("a"), model("b")];
    let (target, catalog) = codex_paths();
    let codex = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            Some(b"keep = \"yes\"\n"),
            None,
            None,
            input(&models, Some("a")),
        )
        .unwrap();
    let codex_receipt = carry(&codex);
    let mut codex_doc = toml(role_bytes(&codex, FileRole::Target).unwrap().as_slice());
    codex_doc["model_providers"]
        .as_table_mut()
        .unwrap()
        .remove("ocg");
    let codex_toml = codex_doc.to_string();
    let mut catalog_bytes = role_bytes(&codex, FileRole::Catalog).unwrap();
    catalog_bytes.extend_from_slice(b"\n");
    let codex_status = CodexAdapter.inspect_bytes(
        Some(codex_toml.as_bytes()),
        Some(&catalog_bytes),
        Some(&codex_receipt),
    );
    assert!(codex_status.user_changed_owned);
    assert_conflict(CodexAdapter.configure(
        &target,
        Some(&catalog),
        Some(codex_toml.as_bytes()),
        Some(&catalog_bytes),
        Some(&codex_receipt),
        input(&models, None),
    ));
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        Some(codex_toml.as_bytes()),
        Some(&catalog_bytes),
        &codex_receipt,
    ));
    assert_conflict(CodexAdapter.remove(
        &target,
        Some(&catalog),
        None,
        Some(&catalog_bytes),
        &codex_receipt,
    ));

    let kimi_target = PathBuf::from("/tmp/ocg-kimi/config.toml");
    let kimi = KimiAdapter
        .configure(
            &kimi_target,
            None,
            None,
            None,
            None,
            input(&models, Some("a")),
        )
        .unwrap();
    let kimi_receipt = carry(&kimi);
    let mut kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    kimi_doc["providers"].as_table_mut().unwrap().remove("ocg");
    kimi_doc["models"]["ocg/a"]["temperature"] = toml_edit::value(0.2);
    let kimi_toml = kimi_doc.to_string();
    assert!(
        KimiAdapter
            .inspect_bytes(Some(kimi_toml.as_bytes()), None, Some(&kimi_receipt))
            .user_changed_owned
    );
    assert_conflict(KimiAdapter.configure(
        &kimi_target,
        None,
        Some(kimi_toml.as_bytes()),
        None,
        Some(&kimi_receipt),
        input(&models, None),
    ));
    assert_conflict(KimiAdapter.remove(
        &kimi_target,
        None,
        Some(kimi_toml.as_bytes()),
        None,
        &kimi_receipt,
    ));
    KimiAdapter
        .configure(&kimi_target, None, None, None, None, input(&models, None))
        .unwrap();

    let mini_target = PathBuf::from("/tmp/ocg-minimax/config.yaml");
    let mini = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(b"logLevel: debug\n"),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let mini_receipt = carry(&mini);
    let mut mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    mini_root["custom_provider"]
        .as_mapping_mut()
        .unwrap()
        .remove(serde_yaml_ng::Value::String("ocg".into()));
    let mini_yaml = serde_yaml_ng::to_string(&mini_root).unwrap();
    assert!(
        MinimaxAdapter
            .inspect_bytes(Some(mini_yaml.as_bytes()), None, Some(&mini_receipt))
            .user_changed_owned
    );
    assert_conflict(MinimaxAdapter.configure(
        &mini_target,
        None,
        Some(mini_yaml.as_bytes()),
        None,
        Some(&mini_receipt),
        input(&models, None),
    ));
    assert_conflict(MinimaxAdapter.remove(
        &mini_target,
        None,
        Some(mini_yaml.as_bytes()),
        None,
        &mini_receipt,
    ));

    let z_target = PathBuf::from("/tmp/ocg-zcode/provider_config.json");
    let zcode = ZcodeAdapter
        .configure(
            &z_target,
            None,
            Some(zcode_shell(None).as_bytes()),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let z_receipt = carry(&zcode);
    let mut z_root = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    z_root["config"]["providerConfigRules"]["providerRules"]
        .as_array_mut()
        .unwrap()
        .retain(|rule| rule["providerId"] != "ocg");
    z_root["config"]["modelConfigRules"]["providerModelRules"][0]["config"]["note"] =
        serde_json::json!("edited");
    let z_bytes = serde_json::to_vec(&z_root).unwrap();
    assert!(
        ZcodeAdapter
            .inspect_bytes(Some(&z_bytes), None, Some(&z_receipt))
            .user_changed_owned
    );
    assert_conflict(ZcodeAdapter.configure(
        &z_target,
        None,
        Some(&z_bytes),
        None,
        Some(&z_receipt),
        input(&models, None),
    ));
    assert_conflict(ZcodeAdapter.remove(&z_target, None, Some(&z_bytes), None, &z_receipt));
}

#[test]
fn zcode_extra_ocg_rule_or_order_entry_conflicts() {
    let target = PathBuf::from("/tmp/ocg-zcode/provider_config.json");
    let first = ZcodeAdapter
        .configure(
            &target,
            None,
            Some(zcode_shell(None).as_bytes()),
            None,
            None,
            input(&[model("z1")], None),
        )
        .unwrap();
    let receipt = carry(&first);
    let mut root = json_doc(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    let rule = root["config"]["providerConfigRules"]["providerRules"][0].clone();
    root["config"]["providerConfigRules"]["providerRules"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    root["config"]["providerOrder"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("ocg"));
    let bytes = serde_json::to_vec(&root).unwrap();
    assert!(bytes.windows(4).any(|item| item == b"keep"));
    assert_conflict(ZcodeAdapter.configure(
        &target,
        None,
        Some(&bytes),
        None,
        Some(&receipt),
        input(&[model("z1")], None),
    ));
    assert_conflict(ZcodeAdapter.remove(&target, None, Some(&bytes), None, &receipt));
}

#[test]
fn minimax_preferences_round_trip_from_the_first_activation() {
    let target = PathBuf::from("/tmp/ocg-minimax/prefs.yaml");
    let models = vec![model("c")];
    let prepared = MinimaxAdapter
        .configure(
            &target,
            None,
            Some(b"defaultModel: A\nlogLevel: debug\n"),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(prepared.first_owned.is_null());
    let prepared_text = yaml_text(&role_bytes(&prepared, FileRole::Target).unwrap());
    assert!(prepared_text.contains("logLevel:"), "{prepared_text}");
    let with_preferences =
        format!("{prepared_text}defaultModelThinking: true\ndefaultModelContextWindow: 4096\n");
    let activated = MinimaxAdapter
        .configure(
            &target,
            None,
            Some(with_preferences.as_bytes()),
            None,
            Some(&carry(&prepared)),
            input(&models, Some("c")),
        )
        .unwrap();
    assert_eq!(
        activated.first_owned["defaultModelThinking"].as_bool(),
        Some(true)
    );
    assert_eq!(
        activated.first_owned["defaultModelContextWindow"].as_i64(),
        Some(4096)
    );
    let activated_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&activated, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert!(activated_yaml.get("defaultModelThinking").is_none());
    assert!(activated_yaml.get("defaultModelContextWindow").is_none());
    let updated = MinimaxAdapter
        .configure(
            &target,
            None,
            role_bytes(&activated, FileRole::Target).as_deref(),
            None,
            Some(&carry(&activated)),
            input(&models, None),
        )
        .unwrap();
    assert_eq!(updated.first_owned, activated.first_owned);
    let updated_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&updated, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert!(updated_yaml.get("defaultModelThinking").is_none());
    let removed = MinimaxAdapter
        .remove(
            &target,
            None,
            role_bytes(&updated, FileRole::Target).as_deref(),
            None,
            &carry(&updated),
        )
        .unwrap();
    let restored: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&removed, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert_eq!(restored["defaultModel"].as_str(), Some("A"));
    assert_eq!(restored["defaultModelThinking"].as_bool(), Some(true));
    assert_eq!(restored["defaultModelContextWindow"].as_i64(), Some(4096));
    assert!(
        restored
            .get("logLevel")
            .and_then(serde_yaml_ng::Value::as_str)
            .is_some()
    );

    let activated_text = yaml_text(&role_bytes(&activated, FileRole::Target).unwrap());
    let altered_yaml = format!("{activated_text}defaultModelThinking: false\n");
    let kept = MinimaxAdapter
        .remove(
            &target,
            None,
            Some(altered_yaml.as_bytes()),
            None,
            &carry(&activated),
        )
        .unwrap();
    let kept_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&kept, FileRole::Target).unwrap().as_slice()).unwrap();
    assert_eq!(kept_yaml["defaultModel"].as_str(), Some("A"));
    assert_eq!(kept_yaml["defaultModelThinking"].as_bool(), Some(false));
    assert_eq!(kept_yaml["defaultModelContextWindow"].as_i64(), Some(4096));

    let switched = altered_yaml.replace("custom_provider:ocg/c", "external");
    assert!(switched.contains("defaultModel:"));
    assert!(!switched.contains("custom_provider:ocg/c"));
    let switched_plan = MinimaxAdapter
        .remove(
            &target,
            None,
            Some(switched.as_bytes()),
            None,
            &carry(&activated),
        )
        .unwrap();
    let switched_yaml: serde_yaml_ng::Value = serde_yaml_ng::from_slice(
        role_bytes(&switched_plan, FileRole::Target)
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert_eq!(switched_yaml["defaultModel"].as_str(), Some("external"));
    assert_eq!(switched_yaml["defaultModelThinking"].as_bool(), Some(false));
    assert!(switched_yaml.get("defaultModelContextWindow").is_none());
}

fn unknown_model(id: &str) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata::default(),
    }
}

fn tools_off(id: &str) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            tool_calling: Some(false),
            ..ModelMetadata::default()
        },
    }
}

#[test]
fn validate_models_keeps_id_rules_and_allows_unknown_metadata() {
    let unknown = vec![unknown_model("plain")];
    let denied = vec![tools_off("denied")];
    let many: Vec<_> = (0..101)
        .map(|index| unknown_model(&format!("model-{index:03}")))
        .collect();
    for client in ByokClient::ALL {
        validate_models(client, &[]).unwrap();
        validate_models(client, &unknown).unwrap();
        validate_models(client, &denied).unwrap();
        validate_models(client, &many).unwrap();
        assert_invalid(validate_models(client, &[unknown_model("   ")]).unwrap_err());
        validate_models(client, &[unknown_model(&"模".repeat(100))]).unwrap();
        assert_invalid(
            validate_models(client, &[unknown_model("dup"), unknown_model("dup")]).unwrap_err(),
        );
    }
}

#[test]
fn adapters_export_unknown_limits_and_explicit_false_tools() {
    let unknown = unknown_model("plain");
    let denied = tools_off("denied");
    let models = [unknown, denied];

    let (codex_target, catalog) = codex_paths();
    let codex = CodexAdapter
        .configure(
            &codex_target,
            Some(&catalog),
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let catalog_json = json_doc(role_bytes(&codex, FileRole::Catalog).unwrap().as_slice());
    let entries = catalog_json["models"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["slug"], "plain");
    assert!(entries[0].get("context_window").is_none());
    assert!(entries[0].get("max_context_window").is_none());
    assert_eq!(entries[1]["slug"], "denied");
    assert!(entries[1].get("context_window").is_none());
    assert!(entries[1].get("max_context_window").is_none());

    let kimi_target = PathBuf::from("/tmp/ocg-kimi/unknown.toml");
    let kimi = KimiAdapter
        .configure(&kimi_target, None, None, None, None, input(&models, None))
        .unwrap();
    let kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    let plain = kimi_doc
        .get("models")
        .and_then(|item| item.get("ocg/plain"))
        .and_then(toml_edit::Item::as_table)
        .unwrap();
    assert_eq!(
        plain.get("model").and_then(toml_edit::Item::as_str),
        Some("plain")
    );
    assert!(plain.get("max_context_size").is_none());
    assert!(plain.get("capabilities").is_none());
    let denied_entry = kimi_doc
        .get("models")
        .and_then(|item| item.get("ocg/denied"))
        .and_then(toml_edit::Item::as_table)
        .unwrap();
    assert_eq!(
        denied_entry.get("model").and_then(toml_edit::Item::as_str),
        Some("denied")
    );
    assert!(denied_entry.get("max_context_size").is_none());
    assert_eq!(
        denied_entry
            .get("capabilities")
            .and_then(toml_edit::Item::as_array)
            .map(toml_edit::Array::len),
        Some(0)
    );

    let mini_target = PathBuf::from("/tmp/ocg-minimax/unknown.yaml");
    let mini = MinimaxAdapter
        .configure(&mini_target, None, None, None, None, input(&models, None))
        .unwrap();
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    let plain_yaml = &mini_root["custom_provider"]["ocg"]["models"]["plain"];
    assert!(plain_yaml.get("limit").is_none());
    assert!(plain_yaml.get("tool_call").is_none());
    let denied_yaml = &mini_root["custom_provider"]["ocg"]["models"]["denied"];
    assert!(denied_yaml.get("limit").is_none());
    assert_eq!(denied_yaml["tool_call"], serde_yaml_ng::Value::Bool(false));

    let z_target = PathBuf::from("/tmp/ocg-zcode/unknown.json");
    let zcode = ZcodeAdapter
        .configure(&z_target, None, None, None, None, input(&models, None))
        .unwrap();
    let z_doc = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    let rules = z_doc["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap();
    let plain_rule = rules
        .iter()
        .find(|rule| rule["modelId"] == "plain")
        .unwrap();
    assert!(
        plain_rule["config"]["properties"]
            .get("contextWindow")
            .is_none()
    );
    assert!(
        plain_rule["config"]["properties"]
            .get("supportsToolCall")
            .is_none()
    );
    let denied_rule = rules
        .iter()
        .find(|rule| rule["modelId"] == "denied")
        .unwrap();
    assert!(
        denied_rule["config"]["properties"]
            .get("contextWindow")
            .is_none()
    );
    assert_eq!(
        denied_rule["config"]["properties"]["supportsToolCall"],
        false
    );
}

#[test]
fn adapters_export_more_than_one_hundred_models_without_truncation() {
    let models: Vec<_> = (0..101)
        .map(|index| unknown_model(&format!("model-{index:03}")))
        .collect();
    let expected: Vec<String> = models.iter().map(|model| model.id.clone()).collect();

    let (codex_target, catalog) = codex_paths();
    let codex = CodexAdapter
        .configure(
            &codex_target,
            Some(&catalog),
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert_eq!(codex.managed.model_ids, expected);
    let slugs: Vec<String> =
        json_doc(role_bytes(&codex, FileRole::Catalog).unwrap().as_slice())["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["slug"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(slugs, expected);

    let kimi = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/many.toml"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert_eq!(kimi.managed.model_ids, expected);
    let kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    for id in &expected {
        let alias = format!("ocg/{id}");
        assert_eq!(
            kimi_doc
                .get("models")
                .and_then(|item| item.get(alias.as_str()))
                .and_then(|item| item.get("model"))
                .and_then(toml_edit::Item::as_str),
            Some(id.as_str())
        );
    }

    let mini = MinimaxAdapter
        .configure(
            Path::new("/tmp/ocg-minimax/many.yaml"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert_eq!(mini.managed.model_ids, expected);
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    let mini_models = mini_root["custom_provider"]["ocg"]["models"]
        .as_mapping()
        .unwrap();
    assert_eq!(mini_models.len(), expected.len());
    for id in &expected {
        assert!(
            mini_models.contains_key(&serde_yaml_ng::Value::String(id.clone())),
            "MiniMax catalog omitted {id}"
        );
    }

    let zcode = ZcodeAdapter
        .configure(
            Path::new("/tmp/ocg-zcode/many.json"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert_eq!(zcode.managed.model_ids, expected);
    let z_doc = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    let ids: Vec<String> =
        z_doc["config"]["providerConfigRules"]["providerRules"][0]["config"]["personalModelIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_string())
            .collect();
    assert_eq!(ids, expected);
    let rule_ids: Vec<String> = z_doc["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|rule| rule["providerId"] == "ocg")
        .map(|rule| rule["modelId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rule_ids, expected);
}

#[test]
fn adapters_preserve_exact_full_model_ids() {
    let id = "acme/gpt-4.1-preview:free+fast";
    let models = [unknown_model(id)];

    let (codex_target, catalog) = codex_paths();
    let codex = CodexAdapter
        .configure(
            &codex_target,
            Some(&catalog),
            None,
            None,
            None,
            input(&models, Some(id)),
        )
        .unwrap();
    let catalog_json = json_doc(role_bytes(&codex, FileRole::Catalog).unwrap().as_slice());
    assert_eq!(catalog_json["models"][0]["slug"], id);
    let codex_doc = toml(role_bytes(&codex, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        codex_doc.get("model").and_then(toml_edit::Item::as_str),
        Some(id)
    );

    let kimi = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/full-id.toml"),
            None,
            None,
            None,
            None,
            input(&models, Some(id)),
        )
        .unwrap();
    let kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    let alias = format!("ocg/{id}");
    assert_eq!(
        kimi_doc
            .get("models")
            .and_then(|item| item.get(alias.as_str()))
            .and_then(|item| item.get("model"))
            .and_then(toml_edit::Item::as_str),
        Some(id)
    );
    assert_eq!(
        kimi_doc
            .get("default_model")
            .and_then(toml_edit::Item::as_str),
        Some(alias.as_str())
    );

    let mini = MinimaxAdapter
        .configure(
            Path::new("/tmp/ocg-minimax/full-id.yaml"),
            None,
            None,
            None,
            None,
            input(&models, Some(id)),
        )
        .unwrap();
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    assert!(
        mini_root["custom_provider"]["ocg"]["models"]
            .as_mapping()
            .unwrap()
            .contains_key(&serde_yaml_ng::Value::String(id.into()))
    );
    assert_eq!(
        mini_root["defaultModel"].as_str(),
        Some("custom_provider:ocg/acme/gpt-4.1-preview:free+fast")
    );

    let zcode = ZcodeAdapter
        .configure(
            Path::new("/tmp/ocg-zcode/full-id.json"),
            None,
            None,
            None,
            None,
            input(&models, Some(id)),
        )
        .unwrap();
    let z_doc = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        z_doc["config"]["providerConfigRules"]["providerRules"][0]["config"]["personalModelIds"][0],
        id
    );
    assert_eq!(
        z_doc["config"]["modelConfigRules"]["providerModelRules"][0]["modelId"],
        id
    );
    assert_eq!(z_doc["config"]["defaultModelSelection"]["modelId"], id);
}

#[test]
fn adapters_export_empty_catalogs_without_placeholder_models() {
    let models: [ByokModel; 0] = [];

    let (codex_target, catalog) = codex_paths();
    let codex = CodexAdapter
        .configure(
            &codex_target,
            Some(&catalog),
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(codex.managed.model_ids.is_empty());
    let catalog_json = json_doc(role_bytes(&codex, FileRole::Catalog).unwrap().as_slice());
    assert_eq!(catalog_json["models"].as_array().unwrap().len(), 0);

    let kimi = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/empty.toml"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(kimi.managed.model_ids.is_empty());
    let kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    assert!(
        kimi_doc
            .get("providers")
            .and_then(|item| item.get("ocg"))
            .is_some()
    );
    let kimi_models = kimi_doc
        .get("models")
        .and_then(toml_edit::Item::as_table)
        .map(|table| table.len())
        .unwrap_or(0);
    assert_eq!(kimi_models, 0);

    let mini = MinimaxAdapter
        .configure(
            Path::new("/tmp/ocg-minimax/empty.yaml"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(mini.managed.model_ids.is_empty());
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    let mini_models = mini_root["custom_provider"]["ocg"]["models"]
        .as_mapping()
        .unwrap();
    assert!(mini_models.is_empty());

    let zcode = ZcodeAdapter
        .configure(
            Path::new("/tmp/ocg-zcode/empty.json"),
            None,
            None,
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    assert!(zcode.managed.model_ids.is_empty());
    let z_doc = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        z_doc["config"]["providerConfigRules"]["providerRules"][0]["config"]["personalModelIds"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(
        z_doc["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| rule["providerId"] != "ocg")
    );
}
