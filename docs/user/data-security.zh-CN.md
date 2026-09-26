[English](data-security.md)

# 数据与安全

Open Console Gateway 把 Key、密码和浏览器会话存在本地磁盘。请保护数据目录：丢失后没有远端恢复。

- **GUI 数据目录**：Windows `%USERPROFILE%\.ocg-mgr`；macOS / Linux `~/.ocg-mgr`。CLI 数据默认 `~/.ocg-mgr-cli`（所有平台一致），可用 `--data-dir <path>` 覆盖。
- **凭据存储**：账号 Key 与保存的登录密码以 AES-256-GCM（`v2:` 密文）存放，密钥由本机 Host cipher 种子派生。更早的 XOR 混淆行仍可解密，因此目录备份能恢复；成功打开后会改写成 v2。这仍是本地磁盘边界，不是远端 KMS：拿到数据目录及其 `.encryption-key`，或能在原 Windows 用户/机器上下文运行 Windows GUI 的人，都能恢复账号 Key 与保存的登录密码。面板接入 Key 存放在 `access_keys` 表。macOS / Linux GUI 与 CLI 的数据目录里还有 `.encryption-key` 文件；**必须和数据库一起备份**，丢失后已存的凭据将无法读取。面板 SPA 不会把 Key 明文写入 `localStorage`；接入中心的秘密只留在内存，直到退出登录或 401。探测与修复错误不会打印明文 Key。
- **浏览器 Profile**：`browser-profiles/` 或 Docker 的 `ocg-browser-profiles` 含长期 Cookie 与官网登录状态，完全不由 Open Console Gateway 加密。备份、传输、访问控制和销毁都应按数据库与账号 Key 的敏感级别处理。
- **可移植节点备份**：每个节点由自己的面板管理。需要迁移时，从回环面板显式生成密码加密的 `.ocgbackup` 文件，无需额外的管理员二次确认。账号与接入 Key 使用 Argon2id 派生密钥并以 AES-256-GCM 加密。迁移密码不会保存，也无法找回；应与文件分开保管和传递。导出使用当前 payload，导入接受 V4 至当前导出版本（V4/V5 保留旧版冷却只属于本机的行为）；当前版本见[升级与备份](upgrade-backup.zh-CN.md)。历史上 V7 起保留目的地、凭据、身份分组、凭据与绑定 ID、模型限制、配额池关系及冷却截止时间；导入不会缩短目标端更晚的冷却。V10 起还携带每个账号的积分配置，V11 起还携带显式 HTTP 协议路由——早于 V11 但携带非空显式路由的包会被拒绝，避免丢失这些路由。浏览器 Profile、登录密码、日志、用量和本机 Host 设置不在迁移包内。回退版本时应恢复包含加密密钥在内的完整数据目录备份。
- **明文 HTTP 警告**：非回环的 `http://` 根地址会把 Key 与请求内容明文传输到网络中。请使用 HTTPS 或仅在可信局域网使用。
- **管理员密码**：唯一的管理员密码以 Argon2 哈希保存在 SQLite 中，没有自助找回流程——请保护好数据目录。
- **Custom API 目的地**：完整的 Custom 推理 Endpoint 由管理员显式信任。允许公网、局域网与回环 HTTP / HTTPS 目的地。元数据、链路本地以及不透明 IPv4 把戏主机会被拒绝。URL 内嵌凭据、query 与 fragment 会被拒绝；携带秘密的请求不会跟随重定向；不会转发 dashboard 或客户端凭据。已保存的 Custom 与用户定义供应商 Key 只会发往该 Key 已保存的端点与 Origin 授权。官方密封 Key 还要求已保存的协议端点 id；清空它们会阻止发送和已存 Key 测试。改供应商或 Custom 地址不会自动授权。显式授权过的、当前已配置的外站 Origin 可以发送；未授权的覆盖不会发送。解密或实际发送前，网关会重读所选账号、绑定、Key 版本、模型范围与授权。轮换或停用该 Key、收窄范围会使这一次尝试失败，而不会发出旧 Key。请只填写本节点确实要访问的目的地。

---

[用户指南索引](../USER.zh-CN.md) · [English](data-security.md) · [文档索引](../README.zh-CN.md)
