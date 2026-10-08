[English](byok-applications.md)

# 本机 BYOK 应用

应用页包含 Codex、Kimi Code、MiniMax Code、ZCode 和 VS Code Copilot 五个固定的原生适配。这是独立自定义供应商配置，与旧版 [Codex 方案](codex-integration-proposal.zh-CN.md) 中的原生登录代理不同。DSH 保留插件接入流程。

已鉴权的 V4 接口是 `GET|POST|DELETE /dashboard/api/v4/applications/byok/{client}` 与 `POST .../{client}/recover`。五个原生客户端都支持只读新鲜预览：`POST /dashboard/api/v4/applications/byok/{client}/preview`，可选 `targetPath` 和 `copilotTokenBudget`。预览返回当前 revision、process generation、检查得到的文件指纹，以及可选的 `ByokApplication.preview` 计划，其中包含 `planFingerprint`、新增/移除/更新的模型 ID、之前和新的默认模型 ID，以及 `requiresTakeover`、`requiresOverwrite`、`removedModelsWithCustomizations`。预览不创建 Key、不写文件、不创建 receipt、不创建原生配置目录，也不修改上游目录。配置提交时附带预览指纹，并在需要时明确提交 `acknowledgeTakeover`、`acknowledgeOverwrite`、`acknowledgeRemoval`；提交会重新校验所有预检值和计划指纹。旧 V4 调用方可在无冲突的普通写入中省略预览指纹，但仍需通过 revision、process generation 和文件字节指纹检查；接管、覆盖和删除自定义内容始终要求匹配预览。应用页始终发送已审阅的计划指纹。

写入需要当前 revision、process generation 和检查得到的文件指纹。配置接口不再接受 Key、模型选择、元数据覆盖或默认模型选择。Copilot 另外接受明确的输入、输出客户端配置预算，仅用于导出副本，不修改发布元数据。控制层持有设置锁，从带鉴权 `/v1/models` 使用的同一发布器导出全部精确公开名称，配置和更新拒绝空目录，然后创建或复用名称为 `codex`、`kimi-code`、`minimax-code`、`zcode` 或 `copilot` 的已启用普通 Key。DSH 默认使用 `dsh`；可选 `keyId` 保留旧 API 调用兼容性。预检在创建 Key 前完成；原生文件 I/O 在创建 Key 后失败时，保留一个启用的同名 Key 供重试，响应准确报告部分完成，不自动删除 Key。GET 不创建 Key，也不构建模型选择目录。移除和恢复不依赖原 Key 或模型仍然存在，空目录也可以移除已有应用配置。原生 CLI 与 Tauri 注册共享 Host；不含 `dsh-local-host` 的构建返回 `unsupported_runtime`。

不设模型数量上限，不按工具能力过滤，不要求补齐元数据。未知的原生可选上限直接省略，不编造数值。Copilot 必需的 token 字段使用明确的客户端预算，并受已知上限约束，详见下文。原生模型目录在配置和刷新时都包含完整发布结果，不提供模型选择器；DSH 继续动态发现模型。新打开页面和每个操作响应都会刷新状态，避免页面保留过期的所有权或恢复状态。

## 格式基线

前四项源码对照日期为 2026-09-28；Copilot 于 2026-10-07 对照 VS Code 1.141 Stable。以下提交描述适配器所采用的格式，不代表所有已安装桌面版本都已验证可以加载。

| 客户端 | 源码基线 | OCG 管理的配置 |
| --- | --- | --- |
| Codex | [0.153.4 schema](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json) | `model_providers.ocg`、固定 Responses 传输和私有 Codex `ModelsResponse` 目录 |
| Kimi Code | [配置服务](https://github.com/MoonshotAI/kimi-code/blob/4fbe065442179435c43d3c3dc8d11bb408b3fd30/packages/agent-core-v2/src/app/config/configService.ts) | 分组 TOML 供应商 `ocg-chat`、`ocg-responses`、`ocg-messages`；公开模型 id 仍为 `ocg/<公开名称>` |
| MiniMax Code | [本地供应商写入](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/local-model-provider-write.ts) | 分组 YAML `custom_provider.ocg-chat`、`ocg-responses`、`ocg-messages`，以及兼容文件锁 |
| ZCode | [文件 codec](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/provider-node/src/provider-config-file-codec.ts) | ZCode 文件 `schemaVersion` 1、分组供应商规则 `ocg-chat`、`ocg-responses`、`ocg-messages`、稀疏模型规则、兼容的所有者标记锁 |
| VS Code Copilot | [1.141 Custom Endpoint 供应商](https://github.com/microsoft/vscode/blob/1.141.0/extensions/copilot/src/extension/byok/vscode-node/customEndpointProvider.ts) | JSONC `chatLanguageModels.json`，一个 `Open Console Gateway` 供应商，`vendor: "customendpoint"`，各模型使用显式端点 |

Codex 的模型目录属于全局选择，配置时启用 OCG 目录。Host 在既有文件锁与指纹校验范围内，保留仍在发布列表中的 OCG 默认模型，否则选择导出的第一个模型。不要仅因旧 schema 包含字段就使用嵌套 `profiles` 或根 `profile`：当前[配置说明](https://learn.chatgpt.com/docs/config-file/config-advanced)采用独立 Profile 文件，当前 App Server 也拒绝这些旧字段。

0.153.4 的 `ModelsResponse` 反序列化要求每个条目提供 `base_instructions` 或 `model_messages.instructions_template`；仅满足 `ModelInfo` 字段结构并不足够。使用固定保存于 `resources/codex-byok/` 的未修改官方通用兜底指令，来源为 [`codex-rs/models-manager/prompt.md`](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/models-manager/prompt.md)。原生 CLI 与桌面包均需保留 Apache 许可证和来源说明。升级时应验证完整目录加载器，包括这一外层反序列化要求。

Kimi Code、MiniMax Code 和 ZCode 按已发布的首选协议把模型分到 `ocg-chat`、`ocg-responses` 和 `ocg-messages`。每个实际使用的协议有一条供应商记录，并使用同一把 Gateway Key。Messages 使用去掉末尾 `/v1` 之后的网关根地址，部署子路径保留。Chat 与 Responses 保留 `/v1` 基址。客户端能够使用该协议时，写入的就是已发布首选。Responses 不是回退协议；配置没有列出 Chat 时也不会补上 Chat。旧的 `ocg` 供应商对象记在所有权 receipt 中，不再使用时移除。与非 OCG 所有的 `ocg` 名称冲突仍然拒绝。Kimi 的公开模型 id 仍是 `ocg/<公开名称>`，每条模型的 provider 字段指向分组后的供应商。Kimi 的供应商类型是 `openai`、`openai_responses` 和 `anthropic`。ZCode 的 api 类型是 `openai-chat-completions`、`openai-responses` 和 `anthropic-messages`。配置选择 Responses 时，适配器会写入 Responses 分组。已安装的 ZCode 3.14.3 在公开 ASAR `out/host/index.js` 中列出 `openai-chat-completions`、`openai-responses` 和 `anthropic-messages`，请求解析把 `openai-responses` 经 `openai` 对应到 `/responses`。这覆盖已安装的请求实现，不覆盖完整桌面界面运行，也不覆盖原生配置解析运行。

Chat Completions 或 Responses 的客户端菜单可以原样携带已发布的 `reasoningEfforts` 拼写。这些拼写保持精确的分类协议值，不会变成 Messages 的思考预算或自适应强度。某个 Responses 供应商是否接受历史 Chat 拼写，仍以该后端合约为准。原生 Messages 推理支持可以为 true。OCG 省去 Messages 配置菜单，也不会把 `reasoning: true` 变成预算。Kimi、MiniMax 及同类 SDK 可以套用自己的预设或默认控件。省去的菜单并不描述该控件，OCG 也不承诺供应商接受它。同一格式上，原生控件原样通过。转换无法保留的跨格式控件会在发出 HTTP 之前拒绝，原生控制参数是否受理由上游后端决定。ZCode 只在 Chat 分组写入 `reasoningLevel`。DSH 按模型选择 API；这三个客户端按协议选择一个供应商分组。Codex 仍使用 `model_providers.ocg` 和它的 Responses 传输。这个客户端选择不改变 Gateway 按次尝试的上游协议选择。

这些客户端负责会话的序列化。供应商、API 或模型身份变化时，它们的 SDK 可以把原生不透明字段改成纯文本，也可以丢掉该字段。OCG 无法恢复一个从未到达的字段，也无法发现这次丢弃。切换协议分组或改写已保存的配置不会迁移旧会话。身份变化时，需要新开会话，或者发送已经解析好的历史。带标记的历史确实到达时，同一条已配置路由就是这项保证的边界：上游模型、端点或凭据版本的直接变化会在发出 HTTP 之前拒绝。DSH 插件是在基础适配器之前做严格预检的客户端。

MiniMax 的默认选择采用 `custom_provider:<分组>/<公开名称>`，`<分组>` 是路由到的供应商（`ocg-chat`、`ocg-responses` 或 `ocg-messages`）。匹配时先取最长的受管理 id，因此 `ocg-chat` 不会被读成旧的 `ocg`。公开名称内部的斜线必须保留：[模型名称解析器](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/local-runtime-v2/src/service/model-system/resolution/model-key.ts)只按第一条斜线拆分，[供应商前缀](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/model-availability.ts)为 `custom_provider:`。

## VS Code Copilot 合约

在既有 V4 `byok/{client}` 路由上使用客户端 ID `copilot`，普通 Key 名称也是 `copilot`。配置、更新、移除和恢复沿用共享 Host、CAS、已检查文件指纹、所有权记录、私有备份和操作日志边界。不增加扩展安装，不写入 `settings.json`。

目标为 JSONC 顶层数组，只管理一个名为 `Open Console Gateway` 的供应商对象，其 `vendor` 为 `customendpoint`，包含 `models: [...]`。保留其他供应商和注释。唯一的现有 OCG 供应商可经预览明确接管；受管值被修改时需要明确确认替换。重复名称、畸形条目，以及 OCG 供应商或模型中大小写不规范的 Authorization 认证头仍然拒绝。不选择默认模型。移除和恢复保持相同的冲突与后续编辑检查，不依赖原 Key 或目录仍然存在。

导出全部可路由公开模型快照。按保存的已发布首选协议，为每个模型设置 `apiType` 和完整 `/v1/chat/completions`、`/v1/responses` 或 `/v1/messages` 地址，保留部署子路径。供应商层省略 `url`：1.141 的动态发现路径可能跳过其目录不知道的 ID，显式 `models` 路径则接受已配置条目。省略 `apiKey`，在每个模型写入字面值 `requestHeaders.Authorization: "Bearer <OCG Key>"`。这是一次外部文件操作，不填充 VS Code secret storage。Key 会以明文写入私有客户端配置和私有备份，不进入 Dashboard 响应或诊断。原生界面的 `${input:...}` 凭据引用与 `${apiKey}` 鉴权头替换是可选的手动方案，不得声称 OCG 写入了这些凭据。

VS Code 要求可用的输入、输出 token 字段。确认草稿初始为输入 100,000、输出 8,192，可编辑、持久化，并在后续预览中再次显示。它们是导出的客户端配置预算，不是发现到的供应商上限。普通刷新会保留兼容的逐模型自定义预算；只有用户实际编辑时才应用新的全局预算。已知输入、输出上限约束对应导出值；两者之和超过已知上下文窗口时，两者按比例缩小。未知上限使用用户明确的预算。只对导出副本应用这些值，不保存元数据，不宣称新增上游能力。这些值用于 VS Code 的上下文管理和输出预留；实际请求参数与上游限制取决于客户端和模型。尤其不能把配置中的 `maxOutputTokens` 当成所有 HTTP 请求的硬性输出上限。

必需的 `toolCalling`、`vision` 布尔值在未知时保守导出为 false。全部模型保留在 Chat 导出中；Agent 选择器要求明确支持工具调用。已验证的声明来自现有模型元数据页面。Chat Completions 和 Responses 使用经验证的推理协议值，去重但不改拼写，写入 `supportsReasoningEffort` 和匹配的 `reasoningEffortFormat`。不制造 Messages 推理菜单或思考预算。

优先解析便携数据（`VSCODE_PORTABLE`），其次是 `VSCODE_APPDATA` 与产品目录名，最后是平台应用数据根目录：Windows `%APPDATA%`、macOS `~/Library/Application Support`，或 Linux `$XDG_CONFIG_HOME`（回退为 `~/.config`）。默认是 `Code/User/chatLanguageModels.json`；Stable 目录不存在而 `Code - Insiders/User` 已存在时，使用后者。命名 Profile、便携安装与 `--user-data-dir` 可使用现有显式路径覆盖。VS Code 不提供上游共享文件锁：配置、更新、移除或恢复前需完整退出，完成后重新打开，在模型选择器中选用该供应商。

高级适配格式固定为 1.141.0（Stable 提交 `2a59476c9bfcb90b3ddc372c36762471b7dfad1c`）。Custom Endpoint 首次发布于 [Stable 1.122](https://code.visualstudio.com/updates/v1_122#_custom-endpoint-provider-in-stable)，这不证明更早版本实现了此处全部字段。已安装 1.142 Insiders 的检查与固定格式基线、真实客户端验收是不同证据。[官方语言模型指南](https://code.visualstudio.com/docs/agent-customization/language-models)涵盖 Chat、Agent、inline chat 与 utility tasks。此集成不提供行内补全或 Next Edit Suggestions。Agent Host BYOK 属于实验功能，需要 `chat.agentHost.byokModels.enabled`；只记录设置，不自动启用。

## 所有权与恢复

新的所有权记录使用版本 2，避免旧 Host 把接管的现有供应商误当成新建条目而删除。版本 1 记录仍可读取，只在成功写入时升级，检查不会改写记录。这是私有记录格式变化，不涉及数据库迁移。旧记录没有生成值来源信息：首次更新保留兼容的客户端预算，删除有自定义内容的模型行需要明确审阅。旧 Codex 目录只有在文件字节或规范化后的缩进序列化与记录哈希匹配时，才能作为原生成值基线；无法匹配的修改需要审阅。旧 Copilot 未记录全局预算时，保留逐模型预算，不反推原全局值。

适配器管理各格式明确的路由、身份和元数据字段，并记录最后写入的值。保留其他内容、未知额外字段和客户端偏好；字节指纹仍用于并发保护，Codex 目录空白在语义上相同。receipt 丢失时，唯一的 OCG 命名空间可以经过审阅后接管，确认时将当前配置保存为私有基线。重复或畸形条目、外部引用仍会阻止操作。真正的受管编辑必须经审阅后明确选择**应用 OCG 更改**或**取消**，不会自动重试 409。已移除的自定义模型行需要确认整行删除。手工 ZCode 规则留在原处；受管模型规则上 OCG 不拥有的字段予以保留。只有默认值仍匹配 OCG 写入的选择时，才进行还原。

检查结果标记为已接管时，移除界面显示**撤销接管**。撤销只恢复 OCG 修改过、且仍匹配最后应用值的受管字段；接管块中后续添加的额外字段和偏好，以及其他无关内容都会保留。竞争性的受管编辑会产生冲突；撤销无法恢复首次 OCG 更改之前未知的状态。

Host 在自身数据目录保存私有的原始备份、所有权记录和操作日志。日志记录部分写入与回滚状态。恢复前先验证所有当前文件状态与备份哈希，支持 Codex 两份文件处于不同写入阶段的情况。文件与管理记录一起恢复，移除后结束本次所有权。含凭据的文件收紧权限，诊断响应不得携带配置片段或 Key。目标、目录、管理记录和备份路径中的符号链接或重解析点不得使操作越过已检查的路径。

Codex、Kimi 和 VS Code 写入前要求用户关闭客户端，其进程内写入服务不提供共享外部锁。MiniMax、ZCode 遵循上游文件锁约定，不接管未知或仍在使用的锁。JSON/YAML 序列化保留无关字段值；MiniMax YAML 注释仅保留在原始备份中。TOML 和 Copilot JSONC 编辑保留重新生成的受管条目之外的注释；重建供应商或模型条目时，条目内的注释可能重新排版或移除，私有备份保留原始字节。

Host 通过 `runtime_log` 共享的控制台 sink 向上进程的 stderr 逐事件输出一行运维信息：写入完成、带原因类别的拒绝、fingerprint 过期、非 OCG 所有的 `ocg` 冲突、所属字段被外部修改、存在未完成写入，以及回滚完成。消息只包含客户端名和文件路径，sink 不会收到 Key、请求体或带凭据的 URL。拒绝事件只记录 `ByokErrorKind` 而非响应原文，未脱敏的报错文本无法进入日志。面向用户的界面仍以 Dashboard 可见的 receipt 为准。

## 验证

运行 `pnpm run contract:v4:check`、`pnpm run build:web`、BYOK 前端领域/状态/组件测试，以及 Applications 行为测试。原生特性下运行 Rust 的 `dashboard_v4::byok_applications`、`byok_application_host` 筛选测试，并执行相关 DSH 回归。先构建原生 CLI，再运行 `node scripts/byok-applications-smoke.mjs`；脚本只使用隔离目录和模拟凭据。另行验证 no-default-features CLI 的能力边界。

格式解析、配置保存、客户端加载和真实推理是不同的证据。升级适配器时，使用目标源码 schema 检查生成文件。真实桌面激活、工具、附件和多轮推理仍需单独验证，不能用保存成功代替这些行为的验证。

## Copilot 扩展边界

Copilot 主标签使用 `copilot_application`、`copilot_application_host` 和不可变的 `copilot_extension_package`；BYOK Copilot 适配器保留为显式旧 JSON 入口。V4 `/applications/copilot-extension` 管理检查/安装/卸载，`/disconnect` 请求扩展自行删除秘密，`/package` 在所有运行时提供同一份无 Key 的 VSIX。原生构建沿用本机 Host 能力注册安装器。认证、CAS、启用普通 Key 的选择、创建 Key 前的目标预检和操作回执仍由控制面负责。

检查只读已有产品、Profile 和扩展注册，不创建目录或调用 CLI。安装使用检测到的可信可执行文件和参数数组，验证 CLI 注册、运行文件摘要和归属回执；交接文件私有且有大小限制，激活使用明确回执。连接位于实际 Profile 的 globalStorage，不写进可执行包或 VS Code 秘密数据库。本机 UI 扩展宿主只读取自己的交接文件，将 Key 写入 SecretStorage，确认导入/删除，并在使用前重新验证 `/v1/models`。未知 Token 元数据引导用户在 OCG 补齐，不生成猜测的全局预算。安装、激活、目录和推理证据分开验证。

运行 `pnpm run build:copilot`、`pnpm run check:copilot` 和 `pnpm run test:copilot`；生成的运行文件与完整许可证声明固定 LF 换行。根 tooling 套件包含扩展测试，Quality 工作流检查可重现打包。`node scripts/copilot-extension-host-smoke.mjs` 用临时用户数据和合成凭据运行已安装的 Windows Insiders 宿主，可用 `OCG_SMOKE_CODE_ROOT` 指定安装目录；不触碰用户 Profile。

Windows 更新互斥锁阻止启动时，可加 `--isolated-runtime` 复制并核验官方可执行文件和应用代码，仅改临时副本的互斥标识。这属于隔离运行时证据，不代表已安装用户 Profile 的激活。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](byok-applications.md) · [文档索引](../README.zh-CN.md)
