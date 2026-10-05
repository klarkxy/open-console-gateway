[简体中文](README.zh-CN.md)

# Archived checkpoint evidence

Read the [work checkpoint](../../work-checkpoint-2026-10-05.md) first. These files preserve selected plans, source reviews and bounded observations before a workstation reinstall. Old dispatch instructions and worker/runtime leases are historical and closed. The archive grants no execution, credential access, publication or acceptance.

- `plans/`: selected hermetic instance-map contract, signatures, fitness and native gate plan. Finish the partial Rust consumers before compilation.
- `reviews/`: rollback, harness cleanup, retained recovery, native CLI archive gate and native SDK source reviews. READY means the exact reviewed source checkpoint, not executed tests.
- `native-critical/`: uncompiled SDK/host test adjuncts and their source receipt. Copy the two SDK files into `sdk/cliproxy/auth` of a fresh pinned source tree and the host file into the isolated host module; do not silently replace shipping source.
- `reports/`: primary CPA suite exits, G failures/interrupted log, restore observation and source/hash snapshots. Synthetic databases, credentials, binary archives and full generated dependency trees are excluded.
- `MANIFEST.json`: SHA-256 values of the archived copies. Verify this archive before reusing its exact source or results.

Recreate pinned CPA sources with the committed build script. Preserve its existing host-module `replace` relationship to the selected patched tree. Use cached/offline dependencies when available and local fake servers; no real provider or installed profile is part of these tests. Source-reviewed SDK cases remain UNRUN, regardless of the earlier dispatch-guard result. Linux/macOS compilation remains separate from native execution.
