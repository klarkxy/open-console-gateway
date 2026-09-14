[English](releasing.md)

# 发布流程

1. 在 `package.json`、`src-tauri/tauri.conf.json`、两份 `Cargo.toml`，以及
   `compose.example.yaml` 的标题和默认镜像中写入 `X.Y.Z`（或
   `X.Y.Z-beta.N`）。
2. 用常规 Cargo/pnpm 命令刷新 lockfile，再运行 `pnpm run test`、
   `pnpm run test:tooling`、`pnpm run design:lint`、
   `pnpm run contract:v3:check`、`pnpm run release:check` 和
   `pnpm run build`。提交工具生成的 lockfile 差异，不要手改。
3. 复核相对上一个 tag 的 diff 和当前平台的 `release/` payload，然后提交
   版本、lockfile、文档与 Release notes。
4. 先合并。在已经位于 `main` 的 commit 上打附注 tag `vX.Y.Z` 并推送。不要
   在之后还会 squash-merge 的 commit 上打 tag。
5. 等待 quality、preflight、原生矩阵、`draft-release`、`verify-release` 和
   `publish-release`。确认公开的是同一个 draft。
6. 对该 tag 触发 `container.yml`
   （`gh workflow run container.yml --ref main -f tag=vX.Y.Z -f publish_latest=true`），
   确认两个 GHCR package 已公开，并匿名拉取两个完整版本标签。

已发布资产与 tag 不可变。发布有误时用新的 patch 版本修复。

## CI 覆盖不到的人工检查

协议、schema、CAS 与本地列表行为属于 `cargo test` / `pnpm run test`，不放进
本清单。按受影响的 Gateway 协议选择真实客户端检查，并在发布证据中记录客户端版本、平台、结果与未执行项；此前验证的排除项不自动沿用。

- [ ] 质量门、签名 `release:check` 与所选平台冒烟全绿；四份版本清单、
      `compose.example.yaml` 与 `Cargo.lock` 中的 workspace 包条目一致。
- [ ] 对受影响的客户端路径完成文本与工具调用。Gemini 兼容性改动需覆盖 Gemini CLI。
      已退役的应用子系统、教程生成与自动连接器不再作为发布验收对象。
      检查接入中心显示的 Key 已脱敏，复制结果是所选 Key。
- [ ] 可选托管注册（登录身份 → 邀请链接 → OpenCode 登录 → 支付前确认 →
      Key 回填）。真实支付只在明确打算时执行。对已完成 Key 账号与托管账号
      验证官方 `/zen/go/v1/usage` 的额度刷新。
- [ ] Windows：SmartScreen 文案、面板、一个账号、一条请求，`auto_start` 与
      `HKCU\...\Run\Open Console Gateway` 对应，卸载后该值消失。
- [ ] macOS：**Open Anyway**、面板、一个账号、一条请求。
- [ ] Linux：在真实 Wayland 或 X11 会话中安装 `.deb` 并运行 AppImage。
- [ ] 浏览器发现（Windows 上 Edge/Chrome；macOS/Linux 上的平台浏览器）、
      Profile 隔离、重启后 Cookie 保留。
- [ ] 发布后：`container.yml` 全绿，按预期 digest 匿名拉取两个镜像，GitHub
      Release 附件集合未变。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](releasing.md) · [文档索引](../README.zh-CN.md)
