[English](dashboard.md)

# 管理面板

管理面板是 Gateway 提供的单页 Vue 3 应用。左侧边栏（宽度低于 1024px 时改为顶部横向菜单）有七个固定核心页面：**仪表盘**、**接入 Key**、**账号**、**供应商**、**别名**、**日志**、**设置**。设置下方的分界线之后是可选的 **扩展** 分组；CPA 是其中仅本机使用的入口。Windows x64、macOS 和 Linux x64 的桌面应用或 CLI 还可以在该页安装并手动启动由 OCG 拥有的 CPA 运行时；其他平台仍只支持连接外部 CPA。顶栏右侧是主题切换、语言切换、退出登录。面板原生支持十种语言：简体中文、繁體中文、English、日本語、한국어、Español、 Français、Deutsch、Português (Brasil)、Русский，默认简体中文。语言选择持久化在 `localStorage` 的 `ocg-manager.locale`；浏览器拒绝持久化时，当前会话仍正常工作。

## 查找和编辑配置

新增账号的搜索会一起筛选方案、预设、已保存的供应商和平台类型；改变搜索不会丢弃当前选中的表单。窄屏下，供应商表格只在面板内部横向滚动，供应商选择和目录操作仍保持可见。日志保留状态、模型和时间筛选；其余选项通过**更多筛选**展开，并显示正在使用的额外筛选数量。CPA 尚未完成配置时会引导返回概览。

## 面板 V3

当前 SPA **只** 访问 `/dashboard/api/v3`。接入中心、接入 Key、账号、供应商、别名、日志、设置，以及登录、注册、退出，全部走这条路径。写入携带 `expectedRevision` 与 `processGeneration` 做 CAS；若同一进程的另一个标签页先保存，服务端返回 HTTP 409，错误码 `revisionConflict`。SPA 会刷新控制 token 与受影响资源，但不会自动重放被拒绝的变更；确认当前值后再次提交即可。这些 token 只属于当前进程；多个进程共用一个数据目录时，并不构成统一的 CAS 域。OpenCode Go 价格快照使用独立的 `pricingRevision`，与设置 token 无关。

明文 Key 只出现在接入中心载荷（`GET /dashboard/api/v3/connection`）里。Settings 资源从不包含 Key 值。浏览器只把秘密留在内存；退出登录或 401 会话失效会立即清除。

切换标签时视图保持缓存（`KeepAlive`），返回时刷新服务端数据；**仪表盘** 视图在浏览器标签回到前台时也会刷新。目录、价格与供应商模型列表不会自动轮询；官方用量同步由服务端调度。**设置** 页开始签名桌面安装后，可能会轮询安装进度，直到进程重启。

仍调用已退役 `/dashboard/api` REST 的缓存页面会收到 HTTP 410，错误码 `dashboardV2Removed`，提示先刷新页面，不够再升级。未登录的退役 REST 会先返回 401。两类 V2 路径仅作为缓存旧页面的兼容例外保留：`/dashboard/api/auth/status`、`/dashboard/api/auth/register`、`/dashboard/api/auth/login`、`/dashboard/api/auth/logout`，以及 `/dashboard/api/browser/sessions/{token}/ws`。当前面板改用 V3 的鉴权与浏览器 WebSocket 路由。

要探测 OpenCode Go Key，请用 CLI `key ping` 或发一次真实客户端请求。Custom 卡片仍有 **验证连接**；托管注册仍有 Key 验证。

## 接入中心

首屏第一个面板——也是唯一固定置顶的面板——是 **接入中心**，它展示客户端需要的全部信息：

- **Key**：支持重新生成、一键复制，以及跳转 **接入 Key** 视图的「管理接入 Key」。重新生成只让当前选中 Key 的旧值立即失效，其他 Key 不受影响。存在多把启用 Key 时，选择器会切换展示值（脱敏）、复制目标与重新生成目标。复制会把完整明文 Key 写入剪贴板；在公用或共享设备上使用后请清除剪贴板历史。新建、改名、启用/停用、删除在 **接入 Key** 页，不在接入中心。主 Key 与子 Key 一样通过重置生成新值，没有自定义值输入框。
- **API Base URL**（例如 `http://127.0.0.1:9042/v1`）：一键复制，另附 Chat Completions、Responses、Messages 的完整端点。
- **HTTP 警告**：当解析出的根地址是非回环的明文 `http://` 时出现，提醒 Key 与请求内容会明文传输。

**设置 → 下游访问根地址（Downstream Access Root）** 只控制面板展示的 URL。有效值按以下顺序决定：

1. 非空的 `OCG_CLIENT_ROOT_URL` 环境变量。
2. 面板保存的手工值。
3. 自动推导值：生产环境使用当前 origin，开发环境使用 `http://127.0.0.1:<Gateway 端口>`。

环境变量接管时输入框只读，修改变量并重启后生效，变量值不会写入 SQLite。自动值会显示在输入框中，但不会被保存。

如果客户端通过反向代理或别的主机访问 Gateway，设置外部可访问的根地址，例如 `https://ocg.example.com`。尾部的 `/v1` 会被自动识别并去掉。**这个设置不会** 改变 Gateway 的监听地址、配置 DNS、也不会创建反向代理——这些必须已经指向正在运行的 Gateway。明文 HTTP 允许用于局域网部署，但会暴露 Key 与请求内容。

## 接入 Key

**接入 Key** 视图是客户端凭证的主名单。主 Key 与子 Key 一起存放在 `access_keys`（schema v27）。新建、改名、启用/停用、重新生成、删除都走 Dashboard V3；成功变更会 bump settings revision。变更回执不含明文，页面会重新加载接入中心以展示新值。

- **主 Key** 始终有效，不能停用或删除，通过重置生成新值。其 id 为 `00000000-0000-0000-0000-000000000001`。没有自定义值输入框。
- **子 Key** 是额外创建的凭证，可命名、重命名、启用/停用、重新生成或删除，适合每台设备分发一把。删除子 Key 是软删除：立即失效且明文被清除，但转发日志仍能按名称归因。子 Key 的值不能与主 Key 或其他子 Key 相同；未删除的子 Key 最多 64 把。

接入中心只复制已启用的 Key。按 Key 看用量在日志页过滤。客户端配置见[手动客户端配置](add-application.zh-CN.md)。

---

[用户指南索引](../USER.zh-CN.md) · [English](dashboard.md) · [文档索引](../README.zh-CN.md)
