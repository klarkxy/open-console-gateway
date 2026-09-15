[简体中文](add-application.zh-CN.md)

# Manual Client Setup

Use this guide to connect a client directly through the ordinary Gateway API.
The [Applications page](applications.md) hosts the DSH plugin flow for
integrating DSH itself.

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
