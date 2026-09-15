[English](install.md)

# 安装与首次启动

安装本地 Gateway，启动它，用完就把浏览器标签页关掉。剩下的，主要是让操作系统相信小作坊开发者也是真实存在的。

## Windows 10/11 x64

1. 运行 NSIS 安装包 `ocg-manager_<version>_windows-x64-setup.exe`，按当前用户安装，不需要管理员权限。
2. 在开始菜单中启动 **Open Console Gateway**。正常启动会在系统浏览器打开管理面板；之后可从托盘图标重新打开。
3. 当前 Windows 包未签名，SmartScreen 可能弹出警告，点击 **更多信息 → 仍要运行** 继续。
4. 在 **账号** 视图添加 OpenCode-Go 账号，复制 Key，把客户端指向 `http://127.0.0.1:9042/v1`。
5. 再次运行安装包会原地替换已有副本，并保留 `%USERPROFILE%\.ocg-mgr`。从 Windows **已安装的应用** 卸载。确认页有 **删除应用数据目录** 选项，不勾选则保留数据。静默卸载和应用内更新都不会删除它。

## macOS 11+ Intel / Apple Silicon

1. 打开 Universal DMG，把 **Open Console Gateway** 拖入 **Applications**。
2. 应用使用临时签名（ad-hoc），首次启动可能被 Gatekeeper 拦截。打开 **Privacy & Security**，点击 **Open Anyway** 放行。
3. 启动应用。正常启动会在系统浏览器打开管理面板；之后可从托盘图标重新打开。添加账号，复制 Key，配置客户端。

## Linux x64

1. 安装前先核对 `SHA256SUMS`。
2. 用发行版包管理器安装 `.deb`，或对 AppImage 执行 `chmod +x ocg-manager_<version>_linux-x64.AppImage`。
3. 启动可执行文件。正常启动会在系统浏览器打开管理面板；之后可从托盘图标重新打开。
4. 数据保存在 `~/.ocg-mgr/`。

Windows 下开启自动启动后，程序只会安静地回到托盘，不会替你重新打开浏览器。

---

[用户指南索引](../USER.zh-CN.md) · [English](install.md) · [文档索引](../README.zh-CN.md)
