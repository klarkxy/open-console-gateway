[English](runtime-invariants.md)

# 运行时不变式

运行系统的行为不变式。修改 Gateway 路由、别名、Zen Free、本地限制策略、套餐目录、访问 Key、出站代理、用量同步或按 Key 额度恢复前请先阅读本文。代码是最终权威；本页梳理容易出错的语义。

## Gateway 与模型列表

- New API / Sub2API 父目的地保存站点根地址，包括部署子路径。平台静态合约按所选协议从该根地址得到 `/v1/chat/completions`、`/v1/responses` 或 `/v1/messages`；模型发现访问同一根地址下的 `/v1/models`。普通可配置 HTTP 目的地继续使用精确保存的路由，不推导同级路径。平台首个端点 ID 仍按操作确定，来源授权仍绑定同一站点来源。

- 通用 HTTP 模型目录刷新使用带 CAS 的 `POST /dashboard/api/v4/destinations/{id}/catalog/refresh`。根据已保存的配置路由派生 `/models`，只使用已经获准访问该精确路由的就绪凭据，并在网络读取后再次检查连接和凭据身份。无鉴权路由不发送 Key。发现结果以精确同名映射写入 `destination_models`，新增模型按官方已知或已配置协议启用。既有关闭状态只有在有支持证据或显式 `force_off` 时才保持关闭；未知 Auto 行等待官方证据，刷新补充证据后可以启用。刷新保留映射、首选、覆盖路由、Key 范围/授权、验证与冷却，同时可以补充文档化协议声明。空响应或失败不写入；截断结果保留旧条目并报告截断。刷新绝不授权 Key 或扩大范围。HTTP 预设和 Custom API 共用此操作；内置目录继续使用各自适配器。
- 通用 HTTP 目的地要么使用遗留路由语义（空 `protocol_routes`：保存的基址/鉴权适用于遗留协议集），要么保存一到三条互不重复的显式协议路由。显式路由为每种协议保存完整端点和鉴权；第一条仍是稳定的遗留默认值。运行时物化、授权、目录刷新和探测共用同一个路由解析器。读路径不会改写已保存主机，也不会合成兄弟路由。单模型上游覆盖仍是单条权威路由，不能据此增加兄弟协议能力。
- GOAT 协议发现按每个模型文档化的 `supported_endpoints` 处理。刷新不增加授权。操作者开启新的 GOAT 协议而选中 Provider Key 缺少所需路由授权时，控制面写入必须明确选择要授权的凭据，并在同一事务中保存授权与 `force_on`。不得静默新增或撤销授权。

- Core Gateway：Axum + Tokio + reqwest。同一端口暴露 OpenAI Chat Completions / Responses、Anthropic Messages 与 Gemini `generateContent` 客户端入口。
- 已认证的 `GET /v1/models` 首先列出最早 OpenCode Go 静态协议表中、已在刷新后的目录里启用协议的可路由 Alias（首次 `/models` 刷新前列表为空）、可唯一解析的 Go 目录原始 ID，以及密封 MiniMax CN、Kimi CN 和选定 GOAT 长名称映射，然后合并已保存用户定义供应商的对外模型名，以及符合条件的 HTTP 凭据所绑定的模型 ID（启用、ready，并在所选鉴权方式需要时提供 Key；验证为可选）。Go 新 ID 保持绑定 Go，不会自动变为跨供应商别名；有歧义的名称不发布。保存的 Zen `-free` 行保留精确 raw pin，并公布去掉后缀后的 Alias；保存的 Command 行可以加入任一代码持有的 Alias；保存的 MiniMax/Kimi 行只激活代码中精确列出的 CN 映射。含 `/` 的 Command id 会公布唯一的最后一节小写 kebab Alias；不含 `/` 且无法匹配的 Command 行，以及无法匹配的 MiniMax/Kimi 行，只保留精确 raw pin。受保护的 `GET /dashboard/api/v4/application-models` 来自已保存 Go 目录中可解析且已启用协议的名称，不再要求价格表覆盖；价格未知仍为未定价。两条 GET 路径都不会请求上游。Go 公开目录刷新不携带 Key，不依赖账号、凭据有效性、额度或冷却状态；MiniMax CN / Kimi Code CN 只要账号就绪且有已存 Key，即使开关关闭也可以取目录。刷新目录不验证账号。具有文档化协议的新模型默认启用，已保存的关闭选择仍然生效。Custom ID 来自符合条件账号的声明能力。**别名**页关闭的对外名称不会出现在 `GET /v1/models` 中，但仍可路由；`application-models` 不过滤。未知模型名在所有支持的客户端格式上返回 `400`。
- Custom 模型身份：合格 Custom 能力同时保存公开模型名与精确 `upstream_model`。`/v1/models` 只公布可路由公开名称。发现只返回上游 ID，导入时精确写入 `public_model = upstream_model`；不剥离后缀，不合成 Alias。raw 冲突从公布列表排除，并以 `ambiguous_model_id` fail-closed。别名页聚合既有合约与能力。对外名称左侧的开关（默认开启）只影响已鉴权 `GET /v1/models` 是否列出该名称，不改变路由。
- Gemini 客户端使用 `/v1beta/models/{model}:generateContent` 或 `:streamGenerateContent`（`/v1/models/...` 也接受），可以用 `x-goog-api-key` 认证；Gemini 只是一种客户端格式，Gateway 总是把请求转换成目标模型的推荐上游协议。未知模型名在 Chat / Responses / Messages / Gemini 上返回 `400`。
- `MODEL_PROTOCOLS` 只提供已知模型的离线协议默认值和既有共享别名，不再充当 Go 模型目录白名单。Go、Zen、Command 刷新时读取官方协议文档；只有明确端点或 Command 的官方模型家族规则提供新证据。文档读取失败或漏列模型时，保留原有证据和已保存首选协议。没有证据的完全未知模型不推断可用协议，保持 Auto。具有官方已知或已配置协议的新模型会启用。既有关闭协议/模型状态只有在有支持证据或显式 `force_off` 时才保持关闭；否则后续官方声明可以启用 Auto 行。在已启用、已配置、且已授权给所选 Key 的协议中，已启用的客户端协议直通，否则按已保存首选协议和适配器顺序转换。该选择在发送前完成。请求路径不探测协议，也不发送付费发现请求。OpenCode Go 的 effort 别名只在所选路由携带该兼容策略时应用。操作者可以从已保存的本地目录删除模型；该写入只改本地快照，之后的官方刷新可能把相同 ID 作为启用的新行加回来。CPA 保持 Chat、Responses、Messages 客户端协议，Gemini 转为 Chat。请求路径使用已保存合约。整篇 JSON 转换内核在 `ocg-gateway` 中。
- 每一次 OpenCode Go 推理尝试都携带 `x-opencode-session`。客户端已有值优先；否则 Gateway 依次映射 OpenCode 自定义 Provider 发送的 `x-session-id` / `x-session-affinity`、根据会话种子生成稳定且不泄露原文的摘要，只有在请求不存在可用会话种子时才使用请求 ID。Zen Free 使用同一套 session 头，并补上官方匿名通道身份头（`User-Agent` 含 `opencode`、`x-opencode-client`、`x-opencode-request`、`x-opencode-project`）。客户端已提供的 OpenCode 值优先；否则 Gateway 在选定账号后补齐（合成的 `User-Agent` 为 `opencode`，`x-opencode-client` 为 `cli`，request/project 回退到请求 ID / session）。其他 Provider 不携带这些头。这组 Provider 专用身份只在选定账号后添加。

一次逻辑请求在设置锁内捕获 `RequestSnapshots`。`RoutingSnapshot` 直接读取持久化目的地、模型映射与执行凭据；缺失或损坏配置会明确失败。兼容 Account 行与历史创建类型不参与正常路由选择。

| 请求入口固定 | 每次发送重新检查 |
| --- | --- |
| 配置、传输与定价 | 凭据启用、准备状态与 Key 版本 |
| 精确目的地和模型映射 | 目的地、模型、协议仍匹配且启用 |
| 原始候选身份和 Endpoint ID | 当前模型范围、Endpoint/Origin 授权与绑定 |
| 显式别名固定映射 | 当前冷却、本地策略等待、共享 Free 限制与按 Key 额度恢复 |

目标变化后拒绝旧候选，不重新映射。迟到的鉴权或冷却结果不能修改已轮换的 Key。模型列表、路由解释和转发共用模型身份及有效协议判断；解释不解密，也不推进选择器状态。

- 协议转换在版本化的 `legacy_compat` 配置下丢弃可选托管工具时，会把该标记带到请求计划上。真正发起转发时再发出运行时诊断，只包含 request id、compat profile/version 和被丢弃的托管工具类型，绝不包含请求正文或密钥，也不会改写已存储的协议配置。

- 供应商卡片在 `settings.routing_cards_v1` 保存稳定的展示分组，多张卡可引用同一目的地。`GET /dashboard/api/v4/routing/cards` 返回同一 revision 的卡片、目的地与凭据；CAS 保护的 PUT 将完整卡片列表及其展开后的 `credentials.routing_rank` 顺序一起提交。观察者不进入卡片。同一目的地内移动凭据不会改变授权、身份、额度池、冷却或额度恢复。兼容的纯排序写入只按请求顺序整理卡片边界。

## Dashboard 控制面：V3、V4 与 V2 墓碑

- 连接验证和模型／协议探测要求成功 JSON 与请求协议匹配：Chat 返回包含 message 对象的 choices，Responses 返回 output 且状态不是失败／取消／等待中，Messages 返回带 content 块的 assistant message。任意 JSON 或非空 error 对象不能证明连接成功。V4 模型测试只选一条精确保存的路由和一把已授权的就绪凭据；请求有界，不能启用模型/协议、设置首选、增加授权或写入路由。其回执绑定目的地、凭据版本、模型、协议和已解析配置；任一项改变后旧回执均不再适用。Custom／动态供应商草稿探测单次最多 30 秒（配置更短则使用配置值），不持久化路由或发现的模型。Responses 最小探测预算为 16 个输出 token，以满足实际兼容上游的最小值；Go／Zen 探测与正常转发共用供应商身份头构造，其他供应商不附带 OpenCode 身份头。轮换 Custom 或用户定义 Key，或改该连接的 Endpoint／协议，会删除该账号 `custom_endpoint` 探测观察并递增合约 scope revision，旧成功不能给新 Key 或新地址当授权。Go Key 轮换不会清掉内置目录证据。

- Dashboard V3 在 `/dashboard/api/v3` 是 410 墓碑。操作处理器挂在 `/dashboard/api/v4`。控制面变更需要 CAS（`expectedRevision`，以及 `processGeneration`；价格写入还需要 `expectedPricingRevision`）。`ConnectionInfo` 是唯一允许返回明文 Open Console Gateway Key 的 V3 响应 DTO；Key 变更响应不包含明文，客户端会重新 `GET /connection`。创建或轮换托管 CPA 客户端 Inference Key 时，该 CPA 密钥只返回一次。
- `GET /contract` 返回当前进程的 live revision / generation token。
- Dashboard V4 是 `/dashboard/api/v4` 上唯一存活的控制面。它复用 V3 会话中间件，其列表返回与 V3 CAS 相同的 `ControlRevision` 令牌。V4 变更使用同一套 CAS 令牌。`POST /onboarding/commit` 按 `operationId` 幂等，并在 CAS 之前用载荷摘要绑定；同一 id 配不同载荷返回 `409` `operationPayloadMismatch`，绝不重放。`complete` 在已有 Key 时要求绑定已授权，不要求账号开关打开（新建就绪 Key 卡默认启用）。一次 commit 的全部对象落在同一 SQLite 事务中；响应与已存结果均不含密钥。内置和平台托管目的地保持不可变；可配置 HTTP 目的地（包括遗留 Custom API connection）使用带 CAS 的 `PATCH|DELETE /destinations/{id}`。PATCH 只给用户明确选中的 credential id 增加安全来源授权；仍有任一凭据引用时 DELETE 拒绝。只读路由包括 `GET /contract`、`GET /templates`、`GET /connections`、`GET /accounts`、`GET /destinations`、`GET /credentials` 与 `GET /routing/explain`；这些读取不会发出出站请求，路由解释也不会解密密钥、写日志／冷却／额度恢复或推进选择器与额度试探状态。connection id 是由遗留身份派生的确定性 UUIDv5，从不由名称或 URL 派生。没有账号的内置项是模板而不是 connection；Custom 账号从不按 URL 合并。资格与授权都是本地投影：`unknown` 不是 `valid`，资格也不是上游健康。`GET /accounts` 是身份列表（`IdentityList`）；`authState` 是本地状态（`unknown` 绝不是 `valid`；`valid` 需要既有验证记录）。`POST /credentials/{id}/rotate` 只走 CAS（没有 `operationId`）：凭据/绑定 id 不变，`version` 与 `authStateVersion` 递增，`authState=unknown`，并清空账号鉴权/验证与额度恢复，避免旧版本污染新 Key。`POST /credentials/{id}/quota-retry` 同样只走 CAS，请求体为扁平的 `MutationExpectation`：只让该凭据有资格进入下一次正常选择，不发出站请求、不改启用、不清除退避；已是 `ready` 或 `probing` 时幂等。`GET /credentials` 可从本机状态带上可选可空的 `quotaRecovery`（缺省不是已验证的上游健康）。`PATCH /bindings/{id}` 同样只走 CAS，可独立改 `modelScope` / 绑定 `enabled`。`POST /identities/{id}/credentials` 再加一把 Key 并复用该身份。新 Key 默认使用独立额度池；加入已有池必须显式指定同一身份下的共享关系。面板会消费该控制面用于供应商页 rail、用户定义供应商创建，以及账号页身份叠加（`GET /accounts`）；Key 轮换、绑定编辑、身份内新增凭据，以及已重挂的账号 / 设置 / 认证 / 日志 / 转移路由都走 V4。
- DSH 应用流程是窄范围 V4 路由 `GET|POST|DELETE /applications/dsh`。GET/POST/DELETE 接受可选的 `profilePath` 与 `runtimeUrl`。原生 `web` / `desktop` 默认分别为 `http://127.0.0.1:3080` 与 `http://127.0.0.1:19387`；真正变更的是页面上显示的运行来源。所选 profile 只提供 Home 与会话上下文；HTTP 路径只写入 OCG 自有插件包和交接文件。Web/Desktop 以及任何已提供的运行地址走正在运行的 plugin-manager HTTP（仅 loopback），并在内存中用所选同用户 `DSH_HOME/.credentials.yaml` 的 browser-session grant 签发 cookie；这是本机兼容，不是公开的外部鉴权 API，不受支持的 grant 格式会明确失败。该路径没有桌面 CLI 回退。Editor 托管的 profile 在未提供 runtime URL 时保持原有离线 CLI 语义。GET 检查选中的 profile、OCG 自有插件根目录、已连接时的运行时行，以及一次性私有凭据交接；POST 要求 live CAS 对和 GET 指纹，在 Core 内按 id 解析一把已启用的 Gateway Key，并只把本机副作用委托给已注册的原生 Host。DELETE 使用同一对 CAS/指纹且不带 `keyId`，只移除 `@open-console-gateway/dsh-plugin`。桌面应用与默认原生 CLI 构建注册同一份 `dsh-local-host` 实现；官方 Docker 镜像构建 CLI 时排除这项能力并返回 `unsupported_runtime`。CLI 路径上 Host 用参数数组调用 DSH 官方插件命令，不把 Key 放进源码或 argv；命令结束后读回准确插件登记，安装失败时只恢复先前的 live 交接文件。Host 只原子写入或替换 live 交接文件；安装或回滚期间从不删除 `.claimed-*` 文件，因为它无法区分崩溃残留与插件正在使用的 claim。已有 claim 保持不动。live 与 claim 同时存在时以 live 为准；这条规则避免 Host 删掉它写入 live 之后插件立刻认领出来的 claim。claim 文件只由插件拥有：激活时用 rename 认领 live 交接文件并导入该 Key；`credentials.set` 成功后（或凭据存储失败并完成既有恢复/对账后）删除所有 `.claimed-*` 旁路文件，避免 `activationRequired` 一直为真。inspect 在存在 live 或任一 claim 时报告 `activationRequired`。假定单个 DSH 运行时一次只执行一次插件 `apply()`；残留 claim 只表示崩溃遗留，不是第二个消费者。live 优先于残留 claim；没有 live 时取最新的残留 claim。若凭据写入失败且没有更新的 live，则把 claim 放回以便重试。不支持并发插件消费者：较旧的 in-flight `credentials.set` 仍可能在新 live 写入之后完成，但 live 会留给下一次激活。其模型列表在 DSH 列表/调用时读取带鉴权的 `GET /v1/models`，绝不使用范围更窄的 `application-models` 投影。
- 挂回 V4 上的 Provider 目录是单一的 `ProviderCatalog` 行，由 destinations 与密封 `BUILTIN_PROVIDERS` 联合得到（v56 之后遗留 `providers` / `provider_models` 表已删除）。每条 `ProviderCatalogEntry` 都带 `origin`（`builtin` | `preset` | `custom`）、`editable`、`deletable` 以及行自身的 `offering`（`plan` | `api`）。builtin 行始终 `editable=false` / `deletable=false`；dynamic 行始终可编辑、可删除。`GET /providers/{id}` 返回这种统一视图：builtin 行返回其密封定义，dynamic 行返回已保存的 `ProviderDefinition`。`PATCH /providers/{id}` 和 `DELETE /providers/{id}` 对任何 builtin id 都以 `400` + `code="builtinProviderImmutable"` 拒绝。builtin 适配器不经过该路由。供应商页 rail 渲染 V4 目的地：至少已有一张凭据的内置目的地、每一个用户定义 HTTP 目的地（有无 Key 都列出），以及每一条已持久化 Custom API 目的地（一把或多把 Key），统一展示在可搜索列表中。未使用的内置模板只留在新增账号的「添加新服务」和供应商页的添加目录中，不会出现在「已有连接」。新增账号的「已有连接」与供应商页 rail 使用同一套 V4 connection 投影。`POST /providers` 保存定义；带 Key 的 keyed 鉴权也会在同一次写入中创建首个账号，省略 `key` 则只保存定义。面板的创建流程走 V4 onboarding commit；挂回的 V4 `POST /providers` 仍供 API 客户端使用。无鉴权仍创建单例账号。删除最后一个账号不会删除定义。唯一的“添加供应商”入口在主区打开预设浏览（嵌入式预设表单或手工表单），keyed 鉴权下 Key 为可选。详情页对所有 origin 统一为 模型 / 价格（始终显示目录可用状态）/ 设置（吸收了 OpenCode 邀请链接）三个页签。深链使用 `connection` / `tab` / `add` / `preset`；旧的 `provider` 值在读取时解析为具有该遗留 id 的内置或用户定义 connection；旧 `scope_kind` / `scope_id` 在读取时映射兼容。
- 前端没有封闭的 Plan 家族注册表。创建、筛选、显示名、用量与定价都按挂回的 V4 目录返回顺序直接投影，并以 `provider_id` 为键；成功空目录保持为空。只有目录读取失败时才启用狭窄的 Go 创建／既有 Zen 展示回退。首张就绪账号的模型刷新复用挂回的 V4 Provider Contract 目录能力；Provider quota windows 与刷新动作读取 `usageAvailability` / `manualUsageCalibration`，V4 只新增 `pricingMultiplierEditable`。代理名单候选来自启用目的地模型目录中的精确上游 ID 和有效协议。Settings 与已安装 route set 在启动和相关快照变化时复用同一个构造器；失效持久化 ID 保持 inert，并在保存时剔除。
- 节点迁移挂回 V4：`POST /accounts/transfer/export|preview|import`，并且只允许从回环面板执行；转发 scheme 请求头不会赋予权限。导出没有管理员二次确认。面板只会收到版本 1 的 Argon2id（64 MiB、3 次迭代、单 lane）+ AES-256-GCM 固定 AAD 密文包，绝不会收到账号或接入 Key 明文。密码学工作在进程级单飞限制下执行，且不占用设置或 SQLite 锁；全部响应都带 `Cache-Control: no-store`。预览与导入复用同一解密/校验流程。导入在 KDF 后重新检查 CAS，并通过一次 SQLite 事务写入数据库归并结果。导入采用稳定 ID 身份语义：同 ID 的账号/接入 Key 采用迁移包字段；Plan 或名称相同但 ID 不同的记录可并存；目标端已有账号 ID 保持当前顺序，来源端新增账号 ID 按迁移包顺序接在后面。目标端独有接入 Key 和 Provider 范围保留。不同 ID 的 Key 值冲突、归并后超过 64 个有效子 Key、包内重复 ID 或任一非法行都会拒绝并回滚整个数据库归并。迁移包包含可用普通账号及验证状态、ready 账号 Key、主/有效子接入 Key、可迁移配置、Zen Free 状态/目录以及 Provider 目录/证据/覆盖。同 ID 目标记录保留本机浏览器数据和用量/冷却历史；迁移包账号凭据覆盖时会清除过时的鉴权错误和最近错误。本机额度恢复不导出：未改动的目标 Key 保留它，替换 Key 则清除。临时停调规则是节点本地策略：可移植导出不携带它们，导入也不会清掉无关的本地规则。完整数据目录备份保留已保存文档。来源端浏览器 Profile/Cookie、登录密码、邀请码、日志、用量、冷却状态、额度恢复和未完成托管草稿不迁移。目标端监听/根地址以及操作系统负责的开机启动/Dock 设置保持本机值。所有可能失败的运行态快照构建都读取尚未提交的归并事务；只有构建成功后 SQLite 才提交，随后执行不会失败的快照替换并只递增一次 revision。
- 密码学 envelope 仍为版本 1；portable 内容为 payload V11。它在加密信封内携带目的地、凭据、按模型路由覆盖、明确的模型解析策略、identity/grant/cooldown extras、额度池、观察者管理凭据、显式 HTTP 协议路由与节点状态。导入接受 V4–V11：V7 确定性补上 `modelResolution`，V8 及以上必须显式携带。遗留 Custom 目的地规范为连接所有、多 Key 的结构，同时保持稳定 ID 与 `public_only` 行为。V1–V3 与 V12+ 返回明确的不支持版本错误。按稳定 ID 的事务归并继续保留目标独有行、顺序、授权、更晚冷却和额度池。用户向的备份参考见[升级与备份](../user/upgrade-backup.zh-CN.md)。
- 平台父账号是观察者目的地（`adapter = http`，`observer_credential_id`）加上已关联的推理凭据。schema v54 之后站点配置写在 destinations + credentials 上（父级目的地的 `platform_kind` / `platform_version` / `platform_snapshot`；已关联推理凭据的 `group_json` / 关联 version / 关联 snapshot）。管理密文留在观察者凭据上。V4 GET 仍不含秘密。关联 Key 保存站点根地址，而不是完整的 `/v1/chat/completions` 路径。New API 与 Sub2API 自己会转换 Chat Completions、Messages 和 Responses，因此关联 Key 启用这三种协议，网关把匹配的客户端格式原样交给上游（Gemini 仍转换为 Chat Completions）。Payload V9 携带平台目的地与观察者凭据，管理密钥和平台 extras 只存在信封内；导入分组证据保持未验证。合并导入时，若包中没有 CPA observer key，会保留目标已有 management key。若迁移包复用目标端已有平台父账号 ID 却对应不同站点或种类，预览与导入都会明确冲突，SQLite 归并不写入任何内容。每次关联 Key 的请求尝试冻结精确模型与分组价格，只记录原币估算，不推断钱包扣款或美元费用。刷新由用户手动触发，写回前核对父账号、关联和 Key 身份。New API 的 Key 导入是 `POST /dashboard/api/v4/platform-accounts/{id}/import-keys`：用已存管理凭证列出远端令牌、读取完整 Key，并创建已关联的本地 Custom Key。本地已有、已停用、或无法读取完整 Key / 模型的令牌会跳过；响应不含秘密。New API 的钱包、订阅与 Key 剩余在站点观测到正的 `quota_per_unit` 时换算为 `usd`；否则保持原生 `quota` 单位。该换算是展示用观测值，不是请求费用的美元推断。
- 受保护 V2 REST（`/dashboard/api/...`，不含 V3 与 V4）向已授权面板会话返回结构化 `410`（`code=dashboardV2Removed`）；匿名请求先返回 `401`。V2/V3 认证与会话、browser WebSocket、推理入口保持既有语义。唯一保留的无版本路径是精确的 `auth/status|register|login|logout` 与 `browser/sessions/{token}/ws`。
- `crates/ocg-core/src/dashboard.rs` 处理 SPA `index`/`assets` 与保留的 V2 认证、browser WS 处理器；V2 REST 的墓碑中间件位于 `host_router.rs`。Provider 模型/协议探测是挂回的 V4 路径 `POST /providers/{provider_id}/protocol-probes`：它拒绝 effective Provider 目录之外的模型，不提供账号选择，可按保存顺序遍历符合条件的账号，并记录观察结果，不改变协议开启状态和优先级。账号页使用独立的 operational 路径 `POST /accounts/{id}/model-tests`，请求仅含 `{ modelId }`。该路径按精确账号的当前 scope 准入模型，自动选择有效协议，并且只通过这一账号发送一次最小请求。已关闭的卡也可以测试：启用开关是路由草稿/上线路闸，不是 Key 授权。绑定启停、范围与目的地授权仍然生效。它不改变控制面状态，因此不带 CAS；不会选择同 Provider 的其他账号，不走 Gateway fallback，不改变冷却、额度恢复、验证或启停，也不写 Provider 证据。它不是额度恢复试探。面板在前端顺序编排 **测试全部**，结果只保留在当前弹窗。V2 account 路径授权后返回 `410`。
- 请求可观测性只属于 `forward_logs`：转发尝试、供应商协议探测，以及已认证的本地解析/校验/路由失败都不得重复写入 `gateway_logs`。账号选择前失败使用“未解析/Gateway”归因，token 字段为零且 `cost_state=not_applicable`。`gateway_logs` 只保留运行生命周期、控制面事件，以及请求日志行持久化或收尾失败。未认证请求与预期的本地 fallback 仍不持久化。

## 访问 Key 与认证

- 回环监听器上的所有 Dashboard API 请求（包括公开的注册、登录）都要求唯一的 `Host` 为 `localhost` 或回环 IP 字面量。若提供 `Origin`，必须使用 HTTP/HTTPS 且主机与端口匹配 Host；跨站 Fetch Metadata 请求会被拒绝。本地原生请求可以不带 Origin。转发头会禁用本地免登录，此时仍须持有现有 Dashboard 会话。Vite 代理开发请求时保留原始 Host 和 Origin。使用公开主机名的反向代理必须连接非回环监听器，该监听器使用单管理员登录。

- 权威表是 SQLite `access_keys`（主 Key 固定 id `gateway_keys::PRIMARY_KEY_ID` / `00000000-0000-0000-0000-000000000001`，名称快照为 "Primary"，永不可禁用/删除；子 Key 是非主行，活跃上限 64，软删除保留名称但清空明文）。消毒后的配置 JSON 把 `gateway_key` 存为 `""`。进程内 `AppConfig.gateway_key` 与 `GET /dashboard/api/v4/connection` 暴露 live 主 Key。生命周期只能通过 `/dashboard/api/v4/keys*`（包括 `POST /keys/primary/regenerate`）。主/子 Key 值互斥由 `gateway_keys::ensure_primary_value_allowed` 强制执行（v27 沿革见 `storage-migration.zh-CN.md`）。
- 认证收集所有非空候选头 Bearer / x-api-key / x-goog-api-key；任一匹配凭据快照（`CoreStateInner.credential_snapshot`，含主 Key 与已启用子 Key）即通过，归因按候选头顺序中的第一个匹配；同一快照也用于转发日志名称快照。
- 非 loopback 监听器使用单管理员登录。Docker 可通过 `OCG_ADMIN_USERNAME` 与 `OCG_ADMIN_PASSWORD` 首次初始化（两者必须同时设置；只设一个会导致启动错误）；未提供时，首个注册用户成为管理员。

## 持久化

- 首选协议独立于启停保存。面板模型覆盖项可对每个模型的一个受支持协议设置 preferred=true；选择与覆盖同事务提交，省略或 false 保留选择，恢复静态基线清除选择，节点迁移携带选择。effective 首选协议使用该选择，关闭状态也保留。Go/Zen 默认 Chat 仅在没有官方文档 Static 证据时回填，Zen 还要求 `-free` 模型。动态账号 DTO 不展示推算的购买或到期日期，存储中的兼容日期锚点保留。偏好行可属于 builtin 或 dynamic 供应商 id，`protocol` 取值为 `chat_completions | responses | messages`（schema 沿革见 `storage-migration.zh-CN.md`）。

- 身份模型之前的账号回填为一个身份 + 一份凭据 + 一条 All 范围绑定。新 id 是与 connection id 同一命名空间的确定性 UUIDv5。schema v52 之后遗留 `accounts` 表已删除：`credentials` 持有 Key 材料，身份 / 绑定 / 入职 / 订阅事实落在凭据行与 `credential_grants` 上。已确认身份可以持有多把 Key：`POST /dashboard/api/v4/identities/{id}/credentials` 会再写一份凭据并复用该身份（`connectionId` 不同时即第二件产品）。平台父账号与被关联 Key 保持独立身份：父账号持有 `platform_observer` 凭据（从不用于推理）；被关联 Key 的身份为 `declared`，`authority_site` = 父账号 `base_url`；关系是已声明、未验证。不要按 URL、名称或 Key 前缀自动合并身份，也不要宣称已验证钱包。冷却 / 额度窗口留在凭据与共享池成员上。回填为每个身份创建一个额度池（`relation_confidence` 为 unknown，`policy_mode` 为 authoritative_limit）；新增凭据默认独立；显式选择与同身份凭据共享时才加入选定额度池，并把置信度升为 declared。普通冷却可经由这一显式成员关系扇出；429 临时冷却只留在收到它的 Key。持久化额度恢复状态不扇出。共享额度只来自池成员关系，从不因名称相同而假定。路由会跳过推理绑定已停用、或 `model_scope` 不允许当前公开/路由模型的账号；默认 All + 启用保持既有路线。
- 当前 schema 是 **v63**；迁移沿革见 `storage-migration.zh-CN.md`。目的地与凭据是权威存储。`destinations.model_resolution` 明确记录 `adapter_defined`、`public_only` 或 `public_and_upstream`；遗留 Custom 行保持 `public_only` 与稳定的目的地/凭据 ID，但允许多份推理凭据引用同一连接，包含可空 `protocol_routes_json` 的连接配置只存一份。密封适配器仍编译在代码中，平台与 CPA 的观察者边界不变，迁移不会合成 Alias。
- GUI 数据目录在 Windows 为 `%USERPROFILE%\.ocg-mgr`，macOS/Linux 为 `~/.ocg-mgr`；CLI 默认为 `~/.ocg-mgr-cli`。升级与回滚详见 `docs/maintainer/storage-migration.zh-CN.md`。
- 本机账号 `key_cipher` / `password_cipher` 使用 AES-256-GCM，前缀 `v2:`，正文为 `base64(nonce || ciphertext || tag)`。错误 Host cipher 或损坏密文 fail closed，明文 Key 不进日志。可移植节点迁移使用信封 v1；导出内容为当前迁移 payload，其版本策略见上文的节点迁移条目。
- 下游访问根 URL 优先级：非空 `OCG_CLIENT_ROOT_URL` > SQLite 手动值 > 前端从生产 origin / 开发 Gateway 端口自动推导。环境变量覆盖是只读的，不得写回 SQLite。
- 桌面 Gateway 端口优先级：有效的 `OCG_GATEWAY_PORT` > SQLite `gateway_port`（默认 `9042`）。桌面宿主与 Vite 开发代理读取同一变量。该覆盖只读，不得写回 SQLite，也不改变 CLI `serve --port` 语义。

## 桌面端宿主

- Tauri v2 跨平台托盘应用；主窗口默认隐藏；托盘/单实例逻辑打开 `http://127.0.0.1:<port>/dashboard/`，loopback 监听器自动跳过登录。宿主能力（Gateway 生命周期、原生浏览器、自动启动、Dock、升级器）注册进 `CoreState`，**不会**注册为 `#[tauri::command]` / `invoke_handler`。
- Settings 页通过受保护 `GET /dashboard/api/v4/settings/check-update` 手动检查最新 GitHub Release。内置了升级器公钥的已安装桌面版可下载、校验签名并原地安装；开发版、CLI、Docker 以及未内置升级器公钥的已安装版本走 release 页面 / 手动覆盖路径。

## 出站代理

- 全局出站代理存储在 `AppConfig` 中，支持 auto、manual HTTP、force-direct 与按模型 List；转发使用请求入口快照，热切换不改变飞行中请求。Custom 与动态 HTTP 遵循同一代理策略，不跟随重定向，不转发面板/客户端认证，并独立使用连接所有的 Bearer、`x-api-key` 或无鉴权配置，不再从协议推断。携带秘密的尝试必须命中该 Key 已保存的实际 Endpoint ID 与 Origin；修改路由不会自动授权。Custom/动态 URL 与实际连接 DNS 继续拒绝元数据、链路本地、未指定、组播、AWS IMDS IPv6 与不透明 IPv4 形式。

## 套餐目录与 Custom API 边界

- 套餐目录位于静态 `ocg_domain::provider` 的 `BUILTIN_PROVIDERS`：OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN Token Plan、Kimi Code CN、Ollama Cloud、Custom API 与静态 CPA 外部接入，再加上运行时持久化的用户定义供应商。内部身份只有 `provider_id`（opencode、opencode-zen-free、command-code、minimax、kimi、custom、cpa，或已保存的动态 UUID）。**适配器注册表保持静态密封。** schema v56 之后遗留 `providers` / `provider_models` 已删除。密封目录仍编译在 `BUILTIN_PROVIDERS` 中；用户定义 / 预设 HTTP Provider 存在 destinations（`legacy_kind=dynamic`）与 `destination_models` 上。流量与路由只走密封适配器代码常量。dynamic 行绑定 `ConfigurableHttp`。静态 CPA 外部接入不进入该目录：它是独立的静态产品入口，从不是一行，也不会出现在 Provider 目录中。类型化 Provider 定义在运行时落在 destinations 上，始终绑定 Configurable HTTP。未知 provider ID 除非匹配已保存的 dynamic 定义或密封 builtin id，否则 fail closed。从不加载用户脚本、插件或二进制。用户定义供应商默认未定价/未知；匹配的官网 API 预设或按账号配置的积分计量可提供费率。本地积分估算不会增加权威额度路由限制。匹配的 DeepSeek/智谱官网 API 预设例外见下文「官网 API 账务依据」，不增加额度裁决能力。发现与真实模型测试是可选控制面动作，从不阻挡保存；真实测试可能消耗上游额度。供应商所有的 Endpoint/协议/鉴权/映射在供应商页编辑；账号 Key/启停/顺序/备注留在账号页。供应商更新里的 Key 只用于必须的无鉴权 → 需 Key 转换，并写到那张单例账号；已经带 Key 的供应商拒绝任何非空更新 Key，绝不会覆盖一张或全部账号。Key 轮换仍在账号页。无鉴权定义只暴露一张单例账号。动态供应商的创建/更新/删除必须在 SQLite 提交前构造提交后的运行态快照，只提交一次，再无失败地安装该快照、重置路由，并只递增一次 `settings_revision`；提交前构造失败则数据库、内存和 revision 都不变。Command Code GOAT 可路由；其官方公开 `GET /models` 是供应商目录发现，不是对已保存 Key 的验证。历史 GOAT 验证状态统一为 `not_required`，真实 Key 鉴权边界仍是推理请求返回的 401/403。账号只贡献 启用、ready，并在所选鉴权方式需要时提供 Key 的凭据与顺序，模型供应由供应商模型/协议合约控制：GOAT 内含预设和具有文档化支持端点的新发现模型默认开启，已保存的明确关闭选择保持关闭；没有账号级 GOAT/全部或 Max 权限模式。已验证的 GOAT 价格快照提供逐请求的价格与倍率核算，并绝不回退套用 Go 价格。官方 CLI 使用的固定第一方 `/alpha/billing/credits` 端点提供手工校准证据，但公开 Provider API 文档未列出。GOAT 目录的用量可用性为 `available`，支持显式官方刷新与本地手工校准，但自动同步和权威额度路由限制仍关闭。刷新会校验 `$14 / $35 / $70` 套餐并原子校准本地三个窗口，随后继续累计 OCG 内已定价日志；月重置时间仍由购买日期推导。官方用量失败或显示用量已满都不会写入推理冷却、额度恢复或改变路由。推理 429 使用按 Key 临时等待并触发用量刷新；GOAT 错误正文不建立持久化额度耗尽。所有持久化变更路径仍会在写入、revision 或 timestamp 变更前拒绝启用真正 `routable=false` 的静态 Provider；桌面 UI 只通过 Dashboard V4 HTTP 变更。
- GOAT 的内含模型预设，以及每个具有文档化 `supported_endpoints` 的新发现模型，默认开启。模型/协议若有证据确认关闭或存在显式 `force_off`，仍保持关闭。其余未知行以 Auto 等待官方协议证据，证据可用后刷新可以声明并启用。给缺少所需路由授权的 Key 新增 GOAT 协议，仍必须按上文在同一事务中显式授权。
- MiniMax（`minimax`）与 Kimi（`kimi`）是两个独立密封适配器，不是 Custom 预设。MiniMax CN 使用固定、禁止重定向的 `https://api.minimax.cn/v1` 推理/目录路由及文档化的 `/anthropic` 路由：Chat Completions 与 Responses 使用 Bearer，Messages 使用 `x-api-key`。旧的 `https://api.minimaxi.com/v1/token_plan/remains` 用量端点保持不变。Kimi 使用固定的 Chat Completions 与 Messages 路由及其文档化鉴权。两者都提供需要鉴权的显式 `/models` 刷新、每个 429 的按 Key 临时冷却，以及 unpriced 转发。MiniMax `1008`/`2056` 与 Kimi 403 文案仅用于错误处理，绝不创建持久化额度状态。MiniMax 密封映射覆盖 `MiniMax-M3`、M2.7/M2.5/M2.1 的标准与 highspeed 变体，以及 `MiniMax-M2`，客户端使用对应的小写 kebab Alias。Kimi 不改名发布滚动产品 ID `kimi-for-coding` 与 `kimi-for-coding-highspeed`，不再将它们标成固定模型版本；`k3` 映射为 `kimi-k3`，`k3-256k` 映射为 `kimi-k3-256k`。转发始终保留精确上游 ID，固定版本 Alias 绝不会解析到 Kimi 的滚动 ID。Kimi 手工读取 `https://api.kimi.com/coding/v1/usages`。返回窗口经过大小限制后，在 V4 CAS 下只替换同账号同来源的快照。这些数据只用于展示：不参与周期轮询，也不改变推理资格；429 可触发受节流的刷新。其他 Kimi/GLM 普通或私有端点仍归 Custom API；GLM 没有内置 Plan。
- Custom API（`custom`，`routable=true`）是可配置可信管理员 HTTP 目的地的兼容身份。目的地拥有名称、默认 URL、鉴权、协议、公开名称 → 上游 ID 映射与可选按模型协议/URL 覆盖；凭据拥有加密 Key、启停、全局顺序、模型范围、发送授权、冷却、额度关系和按 Key 额度恢复。既有 Custom 目的地保持稳定 ID、即使同名或同 URL 也不合并，并保持 `public_only` 解析。多把推理 Key 可以引用一条连接；删除最后一把 Key 后连接保留。`PATCH /dashboard/api/v4/destinations/{id}` 只给用户明确选中的 credential ID 增加安全授权；`DELETE` 在仍有 Key 引用时拒绝。密封与平台管理目的地拒绝这两类变更；旧账号级 Custom 编辑在共享连接上拒绝。
- `GET /dashboard/api/v4/routing/explain` 是只读观察：复用真实别名解析、物化、资格门与选择器克隆预览，不发送、不解密 Key、不做 DNS 探测、不写日志/冷却/额度恢复，也不推进粘性、轮询或额度试探。响应明确把对话绑定、本次重试排除、快照后状态、发送前凭据复查与上游结果列为运行时不确定项。

## 外部接入

- CLI 导入前按不区分大小写的文件名检查冲突，上传后按同一名称核对。CPA 上传 API 不提供原子的“仅在不存在时创建”；现有 OCG 变更锁串行化 OCG 自身的导入，但无法保证另一个管理客户端同时写入相同名称时的跨客户端只创建语义。

- 用户主动触发的本机 CLI 导入是对来源凭据读取的有限例外：`GET /external-integrations/cpa/cli-imports` 仅检测元数据；带 CAS 的 `POST` 才读取一个固定 provider 路径，在内存中转换白名单 OAuth 字段，并通过 CPA typed 凭据上传接口发送。两个入口均要求本机面板传输边界，不接受浏览器传入文件路径或 Token。源文件原样保留，OAuth 内容不进入 OCG 持久化或响应，上传错误正文不会转发。支持 Codex、Claude Code 文件存储、Kimi Code 和兼容的 Grok OIDC 条目；Antigravity 和仅存于系统凭据库的来源保留新登录。确定性文件名用于重试核对，不覆盖已可见的现有账号。明确被拒绝的写入不增加 revision；可能已提交但尚无法确认的上传消耗 revision 并返回 `unconfirmed`。此功能不改变 CPA auth 的回滚排除规则，也不增加后台同步。

- 外部接入是设置下方的静态产品入口，不是 Provider/Plan 插件。CPA 是当前接入。OCG 可以通过 typed V3 adapter 配置和操作用户部署的本机服务，并且不会读取该服务的私有 auth 文件。在 Windows x64、macOS 或 Linux x64 的桌面或 CLI Host 上（不限构建 profile），OCG 还可以在数据目录下安装、启动、更新、回滚和停止一个由 OCG 拥有的 CPA 子进程（`cpa/versions/<version>`、`config.yaml`、`auth/`、`logs/`、`managed.json`）。`managed.json` 是所有权标记，只保存当前/上一版本、精确资源 SHA-256 和回环端口，从不保存 PID 或密钥。启动只能手动；子进程只继承 stdout/stderr，在 Windows 上加入 kill-on-close 的 Job Object，在 Unix 上由同一可执行文件的临时监督进程持有独立进程组，并随应用退出。Unix Host 持有 close-on-exec 生命周期管道；管道 EOF 触发有时限的 TERM/KILL 清理，清理完成前监督进程始终持有进程组身份。即使后代进程保留输出管道，日志读取线程也能有界退出。OCG 从不停止、替换或删除外部 CPA，也不按端口或 PID 杀进程。安装/更新是一次内存单飞操作，只使用官方 `router-for-me/CLIProxyAPI` GitHub Releases 中对应的 Windows x64 zip 或 macOS/Linux tar.gz 与 `checksums.txt`；解压有界，拒绝路径穿越、符号链接/重解析点和重复条目。候选激活前必须通过 health、带版本校验的 Management 鉴权，以及最强的非计费 Inference Key 检查——带鉴权的 `/models`；安装阶段不会发送可能计费的 completion。保留一份上一版本/配置，回滚不触及 `auth/`。检查/更新只能由用户触发。Management 密码只通过 `MANAGEMENT_PASSWORD` 传入，从不写入 `config.yaml`。受保护的 Inference Key 和直连客户端 Key 必然出现在 OCG 数据目录下 CPA 本地配置的 `api-keys` 中。运行时日志只是有界的 stdout/stderr 尾部。移除会先删除 OCG 拥有的 `cpa/auth` 目录及其他规范托管产物并断开数据库，最后才删除 `managed.json`；由于不设 journal，进程若在前序删除期间崩溃，可能留下仍有所有权但内容不完整的运行时，此时支持的恢复方式是重试移除。移除从不触及外部 CPA 路径或进程。其他运行时 fail closed，并给出明确的不可用原因。
- 桌面/CLI 的 CPA 地址只允许 loopback；Compose profile 只允许固定环境覆盖 `http://cpa:8317`。禁止重定向、内嵌凭证、query/fragment、远程/LAN 主机和转发客户端 Key。CPA 推理强制直连，并与全局出站代理隔离。
- CPA 是一张目的地（`adapter = cpa`）加上无密钥凭据，进入普通路由顺序。保留的 CPA 账号 UUID 只作为迁移/遗留 id 桥接。CPA 内部 OAuth 账号只是实时投影；日志只归因到 CPA 订阅池，不虚构内部账号。Management Key 在 OCG 存储中加密，只通过 `MANAGEMENT_PASSWORD` 传给子进程。受保护的 Inference Key 也在 OCG 中加密保存，但 CPA 要求它以及任何直连客户端 Key 出现在子进程本地配置的 `api-keys` 中。创建客户端 Key 时，V4 只返回一次明文；日志和列表保持脱敏。OAuth Token 始终留在 CPA，节点迁移排除全部 CPA 状态。
- CPA 目录只能加入代码持有的 Alias；只有快照里 **enabled** 的 ID 会进入路由集合，未选中的 ID 仍保存在快照中但不发布。其他保存 ID 仍是精确 raw pin，冲突 fail closed。已知和未知的已保存 ID 都保持客户端 Chat、Responses 或 Messages 协议交给 CPA；Gemini 转换为 Chat。CPA 成本/用量保持 unpriced/unknown；任何 CPA 故障只参与普通候选 fallback，不得影响既有路由。

## 别名

- 动态供应商模型默认继承供应商地址和协议，`upstream_override` 可显式覆盖二者。实际转发与账号模型测试共用有效路由解析，鉴权仍归供应商所有。覆盖地址使用与供应商地址相同的可信管理员校验。隔离的 Custom/动态带密钥发送要求实际解析出的端点 id **和** Origin 都在已存绑定授权中；当前连接 URL 不是隐式授权。显式授权过的、当前已配置的外站 Origin 覆盖可以发送；未授权覆盖的 HTTP 命中为零。改供应商 URL 不会授权。路由解析可以构造外站覆盖 AttemptSpec；存档授权才是解密/发送门闩。无鉴权定义仍可指向另一 Origin。已存 Key 的协议探测与账号模型测试走同一条实时授权路径。密封适配器保持密封（官方 Origin 或文档化的回环测试缝）。同一准确上游 ID 的多个公开别名必须解析为相同协议和推理地址；保存或导入时拒绝冲突，不按行顺序挑选。覆盖与模板来源分开保存。草稿测试只请求配置指定的路由，不猜测同级路径。

- 客户端别名位于 `ocg_gateway::alias`（`ocg-core` 的 `alias.rs` 是兼容 facade）。内置 Alias 权威完全由代码持有：`ocg_domain::protocol::supported_model_ids()` 提供最早 OpenCode Go 命名空间，密封精确映射表提供 MiniMax CN、Kimi CN 与选定 GOAT 长名称 Alias，但不会借此新增 Go 路由。Command 会先去掉 Provider 命名空间并复用已有代码 Alias；`-paid` / `-free` 只有在短名已获授权时才去掉；`nvidia/nemotron-3-ultra-550b-a55b` 映射为 `nemotron-3-ultra`，`highspeed`、`vision-exp`、`contributor` 等语义变体不会按长度截断。保存的 MiniMax/Kimi 目录只激活其密封映射。含 `/` 的 Command id 会公布唯一的最后一节小写 kebab Alias；若该短名已是代码持有 Alias 则加入而不是抢占。不含 `/` 且无法匹配的 Command 行，以及无法匹配的 MiniMax/Kimi 行，仍只保留精确 raw pin。Alias 拼写可以大小写折叠；内置 raw ID 则严格区分大小写，包括 MiniMax 官方混合大小写 ID，以及含 `/`、`_` 或空格的 ID。Custom 声明 ID 保持原有大小写折叠 matcher。raw ID 若恰好只有一条注册表映射，则在检查可路由性前先固定到该 mapping；不可路由的 mapping 仍被识别，但无法产生生产路由。重叠的精确 raw ID（包括符合条件的 Custom 声明 ID 与另一套餐 mapping 冲突）返回 `ambiguous_model_id`，不会调用上游。Zen Free 的 `foo-free` 始终是精确 raw pin，并按官方 `-free` 后缀公布 Alias `foo`；共享的已授权 Alias 按账号卡片持久化顺序在 Go/Zen/Command/MiniMax/Kimi/Ollama 候选者中选择。Ollama Cloud 从不创建或抢占 Alias：目录刷新仅在剥除 `:` 标签后恰好命中一个目录 id 时，向 Go 拥有的别名追加一个可路由 Ollama 映射（管理员矩阵钉定会打破轮换平局，并被后续刷新尊重）；带日期标签的快照 id 是运行时目录数据，严禁写进代码。符合条件的 Custom 声明 ID 叠加进解析与 `/v1/models`，但不得抢占已发布 Alias。`/v1/models` 只有在精确保存目录行仍存在且至少一个 mapping 有启用的 effective 协议时才发布该 Alias；供应商全关后不产生路由。转发日志区分 `requested_model`、`resolved_alias` 与 `upstream_model`；`native_cost_*` 为可选。

| 判定 | 结果 |
| --- | --- |
| 代码持有的 kebab Alias | 解析为 Alias；仅在已保存目录行存在且至少一个 mapping 有启用的 effective 协议时发布 |
| 唯一 raw ID | 在检查可路由性前固定到该 mapping；不可路由 mapping 仍被识别但不发布 |
| 重叠的精确 raw ID | `400` `ambiguous_model_id`；不上游 |
| Zen `foo-free` | 始终是精确 raw pin；同时按 `-free` 后缀公布 Alias `foo` |
| GOAT 长名 / 后缀 | 去掉 Provider 命名空间；仅在短名已获授权时剥 `-paid`/`-free`；`nvidia/nemotron-3-ultra-550b-a55b` → `nemotron-3-ultra` |
| MiniMax / Kimi 密封映射 | 保存的目录只激活代码持有映射；未匹配行保持精确 raw pin |
| 符合条件的 Custom 声明 ID | 叠加进解析与 `/v1/models`；不得抢占已发布 Alias |
| `/v1/models` 发布门槛 | 精确保存目录行 + 至少一个启用的 effective 协议；全关 scope 不产生路由 |

## 错误事实、资源恢复与请求预算

- 静态错误配置把受大小限制的上游证据解码为 `FailureFacts` 供传输处理，但上游状态、代码或文案都不能建立持久化额度耗尽。`failure.rs` 统一决策，`recovery.rs` 管理临时准入，不代替选择器或用量账本。本地限制策略是单独的强类型求值器（`policy.rs`），范围只有 Credential 与 CredentialModel。`403` 只在本次请求内处理：既不写 `auth_error`，也不把正文文案提升为额度证据。MiniMax 仍校验结构化错误信封，包括 HTTP 200 中的信封；但其代码和正文都不能建立持久化余额或额度耗尽。GOAT 的 `InsufficientCredits`（沿用已有分类，不再根据错误正文重复识别）会在当前授权绑定以及实际发送的 endpoint/route/protocol/上游模型上建立进程内 `temporary_unavailable` 等待。当前请求仍可按既有首次回退改试下一把合格 Key；同一把 Key 在本次请求中不再重试。它不写额度窗口、`auth_error` 或启用状态。其他或未知 400 仍只在本次请求内失败：本策略不为它们增加首次回退。
- 操作者的临时停调规则在设置页编辑（全局或某一个目的地）。目的地上相同 id 的规则整条替换全局规则；本地禁用行挡住继承；删除本地行即恢复继承。内置 `builtin.goat.credits_rejection` 可覆盖启用/退避，但匹配器与范围保持密封。自定义匹配在可选的 statusCodes/errorCodes/errorTypes/messageContains 之间为 AND，同一字段内为 OR，只使用提取出的顶层 `error.code` / `error.type` / `error.message`。无正则、脚本或原文扫描。只填状态码的规则可以在没有解析出错误体时命中；读正文失败不会变成合成匹配文本。范围只有 Credential 与 CredentialModel；CredentialModel 等待绑定实际上游模型。上限：128 条规则、任一目的地最多 32 条启用、每字段最多 32 个取值、每条匹配字符串最多 256 个 UTF-8 字节、退避 1–86400 且最大值 ≥ 初始值（默认 30/300）。已保存规则落在 `settings.temporary_unavailability_v1`；进程内等待不持久化。诊断 GET 与清除都不发探测。完整数据目录备份保留该文档；可移植账号导出/导入不携带这些规则，也不清掉无关的本地规则。删除目的地时相关引用一并移除或停用，绝不按显示名重新绑定。用户指南：[临时停调](../user/temporary-unavailability.zh-CN.md)。
- 每个 `429` 都为收到它的精确 Key 启动临时冷却（没有可用约束时为 30 秒）。有效的 `Retry-After` 延迟或支持的 HTTP 日期可以延长等待，绝不缩短已有的更晚截止时间。端点约束保留精确上游 URL、模型、协议、路由和显式代理身份。该临时等待不通过额度池扇出。Zen Free 保留既有的进程级匿名出口范围，不使用 Key 范围。独立的 Retry-After / 官方限制不能被本地策略规则删除或清除。OpenRouter Free、Zen Free 和普通 429 仍走既有槽位，不迁到策略范围。
- 临时冷却或本地策略等待到期后由真实客户端请求进入，不会新建合成推理探测或新的周期轮询器。健康资源仍可并发。全局 sticky 保留原目标；等待中的 Key 通过请求内 exclude 跳过。等待映射最多 4096 个资源位置；容量不足本地拒绝，不丢弃活跃证据，也不伪造上游额度。`resource_wait` 与 `local_policy_skip` 日志没有上游 HTTP 状态、费用或身份 digest。既有持久化额度回合不会被批量清除，但新的上游错误正文不会创建它们。全部兼容 Key 只因本地策略等待时返回 `503` 并说明 `local_policy`；既有 429 / Retry-After 的 all-waiting 语义不变。all-waiting 使用与真实发送相同的身份，不用空 endpoint 伪造模型查询。只有完整且协议有效的成功 JSON 或 SSE（包括没有 usage 的成功）会清除本地策略等待。
- 完成请求的路径会在所选 adapter 支持时排入非阻塞、合并且节流的官网用量刷新。失败或不支持的刷新保留既有观测（或未知），绝不写推理冷却、额度恢复或 `auth_error`。只有获取到的 OpenCode Go 权威用量可以建立或清除 Go 额度状态；除非 adapter 明确另有声明，GOAT 与 CN 用量快照只用于展示。既有 CAS 账号解除冷却会清除关联临时资源，但不重置匿名 Free。重启清空临时等待（包括本地策略的 waiting/ready/probing），保留持久化的额度历史回合。已保存的临时停调规则会重新加载。运行时策略快照按来源代际替换（不是内容哈希）；删除后再写入相同内容的规则仍隔离。按 source 的手动清除是本地 fence，不是宣称健康，无网络。
- 一个逻辑请求的不同账号回退及同账号重试共用 32 个**真实发送**位置与一个截止时间。本地跳过（等待或容量）不消耗这些发送位置；候选扫描与请求截止时间仍然有界。非流式使用 `non_stream_timeout_secs`，流式首输出前使用 `stream_idle_timeout_secs`。截止时间下沉到发送、正文读取和首输出阶段，确保超时日志保留实际账号和已知 HTTP 状态。碎片 SSE 帧不能无限刷新首输出期限；交付下游后仍按空闲超时处理，不受非流式总时限截断。不增加 408/5xx 或发送/正文结果未知时的重放；保留既有中断/不完整流首输出前至多一次同账号重试，且受剩余预算约束，输出后不重放或跨账号拼接。
- `gateway_resource_recovery.rs`、`gateway_request_budget.rs`、`custom_trusted_admin.rs`、`v3_runtime_invariants.rs` 通过公开入口验证真实调用序列、逐尝试日志、长流兼容、重置/轮换、共享池隔离及截止时间。时钟读取次数不等于选择次数，恢复准入和观察也会采样。两次逻辑请求的轮询回归证明安全同账号重试不会推进选择器。

## Zen Free

- Zen Free 是特殊的内置账号，没有 Key；只有账号卡片启用开关。管理员在 Providers 页点击“获取模型” (Fetch Models) 时，请求固定官方目录，仅保留以 `-free` 结尾的规范化有效 ID并持久化上次成功快照。每个保存 ID 都保留精确 raw pin，去掉 `-free` 后公布对应 Alias。刷新失败或空结果不会覆盖旧快照。不需要 Free 时关闭卡片；启用时按卡片顺序与其他账号一起被选择。协议探测控件也在 Providers 页，而不是账号卡片。Zen Free 与 Go 使用独立的 `cooldown_free_until`；Zen Free 配额按出口 IP 共享，429 限制匿名 Free 通道而不切换 Key。已知重置证据持久化为 Free 冷却，恢复未知则进入本地单飞等待。路由继续尝试后续兼容卡片；仅有 Free 本地等待且无已知重置时返回 503，不虚构额度截止时间。Zen Free 的推理 `401` 原样返回。OpenCode Go 仅在已限长 JSON 错误体精确满足 `/error/type == "CreditsError"` 时换号并写入 `auth_error`；`ModelError`、未知、畸形、截断或读取失败的 401 仍原样返回。重新保存同一个 Key 会清除该断路状态。面板 Ping / Key 验证的 401 仍记录 `auth_error`。Free 通道成功行记录 `cost_state=free`，不计入 Go 配额。

## OpenRouter Free

- OpenRouter Free 仍是带 Bearer Key 的 Configurable HTTP 预设，使用文档化的 Chat Completions 端点。其种子 `openrouter/free` 是路由器，不是具名底层模型；可手动添加精确的 `:free` 目录行。Free 预设禁用通用 `/models` 刷新，因为该目录同时包含付费 ID，而刷新会启用新行。运行时试错处理基于实际已授权的 `https://openrouter.ai/api/v1/{chat/completions,responses,messages}` 端点与实际的上游 ID（`openrouter/free` 或 `:free`），包括在普通 OpenRouter 连接下配置的 Free ID。非 429 的确认失败使用进程内按模型与凭据的限制，并尝试另一条兼容路由。`429` 保留普通的接收 Key 临时等待与 `Retry-After`。两者都不写持久化额度或 `auth_error`、不冷却付费模型 ID，也不触碰 Zen 的共享出口通道。精确的 raw Free ID 绝不静默变为付费模型；不确定的发送与输出之后的流不会重放。

## 托管账号（Beta）

- `setup_step` 顺序为 `google_account`（UI：登录身份，可跳过）→ `opencode_registration` → `payment` → `key_verification` → `ready`。`PATCH /dashboard/api/v4/accounts/{id}/setup` 允许前进一步或回退到更早步骤；禁止跳过步骤或直达 `ready`。草稿创建可编辑邀请链接并写回 `opencode_invite_url`（`DEFAULT_OPENCODE_INVITE_URL` 是演示默认值）。面板在 OpenCode Go 供应商的 **设置** 页签编辑该字段。浏览器目标包括 Google/GitHub 注册与登录、邀请 URL，以及控制台 `https://opencode.ai/auth`。托管页可通过 dashboard HTTP 打开浏览器；桌面原生浏览器是 Host hook。

## Ollama Cloud

- 家族密封且固定源：`https://ollama.com`，仅 Chat Completions，Bearer，不跟随重定向，`ProcessWideNoRedirect` 代理语义（方向默认段；本家族模型 id 不参与按模型例外段匹配）。协议事实存于 `ocg_domain::protocol` 的家族独立种子（`OLLAMA_CLOUD_*`）；`MODEL_PROTOCOLS` 必须保持无 Ollama 专属 id——该表派生 Go 已发布别名。adapter 标记 `WireNormalization::OllamaCloud`；forwarder 按尝试改写请求字节（assistant `reasoning_content` 缺失时补 `reasoning`，`max_tokens`/`max_completion_tokens` 钳制到 65535），并对响应/SSE 帧做规范化（`reasoning`/`thinking` → 补 `reasoning_content`）。混合候选链中非 Ollama 尝试的请求字节必须逐字节不变；`upstream_body_bytes` 记录实际发送字节；面向客户端的归因保留请求名。
- 价格仅手动刷新 `https://ollama.com/pricing`（批准主机 HTTPS 重定向、内容哈希、CAS、保留上次成功快照）。官方表是 Model / Input / Cached input / Output；行的配额倍率固定 `1.0`，因此 `raw_cost_usd == quota_debit`，`effective_paid_cost_usd` 保持空。运行时先精确匹配；Ollama 仅允许去掉一个无歧义的末端 `:` 标签（`glm-5.3-flash:cloud` → `glm-5.3-flash`，以及日期后缀）。仅当上游报告 cached tokens 时才用缓存价；cache creation 用普通 input 价。缺失 usage 仍记 `success_no_usage`。账号计费在 `ollama_cloud_billing`（schema v37，随账号级联删除）：仅 Pro `$60`、Max `$300`、Team `$1000` 每月 USD Credits。没有行即未配置（账号字段为 `null`），不是 Free 档。新建必须明确选择付费档并填写 `purchase_date`；更新省略该字段则保持原状态。面板发布一个月 `usd_credits` 窗口，区间为 `[purchase_date, 次月同日)`（`forward_logs` + 手工校准）；已用额度不按软上限钳制（UI 进度条钳在 100% 并显示超出）。进度条满了不会改变路由。上游 429 遵循临时冷却策略。实际更换档位会清零 `usage_month_window_cost_offset`，同一档位则保留；更改购买日期仍会清零。日志保留。节点导出/导入携带计费档位。不要调用未文档化的 `GET /api/usage`。
- 别名追加守卫：目录刷新仅在剥除 `:` 标签后恰好命中一个目录 id 时，向 Go 拥有的别名（如共享词干）追加一个可路由 Ollama 映射；同词干多快照并存时该映射退出，别名仍由既有家族服务，管理员矩阵钉定会被后续刷新尊重。带日期标签的快照 id 是运行时目录数据，严禁写进代码。

## 用量同步

- 只有 OpenCode Go 的既有调度器仍是周期性用量调度器。官方 `https://opencode.ai/zen/go/v1/usage`（`go_usage.rs`）是其权威观测，本地 `forward_logs` 在上次成功校准后仍是估算。`usage_sync.rs` 继续以全局并发 1、jitter 和可注入 seams 协调手动与已存在的调度路径；ready+enabled 账号保持原有节奏，disabled/not-ready/空 Key 账号不进入调度。手动 `POST /dashboard/api/v4/accounts/{id}/usage/refresh` 仍受节流并发去重。真实推理 429 在 adapter 支持时额外排入同一条异步、合并、节流的刷新；请求绝不等待该 I/O，也不增加全局轮询循环。失败、限流或不支持的刷新保留最后成功依据或未知。只有获取到的 Go 观测可以建立或清除 Go 额度状态。同步元数据仍在 `provider_usage_sync_state`，共享实现保留 CAS、三窗校准与全局代理。官方 Go 文档未列出该端点。
- Command Code GOAT 保留经由 `command_code_usage.rs` / `dashboard_v3/command_code_usage_refresh.rs` 的显式 V4 刷新路径。它只把选中账号解密后的 Key 经全局代理发送到固定、不跟随重定向的端点，限制响应、校验返回上限，并在 I/O 后复查 CAS 与账号身份，再在既有节流下提交展示基线。其观测只用于展示，不能建立或清除持久化额度状态。失败保留最后成功数据或未知，绝不写推理冷却或 `auth_error`。

- 账号页还通过现有 CAS 保护接口懒刷新官方用量和余额：仅处理已启用且就绪的账号，串行请求，观测和尝试间隔为五分钟，并遵守服务端限流。页面不活跃、隐藏或未登录时不启动后续刷新。覆盖已关联的平台 Key，保留上次有效数据，不额外联动模型目录或供应商价格查询。这属于页面生命周期行为，不新增服务端调度器。

已关联的平台 Key 复用现有快照读取器，快照本身可能同时读取平台的模型与价格，但不会另行触发模型发现操作。打开账号编辑或卡片排序时，自动刷新暂缓执行。

## 按 Key 额度恢复

- 额度状态以凭据 id 与 Key `version` 为键。新耗尽只由获取到的权威 Go 依据建立；健康官方用量、完整成功的恢复试探或替换 Key 都可清除旧状态。进度条满格、403 以及任何上游错误正文都不足为据。既有保存回合不会在本次变更中批量清除。
- 临时 429 冷却不通过已声明额度池扇出。已有普通冷却仍有效，Zen Free 保留独立匿名出口通道冷却。
- 临时等待采用有效的 `Retry-After`，不可用时默认 30 秒，并保留已有的更晚截止时间，之后恢复资格。到期请求是正常客户端请求，绝不由后台探测。包括 GOAT 余额不足文案、MiniMax `1008`/`2056`、Kimi 403 文案和 Configurable HTTP 的 `insufficient_*` 字段在内，任何错误正文都不会新增持久化恢复回合。
- 恢复是本机状态，不导出。导入未改动目标 Key 时保留既有本机状态；替换 Key（包括轮换）会清除绑定该 Key 版本的状态。V4 列表可以继续投影保留的历史状态；缺省不表示已经验证上游健康。

- 到期后，普通选择器本来就会发往该 Key 的下一次请求就是唯一试探（`status=probing`）。不另发探测、不跑调度器，也不把其他流量挂起等待验证。手动 `POST /dashboard/api/v4/credentials/{id}/quota-retry` 只设置资格（`ready`）：无出站请求、不改启用、不清除退避。已是 `ready` 或 `probing` 时幂等，可返回当前行。独立的上游 Retry-After 仍会执行，直到到期或显式账号解除冷却清除该本地限制。
- 完整且协议有效的成功 JSON 或 SSE 也会恢复该 Key，包括没有 usage 的成功。超时、取消或 5xx 保留耗尽、释放试探租约，至少 15 分钟后可再试，且不升高未知窗口阶梯。旧 Key 版本或上一回合的成功不能清掉当前状态。

## 定价、容器与 CI 说明

- 定价按 Provider 独立管理：读取 `GET /dashboard/api/v4/providers/{provider_id}/pricing`，刷新 `POST /dashboard/api/v4/providers/{provider_id}/pricing/refresh`，应用倍率通过 `PUT /dashboard/api/v4/providers/{provider_id}/pricing/multipliers` 写入。Go 与 GOAT 的倍率写入各自使用本 Provider 的 pricing revision 做 CAS；GOAT 写入追加 Provider 快照，只影响后续请求估算。刷新若会覆盖本地倍率必须先展示差异。OpenCode 与 Command Code 各自维护修订号和最后一次成功快照；只有用户点击对应 Provider 的刷新按钮时才访问其固定官方来源，禁止自动轮询。
- 公开 GitHub Release 发布后，`.github/workflows/container.yml` 在原生 amd64（`ubuntu-24.04`）与 arm64（`ubuntu-24.04-arm`）runner 上构建 `linux/amd64` 与 `linux/arm64` 镜像（仅 amd64 跑冒烟测试），按 digest 推送各架构，再合并为同一标签下的多架构 OCI index，发布到 `ghcr.io/klarkxy/opencode-go-mgr`。Compose 默认使用该镜像；本地源码构建需 `OCG_IMAGE=ocg-manager:local` 后 `docker compose up -d --build`。
- `.github/workflows/quality.yml` 在 PR / `main` 上分为三个并行 job：Web（含 `pnpm run contract:v3:check`、`pnpm run contract:v4:check`、前端测试/类型/lint）、Linux workspace Rust 测试/Clippy（排除 Tauri 桌面 crate，无需安装系统包）与 Windows Tauri 目标测试（stub `dist/`，不运行 Vite）。`release.yml` 仅在生产 `v*` tag push 时调用该质量门。`release.yml` 手动候选（即使选择 tag ref）始终未签名，且可能只构建所选平台；只有 `v*` tag push 事件才会构建全部三个平台并读取仓库签名密钥。生产 tag push 会触发发布流水线：工作流逐个校验附件集合与组装产物名称匹配（数量由产物推导，非硬编码）、升级器签名、公钥连续性，以及 GitHub 服务端摘要，然后自动发布同一未改动草稿。
- 容器以固定 UID/GID `10001` 运行，包含 `LICENSE`；Compose 透传可选 `OCG_MANAGER_ENCRYPTION_KEY` 以支持显式 Key 恢复，但正常部署倾向于在卷中保留 `.encryption-key`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](runtime-invariants.md) · [文档索引](../README.zh-CN.md)

## 计费模型与用量来源

- 账务能力按精确 HTTPS authority（包括 443 端口）与完整保存请求路径分类，绝不以主机前缀判断。StepFun 国内推理仍使用 Configurable HTTP：普通 `https://api.stepfun.com/v1/accounts` API 路径可读取所选凭据的可用余额；即使主机相同，`/step_plan` 路径仍排除。读取使用该精确凭据已保存的 endpoint grant，不推算消费，也不累加现金与赠送金额字段。预设数值从资源加载，保持既有数值不变。
- MiniMax 上报的缓存计数在 JSON、SSE 和尝试记账中保持原始值，不得通过模型前缀改写重新解释。
- 周期额度、现金和积分是三种展示模型；官方观测、本地估算和暂不可用是独立的数据来源。Go、供应商价格快照、官方 API 参考价、平台价格和个人积分共用 `ocg_domain::billing` 的 Token 计算。供应商的价格选择、倍率、缓存缺省规则和观测适配器保留各自语义。原币金额、额度扣减和积分不得混入同一个美元合计。
- 一条 Key 就是一个账号。供应商实例包含相互独立的账号，同一厂商可以重复出现在 A1、B1、A2 这样的路由顺序中。个人积分状态属于凭据，不属于供应商容器或共享额度池。仅 custom_account 和 dynamic HTTP 目的地支持个人积分配置。New API / Sub2API 关联 Key 保留现有站点计费视图。
- Step Plan 使用本地估算与手动余额校准。请求前冻结准确上游价格和兑换比例，完成后按当前各笔额度的到期顺序扣减；月度发放与加油包到期分别计算。JSON 和流式完成的结算与日志记录在同一事务内幂等提交。请求发出前先保存待结算记录，所有在途请求完成后才允许校准；取消或所有数据库句柄关闭后的冷启动会将未完成请求转为明确的不确定用量，并发 CLI 打开不会结束仍活跃的结算记录。未知价格或用量不是零。个人估算耗尽不会改变推理资格、授权、冷却或额度恢复。

## 官网 API 账务依据

- 保存的 `deepseek` / `zhipu` API 预设只有在来源、官网目的地和鉴权同时匹配时，才公开 `model_source=official_api_preset` 与账务参考价格；推理仍走 Configurable HTTP。逐次尝试再次检查模型级路由覆盖。普通动态供应商、Custom、Coding Plan 与改过的非官网目的地不继承此能力。
- 受认证保护的新增 V4 GET `/accounts/{id}/official-api`、`/providers/{id}/official-api/pricing` 只投影本地数据。带 CAS 的 POST `/accounts/{id}/official-api/balance`、`/providers/{id}/official-api/pricing` 是唯一网络入口。余额要求所选就绪 Key 的已存端点和 Origin 授权，固定官网、有界响应、不跟随重定向、15 秒节流，并在网络后复查身份与 CAS。公共价格请求不携带 Key。失败保留已有依据，不写推理状态。
- 复用既有价格快照和余额表，不迁移数据库。余额绑定密文凭据与保存的目的地；删除动态供应商时清理其价格参考快照。GET 不读在线网页。价格有效期 30 天，每次尝试固定版本、模型和时间。CNY 不转写成 USD，不混币种求和，不从余额中扣除估算，不回算历史。按 UTC 月和全时段汇总本地成功调用，并单独记录本月未知费用数量。
- 来源解析校验身份、单位、重复行、DeepSeek 完整峰谷对和智谱可支持的非分档 Token 单价。不支持的模型、过期或损坏价格、缺失用量和额外收费请求保持未知。智谱余额明确为不可用，不是零。来源与初始日期见中英文预设使用指南。
