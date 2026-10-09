[English](applications.md)

# 应用

本机配置页签为 **Codex**、**Kimi Code**、**MiniMax Code** 和 **ZCode**。**VS Code Copilot** 与 **DSH** 分别使用下文的扩展和插件流程。

## Codex、Kimi Code、MiniMax Code 与 ZCode

这些页签向本机客户端配置添加 **Open Console Gateway** 模型供应商。Codex 指 CLI 或桌面中的本地 Codex 工作流，不会改变普通 ChatGPT Chat 或云端 Work 的推理地址。

1. 在与客户端相同的电脑上，以同一个系统用户运行 OCG 桌面应用或原生 CLI。选择客户端页签，检查显示的确切配置路径。路径属于 OCG 所在的主机，可能不是打开面板的电脑。
2. 配置 Codex、Kimi 前，先关闭其 CLI 和桌面端。这两种客户端不与 OCG 共用文件锁，操作期间不要同时编辑同一文件。
3. 核对目标路径，确认**配置**。OCG 自动创建或复用名称为 `codex`、`kimi-code`、`minimax-code` 或 `zcode` 的已启用普通 **Key**，并导出带鉴权 `/v1/models` 返回的全部模型。该响应已经是可执行公布目录：每一行都有 schemaVersion 2 和已校验的推导协议配置。不再单独选择或按能力筛选。同一套严格解析器仍会拒绝外部畸形的目录输入；这次导出不会放宽解析。Key 会写入客户端本地配置，并收紧文件权限；结果不显示 Key 明文。
4. 启动客户端或新建会话，选择 OCG 模型发送请求，并在 OCG **日志**中确认。配置保存不代表客户端已加载，也不证明工具、附件和多轮对话已经可用。

Gateway 的路由标记只在客户端原样转发不透明原生历史时保护这段历史。Codex、Kimi Code、MiniMax Code 和 ZCode 各自负责会话的序列化。供应商、API 或模型身份变化时，它们的 SDK 可以把原生不透明字段改成纯文本，也可以丢掉该字段。OCG 无法恢复一个从未到达的字段，也无法发现这次丢弃。切换协议分组或改写客户端配置，不会迁移旧会话。原生身份变化时，需要新开会话，或者发送已经解析好的历史。带标记的历史确实到达时，同一条已配置路由就是这项保证的边界：上游模型、端点或凭据版本的直接变化会在发出 HTTP 之前拒绝。DSH 插件会在基础适配器改写已出现的签名信封之前检查该信封。

在**配置**或**更新配置**之前，页面会先执行只读的新鲜预览。**移除**和**恢复**在确认前重新检查状态。预览显示当前 revision、process generation、检查得到的文件指纹，以及计划中的受管模型/供应商变化。预览不会创建 Key、写入客户端文件、创建 receipt、创建原生配置目录，也不会修改上游目录。配置提交时会附带预览指纹，并在需要时明确确认接管、覆盖和移除。提交会重新校验所有预检值和计划指纹；页面过期时必须重新准备并再次审阅，不会自动重试。

**更新配置**会重新导出完整的已发布模型目录，不提供模型选择器。已有受管供应商和模型、未知额外字段以及客户端偏好会按各适配器的所有权规则保留。**移除**只移除或恢复 OCG 管理的部分，保留客户端登录、其他供应商和 OCG Key。即使原 Key 或已发布目录已不存在，仍可移除或恢复；空目录也可以移除已有应用配置，因为预检不会创建 Key。预检后实际文件 I/O 失败可能留下一个启用的同名 Key；结果会准确报告部分完成，不会自动删除该 Key。

OCG 将适配器管理的路由、身份和元数据字段与无关字节区分开；字节指纹仍用于并发保护。Codex 目录中的空白在语义上相同。receipt 丢失时，唯一的 OCG 命名空间可以经过审阅后接管，确认时将当前配置保存为私有基线；重复或畸形条目、外部引用仍会阻止操作。受管值被修改后，必须明确选择经审阅的**应用 OCG 更改**或**取消**。已移除的自定义行需要明确确认整行删除。不会自动重试 409。

检查结果标记为已接管时，移除操作显示为**撤销接管**。撤销只恢复 OCG 修改过、且仍匹配最后应用值的受管字段；接管块中后来添加的额外字段和偏好，以及其他无关内容都会保留。竞争性的受管编辑会产生冲突；撤销无法恢复首次 OCG 更改之前未知的状态。

配置时，当前 OCG 模型仍在公开列表中就保留，否则启用导出的第一个模型；之后在客户端内切换模型。Codex 会启用 OCG 目录作为全局目录，不会把它合并进原生 ChatGPT 目录。未知模型上限保持未指定；模型可见不代表它支持编程客户端所需的全部工具能力。

已声明的推理档位会导出到各客户端的原生模型选择器：Codex 使用 `supported_reasoning_levels`，Kimi 使用 `support_efforts`，明确的关闭参数放在 `off_effort`；MiniMax 使用 `thinking.effortOptions`，ZCode 使用 `reasoningLevel` 选项。导出值保留已声明的请求参数拼写。这些拼写是已发布的 Chat 选择器协议值。客户端有菜单时，Chat Completions 和 Responses 可以原样携带同一组分类档位。它们不会变成 Messages 的思考预算或自适应强度。某个 Responses 供应商是否接受历史 Chat 拼写，仍以该后端合约为准。ZCode 只在 Chat 分组写入 `reasoningLevel`。模型支持推理但没有已知档位时，OCG 不写入猜出来的低、中、高选项，单独的 `reasoning: true` 也不会制造菜单或预算。省去这份配置菜单，并不描述原生客户端自己的控件。Kimi、MiniMax 及同类 SDK 可以套用自己的预设或默认控件。OCG 不承诺供应商接受这些默认值。OCG 仍会导出该模型。同一格式上，原生控件原样通过。无法在另一种格式中保留的控件会在发出 HTTP 之前拒绝，原生控制参数是否受理由上游后端决定。公开档位变化后，需要更新应用配置并重新加载客户端。来源与声明规则见[模型元数据与推理档位](model-metadata.zh-CN.md)。

Kimi 会将普通档位转成小写，并把 `on`、`off` 当作原生开关。OCG 只导出 Kimi 能原样发送的普通选项；明确的关闭映射仍单独放在 `off_effort`。

MiniMax 会将关闭参数 `off` 改写成 `none`。如果这会改变已声明的上游参数，OCG 就不导出字面值 `off` 选项；明确声明的 `none` 仍可选择。导出不会强制设置思考开关模式，也不会编造默认档位。

已停用的 Key 不会重新启用；没有同名已启用 Key 时才新建。若创建 Key 后客户端文件写入失败，该 Key 仍保留在连接中心，重试会复用它。配置和更新时，模型列表为空会在创建 Key 或写入客户端文件前拒绝；移除已有应用配置仍然可用。

生成的 Codex 目录附带 Codex 0.153.4 官方通用编码指令，该版本要求每个自定义模型提供指令来源。OCG 同时附带来源说明与 Apache 2.0 许可证。

OCG 在替换配置前保存私有恢复数据。操作中断后，如果日志可以安全恢复受影响文件，页签会提供恢复入口；存在用户后续修改时会拒绝覆盖。恢复文件可能含凭据，请保持私有，不要把内容贴进问题报告。

MiniMax 的 YAML 配置会在保存时重新排版，保留其他字段值；原有注释可在首次备份中查看。

页签会解析各客户端的 Home、数据目录等覆盖设置，也可选择自定义 Profile 的配置路径。Codex、Kimi 使用 `config.toml`，MiniMax 使用 `config.yaml`，当前 ZCode 使用带版本的 `provider_config.json` Personal Provider 格式。旧格式或损坏文件不会被覆盖。CLI 与桌面端仅在使用相同位置及兼容格式时共享配置。Docker 或不含本机能力的构建无法配置浏览器电脑上的客户端，请使用[手动客户端配置](add-application.zh-CN.md)。

## VS Code Copilot

主流程安装 **Open Console Gateway** VS Code 模型提供者扩展。OCG 创建或复用名为 `copilot` 的启用普通 Key；扩展将其导入所选 Profile 的 **SecretStorage**。VS Code 发现模型及每次发请求前，扩展读取已鉴权 `/v1/models`。无需再维护一套上下文预算或模型快照。

1. 在 VS Code 所在电脑、同一用户下运行 OCG Desktop 或原生 CLI。打开 **应用 > VS Code Copilot**，核对显示的安装、Profile、用户数据目录和扩展目录。**其他安装目标**可选择 Stable/Insiders、已有命名 Profile 或指定目录。命名 Profile 需先在 VS Code 中打开一次；共享默认全局状态的 Profile 应使用默认 Profile 的连接。要求 VS Code 1.141 或更新版本。
2. 点击 **安装并连接**。OCG 通过官方 VS Code CLI 安装随应用附带、不含 Key 的 VSIX，并将私有一次性交接文件写入该 Profile 的扩展存储。打开或重新加载所选 Profile，再刷新应用状态。**等待 VS Code 激活**与**已连接 OCG**是不同阶段。
3. 在 Chat 模型选择器的 **Open Console Gateway** 下选择模型。显示名称与 OCG **别名**表的对外模型名一致，请求也使用该别名；即使上游展示名相同，不同别名仍分别列出。上下文和输出限制直接来自各模型的 OCG 元数据；输入容量为输出上限预留空间。缺少任一限制的模型会显示在应用的元数据提示中。点击**前往模型能力**，在 OCG 补齐一次，再在 VS Code 执行 **OCG: Refresh Models**。未知工具/图像能力保持关闭；Agent 要求已声明工具调用能力。
4. 发出请求，并在 OCG **日志**中确认。安装、Key 导入和目录发现均不等于推理成功。扩展提供 Chat 和 Agent 模型，不提供行内代码补全或 Next Edit Suggestions。

实验性的 VS Code Agent Host/Agents Window 桥接还需要在 VS Code 启用
`chat.agentHost.byokModels.enabled`。OCG 不代为开启；Agent Host 支持与普通
Chat/Agent 模型发现分开验收。详见[VS Code 模型定制](https://code.visualstudio.com/docs/agent-customization/language-models)。

Key 或网关地址变更后，使用**更新或重新连接**刷新扩展和该 Profile 的连接。它不修改默认模型或 `settings.json`。VS Code 共用扩展文件时，可执行包保持不可变，各 Profile 独立保存交接文件与 SecretStorage 连接。扩展运行在本机 UI 扩展宿主；远程工作区也使用本机宿主，不向远程宿主传递本机交接路径。

**断开连接**请求正在运行的扩展清除加密连接。**卸载扩展**必须收到扩展已删除秘密的明确回执，才会让 CLI 移除该 Profile 的扩展注册。如果仍等待激活，请打开所选 Profile，刷新后继续卸载。移除后会检查其他 Profile。OCG 不编辑 VS Code 的秘密数据库，也不删除 OCG 中的普通 Key。

Docker、另一台电脑或不支持的本机目标可用**下载 VSIX 手动安装**。在 VS Code 执行 **Extensions: Install from VSIX** 安装该文件，再运行 **OCG: Connect**，填写一次网关 `/v1` 地址和启用的 Key。**OCG: Disconnect** 清除扩展连接。远程网关要求 HTTPS；内网和回环地址可用 HTTP。

扩展按模型发布的首选 Chat Completions、Responses 或 Messages 协议请求，支持已声明的图像、工具、流式响应和取消。文本 Token 计数使用 o200k 编码，加上消息/工具开销和保守图像估算；其他模型的分词可能不同。计数估算不修改已发布上限。私有推理签名保存在有界的内存重放缓存中，仅用于匹配的会话轮次；重新加载、断开或重新连接会清空它。

可用 **OCG: Set Reasoning Effort** 为各模型选择 OCG 已声明的 Chat Completions 或 Responses 推理强度，不为 Messages 猜测强度菜单或思考预算。

### 旧 JSON 配置与迁移

展开**旧 JSON 配置与迁移**，可继续使用原有的原生 Custom Endpoint 适配器。扩展连接成功后，通过旧配置的受审查**移除**动作删除 OCG JSON 供应商，避免重复显示。文件操作前需关闭 VS Code。归属回执、CAS、指纹检查、备份、撤销和恢复会保留其他供应商，并拒绝覆盖后续编辑。安装扩展不会静默编辑或删除旧供应商。

JSON 兼容流程仍将快照导出到 `chatLanguageModels.json`，保留原有逐模型客户端设置，并提供显式客户端预算。这些预算只属于兼容流程。Key 以字面值认证 Header 保存在私有 JSON 文件和恢复备份中；扩展流程使用 SecretStorage。详见[手动配置](add-application.zh-CN.md#vs-code-copilot)及[官方提供者接口](https://code.visualstudio.com/api/extension-guides/ai/language-model-chat-provider)。

## DSH

**DSH** 子页签通过页面上显示的**运行地址**安装或卸载 OCG 自有插件。

在 DSH 插件列表中，集成显示为 **Open Console Gateway**，使用 OCG 图标，
并随 DSH 语言显示中文或英文说明。旧版插件需通过 OCG 重新安装，按提示重启 DSH 后更新。

页面还会检测当前用户 `~/.dsh/profiles` 和 `~/.dsh-*/profiles` 下一层的 profile。
只列出具有有效 DSH profile 清单的目录，跳过链接目录。如果 OCG Host 显式设置了 `DSH_HOME`，
检测沿用该 Home 的原有位置。默认 `web` 目标在清单尚未建立时仍可选择。
切换 Profile 会选择对应的本机 DSH Home 和会话上下文；确认装卸前请核对运行地址。

发现到的 profile 指向该 Home 的 DSH 会话文件，并给出建议地址（`web` →
`http://127.0.0.1:3080`，官方 Desktop → `http://127.0.0.1:19387`）。地址可改，以便使用自定义端口。
所选 profile 是签发内存 cookie 时使用的本机 Home 与会话上下文。
**真正执行变更的是确认窗口里显示的运行来源**，不是 profile 目录。插件管理调用成功
不能证明磁盘上是哪个目录；DSH 的 `$events.home` 是操作系统用户目录。OCG 不扫描端口。
走运行地址时，OCG 只写入自有的插件包和交接文件，不会改该 profile 的 `package.json`。

1. 在与 DSH 相同的电脑上，以同一个系统用户运行已安装的 Open Console Gateway 桌面应用或原生 `ocg-manager-cli serve`。
2. 打开 **应用 > DSH**，选择目标 Home 和 profile，核对运行地址。
3. 点击**安装**。在确认窗口中核对本页显示的运行地址和本机目标，然后确认。确认时会使用这台电脑上已有的 DSH 本地会话操作该地址；这不是新的授权向导。
4. 页面提示时启动或重启 DSH。

首次安装可以立即加载；替换已加载的包可能需要重启。失败或尚未确认结果的操作不会显示为成功；
请先刷新状态，再决定是否重试。
重新安装和卸载按包名操作所显示地址上的 `@open-console-gateway/dsh-plugin`，包括其他来源的同名包。
确认窗口会说明这个范围；本机 Profile 不能证明运行中包的来源。

普通 DSH Web 与官方 Desktop 使用同一套正在运行的 HTTP plugin-manager 接口。
对这些目标，以及任何已填写的运行地址，OCG 都不会再回退到 DSH 桌面 CLI。
离线或本机会话格式不受支持时，失败原因会直接显示，不会改去调用 CLI。
DSH Editor 托管的 profile 仍使用原有离线 CLI 流程；只有在你填写了运行地址时，才走同一条 HTTP 路径。

Web/Desktop 这条路径是当前对本机已有 DSH browser-session 授权与既有 HTTP 协议的原生兼容，
不是对外承诺的公开外部鉴权 API，也不增加配对文件、身份路由或附属插件步骤。
授权格式不受支持时会明确失败。

安装不按全局 DSH CLI 版本号设限。只有运行地址实际返回版本时，页面才显示版本。
OCG 只通过正在运行的管理器添加自有的 `@open-console-gateway/dsh-plugin`，保留其他包。
它不会整份替换配置，也不会另装一套 DSH 运行时依赖。失败和冲突会显示实际原因。

OCG 会保留当前要安装的插件包缓存。如果缓存里缺了文件，而已存在的每个文件都与预期一致，OCG 会补齐缓存并继续。文件被改过、出现了预期之外的文件或目录，或者缓存目录是经由链接到达的，OCG 都不会改动这份缓存。页面显示 OCG 返回的具体原因；只有设置修订号确实冲突时，才提示 DSH 状态已变化。

安装时自动创建或复用名称为 `dsh` 的已启用普通 Key。桌面 Host 在后端解析其值，
为选中的目标写入一个收紧权限的一次性交接文件，再请确认窗口里显示的运行地址安装已物化的包。
DSH 加载插件后，插件会把该值导入 DSH 自己的凭据服务，并删除交接文件。Key 不会进入生成的插件源码或命令参数。
对于 Editor 托管的 profile，OCG 还会把插件登记到 Editor 的用户插件状态，使其在 profile 重建后保留；
安装前先退出 Editor，安装后重新启动。

插件以 `ocg` 注册 **Open Console Gateway**。首次使用、读取模型列表，或请求
已加载目录中没有的模型时，会刷新带鉴权的 `GET /v1/models`。已知模型复用现有
快照，不按时间自动刷新；详见[模型元数据](model-metadata.zh-CN.md)。选择器使用的是这份可执行公布目录，
包含符合条件的 Custom ID，而不是范围更窄的 Dashboard `application-models` 列表。
内部 `ocg-rejected` 占位不会出现在可选列表里，以便精确 resolve 与 prepare 仍能失败关闭。
混合无效行会被排除；全无效目录是空列表，不会回退成 Chat。插件解析器对畸形行仍然严格。
因此，模型可见性发生变化时无需重新安装插件。模型是否能接收图片附件完全由
[模型元数据](model-metadata.zh-CN.md) 决定：目录发现或人工声明了 `image` 输入模态的模型才标记为可读图，
没有任何按模型 ID 的例外；其余模型在能力得到验证前仍标记为仅文字。

页面显示**已安装**，只证明插件登记和凭据交接已经准备完成；它不等于 DSH 已重启、已加载插件，
也不等于真实模型调用已经成功。重启后请在 DSH 中选择一个 OCG 模型发送请求，并到 OCG **日志**中确认。
安装被阻止时（环境不支持、未检测到 DSH、插件命令失败或存在冲突），页面会在状态旁显示 Host 返回的具体原因。

安装会跨越两份本机存储，因此回滚有一个明确边界：如果 DSH 已导入 Key，而后续安装步骤失败，
OCG 可以恢复插件登记与仍在等待的交接文件，但无法证明 DSH 的凭据写入尚未提交。需要重试时，
重新打开此页面，点击**重新安装**；随后只启动一次 DSH，并在 OCG **日志**中确认请求。
要从正在运行的 Web 或 Desktop 地址移除集成，请使用本页的**卸载**。它只移除
`@open-console-gateway/dsh-plugin`，保留其他包、DSH 凭据和 OCG Key。
Editor 托管且未填写运行地址的 profile，还需通过 Editor 插件管理移除插件，否则其启动恢复会重新安装。
如果你明确要放弃一次尚未激活的交接，只能在 DSH 已停止时，删除页面列出的
`credential-handoff` 与对应 `.claimed-*` 文件。移除插件不会停用 OCG Key；必要时请在 OCG 中轮换或停用该 Key。

原生无头 CLI 可以把插件安装到其所在宿主机的 DSH。官方 Docker 镜像会明确显示不支持本机安装：
容器不能把插件安装到浏览器所在电脑或 Docker 宿主机的 DSH，但仍可通过普通 Gateway 配置服务 DSH。

## DSH 中的模型信息

参见[模型元数据与推理档位](model-metadata.zh-CN.md)，了解目录刷新、按连接声明参数，以及升级已安装的 OCG 插件。

---

[用户指南索引](../USER.zh-CN.md) · [English](applications.md) · [文档索引](../README.zh-CN.md)
