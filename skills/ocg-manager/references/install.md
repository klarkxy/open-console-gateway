# Download, install, and start

Use the official [release page](https://github.com/klarkxy/open-console-gateway/releases) and the `SHA256SUMS` asset from the same chosen release. Select an exact version and matching platform artifact; do not infer the newest tag from this skill. Compare the downloaded file's SHA-256 with its entry before executing it. PowerShell: `Get-FileHash -Algorithm SHA256 <file>`; macOS: `shasum -a 256 <file>`; Linux: `sha256sum <file>`. Preserve the chosen version and download source in the work report.

| Host | Desktop artifact | CLI archive |
| --- | --- | --- |
| Windows 10/11 x64 | `ocg-manager_<version>_windows-x64-setup.exe` | `ocg-manager-cli_<version>_windows-x64.zip` |
| macOS 11+ Intel/Apple Silicon | `ocg-manager_<version>_macos-universal.dmg` | `ocg-manager-cli_<version>_macos-universal.tar.gz` |
| Linux x64 | `.deb` or `ocg-manager_<version>_linux-x64.AppImage` | `ocg-manager-cli_<version>_linux-x64.tar.gz` |

## Desktop

Install the verified artifact using the platform's normal installer, then launch Open Console Gateway. It opens `http://127.0.0.1:9042/dashboard/` in the system browser and continues in the tray/menu bar. Windows uses a per-user NSIS install; the package may be unsigned. macOS may require the user's Gatekeeper approval. On Linux the AppImage may need executable permission. If port `9042` is occupied, resolve that conflict or change the port in Settings; do not silently kill another process. Desktop data defaults to `%USERPROFILE%\.ocg-mgr` on Windows or `~/.ocg-mgr` on macOS/Linux.

## Native CLI

Extract the **whole** archive. Keep `dist/` beside `ocg-manager-cli` (`.exe` on Windows) so `serve` can show the dashboard. `--data-dir <path>` selects the CLI data directory; its default is `~/.ocg-mgr-cli` on all platforms. Let it create `<data-dir>/.encryption-key` for a new node. Do not place encryption secrets in `--encryption-key` arguments or agent-created environment commands.

Read the installed CLI's `--help` and `serve --help`. Native release builds synchronize the bundled skill when `serve` starts; `skill sync` can repair it without starting the Gateway. Check `status --help` before an agent launches `serve` or reads `status`. Builds with `--show-key` hide the Gateway Key in ordinary `serve` and `status` output. Older builds may print it: in that case, have the user start `serve` in a private terminal and do not collect its stdout/stderr. There is no HTTP health endpoint; check `/dashboard/` for the page.

## Docker Compose

Use the matching release's `compose.example.yaml` as `compose.yaml`, or check out that release tag. Pin `OCG_IMAGE` to the chosen full version or digest; use the matching browser image only when its optional profile is needed. Default Compose publishes host `127.0.0.1:${OCG_PORT:-9042}` to container `9042`. Pull and start with `docker compose pull` and `docker compose up -d --no-build`; use `docker compose ps` and `docker compose config --quiet` for nonsecret checks. Avoid displaying `docker compose config` or raw logs because environment values or older startup logs can contain secrets.

Leave `OCG_ADMIN_USERNAME` and `OCG_ADMIN_PASSWORD` unset for a new loopback-only node; the user creates the administrator in the dashboard. Both bootstrap variables must be supplied together if explicitly chosen. `OCG_MANAGER_ENCRYPTION_KEY` normally stays unset so the key in the persistent `ocg-data` volume is used. Never expose an uninitialized dashboard outside loopback. The optional browser profile keeps cookies in `ocg-browser-profiles`; CPA has separate `cpa-auth` state and setup. Enable either only for the requested workflow.
