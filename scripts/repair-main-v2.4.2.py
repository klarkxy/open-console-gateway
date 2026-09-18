#!/usr/bin/env python3
"""Apply reviewed, fingerprint-locked main repairs and stage a commit, not a branch."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = "klarkxy/open-console-gateway"
SOURCE_BLOBS = {
    "crates/ocg-core/src/control/observability.rs": "8873dbbc25e08a57d9b625992d3edb0914a21df4",
    "crates/ocg-gateway/src/alias.rs": "b3883650d2907e92c301638ca142d1e36948900e",
    "src/domain/provider-aliases.test.ts": "390b076842ed5cdd73668e707423e36bc8b81dca",
    "crates/ocg-core/src/provider_contracts/tests.rs": "620165e46522c4e250e652f22669b300ac33dee3",
    "crates/ocg-core/src/state/tests.rs": "7ae6a1b96aa6fd7b87e2aa0afe65c7bdd9f839fe",
}


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def once(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f"Repair anchor must occur exactly once: {old[:100]!r}")
    return text.replace(old, new, 1)


def edit_test(text, name, transform):
    pattern = rf"(?ms)^#\[test\]\nfn {re.escape(name)}\(\) \{{.*?^\}}\n"
    matches = list(re.finditer(pattern, text))
    if len(matches) != 1:
        raise ValueError(f"Expected exactly one test function: {name}")
    match = matches[0]
    return text[:match.start()] + transform(match.group()) + text[match.end():]


ZEN_TEST = '''#[test]
fn unknown_zen_free_catalog_row_requires_explicit_protocol_enablement() {
    let catalog = ZenFreeModelCatalog {
        models: vec!["brand-new-promo-free".into()],
        refreshed_at: None,
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.to_string(),
    };
    let mut persisted = empty_persisted();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    // A catalog ID and the non-null preferred-protocol placeholder are not
    // protocol evidence. Auto must remain off, including after a ForceOn reset.
    for state in [
        ProtocolOverrideState::Auto,
        ProtocolOverrideState::ForceOn,
        ProtocolOverrideState::ForceOff,
        ProtocolOverrideState::Auto,
    ] {
        persisted.overrides.insert(
            scope.clone(),
            vec![PersistedModelProtocolOverride {
                scope: scope.clone(),
                model_id: "brand-new-promo-free".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                state,
                updated_at: Utc::now(),
            }],
        );
        let set = build_effective_contracts(&catalog, &[], persisted.clone());
        let zen = set.providers.get(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
        let model = zen.model("brand-new-promo-free").unwrap();
        let chat = model.protocols.get("chat_completions").unwrap();
        let explicitly_enabled = state == ProtocolOverrideState::ForceOn;
        assert_eq!(model.preferred_protocol, UpstreamProtocolKind::ChatCompletions);
        assert_eq!(chat.r#override, state);
        assert_eq!(chat.available, explicitly_enabled);
        assert_eq!(chat.enabled, explicitly_enabled);
        assert_eq!(model.routable, explicitly_enabled);
        let selected = select_upstream_protocol(
            zen,
            ApiFormat::ChatCompletions,
            "brand-new-promo-free",
        );
        if explicitly_enabled {
            assert_eq!(selected.unwrap(), ApiFormat::ChatCompletions);
            assert_eq!(model.enabled_protocols(), vec![UpstreamProtocolKind::ChatCompletions]);
        } else {
            assert!(selected.is_err());
            assert!(model.enabled_protocols().is_empty());
        }
    }
}
'''


def repair_o01(body):
    body = once(body, "    assert!(chat.available);\n    assert!(!chat.enabled);", "    assert!(!chat.available);\n    assert!(!chat.enabled);")
    return once(body, '''    // Clearing the refresh-written force_off rows (the matrix "开启" writes
    // auto) enables the provider default protocol and makes the row routable.
    let mut reenabled = persisted;
    reenabled.overrides.clear();
    let set = build_effective_contracts(&zen_seed(), &[], reenabled);
''', '''    // Clearing ForceOff is not evidence for an unknown model's protocol.
    let mut reenabled = persisted;
    reenabled.overrides.clear();
    let set = build_effective_contracts(&zen_seed(), &[], reenabled.clone());
    let go = set.providers.get(OPENCODE_PROVIDER_ID).unwrap();
    let model = go.model("omen-alpha").unwrap();
    assert!(model.enabled_protocols().is_empty());
    assert!(!model.routable);
    assert!(select_upstream_protocol(go, ApiFormat::ChatCompletions, "omen-alpha").is_err());

    // An explicit operator choice admits only that protocol.
    reenabled.overrides.insert(
        scope.clone(),
        vec![PersistedModelProtocolOverride {
            scope,
            model_id: "omen-alpha".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            state: ProtocolOverrideState::ForceOn,
            updated_at: now,
        }],
    );
    let set = build_effective_contracts(&zen_seed(), &[], reenabled);
''')


def repair_state(body):
    body = once(body,
        "use crate::provider::OPENCODE_PROVIDER_ID;",
        "use crate::provider::{OPENCODE_PROVIDER_ID, UpstreamProtocolKind};")
    body = once(body,
        "use crate::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope};",
        "use crate::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope, ProtocolOverrideState};")
    return once(body, "    state.reload_provider_contracts().unwrap();", '''    // This test concerns removing an enabled route, not protocol discovery.
    // The synthetic model IDs therefore need an explicit operator choice.
    state
        .db
        .lock()
        .set_model_protocol_overrides(
            &scope,
            &["gpt-5.6-luna", "gpt-5.6-sol"].map(|id| {
                (
                    id.to_string(),
                    UpstreamProtocolKind::ChatCompletions,
                    ProtocolOverrideState::ForceOn,
                )
            }),
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();''')


def repair_sources():
    updates = {}
    for path, expected in SOURCE_BLOBS.items():
        raw = Path(path).read_bytes()
        actual = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
        if actual != expected:
            raise ValueError(f"Unreviewed source fingerprint: {path}")
        updates[path] = raw.decode("utf-8")
    path = "crates/ocg-core/src/control/observability.rs"
    if updates[path].count("HashSet") != 1:
        raise ValueError("HashSet is no longer an unused import")
    updates[path] = once(updates[path], "use std::collections::{BTreeMap, HashSet};", "use std::collections::BTreeMap;")
    path = "crates/ocg-gateway/src/alias.rs"
    updates[path] = once(updates[path], "catalogs.go.iter().any(|id| *id == mapping.upstream_model)", "catalogs.go.contains(&mapping.upstream_model)")
    path = "src/domain/provider-aliases.test.ts"
    updates[path] = once(updates[path], '''    {
      provider_id: "custom",
      key: "custom:custom-1:public-model:vendor/model:free",''', '''    {
      provider_id: "opencode",
      key: "provider:go:raw-only-model:raw-only-model",
      public_model: "raw-only-model",
      provider_plan: "OpenCode Go",
      custom_account: null,
      upstream_model: "raw-only-model",
      routable: true,
      custom_account_id: null,
    },
    {
      provider_id: "custom",
      key: "custom:custom-1:public-model:vendor/model:free",''')
    updates[path] = once(updates[path], '''    "provider:go:gpt-5.6:gpt-5.6-upstream",
    "custom:custom-1:public-model:vendor/model:free",''', '''    "provider:go:gpt-5.6:gpt-5.6-upstream",
    "provider:go:raw-only-model:raw-only-model",
    "custom:custom-1:public-model:vendor/model:free",''')
    updates[path] += '''

test("Go raw model rows preserve disabled protocol state and account filtering", () => {
  const scope = {
    ...builtinScope,
    models: [{ ...builtinScope.models[1]!, routable: false }],
  } as ProviderScopeView;
  const rows = providerAliasRows([scope], [goAccount]);
  assert.equal(rows.length, 1);
  assert.equal(rows[0]?.public_model, "raw-only-model");
  assert.equal(rows[0]?.routable, false);
  assert.deepEqual(providerAliasRows([scope], [{ ...goAccount, enabled: false }]), []);
});
'''
    path = "crates/ocg-core/src/provider_contracts/tests.rs"
    updates[path] = edit_test(updates[path], "unknown_zen_free_catalog_row_defaults_to_chat_and_honors_force_off", lambda _: ZEN_TEST)
    updates[path] = edit_test(updates[path], "placeholder_empty_zen_scope_still_uses_snapshot_until_a_catalog_is_saved", lambda body: once(body,
        "model.enabled_protocols() == vec![UpstreamProtocolKind::ChatCompletions]",
        "model.enabled_protocols().is_empty() && !model.routable"))
    updates[path] = edit_test(updates[path], "o01_catalog_discovered_model_stays_off_until_explicitly_enabled", repair_o01)
    path = "crates/ocg-core/src/state/tests.rs"
    updates[path] = edit_test(updates[path], "reload_failure_restriction_rebuilds_proxy_membership", repair_state)
    # Fingerprints and all anchors are checked before the first write.
    for path, content in updates.items():
        Path(path).write_bytes(content.encode("utf-8"))


def api(path, payload=None):
    args = ["gh", "api", f"repos/{REPO}/{path}"]
    if payload is not None:
        args += ["--method", "POST", "--input", "-"]
    result = subprocess.run(args, input=None if payload is None else json.dumps(payload),
        check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def stage():
    if os.environ.get("GITHUB_REPOSITORY") != REPO or os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise ValueError("Only the requested repository main is permitted")
    parent = git("rev-parse", "HEAD")
    if parent != os.environ.get("GITHUB_SHA") or git("status", "--porcelain"):
        raise ValueError("Require a clean checkout of the triggering main commit")
    if api("git/ref/heads/main")["object"]["sha"] != parent:
        raise ValueError("main advanced; refusing to stage a stale repair")
    if api("releases/latest")["tag_name"] != "v2.4.1":
        raise ValueError("The expected base release has changed")
    repair_sources()
    subprocess.run(["cargo", "fmt", "--all"], check=True)
    subprocess.run(["git", "diff", "--check"], check=True)
    changed = set(git("diff", "--name-only").splitlines())
    if changed != set(SOURCE_BLOBS):
        raise ValueError(f"Unexpected repair files: {changed}")
    tree = api("git/trees", {
        "base_tree": git("rev-parse", "HEAD^{tree}"),
        "tree": [{"path": path, "mode": "100644", "type": "blob",
                  "content": Path(path).read_text(encoding="utf-8")} for path in sorted(changed)],
    })
    commit = api("git/commits", {
        "message": "fix: align main catalog regressions with explicit protocol enablement\n\nPreserve Go raw aliases and unknown-model safety. Repair stale test fixtures and two lint blockers. Staged for the complete Quality gate before main is advanced.",
        "tree": tree["sha"], "parents": [parent],
    })
    sha = commit["sha"]
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"sha={sha}\n")
    print(f"Staged main-derived candidate {sha}; no branch or tag was created or updated.")


def self_test():
    import unittest
    class Tests(unittest.TestCase):
        def test_named_edit_preserves_adjacent_tests_and_helpers(self):
            source = '#[test]\nfn target() {\n    assert!(true);\n}\n\nfn helper() {}\n\n#[test]\nfn other() {}\n'
            changed = edit_test(source, "target", lambda body: once(body, "true", "false"))
            self.assertEqual(changed, source.replace("true", "false"))
        def test_missing_test_is_rejected(self):
            with self.assertRaises(ValueError):
                edit_test("fn unrelated() {}\n", "target", lambda _: "")
        def test_ambiguous_anchor_is_rejected(self):
            with self.assertRaises(ValueError):
                once("same same", "same", "different")
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Tests))
    if not result.wasSuccessful():
        raise SystemExit(1)


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif sys.argv[1:] == ["--stage"]:
        stage()
    else:
        raise SystemExit("Usage: repair-main-v2.4.2.py --self-test|--stage")
