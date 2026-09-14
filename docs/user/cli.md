[简体中文](cli.zh-CN.md)

# CLI

The CLI is a headless host for the same `ocg-core` process. Download the
archive for your platform and extract it into a directory. Keep `dist/` next
to the executable so `serve` has a dashboard to serve. On Windows the
executable is `ocg-manager-cli.exe`; on Linux you may need
`chmod +x ocg-manager-cli` after extraction.

The CLI data directory defaults to `~/.ocg-mgr-cli` on every platform;
override it with `--data-dir <path>`. The obfuscation secret lives at
`<data-dir>/.encryption-key` by default, or set it with `--encryption-key <key>`
or `OCG_MANAGER_ENCRYPTION_KEY`.

The CLI only does `serve`, `key`, and `status`. `key` manages OpenCode Go
account credentials — not dashboard Keys and not Custom or Zen Free cards.
Keys, Custom destinations, per-model protocol overrides, and catalogs stay on
the dashboard. CLI writes bump that process's settings revision directly; there is
no `--expectedRevision` flag.

```text
ocg-manager-cli
├── serve         Start the gateway server
│   --host        Address to listen on (default 127.0.0.1)
│   -p, --port    Gateway port (sets and saves config)
│   --dashboard-dir  Directory containing the built web dashboard
├── key list      List OpenCode Go API-key accounts (excludes Zen Free)
├── key add <name> <key>
│   --username    OpenCode-Go login account
│   --password    OpenCode-Go login password
├── key remove <id>      Remove an account
├── key enable <id>      Enable an account
├── key disable <id>     Disable an account
├── key ping [id]
│   --model       Model to send (default mimo-v2.5)
│   --message     User message (default "ping")
│   --max-tokens  max_tokens for the ping (default 3)
└── status        Show data dir, gateway port/key, upstream, account totals
```

`key add` stores a disabled OpenCode Go draft. Enable it after you have checked the Key (`key ping`, then `key enable`).

Headless bootstrap:

```bash
./ocg-manager-cli key add main sk-...
./ocg-manager-cli key list
./ocg-manager-cli key ping <id>
./ocg-manager-cli key enable <id>
./ocg-manager-cli serve --port 9042
```

`serve --port <port>` writes the port to SQLite; later runs without the flag
reuse it.

`key ping` sends a tiny chat completion through one key and prints the real
upstream status code with a short body excerpt — a quick way to surface
`401`/`403`/`429`/`200` without opening the dashboard.

---

[User guide index](../USER.md) · [简体中文](cli.zh-CN.md) · [Docs index](../README.md)
