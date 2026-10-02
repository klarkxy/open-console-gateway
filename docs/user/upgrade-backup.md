[简体中文](upgrade-backup.zh-CN.md)

# Upgrade, Backup, Restore, And Uninstall

Download upgrades from the
[latest GitHub Release](https://github.com/klarkxy/open-console-gateway/releases/latest)
and verify them against the release's `SHA256SUMS`:
`Get-FileHash <file> -Algorithm SHA256` on PowerShell, `shasum -a 256 <file>`
on macOS, or `sha256sum <file>` on Linux. Backups, restores, and removal are
the kind of operations that are boring right up until they aren't.

On Windows, install, in-app update, and running the setup again all reuse the
existing installation directory. The installer never uninstalls first. The
upgrade keeps the data directory and auto-start setting and migrates existing
desktop and Start-menu shortcuts. Uninstall only from Windows **Installed apps**.

## Database Migration And Access Keys (Schema v66)

The database schema is **v66**; historical databases migrate in place on
startup. The primary access key keeps the fixed id
`00000000-0000-0000-0000-000000000001`, so clients keep authenticating with
the same value across upgrades. The `access_keys` table holds the primary key
plus up to 64 non-deleted sub keys; deleting a sub key clears its plaintext
but keeps the name for log attribution.

Before protected schema migrations (v27, v35, v42, v48, v58, v59), the migrator writes
a unique, never-overwritten sibling snapshot — `data.sqlite.pre-v3.<timestamp>.bak`,
`data.sqlite.pre-v35.<timestamp>.bak`, `data.sqlite.pre-v42.<timestamp>.bak`,
`data.sqlite.pre-v48.<timestamp>.bak`, `data.sqlite.pre-v58.<timestamp>.bak`, or `data.sqlite.pre-v59.<timestamp>.bak` — plus a SHA-256 sidecar. Very old databases (schema 1–22 / 1–23) also write `data.sqlite.pre-v22.` / `pre-v23.` snapshots. A fresh
empty data directory creates schema v66 directly and skips the snapshot. That
snapshot is a rollback point, not a substitute for a complete backup: verify
the sidecar before restoring it, and restore it only onto a binary that can
open that schema version or to retry an upgrade that never committed. Never
open a migrated database with an older build — newer Keys do not authenticate
there, and a revoked value cannot come back to life by downgrading.
Migrations are fail-closed: data a rewrite cannot migrate safely (for example
a non-empty leftover legacy table) refuses the upgrade instead of being
dropped.

### Portable Node Backup Payloads

Node backups export payload V12 with destinations and credentials as the
authority, including model-resolution policy and per-model route overrides
(secrets and identity extras stay inside the encrypted envelope). V11 also
carries explicit HTTP protocol routes; a pre-V11 backup carrying nonempty
explicit routes is rejected rather than losing those routes. V12 adds each
GOAT Key's plan-window map as a separate field. V4–V12 backups remain importable. A backup older than V12 that carries the plan-window field is rejected. A V4–V11 import is ordinary-only: the same Key keeps the map already on this host, and a changed Key drops it. A V12 import of the same Key merges the later deadline in each window; a changed Key drops the old host map, then applies a valid incoming map. Same-Key preservation keeps or merges that map only while the incoming credential is still GOAT; moving the same id and the same plaintext to a non-GOAT provider, including Custom HTTP, remains a supported remap and discards only the GOAT map while ordinary cooldowns stay. V7 receives deterministic resolution defaults.
Payload V1–V3 backups, and anything newer than V12, are rejected with an
explicit unsupported-version error; that is not a wrong password or a damaged
file. Schema 66 is additive and has no separate pre-upgrade snapshot. An older binary refuses a v66 database. Roll back by restoring the complete pre-upgrade data directory and opening it with the earlier binary. Schema 66 and payload V12 are internal storage versions, not the product release version. A V12 backup is for a current or newer reader; keep an earlier backup for an earlier binary. The maintainer-facing payload policy is documented in [Runtime invariants](../maintainer/runtime-invariants.md).

Supplier card IDs, grouping and order travel in V9 backups. Multiple cards can reference the same supplier without duplicating its configuration or Keys. During a merge, existing accounts keep their order and card membership; source grouping applies to newly imported accounts. Older backups retain their saved credential priority and receive matching cards on import.

V10 also carries each account's stored credit configuration, remaining buckets, and monthly issuance cursor. A merge keeps the destination's existing balance; an older backup does not reset it. Export and import do not reprice stored balances or settle a historical pending receipt. Import keeps legacy rates with that history. The monthly expiry, expired buckets, configuration, counters, and monthly cursor stay exact. Binding and meter ids are new. The configuration you edit is the name, currency, monthly amount, and source URL. A new request does not deduct this balance.

## Backup

1. Stop every process using the data: choose **Quit** from the desktop tray,
   stop the CLI with Ctrl+C or its service manager, or run
   `docker compose stop`.
2. Copy the **entire** GUI or CLI data directory. Desktop
   `browser-profiles/` is already inside the GUI data directory. For Docker,
   back up both sensitive volumes: `ocg-data` and `ocg-browser-profiles`.
   With the containers stopped, run
   `docker compose cp ocg-manager:/data/. ../ocg-data-backup` and
   `docker compose cp ocg-manager:/browser-profiles/. ../ocg-browser-profiles-backup`.
3. Keep the backup outside the repository, and check that it contains
   `data.sqlite` and, where present, `.encryption-key`. Browser profiles hold
   long-lived cookies and login state and are not encrypted by Open Console Gateway;
   protect them like account keys and the database.

## Restore

1. Stop the process, move the current data aside, and copy the whole backup
   back to its original directory or an empty Docker volume.
2. Start the same or a newer version.

Caveats:

- Docker files in `/data` must remain writable by UID/GID `10001`.
- Docker files in `/browser-profiles` must also remain writable by UID/GID
  `10001`.
- Windows GUI obfuscation is bound to the Windows user and machine, so its
  data cannot restore account keys or passwords on another machine — create
  fresh data there and re-enter the credentials.
- macOS/Linux GUI, CLI, and Docker restores must preserve `.encryption-key`
  or the explicitly supplied `--encryption-key` /
  `OCG_MANAGER_ENCRYPTION_KEY` value.
- Open a migrated database only with the same or a newer build.

## Docker Restore Into A Fresh Volume

Verify the backup first and make sure `.env` pins the intended same or
newer image. The `docker compose down -v` command below permanently deletes
all current named volumes; run it only after both persistent data sets are
safely elsewhere:

```bash
docker compose down -v
docker compose run --rm --no-deps --user root \
  --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER \
  --entrypoint sh \
  --volume ../ocg-data-backup:/backup/data:ro \
  --volume ../ocg-browser-profiles-backup:/backup/browser-profiles:ro \
  ocg-manager \
  -c 'cp -a /backup/data/. /data/ && \
      cp -a /backup/browser-profiles/. /browser-profiles/ && \
      chown -R 10001:10001 /data /browser-profiles && \
      find /data /browser-profiles -type d -exec chmod 700 {} + && \
      find /data /browser-profiles -type f -exec chmod 600 {} +'
docker compose --profile browser up -d --no-build
docker compose ps
```

If the original deployment used `OCG_MANAGER_ENCRYPTION_KEY`, put the same
secret back into `.env` before the restore. Keep the backup until the
dashboard, accounts, and a real gateway request have all been verified.

## Upgrade And Uninstall By Surface

The direct GUI steps also work when in-app update is unavailable.

The first successful desktop launch after an upgrade synchronizes the bundled
Codex skill. A native release CLI synchronizes it on the next `serve`, or immediately
with `skill sync`; this also means an intentional binary downgrade installs
that binary's matching skill version. When its content differs, the previous
OCG-managed skill is backed up under `~/.agents/skill-backups/`. Uninstalling the app or CLI leaves the
user-level skill in place so it can still guide a reinstall; remove it
separately only if no other OCG installation uses it.

- **Windows GUI:** quit the tray app and run the new installer; it replaces
  the existing copy in place. Uninstall from Windows **Installed apps**. The
  confirm page deletes `%USERPROFILE%\.ocg-mgr` only when you select **Delete
  application data**. Reinstalling after an uninstall that left the data
  directory in place restores the same configuration.
- **macOS GUI:** replace the app in **Applications** with the new DMG copy.
  Delete the app to uninstall; remove `~/.ocg-mgr` separately only when you
  also intend to delete the data.
- **Linux GUI:** install the new `.deb` over the old package, or replace the
  AppImage. Remove the package or AppImage to uninstall; data remains in
  `~/.ocg-mgr` until you delete it.
- **CLI:** replace the extracted package as a unit so the executable,
  `dist/`, and `LICENSE` stay together. Delete that package to uninstall;
  data remains in `~/.ocg-mgr-cli` or the custom `--data-dir`.
- **Docker:** after backing up, run `docker compose pull` followed by
  `docker compose up -d --no-build`. If the browser profile is enabled, use
  `docker compose --profile browser pull` followed by
  `docker compose --profile browser up -d --no-build` so both images are
  upgraded together. Pin `OCG_IMAGE` and `OCG_BROWSER_IMAGE` to full release tags
  for repeatable production deployments. `docker compose down` removes
  containers but keeps `ocg-data` and `ocg-browser-profiles`;
  `docker compose down -v` permanently deletes them and is only for an
  intentional reset after a verified two-volume backup. Selecting an older
  image does not roll back the database; restore
  the complete backup made by that older version when a database rollback is
  required. A current payload-V12 node backup is for a current or newer reader
  and does not replace that earlier directory backup.

---

[User guide index](../USER.md) · [简体中文](upgrade-backup.zh-CN.md) · [Docs index](../README.md)
