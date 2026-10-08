[简体中文](add-application.zh-CN.md)

# Manual Client Setup

Use this guide to connect a client directly through the ordinary Gateway API.
The [Applications page](applications.md) also offers local BYOK configuration
for Codex, Kimi Code, MiniMax Code, ZCode, and VS Code Copilot, alongside the DSH plugin flow.
These flows create or reuse a harness-named ordinary Key and use its entire
published model list, with no separate model selection. Native configuration
previews are read-only until you confirm the reviewed plan; stale plans must be
prepared again before committing.

## Connect An Unlisted Client

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

After configuration, send one real request and check **Logs**. Close and reopen
the client when its native configuration requires activation; a saved file alone
does not prove that the client loaded it.

## VS Code Copilot

Use the [Applications > VS Code Copilot flow](applications.md#vs-code-copilot) for native configuration. For a manual setup, open VS Code's **Chat: Manage Language Models**, choose **Add Models > Custom Endpoint**, and edit the resulting `chatLanguageModels.json`. Keep its top-level array and other providers. The [official guide](https://code.visualstudio.com/docs/agent-customization/language-models) describes the native UI; OCG's advanced format baseline is [VS Code 1.141](https://github.com/microsoft/vscode/blob/1.141.0/extensions/copilot/src/extension/byok/vscode-node/customEndpointProvider.ts).

The following is a single explicit Chat Completions entry. Replace `public-model-id` with an exact published model ID and `<OCG Key>` with your Key. The numeric values are example client configuration budgets, not vendor metadata. Constrain the configured values to verified input/output limits and proportionally reduce both if their sum exceeds a known context window. These values guide VS Code's context management and output reservation; actual request parameters and upstream limits depend on the client and model, so they do not guarantee a hard output limit for every request.

```jsonc
[
  {
    "name": "Open Console Gateway",
    "vendor": "customendpoint",
    "models": [
      {
        "id": "public-model-id",
        "name": "public-model-id",
        "url": "http://127.0.0.1:9042/v1/chat/completions",
        "apiType": "chat-completions",
        "toolCalling": false,
        "vision": false,
        "maxInputTokens": 100000,
        "maxOutputTokens": 8192,
        "requestHeaders": {
          "Authorization": "Bearer <OCG Key>"
        }
      }
    ]
  }
]
```

For a model published with Responses or Messages as its preferred protocol, use `apiType: "responses"` with the complete `/v1/responses` URL or `apiType: "messages"` with `/v1/messages`. Preserve any deployment subpath. Omit the provider-level `url` to use this explicit model list and omit `apiKey` when using the literal Authorization header. This header also authenticates OCG's Messages endpoint. Keep the file and backups private: they contain the Key in plaintext.

The example conservatively disables unknown tool and vision capabilities. Set `toolCalling: true` only after verifying tool support; Agent's picker requires it. Likewise enable `vision` only for verified image support. The automatic adapter reads these declarations from OCG's model metadata page. For verified Chat/Responses reasoning choices, use the exact deduplicated values in `supportsReasoningEffort` and set `reasoningEffortFormat` to the matching API type; do not invent Messages choices or a thinking budget.

If you choose VS Code secret storage, enter the Key through its native provider UI and use the resulting `apiKey: "${input:...}"` reference with `requestHeaders.Authorization: "Bearer ${apiKey}"`. The reference must be created through VS Code; merely typing a reference into an external file does not seed its secret storage. OCG's automatic adapter writes literal headers and does not write that storage. A unique existing OCG provider can be taken over after reviewing the application preview. Replacing changed OCG-owned values requires explicit confirmation; duplicate or malformed entries remain blocked. Takeover saves a private baseline, and Undo takeover preserves later client-owned preferences.

Fully close VS Code before externally editing the file, then reopen it and select **Open Console Gateway** in the Chat model picker. Send a request and check OCG **Logs**. Chat, Agent, inline chat, and utility tasks can use BYOK models; inline completions and Next Edit Suggestions are outside this integration. Agent Host BYOK is experimental and requires `chat.agentHost.byokModels.enabled` in VS Code. The automatic flow does not change defaults or `settings.json`.

---

[User guide index](../USER.md) · [简体中文](add-application.zh-CN.md) · [Docs index](../README.md)
