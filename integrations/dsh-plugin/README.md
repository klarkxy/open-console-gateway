# Open Console Gateway for DSH

This package is generated and installed by the Open Console Gateway Desktop
application. It registers one `open-console-gateway` provider in the selected DSH
profile. The provider reads the current authenticated `GET /v1/models` catalog
from the local Gateway and forwards model calls through the OpenAI-compatible
Chat Completions endpoint.

Runtime libraries are loaded from the active DSH installation. This package
does not declare or install private copies of DSH/pi-ai peer dependencies;
upgrading DSH must not pull an older runtime into the profile. Installation
is not gated by a version allowlist.

The installer hands the selected Gateway Key to DSH through a private,
one-time live file. This package is activated once per DSH runtime. On
activation the plugin claims that live file by rename, imports the value,
and deletes every leftover claim file so a newer live handoff is never
unlinked and a stale claim cannot keep activation pending. If credential
storage fails and no newer live file exists, the claim is restored for
retry.

Do not copy or edit this generated package by hand. Re-run the installer from
the **Applications > DSH** page when repair is required.

## Model details and reasoning levels

`model-catalog.js` translates OCG's versioned metadata into the active DSH
runtime's native context, text/image input and reasoning-level descriptors.
An explicit level-to-wire mapping is required to offer reasoning controls;
model names and a bare `reasoning: true` never manufacture a level list.
Unknown limits remain marked as fallback values, not upstream specifications.
Maximum output capability is distinct from the default per-request budget.

Upgrade OCG, reinstall this package through **Applications > DSH**, and reload
the selected runtime once to replace an older installed plugin. Refresh the
provider's model directory to populate newly available upstream metadata.
ID-only catalogs can use route-specific declarations described in the
[English guide](../../docs/user/model-metadata.md) or
[中文指南](../../docs/user/model-metadata.zh-CN.md).

## Runtime verification

From the OCG source checkout, run the isolated installation smoke against an
installed official DSH CLI entry point:

```sh
OCG_DSH_SMOKE_BIN=/absolute/path/to/node_modules/@deepseek-ai/dsh/lib/bin.js \
  node scripts/dsh-application-install-smoke.mjs
```

The smoke creates temporary profiles and a loopback mock gateway. It checks
plugin installation, credential handoff, the native context and level list,
stream completion, and `xhigh` being sent as the declared `max` wire value.
It does not touch a real user's DSH home or call a production upstream.
The scenario was verified with official DSH `0.1.7-rc.2`.
