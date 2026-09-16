[简体中文](docker.zh-CN.md)

# Docker

Open Console Gateway runs headlessly in Docker, serving the same dashboard and gateway
on port `9042`. Pull the image from GHCR anonymously — it ships `linux/amd64`
and `linux/arm64`, and Docker picks the right variant. Save the release's
`compose.example.yaml` as `compose.yaml`, add `.env` if needed, and run the
commands below. You can also use a checkout of the matching tag.

```bash
git clone --branch v2.4.1 --depth 1 https://github.com/klarkxy/open-console-gateway.git
cd open-console-gateway
cp .env.example .env
# PowerShell: Copy-Item .env.example .env
# Edit .env before exposing the service outside the host.
docker compose pull
docker compose up -d --no-build
docker compose ps
```

Image tags move; decide how pinned you want to be.

## Choosing An Image

The source repository is `klarkxy/open-console-gateway`. The published GHCR
packages are `ghcr.io/klarkxy/opencode-go-mgr` and
`ghcr.io/klarkxy/opencode-go-mgr-browser`; Compose and the container publishing
workflow use these image names.

- The checkout's `compose.yaml` defaults to `latest`; the Release
  `compose.example.yaml` pins its matching full version.
- For repeatable production deployments, set `OCG_IMAGE` in `.env` to a full
  release tag such as `ghcr.io/klarkxy/opencode-go-mgr:2.4.1`.
- Full-version and `sha-<commit>` tags identify one release and are intended
  not to move; `latest` does. Only a digest such as
  `ghcr.io/klarkxy/opencode-go-mgr@sha256:...` is truly immutable.
- To build the current checkout instead, set `OCG_IMAGE=ocg-manager:local`
  and run `docker compose up -d --build`. `NPM_REGISTRY` and
  `CARGO_REGISTRY` are build arguments for that source-build path only.

| Variable | Scope | Meaning |
| --- | --- | --- |
| `OCG_IMAGE` | Compose | Image tag, mirror, local name, or immutable digest. |
| `OCG_BROWSER_IMAGE` | Compose | Optional Chromium/noVNC sidecar image tag, mirror, local name, or digest. |
| `OCG_PORT` | Compose | Host loopback port; the container still listens on `9042`. |
| `OCG_ADMIN_USERNAME` + `OCG_ADMIN_PASSWORD` | First start | Optional administrator bootstrap; both or neither. |
| `OCG_CLIENT_ROOT_URL` | Runtime | Read-only external client root override. |
| `OCG_CPA_BASE_URL` | Compose CPA profile | Read-only CPA sibling URL; leave at `http://cpa:8317`. |
| `CPA_MANAGEMENT_PASSWORD` | Compose CPA profile | CPA Management API password; keep only in the deployment's `.env`. |
| `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` | Runtime | Standard proxy variables used by `Automatic (system / environment)` outbound proxy mode. |
| `OCG_MANAGER_ENCRYPTION_KEY` | Runtime restore | Original explicit obfuscation key, when one was used. |
| `NPM_REGISTRY` + `CARGO_REGISTRY` | Source build | Dependency registries used only by `--build`. |

Most deployments need only the main service. Add the browser sidecar only
when you need managed onboarding or website login on a headless host.

## Optional Local CPA

CPA is an optional **local** subscription-runtime sibling. It is off by
default. Before enabling it, copy the included template next to your Compose
file and set a distinct CPA inference key:

```bash
cp cpa-config.example.yaml cpa-config.yaml
# PowerShell: Copy-Item cpa-config.example.yaml cpa-config.yaml
# Edit cpa-config.yaml and .env: set api-keys and CPA_MANAGEMENT_PASSWORD.
docker compose --profile cpa up -d
docker compose --profile cpa ps
```

The fixed image is `eceasy/cli-proxy-api:v7.2.145`; it deliberately does not
use `latest`. CPA's inference port `8317` is published only to the private
`cpa-private` bridge, where OCG reaches `http://cpa:8317`. The only host ports
are CPA's OAuth callback ports `1455`, `54545`, and `51121`, each bound to
`127.0.0.1`. Do not add a public `8317` mapping, Docker socket mount, or a
remote CPA URL.

CPA keeps OAuth data in the `cpa-auth` volume at `/root/.cli-proxy-api`. OCG
does not read or copy those files. Back up `cpa-auth` separately from
`ocg-data` and `ocg-browser-profiles`; restoring it requires the matching CPA
configuration and keys. `docker compose down` preserves all three named
volumes, while `docker compose down -v` permanently deletes them.

Open **Extensions → CPA** after the container is running. Save the same CPA
inference key from `cpa-config.yaml` and the Management password, run
the application-level test, and perform OAuth inside CPA. The OCG container
does not start, stop, upgrade, or health-check CPA on your behalf.

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
- The startup log contains the Key, so log output and Docker daemon
  access are sensitive. Configure log rotation on the Docker host if its
  defaults are not bounded.

Routine operational checks:

```bash
docker compose config --quiet
docker compose ps
docker compose logs --tail=100 -f ocg-manager
docker compose --profile browser logs --tail=100 -f browser
docker compose --profile cpa logs --tail=100 -f cpa
curl --fail http://127.0.0.1:9042/dashboard/
```

Replace `9042` in the curl command with the configured host `OCG_PORT` when
you changed it.

## Verifying An Image

Both the main and browser images include an SPDX SBOM, BuildKit SLSA
provenance, and a GitHub signed provenance attestation. Inspect and verify a
release with:

```bash
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr:2.4.1
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr-browser:2.4.1
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr:2.4.1 \
  --repo klarkxy/open-console-gateway
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr-browser:2.4.1 \
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
