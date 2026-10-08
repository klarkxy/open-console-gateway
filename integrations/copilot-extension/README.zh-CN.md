# Open Console Gateway VS Code 扩展

从 OCG「应用 > VS Code Copilot」安装扩展，并打开或重新加载所选 Profile。模型通过鉴权 `/v1/models` 动态发现，请求前会重新核对。上下文与输出限制在 OCG 维护一次；缺失时通过应用页或「OCG: Refresh Models」查看待补模型，并前往 Alias 的模型能力入口。

手动安装 VSIX 后运行「OCG: Connect」，填写网关 `/v1` 地址和 Key。Key 只写进所选 Profile 的 SecretStorage。「OCG: Disconnect」清除加密连接；OCG 托管卸载要等扩展确认已删除秘密，再移除注册。OCG 自身的 Key 保留。

扩展运行在本机 UI 宿主，提供 Chat/Agent 模型，不提供代码补全或 Next Edit Suggestions。Chat、Responses、Messages 使用已发布首选协议；工具和图像按声明能力处理。o200k 文本计数与图像估算可能不同于模型实际分词，但不改动声明上限。私有推理签名仅保存在有界会话内存中。

在仓库根目录运行 `pnpm install --frozen-lockfile`、`pnpm run build:copilot`、`pnpm run test:copilot`。`pnpm run check:copilot` 检查嵌入式运行文件及完整许可证声明是否与源代码、工作区锁文件一致。

「OCG: Set Reasoning Effort」按模型选择 OCG 已声明的推理强度，偏好按模型和 Profile 保存，不为 Messages 猜测思考预算。
