[English](add-provider.md)

# 新增供应商

当你希望 Open Console Gateway 把请求路由到另一个上游服务时，先判断走哪条路径：

| 目标 | 路径 | 是否修改仓库 |
| --- | --- | --- |
| 给本节点增加可跨账号复用的具名供应商 | **供应商** → **添加供应商**（用户定义） | 否 |
| 给一张账号接入 OpenAI 或 Anthropic 兼容端点 | 新增 **Custom API** 账号 | 否 |
| 让所有 Open Console Gateway 用户获得一个具名内置 Provider（产品中的 Provider/Plan 身份） | 新增密封的内置供应商 | 是，需要经过审查的代码与测试 |

**适配器注册表**保持静态密封。用户定义供应商是类型化的持久定义；每一条都绑定代码持有的 Configurable HTTP 适配器。OCG 从不加载用户脚本、插件或二进制。未知 `provider_id` 除非匹配已保存的定义，否则 fail closed。迁移后的 Custom API 是普通可配置 HTTP 连接：地址、鉴权、协议和模型映射在供应商页编辑，可挂多把 Key。

## 从预设创建

**供应商 → 添加供应商** 与 **账号 → 新增账号** 打开同一套账号页选择器。在其中选择 [Plan 或 API 预设](provider-presets.zh-CN.md)，会同时创建供应商和第一把 Key。固定地址预设已填好协议、鉴权、地址和默认对话模型；可选设置提供名称和模型调整。Azure、Bedrock 还需填写客户专属地址和模型／部署信息。切换预设会清除上一渠道的 Key 和映射，再填入新预设的默认模型。完成预设必须填写 Key；**保存草稿** 可以省略。任一按钮的保存都走 `POST /dashboard/api/v4/onboarding/commit`。

## 手动创建用户定义供应商

1. 打开 **供应商** 或 **账号**，点击 **添加供应商** / **新增账号**，并在同一套选择器中选择 **手动配置**。
2. 填写名称、一个 API Endpoint、一个上游协议（Chat Completions、Responses 或 Messages），以及一种鉴权方式（Bearer、`x-api-key` 或无鉴权）。
3. 至少添加一条对外模型名 → 精确上游 ID 映射。**获取模型** 和 **测试模型** 仍在本表单，但需要 Key；需要 Key 的鉴权在填写 Key 之前这两个按钮保持禁用。
4. 保存。需要 Key 的鉴权可填写可选 Key：填写则同一次写入创建首个账号，留空则只保存定义（显示为 **待补充凭据**，之后可用 **添加 Key**）。无鉴权供应商会创建一张不带 Key 的单例账号。写入走 `POST /dashboard/api/v4/onboarding/commit`，不要求探测成功。若响应返回前网络中断，面板会自动重试同一次提交；再次保存未改动的草稿会重放已存结果，而不会创建第二个供应商。

编辑通过 `PATCH /dashboard/api/v4/providers/{id}` 整份替换供应商配置。供应商 id 不可变。从无鉴权改为需要 Key 时必须显式填写替换 Key，并且只写到那张单例账号。已经带 Key 的供应商会拒绝供应商更新里的 Key；请在 **账号** 页轮换 Key。只有先删除全部引用账号后才能删除供应商；不会级联删除。

供应商所有字段留在 **供应商** 页。账号 **Key**、启停、顺序、备注、冷却和测试留在 **账号** 页。用户定义供应商始终未定价：没有官方用量、额度估算或价格行。请求日志仍会归因供应商、账号和模型。

节点备份以 payload V8 导出，携带目的地、凭据、模型解析策略与按模型路由覆盖。导入接受 V4 至 V8。当前 SQLite schema（v58）把可配置 HTTP 连接存在 destinations 与 `destination_models` 上；遗留 Custom 连接保持独立并可挂多把 Key。密封 builtin 仍编译在代码里。

## 立即接入兼容上游

1. 打开 **账号**，选择 **新增账号** → **Custom API**。
2. 填写名称、上游 API Key、一个 API 地址，以及一个上游协议：**Chat Completions**、**Responses** 或 **Messages**。
3. 至少添加一条映射：客户端请求的公开模型名，以及精确上游模型 ID。若上游实现了下文的可选模型目录接口，可用 **获取模型** 以其上游 ID 填充当前草稿。
4. 保存账号。就绪 Key 账号默认启用。**测试连接** 会通过这一张账号发送一次可产生费用的真实请求，属于可选诊断，不会改变开关。
5. 调用 Open Console Gateway 上带鉴权的 `GET /v1/models`，确认可路由公开名称已经公布，再发送一次推理请求。

一张 Custom 账号卡的所有映射共用一个上游协议。同协议客户端请求直接透传，其他受支持客户端格式会转换到所选上游协议。**获取模型** 只返回上游 ID；导入时精确写入“公开模型 = 上游 ID”，不剥离后缀、不生成 Alias。之后可编辑公开名称，同时保留准确上游 ID。

## 上游 HTTP 接口

模型发现、连接验证和正式推理使用相同的常见基址解析规则：

| 配置的 API 地址 | 推理地址 | 可选模型目录地址 |
| --- | --- | --- |
| `https://api.example.com` | 追加 `/v1/chat/completions`、`/v1/responses` 或 `/v1/messages` | `https://api.example.com/v1/models` |
| `https://api.example.com/v1` | 追加 `/chat/completions`、`/responses` 或 `/messages` | `https://api.example.com/v1/models` |
| 完整标准推理地址 | 完全按填写值使用 | 同级 `/models` |
| 非标准完整路径 | 完全按填写值使用 | 不猜测；手动填写模型 ID |

配置地址必须是带主机的 HTTP 或 HTTPS URL。内嵌凭据、query 与 fragment 会被拒绝。受信管理员可以主动选择回环、局域网或公网目的地。元数据、链路本地以及不透明 IPv4 把戏主机会被拒绝。按模型覆盖到另一 Origin 时不会继承供应商 Key。携带秘密的请求不会跟随重定向。

所选协议决定线协议契约：

| 协议 | 标准路径 | 发给上游的鉴权 | 必须实现的行为 |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `/v1/chat/completions` | `Authorization: Bearer <upstream-key>` | 接受 Chat 请求 JSON，返回 Chat JSON 或 Chat SSE |
| OpenAI Responses | `/v1/responses` | `Authorization: Bearer <upstream-key>` | 接受 Responses 请求 JSON，返回 Responses JSON 或 Responses SSE |
| Anthropic Messages | `/v1/messages` | `x-api-key: <upstream-key>`，并带 `anthropic-version: 2023-06-01` | 接受 Messages 请求 JSON，返回 Messages JSON 或 Messages SSE |

OCG 根据协议派生鉴权。它不会同时发送两类鉴权头，不会在 `401` 后换头重试，也不会把面板或客户端 Key 转发给上游。响应必须充分遵守所选协议，能由 OCG 解析并转换；这包括标准错误体，以及流式请求中的 `text/event-stream` 帧。

### 可选模型发现

**获取模型** 会对解析后的模型目录地址发送带鉴权的 `GET`。返回带 `data` 数组的 OpenAI/Anthropic 风格对象：

```json
{
  "data": [
    { "id": "model-a" },
    { "id": "model-b" }
  ],
  "has_more": false
}
```

每个可用条目需要非空字符串 `id`。需要分页时，将 `has_more` 设为 `true`，返回 `last_id`（或确保最后一个可用条目带 ID），并接受下一次请求的 `after_id` query 参数。模型发现只更新未保存表单，不会保存、验证或启用账号。

## 新增内置供应商

只有当供应商需要产品持有的身份、目录、账号生命周期、路由、价格/用量或 Custom API 无法表达的其他语义时，才适合新增内置集成。以当前代码为准。

1. 在 `crates/ocg-domain/src/ids.rs` 与 `provider.rs` 定义一个稳定的 `provider_id`、对应 Provider 行、凭据/额度语义，并穷尽扩展 `ProviderAdapterKind` 映射。Provider 与 Plan 共用 `provider_id`。
2. 只把已经验证的协议事实加入 `crates/ocg-domain/src/protocol.rs`。请求路由使用已保存的合约。
3. 在 `crates/ocg-gateway/src/alias.rs` 添加由代码持有的客户端 Alias 映射。保留准确上游 ID，拒绝有歧义的 raw ID；发现的新目录行不能擅自创造公开 Alias。
4. 在 `ocg-core` 实现宿主路由 resolver。适配器只返回 `AttemptSpec`；数据库访问、Key 解密、代理选择和出站 HTTP 继续由宿主持有。
5. 补齐账号与 **供应商** 控制面/UI 流程；只在该供应商真实支持时加入目录刷新、启停、验证、错误、冷却、价格和用量。所有面板变更都带 CAS，且只走 `/dashboard/api/v4`：用户定义供应商的创建走 onboarding commit；供应商定义编辑、账号操作、Key 轮换、绑定编辑与身份内新增凭据走挂回或原生的 V4 路由。`/dashboard/api/v3` 是 410 墓碑。
6. 更新成对用户文档与测试。按[开发](../maintainer/development.zh-CN.md)对改动的 crate 与 UI 跑对应检查。

提交贡献前，请写清上游来源、鉴权方式、目录来源、支持的模型/协议组合、流式行为、错误语义、额度/价格来源，以及不产生费用的验证方案。在完整路由与控制面路径真正存在前，让新家族保持 fail closed。

仓库架构细节继续阅读[扩展 Open Console Gateway](../maintainer/extending.zh-CN.md)与[运行时不变量](../maintainer/runtime-invariants.zh-CN.md)。

---

[用户指南索引](../USER.zh-CN.md) · [English](add-provider.md) · [新增应用](add-application.zh-CN.md) · [文档索引](../README.zh-CN.md)
