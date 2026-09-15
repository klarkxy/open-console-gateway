[English](applications.md)

# 应用

**应用**是面板的下游集成页面，目前承载 **DSH** 子页签，用于把 DSH 本身接入 Gateway。

## DSH

**DSH** 子页签会把 OCG 自有插件安装到 DSH 的 `web` profile。

1. 在与 DSH `0.1.5-rc.1` 或 `0.1.5-rc.2` 相同的电脑上，以同一个系统用户运行已安装的 Open Console Gateway 桌面应用或原生 `ocg-manager-cli serve`。
2. 打开 **应用 > DSH**，选择一把已启用的 OCG Key。
3. 点击**安装**。在确认窗口中核对检测到的 DSH 版本和准确的本机目标，然后确认。
4. 页面提示时启动或重启 DSH。

浏览器只把选中 Key 的 id 发送给已鉴权的 Dashboard API。桌面 Host 在后端解析其值，
写入一个收紧权限的一次性交接文件，再调用 DSH 官方的 `plugin --profile web add` 流程。
DSH 加载插件后，插件会把该值导入 DSH 自己的凭据服务，并删除交接文件。Key 不会进入生成的插件源码或命令参数。

插件注册一个 **Open Console Gateway** 供应商；当 DSH 读取模型或准备调用时，它会刷新带鉴权的
`GET /v1/models`。这里使用的是 OCG 当前提供给客户端的完整公开名称集合，包含符合条件的
Custom ID，而不是范围更窄的 Dashboard `application-models` 列表。因此，模型可见性发生变化时无需重新安装插件。

页面显示**已安装**，只证明插件登记和凭据交接已经准备完成；它不等于 DSH 已重启、已加载插件，
也不等于真实模型调用已经成功。重启后请在 DSH 中选择一个 OCG 模型发送请求，并到 OCG **日志**中确认。
安装被阻止时（环境不支持、未检测到 DSH、版本不兼容或存在冲突），页面会在状态旁显示 Host 返回的具体原因。

安装会跨越两份本机存储，因此回滚有一个明确边界：如果 DSH 已导入 Key，而后续安装步骤失败，
OCG 可以恢复插件登记与仍在等待的交接文件，但无法证明 DSH 的凭据写入尚未提交。需要重试时，
先停止 DSH，重新打开此页面，用预期的 Key 点击**重新安装**；随后只启动一次 DSH，并在 OCG **日志**中确认请求。
需要移除集成时，先停止 DSH，运行
`dsh plugin --profile web remove @open-console-gateway/dsh-plugin`，再只删除 DSH 凭据设置中的
`OCG_GATEWAY_KEY`（也可以删除 `<DSH_HOME>/.credentials.yaml` 中对应的 `refs` 条目）。
如果你明确要放弃一次尚未激活的交接，只能在 DSH 已停止时，删除页面列出的
`credential-handoff` 与对应 `.claimed-*` 文件。移除插件不会停用 OCG Key；必要时请在 OCG 中轮换或停用该 Key。

原生无头 CLI 可以把插件安装到其所在宿主机的 DSH。官方 Docker 镜像会明确显示不支持本机安装：
容器不能把插件安装到浏览器所在电脑或 Docker 宿主机的 DSH，但仍可通过普通 Gateway 配置服务 DSH。

[用户指南索引](../USER.zh-CN.md) · [手动客户端配置](add-application.zh-CN.md) · [文档索引](../README.zh-CN.md)
