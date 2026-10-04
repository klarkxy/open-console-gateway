[English](README.md)

# 用于 DSH 的 Open Console Gateway

这个包由 Open Console Gateway 桌面应用生成并安装。它在所选 DSH 配置中注册 `ocg`，并从本地 Gateway 读取当前已鉴权的 `GET /v1/models` 目录。每个模型按其已发布的首选协议调用对应 API：`openai-completions`、`openai-responses` 或 `anthropic-messages`。

运行库，包括这三种 pi-ai API，从当前 DSH 安装加载。DSH 0.2.0-rc.2 与 pi-ai 0.87.1 提供它们。本包不声明、也不安装私有的 DSH/pi-ai 对等依赖；升级 DSH 时不应把更旧的运行时拉进该配置。安装不按版本允许列表拦截。

安装程序通过一次性的私有活动文件把所选 Gateway Key 交给 DSH。这个包在每个 DSH 运行时上激活一次。激活时插件用重命名认领该活动文件，导入其中的值，并删除残留的认领文件，因此更新的活动交接不会被解除链接，过期认领也不会让激活一直挂起。凭据存储失败且没有更新的活动文件时，认领会恢复以便重试。

不要手工复制或编辑这个生成的包。需要修复时，从 **应用 > DSH** 页重新运行安装程序。

## 模型细节与推理档位

`model-catalog.js` 要求 `ocg.schemaVersion` 为 2，并且 `protocols.preferred` 出现在 `protocols.supported` 中。它把 `chat_completions` 映射为 `openai-completions`，把 `responses` 映射为 `openai-responses`，把 `messages` 映射为 `anthropic-messages`，每个模型一种 API。未通过这些检查的行是 `ocg-rejected` 占位，并记入该模型的错误。它不会被注册成 Chat。

Chat Completions 与 Responses 保留公布的 `/v1` 基址。Messages 去掉末尾的 `/v1`，并保留部署子路径。Messages SDK 把 Gateway Key 放在 `x-api-key` 中，并带上 `anthropic-version`。本包不改写这些请求头。

准备好的调用会在 `prepare` 的等待返回之前，冻结当时为该模型采集的目录元数据。已签名的 assistant 历史只有在 provider、API 和模型 id 都与正在调用的模型一致时才会发送。在基础适配器把外来的已签名 assistant 消息变成纯文本之前，Harness 会检查已经出现的 pi-ai 信封：种类为 `pi-ai`、版本为 2、与 assistant 内容对齐；信封带有不透明原生数据时，provider、API 和模型也必须相同。缺少回放元数据时，普通文本和工具调用仍然可以携带。这次预检属于插件，并在基础适配器之前运行。客户端已经丢掉的字段，Gateway 无法再套用这次检查。

`reasoningEfforts` 是 Chat 选择器到精确分类协议拼写的映射。同一映射可以原样出现在 Chat Completions 和 Responses 上，不会变成 Messages 的思考预算或自适应强度。即使 `reasoning` 为 true，OCG 写入的 Messages 档位映射仍为空，该标志也不会制造菜单或预算。在本插件中，选择目录未声明的 Messages 推理档位是明确的不兼容。`supported` 列出一种协议，并不因此增加菜单或保证某项能力。某个 Responses 供应商是否接受历史 Chat 拼写，仍以该后端合约为准。选择器提供这些档位之前，需要明确的档位到协议拼写映射；模型名称和单独的 `reasoning: true` 不会制造档位列表。未知上限仍标为回退值，不是上游规格。最大输出能力与每次请求的默认输出额度是两件事。

升级 OCG 后，通过 **应用 > DSH** 重新安装本包，并重新加载所选运行时一次，以替换此前安装的插件。刷新供应商的模型目录，以填入新的上游元数据。只有 ID 的目录可以使用[英文指南](../../docs/user/model-metadata.md)或[中文指南](../../docs/user/model-metadata.zh-CN.md)中的按路由声明。

## 运行时检查

在 OCG 源码检出目录中，对已安装的官方 DSH CLI 入口运行隔离安装检查：

```sh
OCG_DSH_SMOKE_BIN=/absolute/path/to/node_modules/@deepseek-ai/dsh/lib/bin.js \
  node scripts/dsh-application-install-smoke.mjs
```

该检查创建临时配置和一个 loopback 模拟网关。它检查插件安装、凭据交接、原生上下文与档位列表、流式完成，以及 `xhigh` 是否按声明的 `max` 协议值发送。它不接触真实用户的 DSH 主目录，也不调用生产上游。
