[简体中文](docker.zh-CN.md)

# Docker

> Historical scope: published Docker layout of the `open-console-gateway` generation. Not the current `ocg` package. This page does not define a Docker workflow for this generation, and it does not record a test or a completed migration.

This generation is the headless `ocg` CLI (`ocg.exe` on Windows, `ocg` on Linux and macOS). `ocg3` and `open-console-gateway` are two generation branches of one project; the product name stays Open Console Gateway. Execution is one local CPA owned by that process. Custom HTTP providers remain provider routes on that local CPA. The default data root is `~/.ocg3`. A launch does not open, copy, move, delete, or adopt `~/.ocg-mgr` or `~/.ocg-mgr-cli`. The GPUI/Ely GUI is postponed. Whole CLI acceptance is pending. Commands are in the [CLI guide](cli.md). Design is in the [architecture](../architecture.md).

The image names, profiles, and commands below are that published container. It served the dashboard and gateway on port `9042`. The GHCR images shipped `linux/amd64` and `linux/arm64`. The published instructions saved that release's `compose.example.yaml` as `compose.yaml`, added `.env` when needed, and pinned one release with a `VERSION` shell variable matching the `compose.example.yaml` shipped with that release. A checkout of the matching tag was the other published path. Those tags are not a current `ocg` package.

```bash
VERSION=2.6.2
git clone --branch "v$VERSION" --depth 1 https://github.com/klarkxy/open-console-gateway.git
cd open-console-gateway
cp .env.example .env
# PowerShell: Copy-Item .env.example .env
# Edit .env before exposing the service outside the host.
docker compose pull
docker compose up -d --no-build
docker compose ps
```

Image tags of that generation moved. The published advice was to decide how pinned to be.

## Choosing An Image

The source repository is `klarkxy/open-console-gateway`. The published GHCR
packages are `ghcr.io/klarkxy/opencode-go-mgr` and
`ghcr.io/klarkxy/opencode-go-mgr-browser`; Compose and the container publishing
workflow use these image names.

- The checkout's `compose.yaml` defaults to `latest`; the Release
  `compose.example.yaml` pins its matching full version.
- Repeatable deployments of that generation set `OCG_IMAGE` in `.env` to a full
  release tag such as `ghcr.io/klarkxy/opencode-go-mgr:<version>`.
- Full-version and `sha-<commit>` tags identify one release and are intended
  not to move; `latest` does. Only a digest such as
  `ghcr.io/klarkxy/opencode-go-mgr@sha256:...` is truly immutable.
- A source build of that generation set `OCG_IMAGE=ocg-manager:local`
  and ran `docker compose up -d --build`. `NPM_REGISTRY` and
  `CARGO_REGISTRY` were build arguments for that path only.

| Variable | Scope | Meaning |
| --- | --- | --- |
| `OCG_IMAGE` | Compose | Image tag, mirror, local name, or immutable digest. |
| `OCG_BROWSER_IMAGE` | Compose | Optional Chromium/noVNC sidecar image tag, mirror, local name, or digest. |
| `OCG_PORT` | Compose | Host loopback port; the container still listens on `9042`. |
| `OCG_ADMIN_USERNAME` + `OCG_ADMIN_PASSWORD` | First start | Optional administrator bootstrap; both or neither. |
| `OCG_CLIENT_ROOT_URL` | Runtime | Read-only external client root override. |
| `OCG_MAX_REQUEST_BODY_BYTES` | Runtime | Maximum gateway JSON request body size in bytes; defaults to 64 MiB. |
| `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` | Runtime | Standard proxy variables used by `Automatic (system / environment)` outbound proxy mode. |
| `OCG_MANAGER_ENCRYPTION_KEY` | Runtime restore | Original explicit obfuscation key, when one was used. |
| `NPM_REGISTRY` + `CARGO_REGISTRY` | Source build | Dependency registries used only by `--build`. |

Most deployments need only the main service. Add the browser sidecar only
when you need managed onboarding or website login on a headless host.

## Previous-generation Compose CPA profile

The published Compose file had an optional `cpa` profile. It was off unless that profile was selected. It started a separate CPA container, image `eceasy/cli-proxy-api:v7.2.145`, not `latest`. OCG reached it at `http://cpa:8317` when `OCG_CPA_BASE_URL` was set to that origin. `CPA_MANAGEMENT_PASSWORD` was the management password for that container. That profile is not a product mode of this generation. Do not select it, and do not set a remote CPA URL. There is no `OCG_CPA_BASE_URL` switch.

The published enable steps were:

```bash
cp cpa-config.example.yaml cpa-config.yaml
# PowerShell: Copy-Item cpa-config.example.yaml cpa-config.yaml
# Edit cpa-config.yaml and .env: set api-keys and CPA_MANAGEMENT_PASSWORD.
docker compose --profile cpa up -d
docker compose --profile cpa ps
```

Inference port `8317` was published only on the private `cpa-private` bridge. The host ports were the OAuth callback ports `1455`, `54545`, and `51121`, each bound to `127.0.0.1`. The file did not publish `8317` on the host and did not mount the Docker socket.

OAuth data for that profile lived in the `cpa-auth` volume at `/root/.cli-proxy-api`. OCG did not read or copy those files. `cpa-auth` is separate from `ocg-data` and `ocg-browser-profiles`. `docker compose down` kept the named volumes. `docker compose down -v` deleted them. This page does not activate or delete a stored volume. A saved base URL, management password, or inference key from that layout stays stored until an explicit migration. That migration is not implemented.

The published dashboard step opened **Extensions → CPA**, saved the inference key from `cpa-config.yaml` and the management password, and ran the application-level test. The OCG container did not start, stop, upgrade, or health-check that CPA. That connection form is not a current step.

## Optional Remote Browser

The sidecar is off by default. Turn it on only when you need managed
onboarding or website login on a Linux server or Docker host; reserve at
least 2 CPUs, 2 GiB of RAM, and 1 GiB of `/dev/shm`, then run:

```bash
docker compose --profile browser up -d
docker compose ps
```

`OCG_BROWSER_IMAGE` overrides the default browser image. The sidecar is
ordinary Chromium plus Xvfb, a window manager, x11vnc, and noVNC; the
dashboard opens it in a full tab over an authenticated same-origin WebSocket,
with keyboard and pointer input. Use the page's remote clipboard area to
copy or paste a key. Any reverse proxy in front of the dashboard must allow
WebSocket upgrades.
Chromium uses its basic password store, so persistent profiles do not depend
on a host keyring.

Only one remote Chromium runs per node. Switching accounts first shuts down
the current process cleanly and waits for its profile to flush, then starts
the target account; any older remote page becomes invalid immediately.
Dashboard browser tokens are memory-only, bound to the current administrator
session, and Origin-checked. They expire after 30 minutes idle or four hours
total; reopen the account website to create another session.

The sidecar publishes no host port and never mounts the database. Its control
and noVNC endpoints exist only on the Compose `browser-private` network. This
project-scoped bridge is not Docker `internal`, because Chromium needs outbound
HTTPS access to Google and OpenCode; neither sidecar endpoint is published to
the host. A random control token lives in the shared `ocg-browser-runtime`
runtime volume.
Account cookies and profiles live in `ocg-browser-profiles`; do not back up the
runtime volume, but always stop and back up the two sensitive persistent
volumes, `ocg-data` and `ocg-browser-profiles`, together.

Google may treat a data-center egress IP as high risk, require additional
verification, or reject registration/login. Open Console Gateway does not bypass that
risk control. Complete Google's checks yourself, or use the desktop build on
a residential connection. Real payment is always an explicit user action on
the official site.

## Administrator Bootstrap

`OCG_ADMIN_USERNAME` and `OCG_ADMIN_PASSWORD` create the administrator **only
when the database has no administrator yet**.

- Both must be set together; setting only one stops startup with an error.
- Once an administrator exists, later environment changes do not reset it.
- When both are omitted, the first visitor creates the administrator in the
  dashboard.
- After the administrator exists, you may remove both variables while keeping
  the volume; the stored account remains. Remove them from the container
  environment with `docker compose up -d --no-build --force-recreate`.

Bootstrap credentials are visible to anyone with Docker daemon access.
Protect `.env`, use a long random password, and do not expose an
uninitialized dashboard publicly.

## Secrets And Addresses

`OCG_MANAGER_ENCRYPTION_KEY` is for restoring a deployment that originally
set it. Leave it unset normally so the generated `.encryption-key` stays in
the data volume. Changing or losing the value after credentials are saved
makes them unreadable; treat it like a password.

The optional `OCG_CLIENT_ROOT_URL` is the environment equivalent of the
dashboard's Downstream Access Root. Use it when a reverse proxy is present or
the dashboard and gateway have different externally reachable addresses. A
non-empty value must be an absolute HTTP(S) URL; when present, it overrides
the saved SQLite value, and an invalid value stops startup. It does not
configure the listener, DNS, or reverse proxy. Normally use
`https://ocg.example.com`, not `/dashboard/` or a concrete API endpoint; a
trailing `/v1` is accepted.

## Runtime Behavior

Set `OCG_PORT` in `.env` to change the host port; the container still uses
port `9042`. Open `http://127.0.0.1:<OCG_PORT>/dashboard/` and sign in. Use
`/dashboard/`, not the server root `/`.

- Data and the generated `.encryption-key` obfuscation secret persist in the
  `ocg-data` volume; account browser cookies/profiles persist separately in
  `ocg-browser-profiles`.
- The container process binds `0.0.0.0`, so the dashboard requires
  administrator login even when it is published only on host `127.0.0.1`.
  That host mapping limits reachability; it does not enable the loopback
  login bypass.
- The container's `HEALTHCHECK` opens `127.0.0.1:9042` over TCP every 30
  seconds; there is no `/healthz` route. That TCP check proves only that the
  process is listening — not that the dashboard API, an upstream account, or
  a real model request works.
- Both images run as the unprivileged `ocg` user (UID/GID 10001). The supplied
  Compose services make the root filesystem read-only, mount `/tmp` as tmpfs,
  and drop every Linux capability. The main service also enables
  `no-new-privileges`; the browser service instead uses `seccomp=unconfined`
  so ordinary Chromium can establish its own namespace and renderer seccomp
  sandboxes. The sidecar does not use `--no-sandbox` and has 1 GiB of shared
  memory. `ocg-data` and `ocg-browser-profiles` are the two persistent state
  volumes.
- CLI builds with `status --show-key` hide the Gateway Key in startup logs.
  Older images may print it, so existing logs and Docker daemon access remain
  sensitive. Configure log rotation on the Docker host if its defaults are not
  bounded.

Published operational checks for that container, not for the `ocg` CLI:

```bash
docker compose config --quiet
docker compose ps
docker compose logs --tail=100 -f ocg-manager
docker compose --profile browser logs --tail=100 -f browser
curl --fail http://127.0.0.1:9042/dashboard/
```

Replace `9042` in the curl command with the configured host `OCG_PORT` when
you changed it.

## Verifying An Image

Both the main and browser images include an SPDX SBOM, BuildKit SLSA
provenance, and a GitHub signed provenance attestation. Inspect and verify a
release with:

```bash
VERSION=2.6.2
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr:$VERSION
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr-browser:$VERSION
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr:$VERSION \
  --repo klarkxy/open-console-gateway
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr-browser:$VERSION \
  --repo klarkxy/open-console-gateway
```

Both `gh attestation verify` commands require an authenticated GitHub CLI. Public pulls are
anonymous; if the OCI client still requests registry credentials,
authenticate to `ghcr.io` with a token that can read packages. Provenance
proves how the artifact was produced.

Regenerate the Key if it leaks.

## HTTPS

Point an existing reverse proxy at the loopback port. For example, with
Caddy:

```caddyfile
ocg.example.com {
    reverse_proxy 127.0.0.1:9042
}
```

After signing in, set a non-empty Key before sending API traffic.
Stop the service with `docker compose down`; add `-v` only when you
intentionally want to delete all stored accounts, credentials, keys, cookies,
and browser profiles.

---

[User guide index](../USER.md) · [简体中文](docker.zh-CN.md) · [Docs index](../README.md)
