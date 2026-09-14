[简体中文](first-client.zh-CN.md)

# Connect Your First Client

Once the gateway is running, connecting a client is mostly a copy-and-paste
task. Use the exact base URL shown in Connection Center; most OpenAI-compatible
clients expect the trailing `/v1`.

1. In **Accounts**, add an OpenCode Go account with an officially distributable
   API key. The login account is optional; when entered first, it is copied
   into the required display name until you edit that name yourself. The
   dashboard does not collect or manage an OpenCode login password.
2. In the dashboard's **Connection Center**, copy the **Key** and the
   **API Base URL** (`http://127.0.0.1:9042/v1`).
3. Point your client at the base URL with the Key. See [Manual client setup](add-application.md) for URL and protocol selection.
4. Verify the setup with a real request.

The **Key** is the only secret you hand to the client. It accepts three
header shapes — `Authorization: Bearer <key>`, Anthropic-style
`x-api-key: <key>`, or Gemini-style `x-goog-api-key: <key>`. The gateway
pulls the upstream OpenCode-Go account key from SQLite and injects it itself.

Minimal POSIX-shell checks for all five client formats:

```bash
BASE=http://127.0.0.1:9042
KEY=replace-with-gateway-key

# OpenAI Chat Completions
curl "$BASE/v1/chat/completions" -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"ping"}],"stream":false}'

# OpenAI Responses
curl "$BASE/v1/responses" -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","input":"ping","store":false}'

# Anthropic Messages
curl "$BASE/v1/messages" -H "x-api-key: $KEY" \
  -H "anthropic-version: 2023-06-01" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","max_tokens":16,"messages":[{"role":"user","content":"ping"}]}'

# Gemini generateContent
curl "$BASE/v1beta/models/deepseek-v4-flash:generateContent" \
  -H "x-goog-api-key: $KEY" -H "Content-Type: application/json" \
  -d '{"contents":[{"role":"user","parts":[{"text":"ping"}]}]}'
```

---

[User guide index](../USER.md) · [简体中文](first-client.zh-CN.md) · [Docs index](../README.md)
