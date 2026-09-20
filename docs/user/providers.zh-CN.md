[English](providers.md)

# 供应商

左侧列表展示 V4 connection 投影：至少已有一把 Key 的内置供应商，以及每一条可配置 HTTP 连接（有无 Key 都列出，包括已保存草稿和迁移后的 Custom API 记录），按 Plan/API 分组。同名或同 URL 的既有连接不会自动合并。未使用的内置模板只留在 **添加供应商** 目录中。行与详情头的状态只来自服务端投影字段：**草稿**、**待补充凭据**、**已停用**、**凭据无效**、**无可用模型**、**冷却中**；它们是本地资格，不是上游健康。已配置详情里的 **添加 Key** 打开与 **账号** 页相同的凭据编辑器并预填该连接。Endpoint、鉴权、协议、模型映射和按模型覆盖地址都由本页管理；账号页只管理每把 Key 的范围、启停、额度关系与顺序。修改地址或模型覆盖时会列出受影响 Key，只有你明确选中的 Key 才会增加当前端点的安全授权。删除连接前必须先移除引用它的 Key；删除不会级联。页面按 `connection=<id>` 选中，旧 `provider=<id>` 书签仍会解析到对应连接。草稿、完成设置、未知提交结果与修订冲突继续使用同一 onboarding/CAS 规则；模型目录仍有独立搜索和筛选，窄屏可查看公开名称与上游名称。

开启模型会强制启用全部 available 协议，而不只是恢复 `auto`。可用上游一律用同一种芯片：亮着表示可通，蓝色为转换默认；点选设为默认。首选协议独立于启用状态保存，并随节点迁移包传递。模型测试与连接测试不会开启模型或更换协议。

## 协议默认值与连接测试

空草稿可以通过 **删除供应商** 移除。如果已经有账号，需要先移除这些账号；删除供应商不会替你删除账号。

每个用户定义供应商提供默认地址、协议和鉴权方式。模型没有覆盖时继承供应商配置；模型显式设置协议与地址时优先使用模型设置，清除覆盖即恢复继承。鉴权仍归供应商所有。每条带 Key 的路由都要求选定 Key 已保存对应的端点和 Origin 授权。修改供应商地址或模型覆盖不会授权新目标。同一上游模型的多个公开别名必须解析到相同路由。

路由在客户端协议已启用时透传；否则转到模型首选协议，再按适配器顺序回退到其余已启用协议。CPA 保持 Chat、Responses、Messages 客户端格式，Gemini 转为 Chat。

官方预设按文档默认值初始化新供应商，手动配置初始为未验证的 Chat。不自动扫描其他协议，模板更新不改写已经保存的选择。New API、Sub2API 是可包含多个独立实例的站点类型，其站点与管理凭据边界不变。

**测试模型**只按所选模型的有效配置发送最小请求，不猜测其他地址，不自动开启协议、不改变优先级。草稿测试需要显式填写临时 Key，账号测试使用该账号已保存的 Key。模型目录拉取是独立动作，不证明推理一定可用。

默认值复核于 **2026-09-09**：xAI 使用 [Responses](https://docs.x.ai/developers/model-capabilities/text/comparison)，MiniMax 使用 [Messages 与 Bearer 鉴权](https://platform.minimax.io/docs/api-reference/text-chat-anthropic)。其他预设保留文档中的兼容默认值，内置模型设置与用户手动禁用状态继续生效。

创建预设供应商时，可按 Plan/API、厂商和地区变体浏览[渠道预设](provider-presets.zh-CN.md)。这些模板与默认展示的已配置连接分开。固定预设显示准确连接摘要，并填入可修改的默认对话模型；Azure、Bedrock 仍需资源或地区地址，以及部署或模型信息。从 **供应商 → 添加供应商** 或 **账号 → 新增账号**（同一套选择器）**保存草稿** 会把连接留在列表并标为 **草稿**（不参与路由）。**完成设置** 需要模型；keyed 鉴权还需要 Key，或在继续设置时保留已保存 Key。两个入口使用同一次 onboarding commit。模板变化不会改写已保存连接。

要接入另一个上游或贡献内置集成，请先阅读[新增供应商](add-provider.zh-CN.md)；其中包含用户定义供应商、Custom API 与密封适配器注册表路径。

**供应商** 是供应商控制面——如果你的旧书签还挂着 `?view=pricing`，进来的就是这个视图。

适配器注册表保持静态密封。密封适配器与用户定义供应商共用本页，并按来源标注 **供应商预设** 或 **自定义**。Custom API 是迁移后可配置 HTTP 连接的兼容身份。范围划分如下：

- 每个精确的密封供应商合约使用 `Provider(contract_scope_id)`；scope ID 即供应商自身的 ID。
- 用户定义供应商作为类型化定义持久化，并绑定 Configurable HTTP。它们的 Endpoint、协议、鉴权方式和映射在本页编辑。
- 遗留 `CustomEndpoint(account_id)` 范围继续保留每把 Key 的探测证据；连接配置统一在**供应商**页编辑。

供应商预设与可配置连接共用详情壳。**模型** 展示目录或公开名称 → 上游 ID 映射；**价格** 不会在没有快照时编造价格；**设置** 展示并编辑 HTTP 连接信息，密封供应商保持只读。迁移后的 Custom API 连接也在此编辑和删除。**OpenCode Go** 的托管注册 **邀请链接** 仍在设置页签，它不是密封源。用户定义供应商未定价。

**别名** 是独立的核心页面，因为它的表覆盖当前已启用账号，而不是某一个选中的供应商。表里只出现至少有一个启用账号的供应商，并把 CPA 单独列成一个供应商。公开名称和精确上游身份来自这些供应商的合约、用户定义映射、Custom 能力，以及已选中的 CPA 目录。公开名称与其他上游 ID 重叠时会显示检查提示。可以按公开名称、上游 ID 或供应商搜索。对外模型名左侧的开关控制是否对下游列出该名称：默认开启，会出现在已鉴权的 `GET /v1/models` 中；关闭后该列表不再包含它。关闭的名称仍留在本页，知道名称的客户端仍可调用。供应商目录上的启用开关仍然决定是否参与路由。

**模型目录** 是本地的。每个范围按当前目录中每个模型一行渲染，列依次为：模型（别名加原始上游 ID）、上游协议、启用、操作。可用上游一律用同一种芯片，亮着表示可通，蓝色为转换默认。MiniMax CN 与 Kimi Code CN 一开始就是 Chat Completions 与 Messages，不宣称 Responses。**启用** 开关控制模型是否参与路由：开启即强制启用全部 available 协议，关闭则模型退出路由，也不会出现在 `GET /v1/models` 中。开关会先立即更新显示，再在后台执行带 CAS 保护的保存，只有受影响的行显示保存进度。**多选** 进入多选模式：表格左侧出现复选框，工具栏右侧的 **开启**、**关闭** 与 **删除** 只作用于已勾选的行。每一行也可以单独从本地目录删除该模型。删除后该 ID 不再路由；**刷新模型目录** 时，官方仍提供的 ID 可能再次出现并默认关闭。

删除最后一个 Zen Free 模型后，重新加载或重启仍保持空目录；只有显式刷新目录，官方 ID 才可能再次出现。如果删除已保存、随后运行时重载失败，已删除 ID 仍会停止接受新请求，同时操作会报告重载错误。

底层静态、预设与探测证据仍保留在合约中，但按模型列出的列表不显示独立徽标。显式开关写入覆盖前，存储默认仍是 `auto`。连接测试只记录观察结果。Custom 或用户定义连接轮换 Key、或改 Endpoint／协议时，会丢掉该账号的探测观察，旧结果不能代表新 Key 或新地址。账号尝试失败会报告并保留证据，但不会把共享协议固定为 `force_off`，只有显式关闭开关才会这样做。

可刷新范围都在 **刷新模型目录** 时从该供应商官方 `/models` 取模型列表，并在同一次动作里按官方文档写入协议。**OpenCode Go** 读取 `https://opencode.ai/docs/go/` 的 Endpoints 表。**Zen Free** 刷新 `https://opencode.ai/zen/v1/models`，去掉 `-free` 后再查同一张表。**Command Code GOAT** 读取 `https://commandcode.ai/docs/provider`：有分模型表用表，否则 Anthropic ID 用 Messages，其余用 Chat Completions。`stealth/ox-alpha` 不给协议。文档没有的模型，或抓取失败时，默认 Chat Completions。刷新保留已有的手动开关和探测。**MiniMax CN** 与 **Kimi Code CN** 刷新各自官方 `/models`，并按文档家族规则同时支持 Chat Completions 与 Messages，不宣称 Responses。面板不提供单独的恢复快照动作。新发现的非预设 Command Code 模型默认关闭。当前 effective 目标协议保持生效，直到显式改写该行。

轻量来源信息、刷新动作与模型列表共用同一块内容区域。所有可刷新的范围使用同一个动作：OpenCode Go 由后端选择符合条件的 Go 账号访问官方鉴权目录；Zen Free 访问固定的官方无鉴权目录 `https://opencode.ai/zen/v1/models`；Command Code 直接访问固定的公开官方 `/models` 目录，不选择账号。刷新是控制面动作：供应商页按钮，以及面板为该供应商保存第一张就绪账号时自动做的一次刷新。只要账号就绪且有已存 Key，即使账号开关仍关闭也可以取目录。

MiniMax 与 Kimi 需要一个符合条件的账号 Key。MiniMax 刷新 `https://api.minimaxi.com/v1/models`；Kimi 刷新 `https://api.kimi.com/coding/v1/models`。保存的模型只激活代码内的密封映射；无法匹配的模型保留为精确 raw ID。MiniMax 把 M3、M2.7/M2.5/M2.1 的标准与 highspeed 变体，以及 M2 映射到对应的小写 kebab Alias。Kimi 映射为 `kimi-for-coding` → `kimi-k2.7-code`、`kimi-for-coding-highspeed` → `kimi-k2.7-code-highspeed`、`k3` → `kimi-k3`、`k3-256k` → `kimi-k3-256k`。转发始终保留每个准确的上游 ID。

首次成功刷新前目录为空：没有检入的模型列表。刷新成功后，保存的官方 `/models` 快照是权威目录。刷新新增的模型会出现在列表中。OpenCode Go、Zen Free 与 Command Code 的新增行默认关闭（上游协议来自官方文档，文档未列出则 Chat Completions），只有手动打开才会启用。MiniMax CN 与 Kimi Code CN 的新增行默认使用文档家族规则：Chat Completions 与 Messages，Responses 不受支持。仍留在目录中的模型会保留既有覆盖与探测结果；刷新失败或结果为空时继续保留旧快照。

迁移后的 Custom API 连接保留公开名称 → 上游 ID 映射，发现结果不会静默替换。既有连接只按公开名称解析；普通新建可配置 HTTP 连接也允许唯一的精确上游 ID。Command Code 使用官方公开的 `/models` 目录：GOAT 预设默认开启，后续发现的额外模型默认关闭，只有在列表中开启其受支持协议后才会供应。

本地目录会进入解析，请求时不会再访问上游。内置 Alias 权威是静态且由代码持有：最早 OpenCode Go 表提供 Go 名称，密封 MiniMax CN、Kimi CN 与选定 GOAT 长名称映射表提供供应商 Alias，但不会据此新增 Go 路由。Command 会先去掉 Provider 命名空间并复用已有代码持有的 Alias；只有短名已获授权时才去掉已知套餐后缀。例如 `nvidia/nemotron-3-ultra-550b-a55b` 使用 Alias `nemotron-3-ultra`。保存的 CN 行只激活其精确密封映射。无法匹配的 Command/MiniMax/Kimi 模型保留为精确 raw ID，不会作为新 Alias 公布；CN 映射仍保留上游 ID 的准确拼写。Zen Free 按官方 `-free` 后缀公布去掉后缀后的 Alias，原始 `-free` ID 始终可作为精确 raw pin 使用，见 [Zen Free 模型](routing.zh-CN.md#zen-free-模型)。

当某个供应商的全部模型都关闭时，该供应商不再产生路由。带鉴权的下游 `GET /v1/models` 只公布可路由的公开名称；raw-only 身份和 raw 名称冲突都会排除。歧义 raw 身份以 `ambiguous_model_id` 失败，绝不请求上游。

适配器开放连接测试的内置供应商行提供 **测试** 按钮，测试当前有效配置选择的协议。供应商会按已保存的路由顺序尝试符合条件的 Key，并在首次成功后停止。OpenCode Go 与 Zen Free 使用各自可构造的协议集合；GOAT、MiniMax CN 与 Kimi Code CN 只测试密封路径。可配置 HTTP 的具体 Key 测试使用该连接的有效模型路由和鉴权。模型必须属于当前目录；真实最小请求可能消耗额度，并写入脱敏日志。单把 Key 失败不会禁用其他可服务同一协议的 Key。

**价格** 按所选供应商限定范围。默认目录投影按目录顺序包含所有 `offering=plan` 且 V3 `pricingAvailability=available` 的行。快照按 `provider_id` 请求和缓存；渲染器按返回的 `models` 或 `values` 结构选择表格，并从 token 范围、`time_window` 与 `adjustments` 生成档位；未知 adjustment 标签原样显示。来源链接只取后端快照。刷新要求 V3 定价可用，倍率编辑还要求 V4 `pricingMultiplierEditable=true`；V4 不可用时保持只读。

**刷新价格表** 只抓取并校验当前所选 Provider 自己的官方来源。OpenCode 与 Command Code 的 revision 和最后成功快照彼此独立；一个失败不会动另一个。以后某个 Provider 若包含多个有价格的 Plan，一次操作也只刷新该 Provider 内的 Plan。刷新仍只能手动发起：

- OpenCode Go 展示 revision、文档更新时间、token 单价、`Usage`（官方文档中这一列名为 **Monthly limit**）和额度扣减倍率，点击刷新后才会访问 `https://opencode.ai/docs/go/`。抓取或校验失败时继续使用最后一次成功快照。allowance 不是额度池、不会参与路由，只用于推导扣减倍率（“账号月窗口 / 模型月额度”）。临时覆盖会创建新的持久化 revision，供后续估算使用。
- Command Code GOAT 展示从 `https://commandcode.ai/docs/plans/goat` 保存的官方费率快照。带分时费率的模型会保留官方每日高峰窗口（UTC 01:00–04:00、06:00–10:00）及独立的输入、输出、缓存读取价格。每个已定价模型的应用倍率都可手动修改并保存；新请求使用保存后的 Provider revision 计算，缺失或歧义行仍为 unpriced。刷新若将覆盖手动倍率会先请求确认。它与 OpenCode Go 分开；账号卡可显式从 Command Code 第一方 `/alpha/billing/credits` 账号端点校准 `$14 / $35 / $70` 三个窗口。官方 CLI 使用该端点，但公开 Provider API 文档未列出。两次快照之间继续累计 OCG 内已定价日志，并保留手工修正；GOAT 不做自动用量同步。
- Zen Free 未定价（额度按出口 IP 共享）。
- Custom API 为 unpriced：成功转发记 `cost_state=unknown`，不扣额度。没有官方用量窗口。已知主机的当前余额读取（DeepSeek / Moonshot）只用于展示。
- Ollama Cloud 刷新公开且无需鉴权的目录 `https://ollama.com/v1/models`，不选择账号。发现的行立即启用 Chat Completions；Responses 与 Messages 不受支持，也没有协议探测入口。目录刷新仅在剥离 `:` 标签后恰好命中一个目录 id 时，才向 Go 拥有的别名追加一个可路由 Ollama 映射。带日期标签的快照 id 来自运行时目录。手动价格刷新读取 `https://ollama.com/pricing`（Model / Input / Cached input / Output），配额倍率固定 `1.0`。新建账号必须选择 Pro/Max/Team 并填写购买日期。账号卡按官方每请求用量与该档估算一个月 USD Credits 窗口；实际已用可以超过软上限，进度条只把显示钳在 100%，不会写冷却或改变路由。无计费行的既有账号仍可路由且无进度条。
- MiniMax CN 与 Kimi Code CN 在 OCG 内为 unpriced，但账号卡可手工读取官方订阅窗口（`/token_plan/remains` 与 `/usages`）。这些快照只用于展示，不影响推理资格。
- Custom API 与用户定义 Provider 账号，若保存的 Endpoint 主机恰好是 `api.deepseek.com`、`api.moonshot.cn` 或 `api.moonshot.ai`，可手工读取官方当前余额。其他 Custom 主机不会被探测。

请求时流程：别名 → 账号资格 → 适配器上限 → 已保存合约 → 按模型/按协议 effective 状态 → 透传或转换。协议选择使用已保存的合约。带鉴权的 `GET /v1/models` 与受保护的 `GET /dashboard/api/v4/application-models` 只公布当前可路由且 effective 协议已启用的公开名称。`application-models` 仍是 Go 别名 ∩ 当前价格快照，不含 Custom。

---

[用户指南索引](../USER.zh-CN.md) · [English](providers.md) · [文档索引](../README.zh-CN.md)
