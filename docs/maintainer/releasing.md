[简体中文](releasing.zh-CN.md)

# Release Procedure

1. **Prepare once.** Review the previous-tag diff and finish affected behavior
   checks using Node.js 22. Set `X.Y.Z` (or `X.Y.Z-beta.N`) in `package.json`,
   `src-tauri/tauri.conf.json`, both `Cargo.toml` files, and the header/default
   images in `compose.example.yaml`. Refresh affected lockfiles with their
   package managers, write paired release notes, and run `pnpm run release:check`.
   Commit the candidate. Local native packaging is needed only when validating
   an affected installer/packaging path or a requested local trial.
2. **Validate the final commit once.** Push/merge to `main` and wait for its
   Quality workflow. It owns full Web/tooling/Rust tests, type checks, Clippy,
   formatting, contracts, design lint, and Compose validation. Fix failures and
   rerun affected local checks before pushing the correction. Do not also require
   the entire local release checklist when CI supplies the same evidence.
3. **Tag and let CI publish.** Annotated-tag the exact successful main commit
   `vX.Y.Z` and push the tag. Preflight checks the latest main Quality run for
   that SHA instead of running it again; missing, failed, or unfinished evidence
   stops the release before native builds. CI then checks signing, builds and
   smokes all three platforms, verifies the draft assets, and publishes that
   draft. Manual platform candidates remain unsigned and do not publish.
4. **Confirm both publications.** The release workflow automatically dispatches
   `container.yml` from `main` for the published tag. Wait for that separate run,
   including its anonymous pulls, both architectures, and paired image digests.
   Read back GitHub Release metadata and the expected image tags. Report the
   release/tag/SHA, both workflow results, and any affected manual checks.

Published assets and tags are immutable. Fix a bad release with a new patch version.

## Check ownership and completion

Keep the candidate fixed during final validation. A different commit needs its
own successful main Quality run. Local focused results remain useful for
unchanged behavior; do not repeat unrelated suites after a narrow fix. Run
Cargo-based checks sharing `target/` sequentially with a stable configuration.

CI owns signatures, checksums, server asset digests, packaged CLI/GUI smokes,
Windows overwrite installation, and anonymous container pulls. Once both
publication workflows and metadata checks pass, stop. Do not download all
platform assets or pull large images locally again just to duplicate CI.
An affected download/update/client path may warrant one representative official
package trial; use isolated data and record cleanup. Local connectivity trouble
does not invalidate successful CI evidence, unless local operation is itself
part of the requested acceptance.

## Extra checks selected by the change

- Gateway routing/protocol, billing, or storage changes: run the affected
  isolated Gateway Lab scenarios against the candidate binary. Require no
  failures or `NOT_RUN`, `remoteCalls=0`, and process/port cleanup. Use the full
  Lab for broad cross-cutting changes. Exercise affected real clients when
  needed, including DSH plugin/CLI flows when those change.
- Connection Center or managed onboarding changes: verify displayed Key
  masking, selected-Key copying, and the affected login/quota path. Real payment
  only when explicitly intended.
- Installer/startup/update changes: check affected native behavior, including
  Windows autostart/uninstall and data preservation, macOS **Open Anyway**, or
  Linux `.deb`/AppImage in a real desktop session as applicable.
- Browser integration changes: check discovery, profile isolation, and cookie
  persistence across restart on affected platforms.

Record tested client versions, platforms, results, and omissions in release
notes. CI smokes and real interactive trials are distinct evidence.

## Recovery

Rerun failed jobs, retaining successful platforms; native build caches are saved
even after a smoke failure. A source correction needs fresh checks for its SHA.
If container dispatch fails, rerun only `dispatch-containers`, or recover with:

```bash
gh workflow run container.yml --ref main -f tag=vX.Y.Z -f publish_latest=true
```

If containers were already dispatched, inspect/recover that run instead of
dispatching again. Container recovery does not require rebuilding desktop assets.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](releasing.zh-CN.md) · [Docs index](../README.md)
