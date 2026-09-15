[简体中文](releasing.zh-CN.md)

# Release Procedure

1. Set `X.Y.Z` (or `X.Y.Z-beta.N`) in `package.json`,
   `src-tauri/tauri.conf.json`, both `Cargo.toml` files, and the header
   plus default images in `compose.example.yaml`.
2. Refresh lockfiles with the usual Cargo/pnpm commands, then run
   `pnpm run test`, `pnpm run test:tooling`, `pnpm run design:lint`,
   `pnpm run contract:v3:check`, `pnpm run contract:v4:check`,
   `pnpm run release:check`, and
   `pnpm run build`. Commit generated lockfile diffs; do not hand-edit them.
3. Review the previous-tag diff and current-platform `release/` payloads,
   then commit version, lockfile, documentation, and release-note changes.
4. Merge first. On the commit already on `main`, annotated-tag
   `vX.Y.Z` and push it. Do not tag a commit that will later be squash-merged.
5. Wait for quality, preflight, the native matrix, `draft-release`,
   `verify-release`, and `publish-release`. Confirm publication converted
   that same draft.
6. Dispatch `container.yml` for the tag
   (`gh workflow run container.yml --ref main -f tag=vX.Y.Z -f publish_latest=true`),
   confirm both GHCR packages are public, and anonymously pull both
   full-version tags.

Published assets and tags are immutable. Fix a bad release with a new
patch version.

## Human checks CI does not run

Protocol, schema, CAS, and local-list behavior belong in `cargo test` /
`pnpm run test`, not this list. Select real-client checks by the affected
Gateway protocols. Record the tested client versions, platforms, results,
and omissions in the release notes; previous runs do not grant exemptions.

- [ ] Quality gate, signed `release:check`, and selected platform smokes
      are green; the four version manifests, `compose.example.yaml`, and
      workspace `Cargo.lock` entries agree.
- [ ] Exercise affected client paths with text and tool calls (for example,
      Gemini CLI when Gemini compatibility changes). The DSH Applications
      flow is in scope when it changes, including its native plugin and CLI
      smokes.
      Verify that Connection Center masks displayed Keys and copies the selected Key.
- [ ] Optional managed onboarding (sign-in identity → invite URL →
      OpenCode login → payment review → key paste). Real payment only when
      explicitly intended. Refresh quota against official
      `/zen/go/v1/usage` on a ready Key account and a ready managed
      account.
- [ ] Windows: SmartScreen text, dashboard, one account, one request,
      `auto_start` ↔ `HKCU\...\Run\Open Console Gateway`, value gone after uninstall.
      Running the installer over an existing copy keeps `%USERPROFILE%\.ocg-mgr`.
      Interactive uninstall deletes that directory only with **Delete application data** checked.
- [ ] macOS: **Open Anyway**, dashboard, one account, one request.
- [ ] Linux: `.deb` and AppImage under a real Wayland or X11 session.
- [ ] Browser discovery (Edge/Chrome on Windows; platform browsers on
      macOS/Linux), profile isolation, cookie persistence across restart.
- [ ] After publish: `container.yml` green, anonymous pull of both images
      at the expected digests, GitHub Release asset set unchanged.
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](releasing.zh-CN.md) · [Docs index](../README.md)
