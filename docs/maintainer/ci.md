[简体中文](ci.zh-CN.md)

# CI Workflows

Workflows live in `.github/workflows/`. This page records the splits that are
not obvious from the YAML.

## quality.yml

Runs on pull requests, on pushes to `main`, and by `workflow_call`. There is
no Compose job. Two jobs:

- **rust** (`ubuntu-22.04`, 30 minutes) — `cargo fmt --all -- --check`, then
  `cargo test --workspace --locked --no-fail-fast --features ocg-core/ollama-cloud-loopback-test`,
  then `cargo clippy --workspace --all-targets --locked` with the same feature
  and `-D warnings`.
- **windows-cli** (`windows-latest`, 30 minutes) — `cargo test -p ocg-cli --locked`
  and `cargo clippy -p ocg-cli --all-targets --locked -- -D warnings`.
  The package builds the `ocg` binary. This job does not pass the Ollama loopback feature.
  Both package references in `.github/workflows/quality.yml` are `ocg-cli`.
  This page does not record a CI run.

Rust DTO schema tests inside the workspace test are the V3/V4 contract gate.
`scripts/cli-acceptance.mjs` is not a job in this file.

## release.yml

This file is the historical desktop publisher. It is still in the tree, and it
is not a usable publication path for the current CLI. This page does not
authorize a release and does not change the workflow.

Preflight and build still run `pnpm/action-setup`, `pnpm install --frozen-lockfile`,
`pnpm run test:tooling`, `pnpm run release:check`, and `pnpm run build`. The
Windows GUI smoke reads the version from `package.json`. The build job still
passes `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, and
`TAURI_UPDATER_PUBLIC_KEY`. This branch has no root `package.json` and no
`src-tauri/` tree, so those steps cannot publish the `ocg` binary from package `ocg-cli`.

The same file still contains desktop package smokes (installer, DMG, AppImage)
and an updater-manifest step from that older publisher. Those steps were not
re-run for this page. Their platform checks are not current evidence. Signed
desktop update stays unavailable on the CLI host; see the [CLI guide](../user/cli.md).

## container.yml

The file triggers on a published GitHub Release and on `workflow_dispatch`
with a tag. This pass read that header and the resolve-job header only. Image
names, digests, and platform results were not re-checked. It is not a
publication path for the current CLI.


## What CI does not cover

`quality.yml` covers Linux workspace Rust tests and Clippy with the loopback
feature, plus Windows native CLI package tests and Clippy. It does not run
Compose, pnpm, a desktop crate, or `scripts/cli-acceptance.mjs`.

`release.yml` is not a current CLI publication. [Releasing](releasing.md)
still describes the removed desktop package, so it is not the current quality
list.

Outside these workflows: third-party client configuration and inference,
backup and restore drills, live upstream accounts, and Google or OpenCode
login. Real payment is not a routine requirement. Database downgrade is
unsupported; rollback uses a pre-upgrade backup as described in
[Storage And Migrations](storage-migration.md).
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](ci.zh-CN.md) · [Docs index](../README.md)
