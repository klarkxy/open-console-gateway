[简体中文](cli.zh-CN.md)

# CLI

The CLI is a headless host for the same `ocg-core` process. Download the
archive for your platform and extract it into a directory. Keep `dist/` next
to the executable so `serve` has a dashboard to serve. On Windows the
executable is `ocg-manager-cli.exe`; on Linux you may need
`chmod +x ocg-manager-cli` after extraction.

The CLI data directory defaults to `~/.ocg-mgr-cli` on every platform; override
it with `--data-dir <path>`. The obfuscation secret lives at
`<data-dir>/.encryption-key` by default, or set it with `--encryption-key <key>`
or `OCG_MANAGER_ENCRYPTION_KEY`.

The CLI has `serve`, `key`, `status`, and `skill sync`. Run the installed
binary's `--help` and each subcommand's `--help` for its exact version.
`key add` and `key ping` target OpenCode Go accounts. `key list` and `status` count
API-key accounts across Providers; `key remove/enable/disable` act on an
account ID even for other Providers. Confirm its Provider in the dashboard
before changing it. Dashboard Keys, Custom destinations, per-model protocol
overrides, and catalogs stay on the dashboard. CLI writes bump that process's
settings revision directly.

`serve` and `status` hide the primary Gateway Key by default. When you need its
value, run `status --show-key` in a private terminal. Do not paste that output
into an agent conversation or a shared log. The legacy `key add` syntax puts
the upstream Key (and optional password) in process arguments and possibly
shell history; enter credentials in the local dashboard instead when an agent
is assisting. Avoid `--encryption-key <key>` for the same reason; the default
data-directory key file is enough for a new installation.

While native `serve` is running, **Applications > DSH** can install the OCG
plugin into a DSH owned by the same OS user on that machine. The official
Docker image does not support this local installation.

Native release CLI builds bundle the `ocg-manager` Codex skill. `serve`
installs or upgrades it at `~/.agents/skills/ocg-manager`; `skill sync` does
the same without starting the Gateway. When bundled skill content changes, a
prior OCG-managed copy is backed up under `~/.agents/skill-backups/`; a
same-name user-owned skill is left unchanged. Development builds do not
auto-sync on `serve`, and the Docker build does not install a skill into the
container or the Docker host.

`key add` stores a ready, enabled OpenCode Go account; confirm it with
`key ping` before you rely on it. `key ping` chooses a deterministic routable
saved model and its saved supported preferred protocol; pass `--model <id>` to
select a supported saved model explicitly.

Start the headless Gateway, then add the upstream Key in its local dashboard:

```bash
./ocg-manager-cli serve --port 9042
```

After saving the account, a second terminal can run `key list` and
`key ping <id>` when a real upstream diagnostic is needed.

`serve --port <port>` writes the port to SQLite; later runs without the flag
reuse it.

`key ping` decrypts the selected key, sends a tiny request through the selected
saved model and protocol, and prints the real upstream status code with a short
body excerpt — a quick way to surface `401`/`403`/`429`/`200` without opening
the dashboard. Treat
that upstream-provided excerpt as sensitive when you share diagnostics.

---

[User guide index](../USER.md) · [简体中文](cli.zh-CN.md) · [Docs index](../README.md)
