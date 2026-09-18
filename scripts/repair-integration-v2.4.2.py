#!/usr/bin/env python3
"""Extend the reviewed main-only repair with the complete integration findings."""
import importlib.util
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("main_repair", Path(__file__).with_name("repair-main-v2.4.2.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
once = base.once
EXTRA = {
    "crates/ocg-core/tests/dashboard_v3_observability.rs": "e01f800e426d7df7847a2257a2bb1305e53d731c",
    "crates/ocg-core/tests/dashboard_v3_provider_probes.rs": "d964bc3974c0445fac532f642f0bb8c224a95930",
    "crates/ocg-core/tests/dashboard_v3_providers.rs": "b99a07076a40489a5552b7e039d5aa0686b4ad34",
    "crates/ocg-core/tests/gateway_fallback.rs": "78f9f941976a3c5cd86a917fe8d6c71cfbc2e862",
    "crates/ocg-core/tests/fixtures/gateway_fallback.rs": "3cc0237fe82ba31d29b2cf3e54710a5dc1fcd99c",
}


def edit_async(text, name, transform):
    pattern = rf"(?ms)^#\[tokio::test\]\nasync fn {re.escape(name)}\(\)\s*\{{.*?^\}}\n"
    matches = list(re.finditer(pattern, text))
    if len(matches) != 1:
        raise ValueError(f"Expected one async test: {name}")
    match = matches[0]
    return text[:match.start()] + transform(match.group()) + text[match.end():]


def observations(body):
    body = once(body, "follow_current_routeable_intersection", "follow_enabled_go_catalog_independently_of_pricing")
    body = once(body, 'assert!(!models.models.contains(&"glm-5".into()));', 'assert!(models.models.contains(&"glm-5".into()));')
    body = once(body, 'assert_eq!(body["models"], json!(["grok-4.5"]));', 'assert_eq!(body["models"], json!(models.models));')
    return once(body, 'assert_eq!(body["models"], json!([]));', 'assert_eq!(body["models"], json!(models.models));')


def unavailable_reset(body):
    body = once(body, "defaults_to_chat_when_official_docs_are_unavailable", "does_not_mutate_when_official_docs_are_unavailable")
    body = once(body, "    let (status, reset) = send_json(", '''    let _docs = install_official_protocol_fetch_unavailable_for_tests(harness.state.process_generation());
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();
    let before_scope = go_scope_revision(&harness);
    let (status, reset) = send_json(''')
    start = body.index('    assert_eq!(status, StatusCode::OK, "{reset}");')
    end = body.index("    harness.stop();", start)
    return body[:start] + '''    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{reset}");
    assert_v3_error(&reset, ERROR_INTERNAL);
    assert!(reset["message"].as_str().unwrap().contains("without an official document"));
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(go_scope_revision(&harness), before_scope);
    assert_eq!(before_contracts.as_ref(), harness.state.provider_contracts().as_ref());
''' + body[end:]


def unknown_reset(body):
    body = once(body, 'assert_eq!(future["protocols"]["chat_completions"]["enabled"], true);', 'assert_eq!(future["protocols"]["chat_completions"]["enabled"], false);')
    for protocol in ("responses", "messages"):
        old = f'assert_eq!(future["protocols"]["{protocol}"]["override"], "force_off");'
        if old in body:
            body = once(body, old, f'assert_eq!(future["protocols"]["{protocol}"]["enabled"], false);')
    return body


def reload_failure(body):
    return once(body, '''    let _docs =
        install_official_protocol_fetch_unavailable_for_tests(harness.state.process_generation());''', '''    // Reach the post-commit reload fault, rather than failing before any write.
    let _docs = install_official_protocol_fetch_for_tests(harness.state.process_generation(), |_| {
        OfficialProtocolBaseline::mapped([("grok-4.5", UpstreamProtocolKind::Responses)])
    });''')


def capabilities(body):
    return once(body, '    let harness = start_loopback("providers-capabilities").await;', '''    let harness = start_loopback("providers-capabilities").await;
    // Capabilities are persisted discovery, not an implicit legacy seed catalog.
    persist_catalog(&harness, OPENCODE_PROVIDER_ID, &["grok-4.5"], CATALOG_SOURCE_OPENCODE_MODELS);
    harness.state.db.lock().apply_official_protocol_baseline(
        &ContractScope::provider(OPENCODE_PROVIDER_ID),
        &["grok-4.5".to_string()],
        &ocg_core::dashboard_v3::OfficialProtocolBaseline::mapped([
            ("grok-4.5", ocg_core::provider::UpstreamProtocolKind::Responses),
        ]),
        chrono::Utc::now(),
    ).unwrap();
    harness.state.reload_provider_contracts().unwrap();''')


def unknown_zen(body):
    body = once(body, "unknown_zen_catalog_free_id_forwards_as_chat_on_raw_pin_and_stripped_alias", "unknown_zen_catalog_requires_explicit_chat_for_raw_pin_and_stripped_alias")
    return once(body, "    let h = p.bind().await;", '''    let h = p.bind().await;
    let (status, body) = h.protocol("/v1/chat/completions", "brand-new-promo-free").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(h.calls.lock().unwrap().is_empty(), "unknown protocol must reject before upstream");
    h.state.db.lock().set_model_protocol_overrides(
        &ocg_core::provider_contracts::ContractScope::provider(ocg_core::provider::OPENCODE_ZEN_FREE_PROVIDER_ID),
        &[("brand-new-promo-free".to_string(), ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
            ocg_core::provider_contracts::ProtocolOverrideState::ForceOn)],
        Utc::now(),
    ).unwrap();
    h.state.reload_provider_contracts().unwrap();''')


EXPECTED_MODELS = '''pub(crate) fn expected_local_application_models(state: &Arc<CoreStateInner>) -> Vec<String> {
    let contracts = state.provider_contracts();
    let scope = contracts.providers.get(OPENCODE_PROVIDER_ID).unwrap();
    let catalogs = alias::RuntimeCatalogs {
        go: &scope.catalog.models,
        ..alias::RuntimeCatalogs::default()
    };
    alias::routeable_models_for_with_runtime_catalogs(OPENCODE_PROVIDER_ID, catalogs)
        .into_iter()
        .filter(|name| {
            alias::resolve_with_runtime_catalogs(name, catalogs).is_ok_and(|resolved| {
                resolved.routeable_mappings().iter().any(|mapping| {
                    mapping.is_opencode_go() && contracts.mapping_has_enabled_protocol(mapping)
                })
            })
        })
        .collect()
}
'''


def repair_extra():
    texts = {path: Path(path).read_text(encoding="utf-8") for path in EXTRA}
    path = "crates/ocg-core/tests/dashboard_v3_observability.rs"
    texts[path] = edit_async(texts[path], "dashboard_v3_application_models_follow_current_routeable_intersection", observations)
    path = "crates/ocg-core/tests/dashboard_v3_provider_probes.rs"
    text = texts[path]
    text = edit_async(text, "opencode_static_protocol_reset_defaults_to_chat_when_official_docs_are_unavailable", unavailable_reset)
    text = edit_async(text, "opencode_static_protocol_reset_is_cas_protected_and_restores_current_catalog_deterministically", unknown_reset)
    text = edit_async(text, "zen_static_protocol_reset_uses_go_docs_and_defaults_unknown_to_chat", lambda body: once(unknown_reset(body), "defaults_unknown_to_chat", "keeps_unknown_protocols_disabled"))
    text = edit_async(text, "goat_static_protocol_reset_restores_official_family_without_enabling_extra_models", lambda body: once(body, '''        stealth["protocols"]["chat_completions"]["override"],
        "force_off"''', '''        stealth["protocols"]["chat_completions"]["override"],
        "auto"'''))
    text = edit_async(text, "static_reset_advances_global_revision_before_reload_failure", reload_failure)
    texts[path] = text
    path = "crates/ocg-core/tests/dashboard_v3_providers.rs"
    texts[path] = edit_async(texts[path], "dashboard_v3_model_capabilities_are_go_protocol_rows_including_grok_45", capabilities)
    path = "crates/ocg-core/tests/gateway_fallback.rs"
    text = texts[path]
    text = edit_async(text, "application_models_intersects_priced_go_aliases_in_registry_order", lambda body: once(once(body,
        "application_models_intersects_priced_go_aliases_in_registry_order", "application_models_keeps_unpriced_catalog_models_in_registry_order"),
        'serde_json::json!(["glm-5.1", "grok-4.5", "kimi-k3", "minimax-m2.7"])',
        'serde_json::to_value(expected_local_application_models(&h.state)).unwrap()'))
    def empty_pricing(body):
        body = once(body, "application_models_empty_intersection_returns_empty_list", "application_models_remains_available_with_empty_or_disjoint_pricing")
        old = 'assert_eq!(body["models"], serde_json::json!([]));'
        if body.count(old) != 2:
            raise ValueError("Expected both empty/disjoint price-only assertions")
        return body.replace(old, 'assert_eq!(body["models"], serde_json::to_value(expected_local_application_models(&h.state)).unwrap());\n    assert!(application_model_ids(&body).contains(&"glm-5".to_string()));')
    text = edit_async(text, "application_models_empty_intersection_returns_empty_list", empty_pricing)
    text = edit_async(text, "unknown_zen_catalog_free_id_forwards_as_chat_on_raw_pin_and_stripped_alias", unknown_zen)
    texts[path] = text
    path = "crates/ocg-core/tests/fixtures/gateway_fallback.rs"
    pattern = r"(?ms)^pub\(crate\) fn expected_local_application_models\(state: &Arc<CoreStateInner>\) -> Vec<String> \{.*?^\}\n"
    matches = list(re.finditer(pattern, texts[path]))
    if len(matches) != 1:
        raise ValueError("Expected one application model fixture helper")
    match = matches[0]
    texts[path] = texts[path][:match.start()] + EXPECTED_MODELS + texts[path][match.end():]
    texts[path] = once(texts[path], "use std::collections::{HashMap, HashSet, VecDeque};", "use std::collections::{HashMap, VecDeque};")
    for path, text in texts.items():
        Path(path).write_text(text, encoding="utf-8")


def self_test():
    import unittest
    class Tests(unittest.TestCase):
        def test_async_boundaries(self):
            text = '#[tokio::test]\nasync fn target()\n {\n    assert!(true);\n}\n\nfn helper() {}\n\n#[tokio::test]\nasync fn other() {}\n'
            self.assertEqual(edit_async(text, "target", lambda body: once(body, "true", "false")), text.replace("true", "false"))
        def test_missing_target_fails_closed(self):
            with self.assertRaises(ValueError):
                edit_async("", "missing", lambda _: "")
        def test_single_line_signature(self):
            text = '#[tokio::test]\nasync fn target() {\n}\n'
            self.assertEqual(edit_async(text, "target", lambda body: body), text)
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Tests))
    if not result.wasSuccessful():
        raise SystemExit(1)


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif sys.argv[1:] == ["--stage"]:
        base.SOURCE_BLOBS.update(EXTRA)
        original = base.repair_sources
        def all_repairs():
            original()  # Fingerprint-check every source before the first write.
            repair_extra()
        base.repair_sources = all_repairs
        base.stage()
    else:
        raise SystemExit("Usage: repair-integration-v2.4.2.py --self-test|--stage")
