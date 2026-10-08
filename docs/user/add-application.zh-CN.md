[English](add-application.md)

# 手动客户端配置

本指南通过普通 Gateway API 直接连接客户端。[应用页面](applications.zh-CN.md)还提供 Codex、Kimi Code、MiniMax Code、ZCode 和 VS Code Copilot 的本机 BYOK 配置，以及 DSH 插件接入流程。
这些流程自动创建或复用对应 harness 名称的普通 Key，使用全部已发布模型，无需再次单独选择模型。本机配置会先生成只读预览，只有确认审阅后的计划才会提交；计划过期时必须重新准备。

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

配置完成后发送一次真实请求，并在 **日志** 中确认。若客户端需要重新加载原生配置，请关闭后重新打开；文件保存成功本身不代表客户端已经加载。

## VS Code Copilot

推荐从 **应用 > VS Code Copilot** 下载 VSIX，在 VS Code 执行 **Extensions: Install from VSIX**，再运行 **OCG: Connect**。模型目录、上下文和输出限制由 OCG 动态提供，Key 保存在该 Profile 的 SecretStorage。详见[扩展接入与旧配置迁移](applications.zh-CN.md#vs-code-copilot)。

### 原生 Custom Endpoint 兼容配置

本机自动配置使用[应用 > VS Code Copilot 流程](applications.zh-CN.md#vs-code-copilot)。手动配置时，在 VS Code 打开 **Chat: Manage Language Models**，选择 **Add Models > Custom Endpoint**，编辑生成的 `chatLanguageModels.json`。保留顶层数组和其他供应商。[官方指南](https://code.visualstudio.com/docs/agent-customization/language-models)说明原生界面；OCG 的高级格式基线为 [VS Code 1.141](https://github.com/microsoft/vscode/blob/1.141.0/extensions/copilot/src/extension/byok/vscode-node/customEndpointProvider.ts)。

下面是一条显式 Chat Completions 配置。把 `public-model-id` 替换成准确的已发布模型 ID，把 `<OCG Key>` 替换成你的 Key。数值是示例客户端配置预算，不是供应商元数据。按已验证的输入、输出上限约束配置值；两者之和超过已知上下文窗口时，按比例缩小两者。这些值用于 VS Code 的上下文管理和输出预留；实际请求参数与上游限制取决于客户端和模型，不能保证每次请求都有硬性输出上限。

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

模型发布的首选协议为 Responses 或 Messages 时，使用 `apiType: "responses"` 与完整 `/v1/responses` 地址，或者 `apiType: "messages"` 与 `/v1/messages`。保留部署子路径。供应商层省略 `url`，以使用显式模型列表；使用字面 Authorization 鉴权头时省略 `apiKey`。此鉴权头也适用于 OCG 的 Messages 端点。文件和备份含 Key 明文，请保持私有。

示例保守关闭未知工具和视觉能力。验证工具支持后才能设置 `toolCalling: true`，Agent 选择器要求此值。同样，验证图片支持后才能启用 `vision`。自动适配器从 OCG 的模型元数据页面读取这些声明。Chat/Responses 已验证的推理选项在 `supportsReasoningEffort` 中使用准确且去重的参数值，`reasoningEffortFormat` 设置成匹配的 API 类型；不要编造 Messages 选项或思考预算。

选择 VS Code secret storage 时，通过其原生供应商界面输入 Key，再使用生成的 `apiKey: "${input:...}"` 引用与 `requestHeaders.Authorization: "Bearer ${apiKey}"`。引用必须由 VS Code 创建，仅在外部文件中输入引用不会填充 secret storage。OCG 自动适配器写入字面鉴权头，不写入该存储。唯一的现有 OCG 供应商可通过应用页的预览明确接管；替换被修改的 OCG 所属值需要明确确认，重复或畸形条目仍然阻止。接管保存私有基线，撤销接管会保留后续客户端偏好。

从外部编辑文件前完整退出 VS Code，随后重新打开，在 Chat 模型选择器中选择 **Open Console Gateway**。发送请求并检查 OCG **日志**。Chat、Agent、inline chat 和 utility tasks 可使用 BYOK 模型；行内补全和 Next Edit Suggestions 不在此集成范围内。Agent Host BYOK 属于实验功能，需要在 VS Code 设置 `chat.agentHost.byokModels.enabled`。自动流程不改变默认选择，也不修改 `settings.json`。

---

[用户指南索引](../USER.zh-CN.md) · [English](add-application.md) · [文档索引](../README.zh-CN.md)
