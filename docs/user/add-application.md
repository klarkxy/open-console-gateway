[简体中文](add-application.zh-CN.md)

# Manual Client Setup

> Scope: retained current-implementation or published OCG2 operation reference, not the OCG3 design. See [architecture](../architecture.md) for the CPA migration target; UI steps do not apply to this CLI phase.

Use this guide to connect a client directly through the ordinary Gateway API.
The [Applications page](applications.md) also offers local BYOK configuration
for Codex, Kimi Code, MiniMax Code and ZCode, alongside the DSH plugin flow.
These flows create or reuse a harness-named ordinary Key and use its entire published model list, without a separate model selection.

## Connect an unlisted client

Copy the **Key** and URLs from **Connection Center**. Choose the interface the client already supports:

| Client protocol | Typical base value | Open Console Gateway request path | Authentication |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `http://127.0.0.1:9042/v1` | `POST /v1/chat/completions` | `Authorization: Bearer <key>` |
| OpenAI Responses | `http://127.0.0.1:9042/v1` | `POST /v1/responses` | `Authorization: Bearer <key>` |
| Anthropic Messages | `http://127.0.0.1:9042` when the client appends `/v1/messages` | `POST /v1/messages` | `x-api-key: <key>` |
| Gemini | `http://127.0.0.1:9042` with API version `v1beta` | `POST /v1beta/models/{model}:generateContent` or `:streamGenerateContent` | `x-goog-api-key: <key>` |

If a client asks for a **complete endpoint** instead of a base URL, use the request path shown above. If it automatically adds `/v1`, give it the root; if it expects an OpenAI API base, usually give it the `/v1` base. The client's official documentation decides which form is correct.

Use an exact model returned by authenticated local discovery:

```bash
curl http://127.0.0.1:9042/v1/models \
  -H "Authorization: Bearer <key>"
```

This list is a local read of currently routeable code-owned Aliases and eligible Custom IDs. Minimal request bodies for all four interfaces are in [Connect your first client](first-client.md).

After configuration, send one real request and check **Logs**.

[User guide index](../USER.md) · [Docs index](../README.md)
