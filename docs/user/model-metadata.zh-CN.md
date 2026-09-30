[English](model-metadata.md)

# DSH 模型元数据与推理档位

通过 **应用 → DSH** 安装 OCG 供应商。升级到包含此功能的 OCG 后，需要重新安装或替换一次插件，并重新加载对应的 DSH 运行时。只更新网关不会替换此前安装的插件。读取模型列表会刷新目录；请求最多复用五秒内的目录快照。现有 Key 交接与凭据存储流程不变。

## 传递哪些信息

鉴权后的 `GET /v1/models` 保留 OpenAI 兼容结构和公开模型 ID，已知容量增加为 `contextWindow` 与 `maxTokens`。版本化的 `ocg` 对象包含名称、上下文、最大输出、输入与输出模态、推理支持、显式 `reasoningEfforts`、工具调用事实、来源和状态。字段缺失表示未知，不等于不支持。这个 GET 不访问上游。

DSH 插件将上下文和文本/图片输入映射到原生模型描述，将可选档位转换为 pi-ai 的 `thinkingLevelMap`，并显式使用 OpenAI Chat Completions 格式。后续协议转换仍由 OCG 的现有配置负责。例如 `{"low":"low","high":"high","xhigh":"max"}` 仅提供 Low、High、Xhigh，并按声明发送对应参数。未声明档位全部禁用，包括 Off。只有 `reasoning: true` 不会凭空生成档位菜单；`off: "none"` 是明确的协议声明，不是自动默认值。

DSH 现有原生接口并不使用所有能力。额外事实保留在 Adapter 模型描述的 `ocg` 字段中，不代表新增了音视频传输、托管工具或任意能力展示页面。最大输出能力不会被写入 `configuredMaxTokens`，因此不会悄悄变成每次请求的默认输出额度。

目录没有已知上下文容量时，插件仍使用有界的内部兼容默认值，但会省略整个公开的 `context` 描述，`ocg.fallbacks` 会标明使用兜底的字段。DSH 要求存在 `context` 时必须包含正整数 `contextWindow`；空对象会导致模型加载失败。已声明的上下文容量继续正常显示。错误元数据仅阻止对应模型解析，不影响其他有效模型。

## 目录刷新与人工声明

本版在 Go/GOAT 目录刷新和已保存的可配置 HTTP 连接刷新时，采集上游明确提供的元数据，不根据模型名称猜规格。其他适配器，以及只返回 ID 的上游，在接入其元数据来源之前需要人工声明。应刷新供应商目录以采集新信息；只打开 DSH 不会触发供应商目录请求。

路由从未获知的字段——既没有人工声明、上游观测也没有该字段的值——回退到公开的 [models.dev](https://models.dev) 目录。OCG 在后台下载 `https://models.dev/api.json`（绝不在 `/v1/models` 请求内联网），本地缓存约一天刷新一次；刷新失败保留上一份缓存并稍后重试，离线时只是继续使用旧副本。匹配按精确上游模型 ID、ID 的最后一段、精确公开模型 ID 依次进行，不做模糊名称猜测。同一 ID 出现在 models.dev 多个供应商下时只保留共同保证（下限容量、模态交集、一致的档位拼写），`text`/`image`/`audio`/`video` 之外的模态在采集时丢弃。effort 类型的 `reasoning_options` 会转换为可选思考档位（`none` 拼写对应 `off` 档）；纯开关和预算 token 类型的选项没有可选择的线路拼写，档位保持未知。生效事实按字段保持优先级：人工声明 > 上游观测 > models.dev > 未知。声明或目录刷新始终在其已知字段上覆盖公开目录；人工声明不会被公开目录补充。当 models.dev 在上游观测之下补齐了空缺字段时，该行的 `sources` 列表会同时标注两个来源。

在 Dashboard 中声明元数据：打开 **供应商**，选择一个连接，使用模型行的 **模型能力** 操作。表单显示当前生效的元数据及其来源（`operator`、`upstream`、`modelsdev`、`unknown`），应用与服务端一致的校验规则，在 CAS 下保存整份声明，也可以清除人工声明以恢复目录发现的事实。留空表示未知，不等于不支持。

同样的规则也通过带 Dashboard 登录会话的接口提供给脚本使用：

```
GET /dashboard/api/v4/destinations/{id}/model-metadata
```

连接 ID 来自 `GET /dashboard/api/v4/destinations`。响应包含当前版本、精确的公开/上游模型 ID、有效元数据及来源 `operator`、`upstream`、`modelsdev`、`unknown`。推理 Key 不能代替 Dashboard 登录会话。

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

`publicModel` 必须精确匹配已保存目录映射。声明替换该映射的整份元数据，不修改全局同名模型，也不是逐字段叠加。明确发送 `metadata: null` 可删除人工声明，恢复使用目录事实；省略该字段会被拒绝。旧版本写入被拒绝且不修改信息。Dashboard 表单与该接口共享同一套 CAS 行为；表单是默认途径，接口保留给脚本使用。

只应声明实际网关路径可用的能力。此操作不改变路由、协议开关、凭据授权、账号状态或验证结果。不确定的可选字段应省略。容量必须为正的安全整数，最大输出不得大于上下文；档位只能使用 `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`。

## 别名与路由安全

一个别名对应多个启用映射时，容量取共同已知的下限，模态取交集，档位只有在所有映射的协议参数一致时才保留。存在未知候选就不能宣称完整保证。此策略优先避免误报，不会简单宣传最强后端的容量；本版未增加按能力筛选后端的回退调度。

元数据与人工声明绑定连接路由、协议及精确模型映射；改变这些配置会使旧绑定失效。对单模型配置的独立上游地址，不会套用另一个连接地址的发现结果。路由不变时刷新不会覆盖人工声明。不会将上游原始正文或回显凭据保存为模型元数据。
