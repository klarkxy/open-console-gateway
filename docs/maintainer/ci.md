[简体中文](ci.zh-CN.md)

# CI Workflows

Workflows live in `.github/workflows/`. This page records the splits that are
not obvious from the YAML.

## quality.yml

Runs on pull requests and `main`, and via `workflow_call` from a production
tag. Manual release candidates skip it. Three parallel jobs:

- **Web** — `contract:v3:check`, `typecheck`, `test:web`, Vite production
  build, `DESIGN.md` lint, and
  `docker compose -f compose.example.yaml config --quiet`.
  `pnpm run test:tooling` is not in this job.
- **Rust** — `cargo fmt --all -- --check`, locked workspace tests and Clippy
  `-D warnings` with `--exclude ocg-manager` (the desktop crate needs WebKit
  headers and a `dist/index.html` stub; Windows covers it; Linux `src-tauri`
  compile is the release matrix).
- **Windows Tauri** — `cargo test -p ocg-manager --lib` and Clippy `-D warnings`
  against a stub `dist/index.html`. Also covers Windows auto-start registry
  sync.

## release.yml

Triggers: `workflow_dispatch` and `v*` tags.

- Manual dispatch: unsigned smoke artifacts for the selected platforms,
  even if the ref is a tag.
- `v*` tag **push**: full three-platform matrix, repository signing
  secrets, quality gate plus Ubuntu preflight (version manifests, release
  helper tests, signing pair vs `src-tauri/updater-public-key.sha256`).
  Then native builds, CLI/GUI smokes, `draft-release` → `verify-release`
  → `publish-release`.

`verify-release` requires GitHub asset names to match the assembled
`release/` set (currently 16 files). The draft job passes a numeric
Release ID because the tag lookup endpoint does not expose drafts.
Publication is serialized on `release-moving-channels`. `latest` advances
only for a strictly newer stable SemVer. Prerelease tags set
`prerelease=true` and `make_latest=false`.

Windows GUI smoke is `scripts/smoke-windows-release.ps1` (V3 CAS for
auto-start and the in-place NSIS `/UPDATE` path). macOS checks universal
`lipo` plus ad-hoc `codesign`, and re-runs Unix CPA process-ownership
tests (`cpa_runtime::host`) that Linux already covered in quality.yml.
Linux launches the AppImage under Xvfb.

## Updater signing

Generate the key once outside the checkout:

```powershell
node node_modules/@tauri-apps/cli/tauri.js signer generate -w <secure-path>/ocg-updater.key
```

Store private key and password as repository secrets
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
Store public-key **content** in the `TAURI_UPDATER_PUBLIC_KEY` variable.
`src-tauri/updater-public-key.sha256` is the committed trust anchor;
rotation is break-glass (new pair, direct-install bootstrap for existing
clients, reviewed fingerprint change). Keep two independent encrypted
backups. Updater signatures are not OS code signing. Windows installers
are unsigned; macOS uses ad-hoc (`-`).

## container.yml

A Release published with `github.token` does not start this workflow.
After the signed tag pipeline, dispatch it for that tag
(`publish_latest=true` for stable).

Native builds: amd64 on `ubuntu-24.04`, arm64 on `ubuntu-24.04-arm`.
Smoke (main + browser) runs on amd64 only. Images push by digest first;
user-visible tags are created only after local OCI-index preflight,
anonymous pull of both exact version tags, and GitHub provenance.
`X.Y.Z` and `sha-*` are immutable; `X.Y` and `latest` are monotonic
moving channels. Browser is a GHCR package, not a Release asset.

A new browser package is private until someone sets it **Public**; the
first run is expected to stop at the anonymous-pull gate, then a
same-digest rerun completes publication. Later releases must pass that
gate on the first run.

## pages.yml

Publishes `docs/` to GitHub Pages (`docs/index.html`). Set the repository
Pages source to **GitHub Actions** before the first deployment.

## What CI does not cover

Quality covers frontend + Linux Rust excluding the desktop crate + Windows
desktop unit tests. Native installer smokes run on candidates and tags.
Container smoke is amd64 only.

Coverage outside these workflows includes real desktop interaction, third-party
client configuration and inference, backup/restore, live upstream accounts,
Google/OpenCode login, noVNC input, and cookie persistence across restart.
Choose applicable manual checks using [Release Procedure](releasing.md) and
record omissions. Real payment is not a routine release requirement. Database
downgrade is unsupported; rollback uses a pre-upgrade backup as described in
[Storage And Migrations](storage-migration.md).
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](ci.zh-CN.md) · [Docs index](../README.md)
