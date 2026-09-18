#!/usr/bin/env python3
"""One-shot, main-only preparation for v2.4.2. Cargo owns Cargo.lock."""
import copy
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

BASE = "27991937903373e24e6000e15793b5fa7047cfce"
OLD = "2.4.1"
NEW = "2.4.2"
ADDED = {
    ".github/workflows/patch-v2.4.2.yml",
    ".github/workflows/quality.yml",
    "scripts/repair-main-v2.4.2.py",
    "crates/ocg-core/src/control/observability.rs",
    "crates/ocg-gateway/src/alias.rs",
    "src/domain/provider-aliases.test.ts",
    "crates/ocg-core/src/provider_contracts/tests.rs",
    "crates/ocg-core/src/state/tests.rs",
    "scripts/prepare-patch-v2.4.2.py",
    "docs/releases/v2.4.2.md",
    "docs/releases/v2.4.2.zh-CN.md",
}
EXPECTED_BLOBS = {
    "package.json": "b89a63d75ea1fd7fc7c7b7b1b3a240ba262c516c",
    "Cargo.toml": "4c11f7b4084905733969b21dd393798351812324",
    "src-tauri/Cargo.toml": "ec35856c24014fd4e17d5147393741a6b0777ddc",
}


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f"Expected exactly one unchanged release anchor: {old[:90]!r}")
    return text.replace(old, new, 1)


def bump_json(text):
    data = json.loads(text)
    if data.get("version") != OLD:
        raise ValueError("Unexpected manifest version")
    data["version"] = NEW
    return json.dumps(data, indent=2, ensure_ascii=False) + "\n"


def bump_toml(text):
    return replace_once(text, f'version = "{OLD}"\n', f'version = "{NEW}"\n')


def verify_lock(old_text, new_text):
    old = tomllib.loads(old_text)
    new = tomllib.loads(new_text)
    local_names = {p["name"] for p in old["package"] if "source" not in p}
    normalized = copy.deepcopy(new)
    for package in normalized["package"]:
        if package["name"] in local_names and "source" not in package:
            if package["version"] != NEW:
                raise ValueError(f"Workspace package was not bumped: {package['name']}")
            package["version"] = OLD
        deps = package.get("dependencies", [])
        for i, dep in enumerate(deps):
            for name in local_names:
                if dep == f"{name} {NEW}":
                    deps[i] = f"{name} {OLD}"
    if old != normalized:
        raise ValueError("Cargo.lock changed beyond workspace version numbers; refusing release")


def prepare():
    if git("status", "--porcelain"):
        raise ValueError("Preparation requires a clean checkout")
    subprocess.run(["git", "merge-base", "--is-ancestor", BASE, "HEAD"], check=True)
    changed = set(git("diff", "--name-only", BASE, "HEAD").splitlines())
    if not changed <= ADDED:
        raise ValueError(f"Source moved outside the reviewed main snapshot: {changed - ADDED}")
    for path, expected in EXPECTED_BLOBS.items():
        if git("hash-object", path) != expected:
            raise ValueError(f"Source fingerprint changed: {path}")
    updates = {}
    for path in ("package.json", "src-tauri/tauri.conf.json"):
        updates[path] = bump_json(Path(path).read_text())
    package = json.loads(updates["package.json"])
    package["scripts"]["test"] = replace_once(package["scripts"]["test"],
        "pnpm run test:web && ", "pnpm run test:web && pnpm run test:tooling && ")
    updates["package.json"] = json.dumps(package, indent=2, ensure_ascii=False) + "\n"
    for path in ("Cargo.toml", "src-tauri/Cargo.toml"):
        updates[path] = bump_toml(Path(path).read_text())
    compose = Path("compose.example.yaml").read_text()
    for old, new in (
        (f"# Pull-only Docker Compose example for Open Console Gateway v{OLD}.", f"# Pull-only Docker Compose example for Open Console Gateway v{NEW}."),
        (f"${{OCG_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr:{OLD}}}", f"${{OCG_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr:{NEW}}}"),
        (f"${{OCG_BROWSER_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr-browser:{OLD}}}", f"${{OCG_BROWSER_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr-browser:{NEW}}}"),
    ):
        compose = replace_once(compose, old, new)
    updates["compose.example.yaml"] = compose
    # All preconditions are checked before modifying any tracked file.
    for path, text in updates.items():
        Path(path).write_text(text)
    print(f"Prepared {NEW} from main {BASE}; Cargo.lock is deliberately untouched.")


def self_test():
    import unittest
    class Tests(unittest.TestCase):
        def test_bump_preserves_unrelated_json(self):
            data = {"version": OLD, "nested": {"version": OLD}, "name": "test"}
            actual = json.loads(bump_json(json.dumps(data)))
            self.assertEqual(actual, dict(data, version=NEW))
        def test_rejects_unexpected_version(self):
            for version in (NEW, "2.5.0", None, "2.4.1-beta.1"):
                with self.assertRaises(ValueError):
                    bump_json(json.dumps({"version": version}))
        def test_preserves_dependency_versions(self):
            text = f'[package]\nversion = "{OLD}"\n[dependencies]\nother = "{OLD}"\n'
            self.assertEqual(bump_toml(text), text.replace(f'version = "{OLD}"', f'version = "{NEW}"'))
        def test_ambiguous_anchor_is_rejected(self):
            with self.assertRaises(ValueError):
                replace_once("old old", "old", "new")
        def test_workspace_lock_bump_is_accepted(self):
            old = f'[[package]]\nname="ocg-core"\nversion="{OLD}"\n[[package]]\nname="external"\nversion="1.0.0"\nsource="registry+x"\n'
            verify_lock(old, old.replace(f'version="{OLD}"', f'version="{NEW}"'))
        def test_dependency_drift_is_rejected(self):
            old = f'[[package]]\nname="ocg-core"\nversion="{OLD}"\n[[package]]\nname="external"\nversion="1.0.0"\nsource="registry+x"\n'
            new = old.replace(f'version="{OLD}"', f'version="{NEW}"').replace('version="1.0.0"', 'version="1.1.0"')
            with self.assertRaises(ValueError):
                verify_lock(old, new)
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Tests))
    if not result.wasSuccessful():
        raise SystemExit(1)


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif sys.argv[1:] == ["--verify-lock"]:
        verify_lock(git("show", "HEAD:Cargo.lock"), Path("Cargo.lock").read_text())
        print("Cargo.lock contains only the intended workspace version changes.")
    elif not sys.argv[1:]:
        prepare()
    else:
        raise SystemExit("Usage: prepare-patch-v2.4.2.py [--self-test|--verify-lock]")
