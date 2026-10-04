use super::{
    CodexAdapter, ConfigureInput, FormatAdapter, KimiAdapter, MinimaxAdapter, ZcodeAdapter,
    validate_models,
};
use crate::byok_application::{ByokClient, ByokError, ByokErrorKind, ByokModel, ByokResult};
use crate::byok_application_host::fs::MAX_FILE_BYTES;
use crate::byok_application_host::receipt::{ApplyPlan, FileRole, ManagedSnapshot, Receipt};
use crate::model_metadata::{
    ModelMetadata, PublishedModelProtocolProfile, PublishedUpstreamProtocol,
};
use std::path::{Path, PathBuf};

const SECRET: &str = "synthetic-secret";

fn chat_profile() -> PublishedModelProtocolProfile {
    PublishedModelProtocolProfile {
        preferred: PublishedUpstreamProtocol::ChatCompletions,
        supported: vec![PublishedUpstreamProtocol::ChatCompletions],
    }
}

fn protocols(
    preferred: PublishedUpstreamProtocol,
    supported: &[PublishedUpstreamProtocol],
) -> PublishedModelProtocolProfile {
    PublishedModelProtocolProfile {
        preferred,
        supported: supported.to_vec(),
    }
}

fn model(id: &str) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            context_window: Some(8192),
            max_output_tokens: Some(1024),
            ..ModelMetadata::default()
        },
        protocols: chat_profile(),
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
            assert!(text.contains("custom_provider:ocg-chat/a"), "{text}");
            text.replace("custom_provider:ocg-chat/a", "custom_provider:ocg-chat/b")
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
    assert!(!text.contains("custom_provider:ocg-chat/c"));

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
    root["custom_provider"]["ocg-chat"]["region"] = serde_yaml_ng::Value::String("eu".into());
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
        .retain(|item| {
            item.as_str()
                .is_none_or(|id| !super::is_managed_provider(id))
        });
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
            .all(|rule| {
                rule["providerId"]
                    .as_str()
                    .is_none_or(|id| !super::is_managed_provider(id))
            })
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
            .all(|rule| {
                rule["providerId"]
                    .as_str()
                    .is_none_or(|id| !super::is_managed_provider(id))
            })
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
        Some("custom_provider:ocg-chat/vendor/model.name")
    );
    assert_eq!(
        plan.last_applied_default.as_deref(),
        Some("custom_provider:ocg-chat/vendor/model.name")
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
    kimi_doc["providers"]
        .as_table_mut()
        .unwrap()
        .remove("ocg-chat");
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
        .remove(serde_yaml_ng::Value::String("ocg-chat".into()));
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
        .retain(|rule| {
            rule["providerId"]
                .as_str()
                .is_none_or(|id| !super::is_managed_provider(id))
        });
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

    let switched = altered_yaml.replace("custom_provider:ocg-chat/c", "external");
    assert!(switched.contains("defaultModel:"));
    assert!(!switched.contains("custom_provider:ocg-chat/c"));
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
        protocols: chat_profile(),
    }
}

fn tools_off(id: &str) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            tool_calling: Some(false),
            ..ModelMetadata::default()
        },
        protocols: chat_profile(),
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
    let plain_yaml = &mini_root["custom_provider"]["ocg-chat"]["models"]["plain"];
    assert!(plain_yaml.get("limit").is_none());
    assert!(plain_yaml.get("tool_call").is_none());
    let denied_yaml = &mini_root["custom_provider"]["ocg-chat"]["models"]["denied"];
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
    let mini_models = mini_root["custom_provider"]["ocg-chat"]["models"]
        .as_mapping()
        .unwrap();
    assert_eq!(mini_models.len(), expected.len());
    for id in &expected {
        assert!(
            mini_models.contains_key(serde_yaml_ng::Value::String(id.clone())),
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
        .filter(|rule| rule["providerId"] == "ocg-chat")
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
        mini_root["custom_provider"]["ocg-chat"]["models"]
            .as_mapping()
            .unwrap()
            .contains_key(serde_yaml_ng::Value::String(id.into()))
    );
    assert_eq!(
        mini_root["defaultModel"].as_str(),
        Some("custom_provider:ocg-chat/acme/gpt-4.1-preview:free+fast")
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
    for id in ["ocg", "ocg-chat", "ocg-responses", "ocg-messages"] {
        assert!(
            kimi_doc
                .get("providers")
                .and_then(|item| item.get(id))
                .is_none(),
            "{id}"
        );
    }
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
    if let Some(custom) = mini_root.get("custom_provider") {
        for id in ["ocg", "ocg-chat", "ocg-responses", "ocg-messages"] {
            assert!(yaml_field(custom, id).is_none(), "{id}");
        }
    }

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
    assert!(
        z_doc["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| {
                rule["providerId"]
                    .as_str()
                    .is_none_or(|id| !super::is_managed_provider(id))
            })
    );
    assert!(
        z_doc["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| {
                rule["providerId"]
                    .as_str()
                    .is_none_or(|id| !super::is_managed_provider(id))
            })
    );
}

fn reasoning_model(
    id: &str,
    reasoning: Option<bool>,
    efforts: Option<&[(&str, &str)]>,
) -> ByokModel {
    let mut row = model(id);
    row.metadata.reasoning = reasoning;
    row.metadata.reasoning_efforts = efforts.map(|pairs| {
        pairs
            .iter()
            .map(|(level, wire)| ((*level).to_string(), (*wire).to_string()))
            .collect()
    });
    row
}

fn toml_strings(item: Option<&toml_edit::Item>) -> Option<Vec<String>> {
    item.map(|item| {
        item.as_array()
            .expect("string array")
            .iter()
            .map(|value| value.as_str().expect("string").to_string())
            .collect()
    })
}

fn expect_strings(values: Option<&[&str]>) -> Option<Vec<String>> {
    values.map(|items| items.iter().map(|item| (*item).to_string()).collect())
}

fn assert_kimi_reasoning(
    table: &toml_edit::Table,
    support: Option<&[&str]>,
    off: Option<&str>,
    thinking: bool,
) {
    assert_eq!(
        toml_strings(table.get("support_efforts")),
        expect_strings(support)
    );
    assert_eq!(
        table.get("off_effort").and_then(toml_edit::Item::as_str),
        off
    );
    assert!(table.get("default_effort").is_none());
    assert_eq!(
        toml_strings(table.get("capabilities")),
        thinking.then(|| vec!["thinking".to_string()])
    );
}

fn kimi_table<'a>(doc: &'a toml_edit::DocumentMut, id: &str) -> &'a toml_edit::Table {
    let alias = format!("ocg/{id}");
    doc.get("models")
        .and_then(|item| item.get(alias.as_str()))
        .and_then(toml_edit::Item::as_table)
        .unwrap_or_else(|| panic!("missing Kimi model {id}"))
}

fn yaml_field<'a>(value: &'a serde_yaml_ng::Value, key: &str) -> Option<&'a serde_yaml_ng::Value> {
    value
        .as_mapping()
        .and_then(|mapping| mapping.get(serde_yaml_ng::Value::String(key.into())))
}

fn yaml_strings(value: &serde_yaml_ng::Value) -> Vec<String> {
    value
        .as_sequence()
        .expect("sequence")
        .iter()
        .map(|item| item.as_str().expect("string").to_string())
        .collect()
}

fn assert_minimax_reasoning(
    model_value: &serde_yaml_ng::Value,
    options: Option<&[&str]>,
    reasoning: bool,
) {
    assert_eq!(
        yaml_field(model_value, "reasoning").and_then(serde_yaml_ng::Value::as_bool),
        reasoning.then_some(true)
    );
    assert!(yaml_field(model_value, "thinking_config").is_none());
    assert!(yaml_field(model_value, "defaultVariant").is_none());
    match options {
        None => assert!(yaml_field(model_value, "thinking").is_none()),
        Some(expected) => {
            let thinking = yaml_field(model_value, "thinking").expect("thinking menu");
            assert_eq!(
                yaml_strings(yaml_field(thinking, "effortOptions").expect("effort options")),
                expected
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect::<Vec<_>>()
            );
            assert_eq!(thinking.as_mapping().expect("thinking mapping").len(), 1);
        }
    }
}

fn minimax_model<'a>(root: &'a serde_yaml_ng::Value, id: &str) -> &'a serde_yaml_ng::Value {
    yaml_field(
        yaml_field(yaml_field(root, "custom_provider").unwrap(), "ocg-chat").unwrap(),
        "models",
    )
    .unwrap()
    .as_mapping()
    .unwrap()
    .get(serde_yaml_ng::Value::String(id.into()))
    .unwrap_or_else(|| panic!("missing MiniMax model {id}"))
}

#[test]
fn kimi_and_minimax_export_declared_reasoning_choices() {
    let mapped = &[
        ("low", "low"),
        ("high", "high"),
        ("xhigh", "max"),
        ("max", "max"),
        ("off", "none"),
    ];
    let literal_off = &[("high", "high"), ("off", "off")];
    let levels_only = &[("low", "low"), ("high", "high")];
    let aliases = &[
        ("high", "high"),
        ("low", "OFF"),
        ("max", "None"),
        ("minimal", "none"),
        ("xhigh", " none "),
    ];
    let models = vec![
        reasoning_model("reasoner", Some(true), Some(mapped)),
        reasoning_model("literal-off", Some(true), Some(literal_off)),
        reasoning_model("aliases", Some(true), Some(aliases)),
        reasoning_model("levels-only", None, Some(levels_only)),
        reasoning_model("bare", Some(true), None),
        reasoning_model("bare-empty", Some(true), Some(&[] as &[(&str, &str)])),
        reasoning_model("missing", None, None),
        reasoning_model("empty", None, Some(&[] as &[(&str, &str)])),
        reasoning_model("disabled", Some(false), Some(mapped)),
        reasoning_model("retired", Some(true), Some(mapped)),
    ];
    let shaped = &[
        ("high", "MAX_CUSTOM"),
        ("low", "max_custom"),
        ("xhigh", "on"),
        ("max", "off"),
        ("minimal", "wire_custom"),
        ("off", "CUSTOM_OFF"),
    ];
    let mut kimi_models = models.clone();
    kimi_models.push(reasoning_model("shaped", Some(true), Some(shaped)));

    let kimi_target = PathBuf::from("/tmp/ocg-kimi/reasoning.toml");
    let kimi_original = "\
keep = \"yes\"\n\
\n\
[models.moonshot]\n\
provider = \"kimi\"\n\
model = \"moonshot-v1\"\n\
note = \"user\"\n";
    let kimi = KimiAdapter
        .configure(
            &kimi_target,
            None,
            Some(kimi_original.as_bytes()),
            None,
            None,
            input(&kimi_models, None),
        )
        .unwrap();
    let kimi_bytes = role_bytes(&kimi, FileRole::Target).unwrap();
    let kimi_doc = toml(kimi_bytes.as_slice());
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "reasoner"),
        Some(&["high", "low", "max"][..]),
        Some("none"),
        true,
    );
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "literal-off"),
        Some(&["high"][..]),
        Some("off"),
        true,
    );
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "aliases"),
        Some(&["high", "none"][..]),
        None,
        true,
    );
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "shaped"),
        Some(&["max_custom", "wire_custom"][..]),
        Some("CUSTOM_OFF"),
        true,
    );
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "levels-only"),
        Some(&["high", "low"][..]),
        None,
        false,
    );
    for id in ["bare", "bare-empty"] {
        assert_kimi_reasoning(kimi_table(&kimi_doc, id), None, None, true);
    }
    for id in ["missing", "empty", "disabled"] {
        assert_kimi_reasoning(kimi_table(&kimi_doc, id), None, None, false);
    }
    assert_eq!(
        kimi_doc
            .get("models")
            .and_then(|item| item.get("moonshot"))
            .and_then(|item| item.get("note"))
            .and_then(toml_edit::Item::as_str),
        Some("user")
    );
    assert_eq!(
        kimi_doc.get("keep").and_then(toml_edit::Item::as_str),
        Some("yes")
    );

    let kimi_receipt = carry(&kimi);
    let mut tier_edit = kimi_doc.clone();
    let mut replacement = toml_edit::Array::new();
    replacement.push("low");
    tier_edit["models"]["ocg/reasoner"]["support_efforts"] =
        toml_edit::Item::Value(replacement.into());
    let tier_edit = tier_edit.to_string();
    assert_conflict(KimiAdapter.configure(
        &kimi_target,
        None,
        Some(tier_edit.as_bytes()),
        None,
        Some(&kimi_receipt),
        input(&kimi_models, None),
    ));
    assert_conflict(KimiAdapter.remove(
        &kimi_target,
        None,
        Some(tier_edit.as_bytes()),
        None,
        &kimi_receipt,
    ));

    let mut unrelated = kimi_doc.clone();
    unrelated["models"]["moonshot"]["model"] = toml_edit::value("moonshot-v2");
    let unrelated = unrelated.to_string();
    let preserved = KimiAdapter
        .configure(
            &kimi_target,
            None,
            Some(unrelated.as_bytes()),
            None,
            Some(&kimi_receipt),
            input(&kimi_models, None),
        )
        .unwrap();
    let preserved_doc = toml(role_bytes(&preserved, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        preserved_doc
            .get("models")
            .and_then(|item| item.get("moonshot"))
            .and_then(|item| item.get("model"))
            .and_then(toml_edit::Item::as_str),
        Some("moonshot-v2")
    );
    assert_kimi_reasoning(
        kimi_table(&preserved_doc, "reasoner"),
        Some(&["high", "low", "max"][..]),
        Some("none"),
        true,
    );

    let cleared = vec![
        reasoning_model("reasoner", Some(true), None),
        reasoning_model("literal-off", Some(false), Some(mapped)),
        reasoning_model("aliases", Some(true), Some(aliases)),
        reasoning_model("levels-only", None, Some(levels_only)),
        reasoning_model("bare", Some(true), None),
        reasoning_model("bare-empty", Some(true), Some(&[] as &[(&str, &str)])),
        reasoning_model("missing", None, None),
        reasoning_model("empty", None, Some(&[] as &[(&str, &str)])),
        reasoning_model("disabled", Some(false), Some(mapped)),
    ];
    let shaped_kept = &[("low", "wire_custom")];
    let mut kimi_cleared = cleared.clone();
    kimi_cleared.push(reasoning_model("shaped", Some(true), Some(shaped_kept)));
    let reconfigured = KimiAdapter
        .configure(
            &kimi_target,
            None,
            Some(kimi_bytes.as_slice()),
            None,
            Some(&kimi_receipt),
            input(&kimi_cleared, None),
        )
        .unwrap();
    let reconfigured_doc = toml(
        role_bytes(&reconfigured, FileRole::Target)
            .unwrap()
            .as_slice(),
    );
    assert_kimi_reasoning(kimi_table(&reconfigured_doc, "reasoner"), None, None, true);
    assert_kimi_reasoning(
        kimi_table(&reconfigured_doc, "literal-off"),
        None,
        None,
        false,
    );
    assert_kimi_reasoning(
        kimi_table(&reconfigured_doc, "levels-only"),
        Some(&["high", "low"][..]),
        None,
        false,
    );
    assert_kimi_reasoning(
        kimi_table(&reconfigured_doc, "aliases"),
        Some(&["high", "none"][..]),
        None,
        true,
    );
    assert_kimi_reasoning(
        kimi_table(&reconfigured_doc, "shaped"),
        Some(&["wire_custom"][..]),
        None,
        true,
    );
    assert!(
        reconfigured_doc
            .get("models")
            .and_then(|item| item.get("ocg/retired"))
            .is_none()
    );
    assert_eq!(
        reconfigured_doc
            .get("models")
            .and_then(|item| item.get("moonshot"))
            .and_then(|item| item.get("note"))
            .and_then(toml_edit::Item::as_str),
        Some("user")
    );

    let removed = KimiAdapter
        .remove(
            &kimi_target,
            None,
            Some(kimi_bytes.as_slice()),
            None,
            &kimi_receipt,
        )
        .unwrap();
    let removed_doc = toml(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert!(
        removed_doc
            .get("models")
            .and_then(toml_edit::Item::as_table)
            .unwrap()
            .get("ocg/reasoner")
            .is_none()
    );
    assert_eq!(
        removed_doc
            .get("models")
            .and_then(|item| item.get("moonshot"))
            .and_then(|item| item.get("model"))
            .and_then(toml_edit::Item::as_str),
        Some("moonshot-v1")
    );
    assert_eq!(
        removed_doc.get("keep").and_then(toml_edit::Item::as_str),
        Some("yes")
    );

    let mini_target = PathBuf::from("/tmp/ocg-minimax/reasoning.yaml");
    let mini_original = r#"logLevel: debug
extra: stay
defaultModel: provider:minimax/kept
defaultModelThinking:
  effort: user-effort
defaultModelContextWindow: 4096
provider:
  minimax:
    name: official
"#;
    let mini = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(mini_original.as_bytes()),
            None,
            None,
            input(&models, None),
        )
        .unwrap();
    let mini_bytes = role_bytes(&mini, FileRole::Target).unwrap();
    let mini_root: serde_yaml_ng::Value = serde_yaml_ng::from_slice(mini_bytes.as_slice()).unwrap();
    assert_minimax_reasoning(
        minimax_model(&mini_root, "reasoner"),
        Some(&["high", "low", "max", "none"][..]),
        true,
    );
    assert_minimax_reasoning(
        minimax_model(&mini_root, "literal-off"),
        Some(&["high"][..]),
        true,
    );
    assert_minimax_reasoning(
        minimax_model(&mini_root, "aliases"),
        Some(&["high", "none"][..]),
        true,
    );
    assert_minimax_reasoning(
        minimax_model(&mini_root, "levels-only"),
        Some(&["high", "low"][..]),
        false,
    );
    for id in ["bare", "bare-empty", "missing", "empty", "disabled"] {
        assert_minimax_reasoning(minimax_model(&mini_root, id), None, id.starts_with("bare"));
    }
    assert_eq!(mini_root["logLevel"].as_str(), Some("debug"));
    assert_eq!(mini_root["extra"].as_str(), Some("stay"));
    assert_minimax_preferences(&mini_root);
    assert_eq!(
        mini_root["provider"]["minimax"]["name"].as_str(),
        Some("official")
    );

    let mini_receipt = carry(&mini);
    let mut tier_edit = mini_root.clone();
    tier_edit["custom_provider"]["ocg-chat"]["models"]["reasoner"]["thinking"]["effortOptions"] =
        serde_yaml_ng::Value::Sequence(vec![serde_yaml_ng::Value::String("low".into())]);
    let tier_edit = serde_yaml_ng::to_string(&tier_edit).unwrap();
    assert_conflict(MinimaxAdapter.configure(
        &mini_target,
        None,
        Some(tier_edit.as_bytes()),
        None,
        Some(&mini_receipt),
        input(&models, None),
    ));
    assert_conflict(MinimaxAdapter.remove(
        &mini_target,
        None,
        Some(tier_edit.as_bytes()),
        None,
        &mini_receipt,
    ));

    let logged = yaml_text(mini_bytes.as_slice()).replace("logLevel: debug", "logLevel: info");
    let preserved = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(logged.as_bytes()),
            None,
            Some(&mini_receipt),
            input(&models, None),
        )
        .unwrap();
    let preserved_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&preserved, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert_eq!(preserved_root["logLevel"].as_str(), Some("info"));
    assert_eq!(
        preserved_root["provider"]["minimax"]["name"].as_str(),
        Some("official")
    );
    assert_minimax_reasoning(
        minimax_model(&preserved_root, "reasoner"),
        Some(&["high", "low", "max", "none"][..]),
        true,
    );

    let reconfigured = MinimaxAdapter
        .configure(
            &mini_target,
            None,
            Some(mini_bytes.as_slice()),
            None,
            Some(&mini_receipt),
            input(&cleared, None),
        )
        .unwrap();
    let reconfigured_root: serde_yaml_ng::Value = serde_yaml_ng::from_slice(
        role_bytes(&reconfigured, FileRole::Target)
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert_minimax_reasoning(minimax_model(&reconfigured_root, "reasoner"), None, true);
    assert_minimax_reasoning(
        minimax_model(&reconfigured_root, "literal-off"),
        None,
        false,
    );
    assert_minimax_reasoning(
        minimax_model(&reconfigured_root, "aliases"),
        Some(&["high", "none"][..]),
        true,
    );
    assert_minimax_reasoning(
        minimax_model(&reconfigured_root, "levels-only"),
        Some(&["high", "low"][..]),
        false,
    );
    assert!(
        yaml_field(
            yaml_field(
                yaml_field(&reconfigured_root, "custom_provider").unwrap(),
                "ocg-chat",
            )
            .unwrap(),
            "models",
        )
        .unwrap()
        .as_mapping()
        .unwrap()
        .get(serde_yaml_ng::Value::String("retired".into()))
        .is_none()
    );
    assert_eq!(reconfigured_root["extra"].as_str(), Some("stay"));
    assert_minimax_preferences(&reconfigured_root);
    assert_eq!(
        reconfigured_root["provider"]["minimax"]["name"].as_str(),
        Some("official")
    );

    let removed = MinimaxAdapter
        .remove(
            &mini_target,
            None,
            Some(mini_bytes.as_slice()),
            None,
            &mini_receipt,
        )
        .unwrap();
    let removed_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&removed, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert!(
        yaml_field(&removed_root, "custom_provider")
            .and_then(|custom| yaml_field(custom, "ocg-chat"))
            .is_none()
    );
    assert_eq!(removed_root["logLevel"].as_str(), Some("debug"));
    assert_eq!(removed_root["extra"].as_str(), Some("stay"));
    assert_minimax_preferences(&removed_root);
    assert_eq!(
        removed_root["provider"]["minimax"]["name"].as_str(),
        Some("official")
    );
}

fn assert_minimax_preferences(root: &serde_yaml_ng::Value) {
    assert_eq!(root["defaultModel"].as_str(), Some("provider:minimax/kept"));
    assert_eq!(
        root["defaultModelThinking"]["effort"].as_str(),
        Some("user-effort")
    );
    assert_eq!(root["defaultModelContextWindow"].as_u64(), Some(4096));
}

fn with_protocols(mut row: ByokModel, profile: PublishedModelProtocolProfile) -> ByokModel {
    row.protocols = profile;
    row
}

fn owned_receipt(
    owned: serde_json::Value,
    model_ids: Vec<String>,
    applied: Option<&str>,
) -> Receipt {
    Receipt {
        version: 1,
        client: "test".into(),
        target_path: "test".into(),
        identity: "test".into(),
        created_target: false,
        created_catalog: false,
        baseline_default: None,
        last_applied_default: applied.map(str::to_string),
        first_owned: serde_json::Value::Null,
        last_managed: ManagedSnapshot {
            provider_id: "ocg".into(),
            model_ids,
            owned,
            applied_default: applied.map(str::to_string),
        },
        pending: None,
    }
}

#[test]
fn route_uses_preferred_then_messages_then_chat_and_never_invents_chat() {
    let chat = PublishedUpstreamProtocol::ChatCompletions;
    let responses = PublishedUpstreamProtocol::Responses;
    let messages = PublishedUpstreamProtocol::Messages;
    let mut preferred = model("preferred");
    preferred.protocols = protocols(responses, &[responses, messages, chat]);
    assert_eq!(
        super::route_model(&preferred, &[messages, chat]).unwrap(),
        messages
    );
    assert_eq!(
        super::route_model(&preferred, &[chat, responses, messages]).unwrap(),
        responses
    );
    let mut messages_only = model("messages-only");
    messages_only.protocols = protocols(messages, &[messages]);
    assert_eq!(
        super::route_model(&messages_only, &[messages, chat]).unwrap(),
        messages
    );
    let mut responses_only = model("responses-only");
    responses_only.protocols = protocols(responses, &[responses]);
    let error = super::route_model(&responses_only, &[messages, chat]).unwrap_err();
    assert_invalid(error.clone());
    assert!(error.message.contains("responses-only"));
    assert!(error.message.contains("no client transport"));
    assert!(!error.message.contains("chat_completions"));
}

#[test]
fn invalid_profiles_fail_before_an_unowned_provider_is_reported() {
    let mut broken = model("broken");
    broken.protocols = protocols(
        PublishedUpstreamProtocol::Responses,
        &[PublishedUpstreamProtocol::ChatCompletions],
    );
    let foreign = b"[providers.ocg]\ntype = \"openai\"\n";
    let error = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/invalid.toml"),
            None,
            Some(foreign),
            None,
            None,
            input(&[broken], None),
        )
        .unwrap_err();
    assert_invalid(error.clone());
    assert!(error.message.contains("broken"));
    assert!(error.message.contains("invalid"));
    for client in ByokClient::ALL {
        assert_invalid(
            validate_models(client, &[model(""), model("dup"), model("dup")]).unwrap_err(),
        );
    }
}

#[test]
fn grouped_clients_keep_public_ids_and_split_bases_by_saved_protocol() {
    let chat = PublishedUpstreamProtocol::ChatCompletions;
    let responses = PublishedUpstreamProtocol::Responses;
    let messages = PublishedUpstreamProtocol::Messages;
    let mut chat_model = with_protocols(model("chat-model"), protocols(chat, &[chat, messages]));
    chat_model.metadata.reasoning = Some(true);
    chat_model.metadata.reasoning_efforts = Some([("high".into(), "high".into())].into());
    let mut responses_model =
        with_protocols(model("responses-model"), protocols(responses, &[responses]));
    responses_model.metadata.reasoning = Some(true);
    responses_model.metadata.reasoning_efforts = None;
    let mut messages_model = with_protocols(
        model("messages-model"),
        protocols(messages, &[messages, chat]),
    );
    messages_model.metadata.reasoning = Some(true);
    messages_model.metadata.reasoning_efforts = Some([("high".into(), "high".into())].into());
    let models = vec![chat_model, responses_model, messages_model];
    let gateway = "http://127.0.0.1:9/gateway/v1/";
    let configured = ConfigureInput {
        gateway_v1_url: gateway,
        secret: SECRET,
        models: &models,
        default_model_id: Some("messages-model"),
    };

    let kimi = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/mixed.toml"),
            None,
            Some(b"default_model = \"user-picked\"\nkeep = \"yes\"\n[providers.foreign]\ntype = \"kimi\"\n"),
            None,
            None,
            ConfigureInput {
                default_model_id: None,
                ..configured
            },
        )
        .unwrap();
    let kimi_doc = toml(role_bytes(&kimi, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        kimi_doc
            .get("default_model")
            .and_then(toml_edit::Item::as_str),
        Some("user-picked")
    );
    assert_eq!(
        kimi_doc
            .get("providers")
            .and_then(|item| item.get("foreign"))
            .and_then(|item| item.get("type"))
            .and_then(toml_edit::Item::as_str),
        Some("kimi")
    );
    assert!(
        kimi_doc
            .get("providers")
            .and_then(|item| item.get("ocg"))
            .is_none()
    );
    let chat_provider = kimi_doc.get("providers").unwrap().get("ocg-chat").unwrap();
    assert_eq!(
        chat_provider.get("type").and_then(toml_edit::Item::as_str),
        Some("openai")
    );
    assert_eq!(
        chat_provider
            .get("base_url")
            .and_then(toml_edit::Item::as_str),
        Some("http://127.0.0.1:9/gateway/v1")
    );
    assert_eq!(
        chat_provider
            .get("api_key")
            .and_then(toml_edit::Item::as_str),
        Some(SECRET)
    );
    let responses_provider = kimi_doc
        .get("providers")
        .unwrap()
        .get("ocg-responses")
        .unwrap();
    assert_eq!(
        responses_provider
            .get("type")
            .and_then(toml_edit::Item::as_str),
        Some("openai_responses")
    );
    assert_eq!(
        responses_provider
            .get("base_url")
            .and_then(toml_edit::Item::as_str),
        Some("http://127.0.0.1:9/gateway/v1")
    );
    let messages_provider = kimi_doc
        .get("providers")
        .unwrap()
        .get("ocg-messages")
        .unwrap();
    assert_eq!(
        messages_provider
            .get("type")
            .and_then(toml_edit::Item::as_str),
        Some("anthropic")
    );
    assert_eq!(
        messages_provider
            .get("base_url")
            .and_then(toml_edit::Item::as_str),
        Some("http://127.0.0.1:9/gateway")
    );
    let messages_entry = kimi_table(&kimi_doc, "messages-model");
    assert_eq!(
        messages_entry
            .get("provider")
            .and_then(toml_edit::Item::as_str),
        Some("ocg-messages")
    );
    assert_eq!(
        messages_entry
            .get("model")
            .and_then(toml_edit::Item::as_str),
        Some("messages-model")
    );
    assert!(messages_entry.get("support_efforts").is_none());
    assert!(messages_entry.get("off_effort").is_none());
    assert!(messages_entry.get("adaptive_thinking").is_none());
    assert_eq!(
        toml_strings(messages_entry.get("capabilities")),
        Some(vec!["thinking".to_string()])
    );
    assert_kimi_reasoning(
        kimi_table(&kimi_doc, "chat-model"),
        Some(&["high"][..]),
        None,
        true,
    );
    assert_kimi_reasoning(kimi_table(&kimi_doc, "responses-model"), None, None, true);

    let bare_gateway = ConfigureInput {
        gateway_v1_url: "http://127.0.0.1:9/v1extra",
        ..configured
    };
    let bare = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/v1extra.toml"),
            None,
            None,
            None,
            None,
            bare_gateway,
        )
        .unwrap();
    let bare_doc = toml(role_bytes(&bare, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        bare_doc
            .get("providers")
            .unwrap()
            .get("ocg-messages")
            .unwrap()
            .get("base_url")
            .and_then(toml_edit::Item::as_str),
        Some("http://127.0.0.1:9/v1extra")
    );

    let mini = MinimaxAdapter
        .configure(
            Path::new("/tmp/ocg-minimax/mixed.yaml"),
            None,
            Some(b"defaultModel: provider:minimax/kept\nlogLevel: debug\n"),
            None,
            None,
            ConfigureInput {
                default_model_id: None,
                ..configured
            },
        )
        .unwrap();
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    assert_eq!(
        mini_root["defaultModel"].as_str(),
        Some("provider:minimax/kept")
    );
    assert_eq!(mini_root["logLevel"].as_str(), Some("debug"));
    assert_eq!(
        mini_root["custom_provider"]["ocg-chat"]["api"].as_str(),
        Some("openai-completions")
    );
    assert_eq!(
        mini_root["custom_provider"]["ocg-chat"]["options"]["baseURL"].as_str(),
        Some("http://127.0.0.1:9/gateway/v1")
    );
    assert_eq!(
        mini_root["custom_provider"]["ocg-responses"]["api"].as_str(),
        Some("openai-responses")
    );
    assert_eq!(
        mini_root["custom_provider"]["ocg-responses"]["options"]["baseURL"].as_str(),
        Some("http://127.0.0.1:9/gateway/v1")
    );
    assert_eq!(
        mini_root["custom_provider"]["ocg-messages"]["api"].as_str(),
        Some("anthropic-messages")
    );
    assert_eq!(
        mini_root["custom_provider"]["ocg-messages"]["options"]["baseURL"].as_str(),
        Some("http://127.0.0.1:9/gateway")
    );
    assert!(mini_root["custom_provider"].get("ocg").is_none());
    assert_minimax_reasoning(
        &mini_root["custom_provider"]["ocg-chat"]["models"]["chat-model"],
        Some(&["high"][..]),
        true,
    );
    assert_minimax_reasoning(
        &mini_root["custom_provider"]["ocg-responses"]["models"]["responses-model"],
        None,
        true,
    );
    assert_minimax_reasoning(
        &mini_root["custom_provider"]["ocg-messages"]["models"]["messages-model"],
        None,
        true,
    );
    assert!(
        mini_root["custom_provider"]["ocg-messages"]["models"]["messages-model"]
            .get("thinking_config")
            .is_none()
    );

    let zcode = ZcodeAdapter
        .configure(
            Path::new("/tmp/ocg-zcode/mixed.json"),
            None,
            Some(zcode_shell(Some(r#"{"providerId":"openai","modelId":"kept"}"#)).as_bytes()),
            None,
            None,
            ConfigureInput {
                default_model_id: None,
                ..configured
            },
        )
        .unwrap();
    let z_doc = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    assert_eq!(
        z_doc["config"]["defaultModelSelection"]["providerId"],
        "openai"
    );
    assert_eq!(z_doc["config"]["defaultModelSelection"]["modelId"], "kept");
    assert_eq!(z_doc["config"]["extraUser"], true);
    let rules = z_doc["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap();
    let rule = |id: &str| rules.iter().find(|rule| rule["providerId"] == id).unwrap();
    assert_eq!(
        rule("ocg-chat")["config"]["api"]["type"],
        "openai-chat-completions"
    );
    assert_eq!(
        rule("ocg-chat")["config"]["api"]["baseUrl"],
        "http://127.0.0.1:9/gateway/v1"
    );
    assert_eq!(
        rule("ocg-responses")["config"]["api"]["type"],
        "openai-responses"
    );
    assert_eq!(
        rule("ocg-messages")["config"]["api"]["type"],
        "anthropic-messages"
    );
    assert_eq!(
        rule("ocg-messages")["config"]["api"]["baseUrl"],
        "http://127.0.0.1:9/gateway"
    );
    assert_eq!(
        rule("ocg-messages")["config"]["personalModelIds"][0],
        "messages-model"
    );
    let model_rules = z_doc["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap();
    let messages_rule = model_rules
        .iter()
        .find(|rule| rule["modelId"] == "messages-model")
        .unwrap();
    assert!(
        messages_rule["config"]["optionSpecs"]
            .get("reasoningLevel")
            .is_none()
    );
    let chat_rule = model_rules
        .iter()
        .find(|rule| rule["modelId"] == "chat-model")
        .unwrap();
    assert_eq!(
        chat_rule["config"]["optionSpecs"]["reasoningLevel"]["values"],
        serde_json::json!(["high"])
    );
    let responses_rule = model_rules
        .iter()
        .find(|rule| rule["modelId"] == "responses-model")
        .unwrap();
    assert!(
        responses_rule["config"]["optionSpecs"]
            .get("reasoningLevel")
            .is_none()
    );
    assert_eq!(
        z_doc["config"]["providerOrder"],
        serde_json::json!(["keep", "ocg-chat", "ocg-responses", "ocg-messages"])
    );
}

#[test]
fn legacy_owned_chat_provider_migrates_and_remove_clears_only_that_provider() {
    let legacy_toml = "\
keep = \"yes\"\n\
default_model = \"ocg/legacy\"\n\
\n\
[providers.ocg]\n\
type = \"openai\"\n\
base_url = \"http://127.0.0.1:9/v1\"\n\
api_key = \"synthetic-secret\"\n\
name = \"Open Console Gateway\"\n\
\n\
[providers.moonshot]\n\
type = \"kimi\"\n\
\n\
[models.\"ocg/legacy\"]\n\
provider = \"ocg\"\n\
model = \"legacy\"\n\
display_name = \"legacy\"\n\
max_context_size = 8192\n";
    let receipt = owned_receipt(
        serde_json::json!({
            "provider": {
                "type": "openai",
                "base_url": "http://127.0.0.1:9/v1",
                "name": "Open Console Gateway"
            },
            "models": {
                "ocg/legacy": {
                    "provider": "ocg",
                    "model": "legacy",
                    "display_name": "legacy",
                    "max_context_size": 8192
                }
            }
        }),
        vec!["legacy".into()],
        Some("ocg/legacy"),
    );
    let target = Path::new("/tmp/ocg-kimi/legacy.toml");
    let migrated = KimiAdapter
        .configure(
            target,
            None,
            Some(legacy_toml.as_bytes()),
            None,
            Some(&receipt),
            input(&[model("legacy")], None),
        )
        .unwrap();
    let doc = toml(role_bytes(&migrated, FileRole::Target).unwrap().as_slice());
    assert!(doc.get("providers").unwrap().get("ocg").is_none());
    assert_eq!(
        doc.get("providers")
            .unwrap()
            .get("ocg-chat")
            .unwrap()
            .get("type")
            .and_then(toml_edit::Item::as_str),
        Some("openai")
    );
    assert_eq!(
        kimi_table(&doc, "legacy")
            .get("provider")
            .and_then(toml_edit::Item::as_str),
        Some("ocg-chat")
    );
    assert_eq!(
        doc.get("providers")
            .unwrap()
            .get("moonshot")
            .unwrap()
            .get("type")
            .and_then(toml_edit::Item::as_str),
        Some("kimi")
    );
    assert_eq!(
        doc.get("default_model").and_then(toml_edit::Item::as_str),
        Some("ocg/legacy")
    );
    let removed = KimiAdapter
        .remove(target, None, Some(legacy_toml.as_bytes()), None, &receipt)
        .unwrap();
    let removed_doc = toml(role_bytes(&removed, FileRole::Target).unwrap().as_slice());
    assert!(
        removed_doc
            .get("providers")
            .and_then(|item| item.get("ocg"))
            .is_none()
    );
    assert!(
        removed_doc
            .get("models")
            .and_then(|item| item.get("ocg/legacy"))
            .is_none()
    );
    assert_eq!(
        removed_doc
            .get("providers")
            .unwrap()
            .get("moonshot")
            .unwrap()
            .get("type")
            .and_then(toml_edit::Item::as_str),
        Some("kimi")
    );
    assert!(removed_doc.get("keep").is_some());

    let legacy_yaml = r#"logLevel: debug
defaultModel: custom_provider:ocg/legacy
defaultModelThinking:
  effort: user-effort
defaultModelContextWindow: 4096
custom_provider:
  ocg:
    name: Open Console Gateway
    kind: custom
    enabled: true
    api: openai-completions
    region: eu
    options:
      apiKey: synthetic-secret
      baseURL: http://127.0.0.1:9/v1
      authMode: api-key
    models:
      legacy:
        name: legacy
        enabled: true
        configuration_source: manual
        limit:
          context: 8192
          output: 1024
          userUnit: tokens
        note: kept-on-model
        thinking:
          userToggle: true
  other:
    name: foreign
"#;
    let mut mini_receipt = owned_receipt(
        serde_json::json!({
            "provider": {
                "name": "Open Console Gateway",
                "kind": "custom",
                "enabled": true,
                "api": "openai-completions",
                "region": "eu",
                "options": {
                    "baseURL": "http://127.0.0.1:9/v1",
                    "authMode": "api-key"
                },
                "models": {
                    "legacy": {
                        "name": "legacy",
                        "enabled": true,
                        "configuration_source": "manual",
                        "limit": { "context": 8192, "output": 1024, "userUnit": "tokens" },
                        "note": "kept-on-model",
                        "thinking": { "userToggle": true }
                    }
                }
            }
        }),
        vec!["legacy".into()],
        Some("custom_provider:ocg/legacy"),
    );
    mini_receipt.baseline_default = Some("provider:minimax/kept".into());
    let mini_target = Path::new("/tmp/ocg-minimax/legacy.yaml");
    let mini = MinimaxAdapter
        .configure(
            mini_target,
            None,
            Some(legacy_yaml.as_bytes()),
            None,
            Some(&mini_receipt),
            input(&[model("legacy")], None),
        )
        .unwrap();
    let mini_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&mini, FileRole::Target).unwrap().as_slice()).unwrap();
    assert!(mini_root["custom_provider"].get("ocg").is_none());
    assert_eq!(
        mini_root["custom_provider"]["ocg-chat"]["models"]["legacy"]["name"].as_str(),
        Some("legacy")
    );
    assert!(
        mini_root["custom_provider"]["ocg-chat"]
            .get("region")
            .is_none()
    );
    let moved = &mini_root["custom_provider"]["ocg-chat"]["models"]["legacy"];
    assert_eq!(moved["note"].as_str(), Some("kept-on-model"));
    assert_eq!(moved["limit"]["userUnit"].as_str(), Some("tokens"));
    assert_eq!(moved["limit"]["context"].as_u64(), Some(8192));
    assert_eq!(moved["thinking"]["userToggle"].as_bool(), Some(true));
    assert!(moved["thinking"].get("effortOptions").is_none());
    assert_eq!(
        mini_root["custom_provider"]["other"]["name"].as_str(),
        Some("foreign")
    );
    assert_eq!(mini_root["logLevel"].as_str(), Some("debug"));
    assert_eq!(
        mini_root["defaultModel"].as_str(),
        Some("custom_provider:ocg-chat/legacy")
    );
    assert_eq!(
        mini_root["defaultModelThinking"]["effort"].as_str(),
        Some("user-effort")
    );
    assert_eq!(mini_root["defaultModelContextWindow"].as_u64(), Some(4096));
    assert_eq!(
        mini.last_applied_default.as_deref(),
        Some("custom_provider:ocg-chat/legacy")
    );
    assert_eq!(
        mini.baseline_default.as_deref(),
        Some("provider:minimax/kept")
    );

    let z_rule = serde_json::json!({
        "providerId": "ocg",
        "providerName": "Open Console Gateway",
        "enabled": true,
        "config": {
            "group": "standard-personal",
            "access": { "type": "api-key", "apiKey": "synthetic-secret" },
            "api": { "type": "openai-chat-completions", "baseUrl": "http://127.0.0.1:9/v1", "extraHeader": "legacy-only" },
            "personalModelIds": ["legacy"],
            "modelOrder": ["legacy"]
        }
    });
    let z_model = serde_json::json!({
        "providerId": "ocg",
        "modelId": "legacy",
        "label": "kept-label",
        "config": {
            "enabled": true,
            "note": "kept-on-model",
            "properties": { "contextWindow": 8192, "userDefault": "warm" },
            "optionSpecs": { "maxOutputTokens": { "max": 1024, "unit": "tokens" } }
        }
    });
    let z_root = serde_json::json!({
        "schemaVersion": 1,
        "config": {
            "providerOrder": ["keep", "ocg"],
            "providerConfigRules": { "providerRules": [z_rule] },
            "modelConfigRules": {
                "providerModelRules": [z_model],
                "manualProviderModelRules": []
            },
            "note": "user"
        }
    });
    let z_receipt = owned_receipt(
        serde_json::json!({
            "providers": [z_root["config"]["providerConfigRules"]["providerRules"][0].clone()],
            "providerOrder": ["ocg"],
            "models": [z_root["config"]["modelConfigRules"]["providerModelRules"][0].clone()]
        }),
        vec!["legacy".into()],
        None,
    );
    let z_bytes = serde_json::to_vec(&z_root).unwrap();
    let z_target = Path::new("/tmp/ocg-zcode/legacy.json");
    let zcode = ZcodeAdapter
        .configure(
            z_target,
            None,
            Some(&z_bytes),
            None,
            Some(&z_receipt),
            input(&[model("legacy")], None),
        )
        .unwrap();
    let migrated_z = json_doc(role_bytes(&zcode, FileRole::Target).unwrap().as_slice());
    let ids: Vec<_> = migrated_z["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["providerId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["ocg-chat".to_string()]);
    assert_eq!(migrated_z["config"]["note"], "user");
    let chat_provider = migrated_z["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["providerId"] == "ocg-chat")
        .unwrap();
    assert!(chat_provider["config"]["api"].get("extraHeader").is_none());
    assert_eq!(
        chat_provider["config"]["api"]["type"],
        "openai-chat-completions"
    );
    assert!(
        migrated_z["config"]["modelConfigRules"]["manualProviderModelRules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let moved_rule = &migrated_z["config"]["modelConfigRules"]["providerModelRules"][0];
    assert_eq!(moved_rule["providerId"], "ocg-chat");
    assert_eq!(moved_rule["modelId"], "legacy");
    assert_eq!(moved_rule["label"], "kept-label");
    assert_eq!(moved_rule["config"]["note"], "kept-on-model");
    assert_eq!(moved_rule["config"]["properties"]["userDefault"], "warm");
    assert_eq!(moved_rule["config"]["properties"]["contextWindow"], 8192);
    assert_eq!(
        moved_rule["config"]["optionSpecs"]["maxOutputTokens"]["unit"],
        "tokens"
    );
    assert_eq!(
        moved_rule["config"]["optionSpecs"]["maxOutputTokens"]["max"],
        1024
    );
    assert!(
        moved_rule["config"]["optionSpecs"]
            .get("reasoningLevel")
            .is_none()
    );
    assert!(
        migrated_z["config"]["providerOrder"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "keep")
    );
    assert!(
        migrated_z["config"]["providerOrder"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item != "ocg")
    );
    let _ = z_root;
}

fn assert_manual_conflict(result: ByokResult<ApplyPlan>, message: &str) {
    let error = result.expect_err("manual rule must block the write");
    assert_eq!(error.kind, ByokErrorKind::Conflict);
    assert!(
        error.message.contains(message),
        "expected {message} in {}",
        error.message
    );
    assert!(!error.message.contains(SECRET));
}

fn zcode_owned_view(doc: &serde_json::Value) -> serde_json::Value {
    let managed = |id: Option<&str>| {
        matches!(
            id,
            Some("ocg" | "ocg-chat" | "ocg-responses" | "ocg-messages")
        )
    };
    let providers: Vec<_> = doc["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|rule| managed(rule["providerId"].as_str()))
        .cloned()
        .collect();
    let provider_order: Vec<_> = doc["config"]["providerOrder"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| managed(item.as_str()))
        .cloned()
        .collect();
    let models: Vec<_> = doc["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|rule| managed(rule["providerId"].as_str()))
        .cloned()
        .collect();
    serde_json::json!({
        "providers": providers,
        "providerOrder": provider_order,
        "models": models,
    })
}

fn minimax_owned_view(root: &serde_yaml_ng::Value) -> serde_json::Value {
    let custom = yaml_field(root, "custom_provider");
    let mut providers = serde_json::Map::new();
    for id in ["ocg", "ocg-chat", "ocg-responses", "ocg-messages"] {
        let value = custom
            .and_then(|custom| yaml_field(custom, id))
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or(serde_json::Value::Null);
        providers.insert(id.to_string(), value);
    }
    serde_json::json!({ "providers": providers })
}

fn reasoner(id: &str) -> ByokModel {
    let mut row = model(id);
    row.metadata.reasoning = Some(true);
    row.metadata.reasoning_efforts = Some([("high".into(), "high".into())].into());
    row
}

#[test]
fn zcode_manual_rules_conflict_when_they_would_collide_or_be_orphaned() {
    let target = Path::new("/tmp/ocg-zcode/manual.json");
    let shell = zcode_shell(None);
    let first = ZcodeAdapter
        .configure(
            target,
            None,
            Some(shell.as_bytes()),
            None,
            None,
            input(&[model("kept")], None),
        )
        .unwrap();
    let mut doc = json_doc(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    doc["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([
        {"providerId": "ocg-chat", "modelId": "new", "config": {"enabled": true, "note": "manual"}}
    ]);
    let collided = serde_json::to_vec(&doc).unwrap();
    let held = collided.clone();
    assert_manual_conflict(
        ZcodeAdapter.configure(
            target,
            None,
            Some(&collided),
            None,
            Some(&carry(&first)),
            input(&[model("new")], None),
        ),
        "same provider and model",
    );
    assert_eq!(collided, held);

    doc["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([
        {"providerId": "ocg-chat", "modelId": "manual-other", "config": {"enabled": true, "note": "stay"}},
        {"providerId": "other", "modelId": "foreign", "config": {"enabled": true}}
    ]);
    let kept_bytes = serde_json::to_vec(&doc).unwrap();
    let kept = ZcodeAdapter
        .configure(
            target,
            None,
            Some(&kept_bytes),
            None,
            Some(&carry(&first)),
            input(&[model("kept")], None),
        )
        .unwrap();
    let kept_doc = json_doc(role_bytes(&kept, FileRole::Target).unwrap().as_slice());
    let manuals = kept_doc["config"]["modelConfigRules"]["manualProviderModelRules"]
        .as_array()
        .unwrap();
    assert_eq!(manuals.len(), 2);
    assert!(
        manuals
            .iter()
            .any(|rule| { rule["modelId"] == "manual-other" && rule["config"]["note"] == "stay" })
    );
    assert!(manuals.iter().any(|rule| rule["providerId"] == "other"));
    assert!(
        kept_doc["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["providerId"] == "ocg-chat")
    );

    let grouped = ZcodeAdapter
        .configure(
            target,
            None,
            Some(shell.as_bytes()),
            None,
            None,
            input(
                &[
                    model("kept"),
                    with_protocols(
                        model("messaged"),
                        protocols(
                            PublishedUpstreamProtocol::Messages,
                            &[PublishedUpstreamProtocol::Messages],
                        ),
                    ),
                ],
                None,
            ),
        )
        .unwrap();
    let mut grouped_doc = json_doc(role_bytes(&grouped, FileRole::Target).unwrap().as_slice());
    grouped_doc["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([
        {"providerId": "ocg-messages", "modelId": "manual-msg", "config": {"enabled": true}}
    ]);
    let grouped_bytes = serde_json::to_vec(&grouped_doc).unwrap();
    assert_manual_conflict(
        ZcodeAdapter.configure(
            target,
            None,
            Some(&grouped_bytes),
            None,
            Some(&carry(&grouped)),
            input(&[model("kept")], None),
        ),
        "would be removed",
    );
    assert_manual_conflict(
        ZcodeAdapter.remove(target, None, Some(&grouped_bytes), None, &carry(&grouped)),
        "would be removed",
    );

    let retired = serde_json::json!({
        "schemaVersion": 1,
        "config": {
            "providerOrder": ["ocg"],
            "providerConfigRules": {"providerRules": [{
                "providerId": "ocg",
                "providerName": "Open Console Gateway",
                "enabled": true,
                "config": {
                    "group": "standard-personal",
                    "access": {"type": "api-key", "apiKey": SECRET},
                    "api": {"type": "openai-chat-completions", "baseUrl": "http://127.0.0.1:9/v1"},
                    "personalModelIds": ["legacy"],
                    "modelOrder": ["legacy"]
                }
            }]},
            "modelConfigRules": {
                "providerModelRules": [{
                    "providerId": "ocg",
                    "modelId": "legacy",
                    "config": {
                        "enabled": true,
                        "properties": {"contextWindow": 8192},
                        "optionSpecs": {"maxOutputTokens": {"max": 1024}}
                    }
                }],
                "manualProviderModelRules": [
                    {"providerId": "ocg", "modelId": "manual", "config": {"enabled": true}}
                ]
            }
        }
    });
    let retired_bytes = serde_json::to_vec(&retired).unwrap();
    let retired_receipt = owned_receipt(zcode_owned_view(&retired), vec!["legacy".into()], None);
    assert_manual_conflict(
        ZcodeAdapter.configure(
            target,
            None,
            Some(&retired_bytes),
            None,
            Some(&retired_receipt),
            input(&[model("legacy")], None),
        ),
        "would be removed",
    );
    assert_manual_conflict(
        ZcodeAdapter.remove(target, None, Some(&retired_bytes), None, &retired_receipt),
        "would be removed",
    );
}

#[test]
fn receipt_matching_fields_follow_a_model_across_configure_update_and_remove() {
    let chat = reasoner("moved");
    let mini_target = Path::new("/tmp/ocg-minimax/preserve.yaml");
    let first = MinimaxAdapter
        .configure(
            mini_target,
            None,
            Some(b"logLevel: info\nextra: stay\n"),
            None,
            None,
            input(std::slice::from_ref(&chat), None),
        )
        .unwrap();
    let original = role_bytes(&first, FileRole::Target).unwrap();
    let mut foreign: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&original).unwrap();
    foreign["custom_provider"]["ocg-chat"]["models"]["moved"]["note"] =
        serde_yaml_ng::Value::String("foreign".into());
    let foreign_yaml = serde_yaml_ng::to_string(&foreign).unwrap();
    assert_conflict(MinimaxAdapter.configure(
        mini_target,
        None,
        Some(foreign_yaml.as_bytes()),
        None,
        Some(&carry(&first)),
        input(std::slice::from_ref(&chat), None),
    ));

    let mut matched: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&original).unwrap();
    matched["custom_provider"]["ocg-chat"]["options"]["extraOption"] =
        serde_yaml_ng::Value::String("kept-option".into());
    matched["custom_provider"]["ocg-chat"]["models"]["moved"]["note"] =
        serde_yaml_ng::Value::String("model-kept".into());
    matched["custom_provider"]["ocg-chat"]["models"]["moved"]["limit"]["userUnit"] =
        serde_yaml_ng::Value::String("tokens".into());
    matched["custom_provider"]["ocg-chat"]["models"]["moved"]["thinking"]["userToggle"] =
        serde_yaml_ng::Value::Bool(true);
    let matched_yaml = serde_yaml_ng::to_string(&matched).unwrap();
    let matched: serde_yaml_ng::Value = serde_yaml_ng::from_str(&matched_yaml).unwrap();
    let mut receipt = carry(&first);
    receipt.last_managed.owned = minimax_owned_view(&matched);
    let mut updated_model = chat.clone();
    updated_model.metadata.context_window = Some(1000);
    let updated = MinimaxAdapter
        .configure(
            mini_target,
            None,
            Some(matched_yaml.as_bytes()),
            None,
            Some(&receipt),
            input(&[updated_model.clone()], None),
        )
        .unwrap();
    let updated_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&updated, FileRole::Target).unwrap().as_slice())
            .unwrap();
    let model_node = &updated_root["custom_provider"]["ocg-chat"]["models"]["moved"];
    assert_eq!(model_node["note"].as_str(), Some("model-kept"));
    assert_eq!(model_node["limit"]["userUnit"].as_str(), Some("tokens"));
    assert_eq!(model_node["limit"]["context"].as_u64(), Some(1000));
    assert_eq!(model_node["thinking"]["userToggle"].as_bool(), Some(true));
    assert_eq!(
        yaml_strings(
            yaml_field(yaml_field(model_node, "thinking").unwrap(), "effortOptions").unwrap()
        ),
        vec!["high".to_string()]
    );
    assert_eq!(
        updated_root["custom_provider"]["ocg-chat"]["options"]["extraOption"].as_str(),
        Some("kept-option")
    );
    assert_eq!(updated_root["logLevel"].as_str(), Some("info"));
    assert_eq!(updated_root["extra"].as_str(), Some("stay"));
    assert_eq!(
        updated.managed.owned["providers"]["ocg-chat"]["models"]["moved"]["note"],
        "model-kept"
    );

    let mut moved_model = updated_model.clone();
    moved_model.metadata.max_output_tokens = Some(2048);
    let moved_model = with_protocols(
        moved_model,
        protocols(
            PublishedUpstreamProtocol::Messages,
            &[PublishedUpstreamProtocol::Messages],
        ),
    );
    let moved = MinimaxAdapter
        .configure(
            mini_target,
            None,
            role_bytes(&updated, FileRole::Target).as_deref(),
            None,
            Some(&carry(&updated)),
            input(&[moved_model], None),
        )
        .unwrap();
    let moved_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&moved, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert!(moved_root["custom_provider"].get("ocg-chat").is_none());
    let moved_node = &moved_root["custom_provider"]["ocg-messages"]["models"]["moved"];
    assert_eq!(moved_node["note"].as_str(), Some("model-kept"));
    assert_eq!(moved_node["limit"]["userUnit"].as_str(), Some("tokens"));
    assert_eq!(moved_node["limit"]["output"].as_u64(), Some(2048));
    assert_eq!(moved_node["thinking"]["userToggle"].as_bool(), Some(true));
    assert!(moved_node["thinking"].get("effortOptions").is_none());
    assert_eq!(moved_node["reasoning"].as_bool(), Some(true));
    assert!(
        moved_root["custom_provider"]["ocg-messages"]["options"]
            .get("extraOption")
            .is_none()
    );
    assert_eq!(
        moved.managed.owned["providers"]["ocg-messages"]["models"]["moved"]["note"],
        "model-kept"
    );
    assert!(
        moved.managed.owned["providers"]["ocg-messages"]["models"]["moved"]["thinking"]
            .get("effortOptions")
            .is_none()
    );
    let removed = MinimaxAdapter
        .remove(
            mini_target,
            None,
            role_bytes(&moved, FileRole::Target).as_deref(),
            None,
            &carry(&moved),
        )
        .unwrap();
    let removed_root: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(role_bytes(&removed, FileRole::Target).unwrap().as_slice())
            .unwrap();
    assert!(
        yaml_field(&removed_root, "custom_provider")
            .and_then(|custom| yaml_field(custom, "ocg-messages"))
            .is_none()
    );
    assert_eq!(removed_root["logLevel"].as_str(), Some("info"));
    assert_eq!(removed_root["extra"].as_str(), Some("stay"));

    let z_target = Path::new("/tmp/ocg-zcode/preserve.json");
    let z_first = ZcodeAdapter
        .configure(
            z_target,
            None,
            Some(zcode_shell(None).as_bytes()),
            None,
            None,
            input(std::slice::from_ref(&chat), None),
        )
        .unwrap();
    let mut z_foreign = json_doc(role_bytes(&z_first, FileRole::Target).unwrap().as_slice());
    z_foreign["config"]["modelConfigRules"]["providerModelRules"][0]["config"]["note"] =
        serde_json::json!("foreign");
    assert_conflict(ZcodeAdapter.configure(
        z_target,
        None,
        Some(&serde_json::to_vec(&z_foreign).unwrap()),
        None,
        Some(&carry(&z_first)),
        input(std::slice::from_ref(&chat), None),
    ));

    let mut z_doc = json_doc(role_bytes(&z_first, FileRole::Target).unwrap().as_slice());
    {
        let provider = z_doc["config"]["providerConfigRules"]["providerRules"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|rule| rule["providerId"] == "ocg-chat")
            .unwrap();
        provider["config"]["api"]["extraHeader"] = serde_json::json!("x-kept");
        provider["config"]["access"]["note"] = serde_json::json!("access-kept");
        provider["config"]["regionHint"] = serde_json::json!("user-region");
    }
    {
        let rule = &mut z_doc["config"]["modelConfigRules"]["providerModelRules"][0];
        rule["label"] = serde_json::json!("kept-label");
        rule["config"]["note"] = serde_json::json!("model-kept");
        rule["config"]["properties"]["userDefault"] = serde_json::json!("warm");
        rule["config"]["optionSpecs"]["customToggle"] = serde_json::json!(true);
        rule["config"]["optionSpecs"]["maxOutputTokens"]["unit"] = serde_json::json!("tokens");
        rule["config"]["optionSpecs"]["reasoningLevel"]["userLabel"] =
            serde_json::json!("kept-level");
    }
    let z_bytes = serde_json::to_vec(&z_doc).unwrap();
    let mut z_receipt = carry(&z_first);
    z_receipt.last_managed.owned = zcode_owned_view(&z_doc);
    let mut z_updated_model = chat.clone();
    z_updated_model.metadata.context_window = Some(1000);
    let z_updated = ZcodeAdapter
        .configure(
            z_target,
            None,
            Some(&z_bytes),
            None,
            Some(&z_receipt),
            input(&[z_updated_model.clone()], None),
        )
        .unwrap();
    let z_updated_doc = json_doc(role_bytes(&z_updated, FileRole::Target).unwrap().as_slice());
    let z_provider = z_updated_doc["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["providerId"] == "ocg-chat")
        .unwrap();
    assert_eq!(z_provider["config"]["api"]["extraHeader"], "x-kept");
    assert_eq!(
        z_provider["config"]["api"]["type"],
        "openai-chat-completions"
    );
    assert_eq!(
        z_provider["config"]["api"]["baseUrl"],
        "http://127.0.0.1:9/v1"
    );
    assert_eq!(z_provider["config"]["access"]["note"], "access-kept");
    assert_eq!(z_provider["config"]["access"]["type"], "api-key");
    assert_eq!(z_provider["config"]["access"]["apiKey"], SECRET);
    assert_eq!(z_provider["config"]["regionHint"], "user-region");
    let z_rule = &z_updated_doc["config"]["modelConfigRules"]["providerModelRules"][0];
    assert_eq!(z_rule["label"], "kept-label");
    assert_eq!(z_rule["config"]["note"], "model-kept");
    assert_eq!(z_rule["config"]["properties"]["userDefault"], "warm");
    assert_eq!(z_rule["config"]["properties"]["contextWindow"], 1000);
    assert_eq!(z_rule["config"]["optionSpecs"]["customToggle"], true);
    assert_eq!(
        z_rule["config"]["optionSpecs"]["maxOutputTokens"]["unit"],
        "tokens"
    );
    assert_eq!(
        z_rule["config"]["optionSpecs"]["maxOutputTokens"]["max"],
        1024
    );
    assert_eq!(
        z_rule["config"]["optionSpecs"]["reasoningLevel"]["values"],
        serde_json::json!(["high"])
    );
    assert_eq!(
        z_rule["config"]["optionSpecs"]["reasoningLevel"]["userLabel"],
        "kept-level"
    );
    assert_eq!(
        z_updated.managed.owned["providers"][0]["config"]["api"]["extraHeader"],
        "x-kept"
    );
    assert_eq!(
        z_updated.managed.owned["models"][0]["config"]["note"],
        "model-kept"
    );

    let mut z_moved_model = z_updated_model;
    z_moved_model.metadata.max_output_tokens = Some(2048);
    let z_moved_model = with_protocols(
        z_moved_model,
        protocols(
            PublishedUpstreamProtocol::Messages,
            &[PublishedUpstreamProtocol::Messages],
        ),
    );
    let z_moved = ZcodeAdapter
        .configure(
            z_target,
            None,
            role_bytes(&z_updated, FileRole::Target).as_deref(),
            None,
            Some(&carry(&z_updated)),
            input(&[z_moved_model], None),
        )
        .unwrap();
    let z_moved_doc = json_doc(role_bytes(&z_moved, FileRole::Target).unwrap().as_slice());
    assert!(
        z_moved_doc["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| rule["providerId"] != "ocg-chat")
    );
    let z_messages = z_moved_doc["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["providerId"] == "ocg-messages")
        .unwrap();
    assert!(z_messages["config"]["api"].get("extraHeader").is_none());
    assert_eq!(z_messages["config"]["api"]["type"], "anthropic-messages");
    let z_moved_rule = &z_moved_doc["config"]["modelConfigRules"]["providerModelRules"][0];
    assert_eq!(z_moved_rule["providerId"], "ocg-messages");
    assert_eq!(z_moved_rule["label"], "kept-label");
    assert_eq!(z_moved_rule["config"]["note"], "model-kept");
    assert_eq!(z_moved_rule["config"]["properties"]["userDefault"], "warm");
    assert_eq!(z_moved_rule["config"]["optionSpecs"]["customToggle"], true);
    assert_eq!(
        z_moved_rule["config"]["optionSpecs"]["maxOutputTokens"]["unit"],
        "tokens"
    );
    assert_eq!(
        z_moved_rule["config"]["optionSpecs"]["maxOutputTokens"]["max"],
        2048
    );
    assert!(
        z_moved_rule["config"]["optionSpecs"]["reasoningLevel"]
            .get("values")
            .is_none()
    );
    assert!(
        z_moved_rule["config"]["optionSpecs"]["reasoningLevel"]
            .get("map")
            .is_none()
    );
    assert_eq!(
        z_moved_rule["config"]["optionSpecs"]["reasoningLevel"]["userLabel"],
        "kept-level"
    );
    assert_eq!(
        z_moved.managed.owned["models"][0]["config"]["note"],
        "model-kept"
    );
    assert!(
        z_moved.managed.owned["models"][0]["config"]["optionSpecs"]["reasoningLevel"]
            .get("values")
            .is_none()
    );
    assert_eq!(
        z_moved.managed.owned["models"][0]["config"]["optionSpecs"]["reasoningLevel"]["userLabel"],
        "kept-level"
    );
    let z_removed = ZcodeAdapter
        .remove(
            z_target,
            None,
            role_bytes(&z_moved, FileRole::Target).as_deref(),
            None,
            &carry(&z_moved),
        )
        .unwrap();
    let z_removed_doc = json_doc(role_bytes(&z_removed, FileRole::Target).unwrap().as_slice());
    assert_eq!(z_removed_doc["config"]["extraUser"], true);
    assert!(
        z_removed_doc["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| {
                rule["providerId"]
                    .as_str()
                    .is_none_or(|id| !super::is_managed_provider(id))
            })
    );
}

#[test]
fn every_managed_provider_id_collides_when_unowned_and_a_foreign_one_conflicts() {
    for id in ["ocg", "ocg-chat", "ocg-responses", "ocg-messages"] {
        let toml_text = format!("[providers.{id}]\ntype = \"openai\"\n");
        assert_conflict(KimiAdapter.configure(
            Path::new("/tmp/ocg-kimi/collide.toml"),
            None,
            Some(toml_text.as_bytes()),
            None,
            None,
            input(&[model("a")], None),
        ));
        let yaml_text = format!("custom_provider:\n  {id}:\n    name: foreign\n");
        assert_conflict(MinimaxAdapter.configure(
            Path::new("/tmp/ocg-minimax/collide.yaml"),
            None,
            Some(yaml_text.as_bytes()),
            None,
            None,
            input(&[model("a")], None),
        ));
        let json_text = format!(
            "{{\"schemaVersion\":1,\"config\":{{\"providerOrder\":[],\"providerConfigRules\":{{\"providerRules\":[{{\"providerId\":\"{id}\"}}]}},\"modelConfigRules\":{{\"providerModelRules\":[],\"manualProviderModelRules\":[]}}}}}}"
        );
        assert_conflict(ZcodeAdapter.configure(
            Path::new("/tmp/ocg-zcode/collide.json"),
            None,
            Some(json_text.as_bytes()),
            None,
            None,
            input(&[model("a")], None),
        ));
    }

    let first = KimiAdapter
        .configure(
            Path::new("/tmp/ocg-kimi/foreign-managed.toml"),
            None,
            None,
            None,
            None,
            input(&[model("a")], None),
        )
        .unwrap();
    let mut doc = toml(role_bytes(&first, FileRole::Target).unwrap().as_slice());
    doc["providers"]["ocg-messages"] = toml_edit::Item::Table(toml_edit::Table::new());
    doc["providers"]["ocg-messages"]["type"] = toml_edit::value("anthropic");
    assert_conflict(KimiAdapter.configure(
        Path::new("/tmp/ocg-kimi/foreign-managed.toml"),
        None,
        Some(doc.to_string().as_bytes()),
        None,
        Some(&carry(&first)),
        input(&[model("a")], None),
    ));
}

#[test]
fn codex_keeps_responses_transport_when_the_upstream_profile_has_no_responses() {
    let (target, catalog) = codex_paths();
    let mut row = model("messages-upstream");
    row.metadata.reasoning = Some(false);
    row.metadata.reasoning_efforts = Some([("high".into(), "max".into())].into());
    row.protocols = protocols(
        PublishedUpstreamProtocol::Messages,
        &[PublishedUpstreamProtocol::Messages],
    );
    let plan = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            None,
            None,
            None,
            input(&[row.clone()], None),
        )
        .unwrap();
    let doc = toml(role_bytes(&plan, FileRole::Target).unwrap().as_slice());
    let provider = doc.get("model_providers").unwrap().get("ocg").unwrap();
    assert_eq!(
        provider.get("wire_api").and_then(toml_edit::Item::as_str),
        Some("responses")
    );
    assert_eq!(
        provider.get("base_url").and_then(toml_edit::Item::as_str),
        Some("http://127.0.0.1:9/v1")
    );
    let catalog_json = json_doc(role_bytes(&plan, FileRole::Catalog).unwrap().as_slice());
    assert!(
        catalog_json["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(catalog_json["models"][0]["default_reasoning_level"].is_null());

    let mut bare = model("support-without-choices");
    bare.metadata.reasoning = Some(true);
    bare.metadata.reasoning_efforts = Some(std::collections::BTreeMap::new());
    let bare_plan = CodexAdapter
        .configure(
            &target,
            Some(&catalog),
            None,
            None,
            None,
            input(&[bare], None),
        )
        .unwrap();
    let bare_catalog = json_doc(
        role_bytes(&bare_plan, FileRole::Catalog)
            .unwrap()
            .as_slice(),
    );
    assert!(
        bare_catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(bare_catalog["models"][0]["default_reasoning_level"].is_null());
}
