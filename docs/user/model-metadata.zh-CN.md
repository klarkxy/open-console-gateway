[English](model-metadata.md)

# DSH 模型元数据与推理档位

通过 **应用 → DSH** 安装 OCG 供应商。升级到包含此功能的 OCG 后，需要重新安装或替换一次插件，并重新加载对应的 DSH 运行时。只更新网关不会替换此前安装的插件。读取模型列表会刷新目录；请求最多复用五秒内的目录快照。现有 Key 交接与凭据存储流程不变。

## 传递哪些信息

鉴权后的 `GET /v1/models` 保留 OpenAI 兼容结构和公开模型 ID，已知容量增加为 `contextWindow` 与 `maxTokens`。版本化的 `ocg` 对象包含名称、上下文、最大输出、输入与输出模态、推理支持、显式 `reasoningEfforts`、工具调用事实、来源和状态。字段缺失表示未知，不等于不支持。这个 GET 不访问上游。

DSH 插件将上下文和文本/图片输入映射到原生模型描述，将可选档位转换为 pi-ai 的 `thinkingLevelMap`，并显式使用 OpenAI Chat Completions 格式。后续协议转换仍由 OCG 的现有配置负责。例如 `{"low":"low","high":"high","xhigh":"max"}` 仅提供 Low、High、Xhigh，并按声明发送对应参数。未声明档位全部禁用，包括 Off。只有 `reasoning: true` 不会凭空生成档位菜单；`off: "none"` 是明确的协议声明，不是自动默认值。

DSH 现有原生接口并不使用所有能力。额外事实保留在 Adapter 模型描述的 `ocg` 字段中，不代表新增了音视频传输、托管工具或任意能力展示页面。最大输出能力不会被写入 `configuredMaxTokens`，因此不会悄悄变成每次请求的默认输出额度。

旧网关或仅有 ID 的目录仍使用有界的内部兼容默认值，但公开的已解析描述不会把默认上下文冒充真实规格，`ocg.fallbacks` 会标明使用兜底的字段。错误元数据仅阻止对应模型解析，不影响其他有效模型。

## 目录刷新与人工声明

本版在 Go/GOAT 目录刷新和已保存的可配置 HTTP 连接刷新时，采集上游明确提供的元数据，不根据模型名称猜规格。其他适配器，以及只返回 ID 的上游，在接入其元数据来源之前需要人工声明。应刷新供应商目录以采集新信息；只打开 DSH 不会触发供应商目录请求。

通过带 Dashboard 登录会话的接口读取连接元数据：

```
GET /dashboard/api/v4/destinations/{id}/model-metadata
```

连接 ID 来自 `GET /dashboard/api/v4/destinations`。响应包含当前版本、精确的公开/上游模型 ID、有效元数据及来源 `operator`、`upstream`、`unknown`。推理 Key 不能代替 Dashboard 登录会话。

向同一路径发送 `PUT`，附上最新 CAS 版本令牌来声明信息。下面数字仅为格式示例，不是任何真实模型的规格：

```json
{
  "expectedRevision": 123,
  "processGeneration": 456,
  "publicModel": "my-model",
  "metadata": {
    "name": "我的模型",
    "contextWindow": 262144,
    "maxOutputTokens": 32768,
    "inputModalities": ["text", "image"],
    "outputModalities": ["text"],
    "reasoning": true,
    "reasoningEfforts": {"low": "low", "high": "high", "xhigh": "max"},
    "toolCalling": true
  }
}
```

`publicModel` 必须精确匹配已保存目录映射。声明替换该映射的整份元数据，不修改全局同名模型，也不是逐字段叠加。明确发送 `metadata: null` 可删除人工声明，恢复使用目录事实；省略该字段会被拒绝。旧版本写入被拒绝且不修改信息。本版提供声明 API，没有新增 Dashboard 表单。

只应声明实际网关路径可用的能力。此操作不改变路由、协议开关、凭据授权、账号状态或验证结果。不确定的可选字段应省略。容量必须为正的安全整数，最大输出不得大于上下文；档位只能使用 `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`。

## 别名与路由安全

一个别名对应多个启用映射时，容量取共同已知的下限，模态取交集，档位只有在所有映射的协议参数一致时才保留。存在未知候选就不能宣称完整保证。此策略优先避免误报，不会简单宣传最强后端的容量；本版未增加按能力筛选后端的回退调度。

元数据与人工声明绑定连接路由、协议及精确模型映射；改变这些配置会使旧绑定失效。对单模型配置的独立上游地址，不会套用另一个连接地址的发现结果。路由不变时刷新不会覆盖人工声明。不会将上游原始正文或回显凭据保存为模型元数据。
