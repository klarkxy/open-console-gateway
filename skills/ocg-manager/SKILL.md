---
name: ocg-manager
description: Install, run, upgrade, and configure Open Console Gateway on a local computer or Docker host. Use for its desktop app, headless CLI, dashboard, accounts, access Keys, client setup, and local troubleshooting.
---

# Open Console Gateway

Help the user reach a working local Gateway and verify the requested result. The skill is bundled into release binaries and synchronized on first launch after installation or upgrade. CLI syntax and capability details belong to that binary's `--help`; use the installed build and matching release documentation as the final authority.

## Choose the path

- Desktop app for a person using the same computer: Windows x64 installer, macOS universal DMG, or Linux x64 deb/AppImage.
- Native headless CLI when the machine needs a foreground Gateway and dashboard without the tray app. Its data directory is separate from the desktop app.
- Docker Compose for a container deployment. The default published host port is loopback only; the container still requires dashboard admin login. The optional browser and CPA profiles need their own setup.

Read [installation and deployment](references/install.md) for downloading, integrity checks, first start, and topology. Read [configuration](references/configure.md) for accounts, providers, routing, settings, clients, and what the CLI can actually change. Read [operations](references/operations.md) for upgrades, backup, diagnosis, and local verification. Always apply [secret handling](references/secrets.md) before a tool call, browser action, log read, or configuration write that might involve a credential.

## Work with the installed version

Inspect the actual executable's `--help`, relevant subcommand `--help`, release assets, dashboard controls, and existing configuration before assuming a feature is available. Never invent a CLI subcommand for dashboard settings. Current native release builds install or upgrade this skill when the desktop app or CLI `serve` starts; `skill sync` performs the same action explicitly. Docker cannot install a skill on the host. Use the dashboard for Access Keys, Custom endpoints, protocol overrides, proxy, routing, and most settings.

Keep the user's existing data and installation. Before changing a live node, identify its host, data directory, listener, version, and backup path. Do not use the desktop and CLI data directories interchangeably or start two writers on one directory. A public listener, reverse proxy, remote access, or destructive reset needs its own explicit scope and access decision.

Verify each layer separately: downloaded file and checksum; process/container and dashboard reachability; saved dashboard configuration; user-run authenticated client request and resulting request log. A reachable dashboard or open TCP port does not prove the upstream Key or model route works. Report the exact point reached when a secret handoff remains with the user.
