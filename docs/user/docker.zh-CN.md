[English](docker.md)

# Docker

Docker 版在同一个端口 `9042` 上无头提供 Dashboard 和 Gateway。镜像在 GHCR 上匿名可拉，`linux/amd64`
与 `linux/arm64` 自动匹配。把 Release 的 `compose.example.yaml` 存为
`compose.yaml`，按需加 `.env`，然后执行下面命令；或者检出对应 tag
的仓库：

```bash
git clone --branch v2.4.1 --depth 1 https://github.com/klarkxy/open-console-gateway.git
cd open-console-gateway
cp .env.example .env
# PowerShell: Copy-Item .env.example .env
# Edit .env before exposing the service outside the host.
docker compose pull
docker compose up -d --no-build
docker compose ps
```

镜像标签会动；先决定是跟车还是钉死。

源码仓库现为 `klarkxy/open-console-gateway`。已发布的 GHCR 镜像继续使用
`ghcr.io/klarkxy/opencode-go-mgr` 和 `ghcr.io/klarkxy/opencode-go-mgr-browser`；
仓库改名不会同步更改镜像包名。Compose 和容器发布工作流保留这些现有名称，
使已有安装继续沿用同一升级通道。

## 选择镜像

- 仓库源码里的 `compose.yaml` 默认用 `latest`；Release 的
  `compose.example.yaml` 钉死对应完整版本。
- 生产部署建议在 `.env` 中用 `OCG_IMAGE` 固定完整版本标签，例如
  `ghcr.io/klarkxy/opencode-go-mgr:2.4.1`。
- 完整版本与 `sha-<commit>` 标签指向单次发布，按策略不应移动；
  `1.5` 与 `latest` 会继续移动。技术上只有 digest
  `ghcr.io/klarkxy/opencode-go-mgr@sha256:...` 真正不可变。
- 想调试当前源码时，设置 `OCG_IMAGE=ocg-manager:local`，再执行
  `docker compose up -d --build`。`NPM_REGISTRY` 与 `CARGO_REGISTRY`
  只属于源码构建参数。

| 变量 | 作用范围 | 含义 |
| --- | --- | --- |
| `OCG_IMAGE` | Compose | 镜像标签、镜像站、本地名称或不可变 digest。 |
| `OCG_BROWSER_IMAGE` | Compose | 可选 Chromium/noVNC Sidecar 的镜像标签、镜像站、本地名称或 digest。 |
| `OCG_PORT` | Compose | 宿主机回环端口；容器内仍监听 `9042`。 |
| `OCG_ADMIN_USERNAME` + `OCG_ADMIN_PASSWORD` | 首次启动 | 可选管理员引导；必须同时设置或都不设置。 |
| `OCG_CLIENT_ROOT_URL` | 运行时 | 只读覆盖外部客户端根地址。 |
| `OCG_CPA_BASE_URL` | Compose CPA profile | 只读 CPA 并列服务地址；保持 `http://cpa:8317`。 |
| `CPA_MANAGEMENT_PASSWORD` | Compose CPA profile | CPA Management API 密码；只保存在部署用 `.env`。 |
| `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` | 运行时 | “自动（系统 / 环境）”出站代理模式使用的标准代理变量。 |
| `OCG_MANAGER_ENCRYPTION_KEY` | 恢复时 | 原部署曾显式使用的混淆密钥。 |
| `NPM_REGISTRY` + `CARGO_REGISTRY` | 源码构建 | 仅 `--build` 使用的依赖注册表。 |

大多数部署只跑主服务；只有需要在无头主机上做托管注册或官网登录时，
才加浏览器 Sidecar。

## 可选本机 CPA

CPA 是可选的**本机**订阅运行时并列服务，默认关闭。启用前先在 Compose 文件旁复制模板，并设置独立的 CPA 推理 Key：

```bash
cp cpa-config.example.yaml cpa-config.yaml
# PowerShell: Copy-Item cpa-config.example.yaml cpa-config.yaml
# 编辑 cpa-config.yaml 与 .env：设置 api-keys 和 CPA_MANAGEMENT_PASSWORD。
docker compose --profile cpa up -d
docker compose --profile cpa ps
```

镜像固定为 `eceasy/cli-proxy-api:v7.2.145`，有意不使用 `latest`。CPA 推理端口 `8317` 只发布到私有 `cpa-private` bridge，OCG 通过 `http://cpa:8317` 访问。宿主机只暴露 CPA 的 OAuth 回调端口 `1455`、`54545` 与 `51121`，且都绑定 `127.0.0.1`。不要添加公开的 `8317` 映射、Docker socket 挂载或远程 CPA URL。

CPA 把 OAuth 数据保存在 `cpa-auth` 卷的 `/root/.cli-proxy-api`；OCG 不读取或复制这些文件。`cpa-auth` 必须与 `ocg-data`、`ocg-browser-profiles` 分开备份；恢复它还需要对应的 CPA 配置和 Key。`docker compose down` 保留三个命名卷，`docker compose down -v` 会永久删除它们。

容器运行后打开 **扩展 → CPA**，填入与 `cpa-config.yaml` 相同的 CPA 推理 Key 和 Management password，运行应用级检测，再在 CPA 内完成 OAuth。OCG 容器不会替你启动、停止、升级或 health-check CPA。

## 可选远程浏览器

浏览器 Sidecar 默认关闭。只在 Linux 服务器或 Docker 宿主机上需要托管
注册或官网登录时才打开；至少预留 2 CPU、2 GiB 内存和 1 GiB `/dev/shm`，
然后执行：

```bash
docker compose --profile browser up -d
docker compose ps
```

`OCG_BROWSER_IMAGE` 覆盖默认浏览器镜像。Sidecar 是普通 Chromium 加
Xvfb、窗口管理器、x11vnc 与 noVNC；Dashboard 通过认证同源 WebSocket
在完整标签页中显示画面，键鼠输入也走该连接。复制或粘贴 Key 用页面上的
远程剪贴板区域。反向代理必须支持 WebSocket 升级。
Chromium 使用 basic 密码存储，持久化 Profile 不依赖宿主机密钥环。

每个节点同一时刻只允许一个远程 Chromium。切换账号时会先正常关闭当前
Chromium、等待 Profile 写盘，再启动目标账号；之前打开的远程页面立即
失效。Dashboard 浏览会话令牌只在主服务内存中保存，绑定当前管理员会话并
校验 Origin；空闲 30 分钟或创建满 4 小时后失效。重新打开账号页面即可
取得新会话。

Sidecar 不发布宿主机端口，也不挂载数据库。控制端口和 noVNC 只在 Compose
的 `browser-private` 项目私网内可见。该桥接网络不能设为 Docker
`internal`，因为 Chromium 需要访问 Google/OpenCode 的 HTTPS 出站网络；
Sidecar 的两个端点仍不会发布到宿主机。随机控制令牌存放在共享的
`ocg-browser-runtime` 运行时卷。账号 Cookie/Profile 则持久化在
`ocg-browser-profiles`；运行时卷不属于备份，`ocg-data` 与
`ocg-browser-profiles` 才是必须成对停止并备份的两个敏感卷。

Google 可能把数据中心出口 IP 视为高风险，要求额外验证，甚至拒绝注册或
登录。Open Console Gateway 不绕过这类风控；遇到时由用户完成 Google 要求的验证，
或改用桌面端住宅网络完成注册。真实付款始终由用户在官网明确执行。

## 管理员引导

`OCG_ADMIN_USERNAME` 与 `OCG_ADMIN_PASSWORD` **只在数据库里还没有管理员时**
生效。

- 两个变量必须同时设置；只设一个会启动报错。
- 已有管理员后，后续修改环境变量不会再覆盖。
- 都不设置时，由首位访客在面板里创建管理员。
- 管理员创建后，只要保留卷，就可以移除这两个变量，数据库里的账号仍然
  有效。执行 `docker compose up -d --no-build --force-recreate` 把它们从
  容器环境中移除。

拥有 Docker daemon 权限的人可以看到容器环境变量；请保护 `.env`、使用
长随机密码，并避免把未初始化的面板直接暴露到公网。

## 密钥与地址

`OCG_MANAGER_ENCRYPTION_KEY` 用于恢复曾经显式设置过它的部署。正常部署
请留空，让生成的 `.encryption-key` 留在数据卷中。凭据保存后再修改或
丢失该值，会导致已保存凭据无法读取；请把它当作密码保管。

可选的 `OCG_CLIENT_ROOT_URL` 等同于面板里的“下游访问根地址”，适合在反向
代理或 Dashboard 与 Gateway 使用不同外部地址时显式指定客户端根地址。非空
值必须是绝对 HTTP(S) URL；设置后优先于 SQLite 中的手工值，非法值会让
进程启动失败。它不配置监听、DNS 或反向代理。一般填写
`https://ocg.example.com`，不需要填写 `/dashboard/` 或具体 API 端点；
末尾 `/v1` 可省略或保留。

## 运行时行为

在 `.env` 中设置 `OCG_PORT` 可修改宿主机端口，容器内仍固定使用 `9042`。
打开 `http://127.0.0.1:<OCG_PORT>/dashboard/` 并登录。请访问
`/dashboard/`，服务根路径 `/` 不是面板地址。

- 数据与生成的 `.encryption-key` 混淆密钥持久化在 `ocg-data` 卷中；账号
  浏览器 Cookie/Profile 持久化在独立的 `ocg-browser-profiles` 卷中。
- 容器进程监听 `0.0.0.0`，因此即使只发布到宿主机 `127.0.0.1`，管理面板
  也必须使用管理员登录；宿主机端口映射只限制可达范围，不会启用回环免
  登录。
- 容器的 `HEALTHCHECK` 每 30 秒对容器内 `127.0.0.1:9042` 做 TCP 探活，
  不存在 `/healthz` 路由。这个 TCP 检查只说明进程正在监听，不能证明面板
  API、上游账号或真实模型请求可用。
- 两个镜像都以非特权 `ocg` 用户（UID/GID 10001）运行。随附 Compose 把根
  文件系统设为只读、把 `/tmp` 挂成 tmpfs，并丢弃全部 Linux capability。
  主服务另外启用 `no-new-privileges`；browser 服务改用
  `seccomp=unconfined`，以便普通 Chromium 建立自身的 namespace 和 renderer
  seccomp 沙箱。Sidecar 不使用 `--no-sandbox`，另有 1 GiB 共享内存；命名卷
  `ocg-data` 与 `ocg-browser-profiles` 是两类持久化应用状态。
- 启动日志会打印 Key，因此日志输出和 Docker daemon 权限都属于敏感信息。
  如果 Docker 主机默认没有限制日志大小，请由部署方配置日志轮转。

常用检查命令：

```bash
docker compose config --quiet
docker compose ps
docker compose logs --tail=100 -f ocg-manager
docker compose --profile browser logs --tail=100 -f browser
docker compose --profile cpa logs --tail=100 -f cpa
curl --fail http://127.0.0.1:9042/dashboard/
```

如果修改过 `OCG_PORT`，请把 curl 命令里的 `9042` 替换成实际宿主机端口。

## 校验镜像

主镜像与浏览器镜像都带 SPDX SBOM、BuildKit SLSA provenance 与 GitHub 签名
的 provenance attestation。可这样检查发布版本：

```bash
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr:2.4.1
docker buildx imagetools inspect ghcr.io/klarkxy/opencode-go-mgr-browser:2.4.1
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr:2.4.1 \
  --repo klarkxy/open-console-gateway
gh attestation verify \
  oci://ghcr.io/klarkxy/opencode-go-mgr-browser:2.4.1 \
  --repo klarkxy/open-console-gateway
```

两条 `gh attestation verify` 命令都要求 GitHub CLI 已登录。公开镜像可匿名
拉取；如果 OCI 客户端仍要求 registry 凭据，请用具备 package 读取权限的
token 登录 `ghcr.io`。Provenance 证明产物如何构建。

如果 Key 泄露，请重新生成。

## HTTPS

需要 HTTPS 时，把现有反向代理指向该回环端口即可，例如 Caddy：

```caddyfile
ocg.example.com {
    reverse_proxy 127.0.0.1:9042
}
```

登录后先在面板里设置一个非空的 Key，再发送 API 流量。用
`docker compose down` 停止服务；只有当你想彻底删除账号、凭据、Key、Cookie
与浏览器 Profile 时才追加 `-v`。

---

[用户指南索引](../USER.zh-CN.md) · [English](docker.md) · [文档索引](../README.zh-CN.md)
