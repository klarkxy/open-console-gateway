# Verify, upgrade, and recover

Check the intended result at the final node. Separate these observations:

- Artifact: selected release, platform, file checksum, unpacked CLI `dist/` or installed desktop package.
- Runtime: process/container running and `http://127.0.0.1:<port>/dashboard/` reachable. Docker's TCP health check proves only that its listener is open.
- Control plane: expected account/provider/settings state visible after refresh. Dashboard V4 writes use revision tokens; prefer the dashboard rather than improvised direct API mutations.
- Inference: the user sends an authenticated request with a saved public model, then the dashboard request log shows the actual selected route and outcome. A listing or successful network test is not inference proof.

For a CLI node, `key list` gives API-key account IDs and enabled state across Providers but does not print Provider identity. Confirm the ID in the dashboard before `key remove/enable/disable`, especially when another Provider is present. Use `key ping [id]` only with the user's approval for a real upstream call; it is limited to OpenCode Go and can show an upstream body excerpt. Do not use it to validate Custom/other Providers. For Docker, prefer `docker compose ps`, dashboard reachability, and selected safe log fields; never paste raw Compose configuration or startup logs into the conversation. `GET /v1/models` requires a Gateway Key, so let the user test it through their client or a private terminal.

Before upgrading or moving a node, stop every process that writes its data. Copy the **whole** GUI or CLI data directory, including `data.sqlite` and `.encryption-key` where present, to a protected location outside the repository. Docker needs both `ocg-data` and `ocg-browser-profiles` volumes; the optional CPA `cpa-auth` volume is separate. Browser profiles contain live cookies and need the same protection as Keys. Preserve the version and backup path. Replace the desktop installer, whole CLI extraction, or pinned Docker images with the matching newer release. Do not substitute an old binary for a newer database and call that a rollback: restore the matching complete backup if rollback is needed.

Do not use `docker compose down -v` or delete data directories in an ordinary upgrade. `docker compose down` preserves named volumes. On Windows, stop the release tray app before local Tauri development; the development Gateway and release app can contend for instance/port ownership. If the user asks to expose the node remotely, plan the listener, admin login, HTTPS, and firewall/proxy boundary explicitly before changing the bind or published port.
