[English](extending.md)

# 扩展 Open Console Gateway

供应商适配器与外部接入是当前扩展路径。

## 1. 供应商或套餐：静态、密封

只用于 OCG 自己拥有完整路由、目录、协议、Key 与故障契约的上游家族。

1. 在 `ocg-domain`（`ids.rs`、`provider.rs`）加入身份与目录事实，穷尽扩展 `ProviderAdapterKind`，并保持每个静态 Provider 的合约范围稳定。Provider 与 Plan 是同一个 `provider_id` 身份。Custom 保持 `ConfigurableHttp`。
2. 在 `ocg-domain::protocol` 加所需协议行，在 `ocg-gateway::alias` 加 Alias mapping。请求路径使用已保存的合约。
3. 在 `ocg-core` 实现只返回 `AttemptSpec` 的 `resolve_route`。适配器不能持有 DB、`CoreState` 或原始 reqwest client。
4. 在 schema v42 的 builtin 种子中登记该密封适配器，使统一 `providers` 表以 `builtin` 行的形式在 V3 Provider 目录中暴露。行的 `endpoint_url` / `upstream_protocol` / `auth_kind` / `offering` / `endpoint_per_account` 列是密封注册表的展示镜像——流量与路由仍然只走密封适配器代码常量，绝不读取已种子化的行。同时把新 id 加入 `ocg-domain::provider` 的 `builtin_offering` 映射（付费家族为 `plan`，免费或账号所有为 `api`）。CPA 是静态外部接入，**不**进种子表：不要在这里登记。
5. 控制面与路由语义未完成前保持 fail closed，完成后测试 domain、gateway 与 core 边界。

Provider 注册表始终静态、密封。
每个静态 Provider 在自己的单一 `provider_id` 身份下拥有目录、证据与覆盖状态。

新增 **preset**（`resources/provider-presets.json` 中的用户定义供应商模板）只需在 JSON 中加入条目；若该 preset 声明 `plan` offering，还需在 `ocg-domain::provider` 的 `PRESET_OFFERINGS` 映射中加入一条，使 `preset_offering(preset_id)` 返回 `"plan"`。其他 preset 保持默认 `"api"`。

## 2. 应用

应用页目前只有一个 DSH 专用 V4 流程和一份 OCG 自有插件包；它不是通用注册表，也不是承诺开放的扩展点。只有第二个应用具备明确的安装、凭据、生命周期与验收契约时，才按该具体需求增加。见[应用](../user/applications.zh-CN.md)。

## 3. 外部接入：静态本机服务适配器

用于通过代码评审的适配器接入本机服务。它在设置下方通用的 **扩展** 导航组中出现，不属于供应商、套餐或新增账号选择器。

- 定义窄的 typed Dashboard V4 contract 与 CAS 写入；不增加原始管理 API 代理或任意上游 path/body 转发。
- 明确数据归属：OCG 只保存连接与路由必需内容；外部服务保留自己的 OAuth Token、auth 文件、浏览器回调和内部调度。自行运行的外部服务也自行管理生命周期；CPA 托管模式则单独拥有其安装文件与子进程，见[运行时不变量](runtime-invariants.zh-CN.md#外部接入)。
- 保持本机边界：桌面/CLI 仅回环，Docker 仅显式私有 Compose sibling。远程服务地址和任意进程控制不属于该边界。CPA 托管生命周期操作仅针对 OCG 拥有的运行时，安装与更新由用户触发。
- 只有产品契约明确要求时才复用 OCG 排序/选择/日志约定；不要虚构外部服务未提供的内部账号、费用或额度。

CPA 是此路径的当前实例。适合时复用已有 helper；需要抽取通用框架时，应以实际使用它的接入需求说明理由。

## Dashboard V3 与 V4 端点变更

新的供应商、connection 或凭据语义进入 `dashboard_v4`（`types.rs` 及其 `CATALOG_TYPE_NAMES`；路由在 `dashboard_v4/mod.rs`），并运行 `pnpm run contract:v4:check`。V3 冻结，不接受新的 DTO 字段或路由，只修缺陷。

冻结 V3 契约的维护步骤：

1. 在 `dashboard_v3/types.rs` 增加或扩展 DTO，并把新名字追加到 `CATALOG_TYPE_NAMES`；既有 `$defs` 不变。
2. 在 `dashboard_v3/mod.rs` 挂路由；写入走 `parse_mutation_json` 与 `check_expectation`，保持秘密脱敏。
3. 优先复用已有持久化/控制 helper，`dashboard_v3` 独立于 `gateway`。
4. 补聚焦集成测试、更新 `src/api/dashboard-v3.ts`，并运行 `pnpm run contract:v3:check`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](extending.md) · [文档索引](../README.zh-CN.md)
