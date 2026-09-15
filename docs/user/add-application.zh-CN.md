[English](add-application.md)

# 手动客户端配置

本指南通过普通 Gateway API 直接连接客户端。[应用页面](applications.zh-CN.md)承载 DSH 插件流程，用于把 DSH 本身接入 Gateway。

## 接入未收录客户端

从 **接入中心** 复制 **Key** 与地址，选择客户端本身已经支持的接口：

| 客户端协议 | 常见 Base 值 | Open Console Gateway 请求路径 | 鉴权 |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `http://127.0.0.1:9042/v1` | `POST /v1/chat/completions` | `Authorization: Bearer <key>` |
| OpenAI Responses | `http://127.0.0.1:9042/v1` | `POST /v1/responses` | `Authorization: Bearer <key>` |
| Anthropic Messages | 客户端会追加 `/v1/messages` 时使用 `http://127.0.0.1:9042` | `POST /v1/messages` | `x-api-key: <key>` |
| Gemini | `http://127.0.0.1:9042`，API 版本为 `v1beta` | `POST /v1beta/models/{model}:generateContent` 或 `:streamGenerateContent` | `x-goog-api-key: <key>` |

若客户端要求填写 **完整 Endpoint** 而不是 Base URL，就使用表中的请求路径。若它会自动追加 `/v1`，填写根地址；若它要求 OpenAI API Base，通常填写带 `/v1` 的地址。最终以该客户端的官方文档为准。

使用本地带鉴权模型发现返回的准确模型：

```bash
curl http://127.0.0.1:9042/v1/models \
  -H "Authorization: Bearer <key>"
```

这份列表是本地读取，包含当前可路由且由代码持有的 Alias 与合格 Custom ID。四类接口的最小请求体见[接入第一个客户端](first-client.zh-CN.md)。

配置完成后发送一次真实请求，并在 **日志** 中确认。

[用户指南索引](../USER.zh-CN.md) · [文档索引](../README.zh-CN.md)
