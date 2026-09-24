# 控制台 UI 设计

[English](ui-design.md)

## 参考来源与范围

参考 **MoonshotAI 官方** `kimi-code` 仓库的历史提交 `e7d5a0aee74e7f116cca0273c416ece9139a78a0`，尤其是 [`apps/kimi-web/src/style.css`](https://github.com/MoonshotAI/kimi-code/blob/e7d5a0aee74e7f116cca0273c416ece9139a78a0/apps/kimi-web/src/style.css) 及其应用内设计系统。这是可检查的历史源码，不代表当前内部实现，也不是第三方同名桌面客户端。

控制台借鉴其冷中性表面、克制的蓝色强调、紧凑排版、分层暗色、统一圆角和按需展开的信息。实现仍是 OCG 自己的 Vue/Naive UI；不引入 Kimi Logo、字体文件、私有构建产物或聊天专用流程。

不机械套用参考色 `#1783FF`：浅色主题的主要操作与必要文字采用更深的 `#0967D2`；暗色蓝色按钮明确使用深色前景。必要文字会在各层表面上测试，而不只对白色测试。原来的七个主题偏好 ID 保留；彩色主题仍整体带色，但降低饱和度。

## 实现位置

- `src/theme.ts`：配色、表面语义、尺寸及 Naive UI 公共主题覆盖。`src/styles/main.css`：首屏回退值、焦点、字体和减少动态效果。
- `src/App.vue` / `src/styles/shell.css`：登录后的应用外壳、侧栏折叠记忆、移动菜单、内容宽度和登录页。原有会话、安全处理与 KeepAlive 导航保持不变。
- `src/components/AppCommandPalette.vue` / `src/domain/navigation-search.ts`：Ctrl/Command K 本地导航、名称/ID 搜索、方向键选择和输入法安全处理。不请求供应商，也不读取密钥。
- `src/views/Dashboard.vue` / `src/styles/dashboard.css`：明确标注的接入行、脱敏 Key 操作、缩小的装饰形象、轻量关注列表与图表。保留原 store 调用、轮换确认和首次加载标记。
- `AccountCardFrame.vue`：统一账号表现，冷却使用警告色。`FormSurface.vue`：弹窗正文按视口限制高度并滚动；验证与保存仍由原表单负责。

当前外观由 [`DESIGN.md`](../../DESIGN.md) 规定。上一版的详细产品交互要求保留在 [`DESIGN.product.md`](../../DESIGN.product.md)；只替代其中的旧视觉规则，不替代账号、路由和 Key 的行为约束。

## 回归检查

运行 `pnpm run test:web`、`pnpm run build:web` 和 `pnpm run design:lint`。单元测试覆盖偏好迁移、存储不可用、表面与组件一致性、对比度、本地化导航搜索、方向键循环和组合输入期间的快捷键。这不等于浏览器或桌面视觉检查通过。

在浏览器中检查 1440×900、1280×720、768×1024、390×844 的浅色和暗色。还要检查所有彩色主题、中英文长标签、有数据和空数据、加载/重试、长表单滚动、Tab/Escape/焦点恢复、Ctrl/Command K 选择与取消、折叠状态持久化，以及减少动态效果。确认复制使用当前 Key，轮换仍需确认，外观检查不会触发上游测试。

Tauri 另行检查原生窗口缩放、125%/150% 系统缩放、中文输入法和剪贴板权限。记录被测提交，把单元测试、构建、浏览器和桌面结果分开报告。使用模拟数据的截图必须说明是测试样例，不得当成真实余额或服务状态。
