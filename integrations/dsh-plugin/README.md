[简体中文](README.zh-CN.md)

# Open Console Gateway for DSH

This package is generated and installed by the Open Console Gateway Desktop
application. It registers `ocg` in the selected DSH profile and reads the
current authenticated `GET /v1/models` catalog from the local Gateway.
Each model is called through the API named by that model's published
preferred protocol: `openai-completions`, `openai-responses`, or
`anthropic-messages`.

Runtime libraries, including those three pi-ai APIs, are loaded from the
active DSH installation. DSH 0.2.0-rc.2 with pi-ai 0.87.1 provides them.
This package does not declare or install private copies of DSH/pi-ai peer
dependencies; upgrading DSH must not pull an older runtime into the profile.
Installation is not gated by a version allowlist.

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

`model-catalog.js` requires `ocg.schemaVersion` 2 and a `protocols.preferred`
value listed in `protocols.supported`. It maps `chat_completions` to
`openai-completions`, `responses` to `openai-responses`, and `messages` to
`anthropic-messages`, one API per model. A row that fails those checks is an
`ocg-rejected` placeholder and is reported in that model's errors. It is not
registered as Chat. `listModels` advertises only rows that are not recorded
in those errors, so an all-invalid catalog is an empty list and a duplicate
ID is omitted. Exact `resolve` and `prepare` of a rejected id stay
`INVALID_CONFIG` and do not POST.

Chat Completions and Responses keep the published `/v1` base. Messages drops
a trailing `/v1` and keeps a deployment subpath. The Messages SDK sends the
Gateway Key as `x-api-key` plus `anthropic-version`. This package does not
rewrite those headers.

A prepared call freezes the catalog metadata captured for that model before
the prepare await returns. Signed assistant history is sent only when the
provider, API, and model id all match the model being called. Before the
base adapter can turn a foreign signed assistant message into plain text,
the Harness checks a present pi-ai envelope: kind `pi-ai`, version 2, aligned
with the assistant content, and the same tuple when that envelope carries
opaque native data. Missing replay metadata leaves ordinary text and tools
portable. This preflight belongs to the plugin and runs before the base
adapter. The gateway cannot apply it to a field the client already dropped.

For Chat Completions, the plugin removes the stream-only `index` from each
assistant `reasoning_details` entry immediately before sending. This also
handles entries already saved by pi-ai 0.87.1, so an affected conversation can
continue after reloading the corrected plugin. Stored history, signatures,
encrypted data, and other replay fields are preserved.
Unsigned text entries with an absent or `unknown` format and no provider
metadata are replayed as `reasoning_content`. This avoids sending
OpenRouter-style `reasoning.text` entries to endpoints such as Kimi Code that
reject that type. Signed, encrypted, identified, and vendor-specific entries
keep their structured representation.

`reasoningEfforts` is the Chat selector-to-wire map of exact categorical
spellings. The same map may be carried unchanged on Chat Completions and on
Responses. It is never a Messages thinking budget or an adaptive effort. OCG writes an
empty Messages level map even when `reasoning` is true, and that flag does
not manufacture a menu or a budget. Selecting a Messages reasoning level the
catalog does not declare is an explicit incompatibility in this plugin. A protocol listed in `supported` does not by itself add a
menu or guarantee a feature. A Responses vendor still applies its own
contract to a historical Chat spelling. An explicit level-to-wire mapping is
required before the selector offers those efforts; model names and a bare
`reasoning: true` never manufacture a level list. DSH's public `reasoning`
descriptor is that effort menu. This package omits the descriptor when the
menu is empty. The pi-ai capability flag and raw `ocg.reasoning` stay true,
and a Messages categorical map stays catalog metadata. Unknown limits remain
marked as fallback values, not upstream specifications. Maximum output
capability is distinct from the default per-request budget.

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
