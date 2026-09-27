[English](applications.md)

# 应用

**应用**是面板的下游集成页面，目前承载 **DSH** 子页签，用于把 DSH 本身接入 Gateway。

## DSH

**DSH** 子页签通过页面上显示的**运行地址**安装或卸载 OCG 自有插件。

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
2. 打开 **应用 > DSH**，选择目标 Home 和 profile，核对运行地址，再选择一把已启用的 OCG Key。
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

浏览器只把选中 Key 的 id 发送给已鉴权的 Dashboard API。桌面 Host 在后端解析其值，
为选中的目标写入一个收紧权限的一次性交接文件，再请确认窗口里显示的运行地址安装已物化的包。
DSH 加载插件后，插件会把该值导入 DSH 自己的凭据服务，并删除交接文件。Key 不会进入生成的插件源码或命令参数。
对于 Editor 托管的 profile，OCG 还会把插件登记到 Editor 的用户插件状态，使其在 profile 重建后保留；
安装前先退出 Editor，安装后重新启动。

插件注册一个 **Open Console Gateway** 供应商；当 DSH 读取模型或准备调用时，它会刷新带鉴权的
`GET /v1/models`。这里使用的是 OCG 当前提供给客户端的完整公开名称集合，包含符合条件的
Custom ID，而不是范围更窄的 Dashboard `application-models` 列表。因此，模型可见性发生变化时无需重新安装插件。

页面显示**已安装**，只证明插件登记和凭据交接已经准备完成；它不等于 DSH 已重启、已加载插件，
也不等于真实模型调用已经成功。重启后请在 DSH 中选择一个 OCG 模型发送请求，并到 OCG **日志**中确认。
安装被阻止时（环境不支持、未检测到 DSH、插件命令失败或存在冲突），页面会在状态旁显示 Host 返回的具体原因。

安装会跨越两份本机存储，因此回滚有一个明确边界：如果 DSH 已导入 Key，而后续安装步骤失败，
OCG 可以恢复插件登记与仍在等待的交接文件，但无法证明 DSH 的凭据写入尚未提交。需要重试时，
重新打开此页面，用预期的 Key 点击**重新安装**；随后只启动一次 DSH，并在 OCG **日志**中确认请求。
要从正在运行的 Web 或 Desktop 地址移除集成，请使用本页的**卸载**。它只移除
`@open-console-gateway/dsh-plugin`，保留其他包、DSH 凭据和 OCG Key。
Editor 托管且未填写运行地址的 profile，还需通过 Editor 插件管理移除插件，否则其启动恢复会重新安装。
如果你明确要放弃一次尚未激活的交接，只能在 DSH 已停止时，删除页面列出的
`credential-handoff` 与对应 `.claimed-*` 文件。移除插件不会停用 OCG Key；必要时请在 OCG 中轮换或停用该 Key。

原生无头 CLI 可以把插件安装到其所在宿主机的 DSH。官方 Docker 镜像会明确显示不支持本机安装：
容器不能把插件安装到浏览器所在电脑或 Docker 宿主机的 DSH，但仍可通过普通 Gateway 配置服务 DSH。

[用户指南索引](../USER.zh-CN.md) · [手动客户端配置](add-application.zh-CN.md) · [文档索引](../README.zh-CN.md)

## DSH 中的模型信息

参见[模型元数据与推理档位](model-metadata.zh-CN.md)，了解目录刷新、按连接声明参数，以及升级已安装的 OCG 插件。
