[English](model-metadata.md)

# DSH 模型元数据与推理档位

通过 **应用 → DSH** 安装 OCG 供应商。升级到包含此功能的 OCG 后，需要重新安装或替换一次插件，并重新加载对应的 DSH 运行时。只更新网关不会替换此前安装的插件。读取模型列表会刷新目录；请求最多复用五秒内的目录快照。现有 Key 交接与凭据存储流程不变。

## 传递哪些信息

鉴权后的 `GET /v1/models` 保留 OpenAI 兼容结构和公开模型 ID，已知容量增加为 `contextWindow` 与 `maxTokens`。版本化的 `ocg` 对象使用 `schemaVersion` 2。它包含名称、上下文、最大输出、输入与输出模态、推理支持、显式 `reasoningEfforts`、工具调用事实、来源和状态。有合格路由能承载该模型时，还会包含 `protocols`。能力字段缺失表示未知，不等于不支持。这个 GET 不访问上游。

`protocols` 只在这次响应里推导，不写入元数据记录。`preferred` 取 `chat_completions`、`responses` 或 `messages`。`supported` 按这个固定顺序列出已授权的上游协议。一条协议出现在列表中，表示有合格路由可以发送它：目的地和模型已启用，名称解析到该映射，并且有一把已启用、ready、绑定已启用、范围允许该模型、在路由需要时持有 Key、并持有端点授权的凭据。多条映射按 routing rank、目的地 id、上游模型排序。`preferred` 是第一条映射里已授权的已保存首选；该首选未授权时，是第一条已保存且已授权的协议。冷却、探测标记、鉴权错误和上次胜出的凭据不改变这个对象。`supported` 只命名这些上游协议。它不表示每种协议都保留全部能力，也不指定调用方使用哪一个客户端 URL。`protocols` 缺失，或者 `preferred` 缺失、非法、或不在 `supported` 中时，结果是未知或无效。这两种结果都不是 Chat Completions。

`reasoning` 与 `reasoningEfforts` 彼此独立。`reasoning` 记录是否支持推理。`reasoningEfforts` 把选择器档位映射到精确的分类 `reasoning_effort` 拼写。同一映射可以原样出现在 Chat Completions 和 Responses 上。它不会变成 Messages 的思考预算或自适应强度。某个 Responses 供应商是否接受历史 Chat 拼写，仍以该后端合约为准。

```json
{
  "id": "my-model",
  "object": "model",
  "ocg": {
    "schemaVersion": 2,
    "name": "My model",
    "contextWindow": 262144,
    "maxOutputTokens": 32768,
    "inputModalities": ["text", "image"],
    "outputModalities": ["text"],
    "reasoning": true,
    "reasoningEfforts": {"low": "low", "high": "high", "xhigh": "max"},
    "toolCalling": true,
    "sources": ["operator"],
    "status": "declared",
    "protocols": {
      "preferred": "messages",
      "supported": ["chat_completions", "messages"]
    }
  }
}
```

上面的数字和 Messages 首选只说明形状，不是某个真实模型的规格。能力记录里有事实时 `status` 为 `declared`，能力记录为空时为 `unknown`。没有合格路由能承载该模型时省略 `protocols`。

DSH 插件要求 `ocg.schemaVersion` 为 2，并且 `protocols.preferred` 出现在 `protocols.supported` 中。它从当前 DSH 运行时加载 `openai-completions`、`openai-responses` 和 `anthropic-messages` 三种 API，并按该模型的首选协议各选一种。DSH 0.2.0-rc.2 与 pi-ai 0.87.1 提供这三种 API。插件包不自带这些库，安装也不按版本允许列表拦截。Chat Completions 与 Responses 保留公布的 `/v1` 基址。Messages 去掉末尾的 `/v1`，并保留部署子路径。Messages 客户端把 Gateway Key 放在 `x-api-key` 中，并带上 `anthropic-version`；插件不改写这些请求头。准备好的调用会在 `prepare` 的等待返回之前，冻结当时为该模型采集的目录元数据。配置缺失或无效的行保留为 `ocg-rejected` 占位，并记入该模型的错误。它不会被注册成可执行的 Chat 模型。

`reasoningEfforts` 把这些精确的分类拼写带到 Chat Completions 和 Responses。`reasoning` 为 true 时，Messages 仍保留原生推理能力。OCG 不从该标志写入 Messages 档位菜单或思考预算：这些拼写不会变成 Messages 的预算或自适应强度。原生 SDK 可以套用自己的预设或默认控件。省去 OCG 菜单并不描述该控件，也不表示供应商接受 SDK 的默认值。在 DSH 插件中，选择目录未声明的 Messages 推理档位是明确的不兼容。`supported` 里列出一种协议，并不因此增加菜单或保证某项能力。某个 Responses 供应商是否接受历史 Chat 拼写，仍以该后端合约为准。例如 `{"low":"low","high":"high","xhigh":"max"}` 只提供 Low、High、Xhigh，并在 Chat Completions 和 Responses 上发送这些分类拼写。未声明档位保持禁用，包括 Off。只有 `reasoning: true` 不会凭空生成可选档位。`off: "none"` 是明确的协议声明，不是自动默认值。

已签名的 assistant 历史只有在 provider、API 和模型 id 都与即将调用的模型一致时才会发送。在基础适配器把外来的已签名 assistant 消息变成纯文本之前，Harness 会检查已经出现的 pi-ai 信封：种类为 `pi-ai`、版本为 2、与 assistant 内容对齐；信封带有不透明原生数据时，provider、API 和模型也必须相同。缺少回放元数据时，普通文本和工具调用仍然可以携带。请求到达 Gateway 之后的转换仍按[协议转换](protocol-conversion.zh-CN.md)中的已保存路由执行。

DSH 现有原生接口并不使用所有能力。额外事实保留在 Adapter 模型描述的 `ocg` 字段中，不代表新增了音视频传输、托管工具或任意能力展示页面。最大输出能力不会被写入 `configuredMaxTokens`，因此不会悄悄变成每次请求的默认输出额度。

目录没有已知上下文容量时，插件仍使用有界的内部兼容默认值，但会省略整个公开的 `context` 描述，`ocg.fallbacks` 会标明使用兜底的字段。DSH 要求存在 `context` 时必须包含正整数 `contextWindow`；空对象会导致模型加载失败。已声明的上下文容量继续正常显示。错误元数据仅阻止对应模型解析，不影响其他有效模型。

## 目录刷新与人工声明

本版在 Go/GOAT 目录刷新和已保存的可配置 HTTP 连接刷新时，采集上游明确提供的元数据，不根据模型名称猜规格。其他适配器，以及只返回 ID 的上游，可以使用下文的公开目录兜底；公开目录没有可用事实时，再补充人工声明。应刷新供应商目录以采集新信息；只打开 DSH 不会触发供应商目录请求。

路由从未获知的字段——既没有人工声明、上游观测也没有该字段的值——回退到公开的 [models.dev](https://models.dev) 目录。OCG 在后台下载 `https://models.dev/api.json`（绝不在 `/v1/models` 请求内联网），并按供应商缓存其模型与 API 地址。下载失败、超时，或 HTTP 200 但正文不是可用目录时，都保留上一份缓存并稍后重试。离线时继续使用这份最后成功的副本。当前格式的缓存大约一天内重复使用。旧版本只保存扁平模型索引的缓存仍然可以回答，即使文件本身还新，下次也会立即刷新。

已保存路由的 URL 与某个供应商一致时，使用该供应商自己的模型行：协议、主机和端口相同，且供应商路径是路由路径按段对齐的前缀。最长的匹配路径胜出。模型条目可以自带 API 地址，该地址替换供应商地址，只作用于这个模型。匹配到的供应商如果没有列出该模型，或列出了空的推理档位，搜索即到此为止；不会改用同一主机上更宽的供应商，也不会改用 canonical 目标。

没有匹配到供应商的路由使用模型 ID 的通用基线。这是对未识别代理的推断，不是对该代理的核实。该精确 ID 的各行如果共同指向同一个 canonical 目标且目标存在，就采用目标的事实。因此通用自定义路由仍能发布 `gpt-5.2`、`gpt-5.3-codex`、`o3` 这类 ID 的 canonical 档位，即使另有无关供应商根本没有这些行。链接互相冲突、目标缺失或没有链接时，只保留这些行共同具备的事实。写成 `provider/model` 的 ID 直接读取该目录行，不再跟随下一跳 canonical。查找顺序仍是精确上游 ID、ID 的最后一段、精确公开 ID，不做模糊名称猜测。`text`/`image`/`audio`/`video` 之外的模态在采集时丢弃。effort 类型的 `reasoning_options` 会转换为可选思考档位（`none` 拼写对应 `off` 档）。单独的 `reasoning: true`、纯开关和预算 token 都不会生成档位。

生效事实按字段保持优先级：人工声明 > 上游观测 > models.dev > 未知。人工声明替换整份记录，不会再用公开目录补字段。上游观测若明确给出空的推理档位，就保持为空。当 models.dev 在上游观测之下补齐了空缺字段时，该行的 `sources` 列表会同时标注两个来源。

在 Dashboard 中声明元数据：打开 **供应商**，选择一个连接，使用模型行的 **模型能力** 操作。表单显示当前生效的元数据及其来源（`operator`、`upstream`、`modelsdev`、`unknown`），应用与服务端一致的校验规则，在 CAS 下保存整份声明，也可以清除人工声明以恢复目录发现的事实。留空表示未知，不等于不支持。**别名**页会展示每个映射的生效输入模态及其来源，未知行上的“去声明”链接会直接落到这个编辑器。

同样的规则也通过带 Dashboard 登录会话的接口提供给脚本使用：

```
GET /dashboard/api/v4/destinations/{id}/model-metadata
```

`GET /dashboard/api/v4/model-metadata` 一次聚合返回所有连接的同类条目——需要批量读取时用它，不要把逐连接接口扇出成几十次请求。

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

一个别名对应多个启用映射时，容量取共同已知的下限，模态取交集，档位只有在所有映射的协议参数一致时才保留。存在未知候选就不能宣称完整保证。`ocg.protocols` 使用同一组合格映射，并按 routing rank、目的地 id 和上游模型排序。同一模型启用了多条协议路由时同样处理：每条路由贡献已核实供应商的事实；路由没有匹配供应商时贡献通用基线。两者都没有的路由视为未知，并撤回正面保证。此策略优先避免误报，不会简单宣传最强后端的容量；本版未增加按能力筛选后端的回退调度。

元数据与人工声明绑定连接路由、协议及精确模型映射；改变这些配置会使旧绑定失效。对单模型配置的独立上游地址，不会套用另一个连接地址的发现结果。路由不变时刷新不会覆盖人工声明。不会将上游原始正文或回显凭据保存为模型元数据。

---

[用户指南索引](../USER.zh-CN.md) · [English](model-metadata.md) · [文档索引](../README.zh-CN.md)
