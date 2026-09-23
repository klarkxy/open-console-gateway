[简体中文](release-artifacts.zh-CN.md)

# Release Artifacts

Open Console Gateway ships desktop installers for three platforms, a CLI archive for each,
and a multi-arch container image.

| Runner | GUI | CLI |
| --- | --- | --- |
| Windows 10/11 x64 | NSIS current-user setup | x64 ZIP |
| macOS 11+ | Universal DMG (x64 + ARM64) | Universal tar.gz |
| Linux x64 | AppImage + deb | x64 tar.gz |

Stable delivery names are:

```text
ocg-manager_<version>_windows-x64-setup.exe
ocg-manager_<version>_windows-x64-setup.exe.sig
ocg-manager-cli_<version>_windows-x64.zip
ocg-manager_<version>_macos-universal.dmg
ocg-manager_<version>_macos-universal.app.tar.gz
ocg-manager_<version>_macos-universal.app.tar.gz.sig
ocg-manager-cli_<version>_macos-universal.tar.gz
ocg-manager_<version>_linux-x64.AppImage
ocg-manager_<version>_linux-x64.AppImage.sig
ocg-manager_<version>_linux-x64.deb
ocg-manager_<version>_linux-x64.deb.sig
ocg-manager-cli_<version>_linux-x64.tar.gz
compose.example.yaml
cpa-config.example.yaml
latest.json
SHA256SUMS
```

Each CLI archive ships with its executable, a `dist/` directory, and `LICENSE`.
`serve` needs the sibling dashboard assets, so distribute the whole archive.
Windows has no portable GUI artifact.
The `ocg-manager` Codex skill is embedded in the desktop and CLI binaries,
not a separate archive member. Native release startup synchronizes that exact
build's skill into the current user's `~/.agents/skills`; the packaged CLI
smoke uses an isolated home and checks the installed copy. The Docker build
does not synchronize a skill into the container or host.

The `linux/amd64` and `linux/arm64` containers are published separately as
`ghcr.io/klarkxy/opencode-go-mgr`. A GitHub Release contains the seven platform
payloads, the extra macOS updater archive, four updater signatures, the Compose
and CPA configuration examples, `latest.json`, and `SHA256SUMS` — currently 16
attachments. The local verifier and the workflow both require the GitHub asset
names and count to match the assembled `release/` directory exactly.

## scripts/release.mjs

`pnpm run build` runs this script. It checks version pins, optional updater
signing (`OCG_REQUIRE_UPDATER_ARTIFACTS=1` fail-closes without keys; the public
key must match `src-tauri/updater-public-key.sha256`), builds the current
platform via `@tauri-apps/cli`, packages the CLI with `dist/` and `LICENSE`,
writes `SHA256SUMS`, and atomically replaces `release/`. Unsigned local builds
are smoke-only.

macOS updater archives require the `app` bundle target. deb is signed with
`tauri signer sign` because it is not a native Tauri updater artifact.
`pnpm run release:check` is the same validation without a native build.
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](release-artifacts.zh-CN.md) · [Docs index](../README.md)
