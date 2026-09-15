[English](upgrade-backup.md)

# 升级、备份、恢复与卸载

从 [GitHub 最新 Release](https://github.com/klarkxy/open-console-gateway/releases/latest) 下载升级包，并用同一 Release 的 `SHA256SUMS` 校验：PowerShell 用 `Get-FileHash <文件> -Algorithm SHA256`，macOS 用 `shasum -a 256 <文件>`，Linux 用 `sha256sum <文件>`。下面把备份、恢复和卸载一起讲完——都是平时很枯燥、关键时刻恨自己没看的操作。

Windows 上，安装、应用内更新和再次运行安装包都会沿用已有安装目录，包括尚未改名的 OCG Manager 副本。安装器不会先卸载再安装。升级保留数据目录与开机启动设置，迁移已有桌面和开始菜单快捷方式，并将已安装应用记录更新为新名称。只从 Windows **已安装的应用** 卸载。

## 数据库迁移与接入 Key（schema v49）

数据库 schema 是 **v49**，历史库启动时原地迁移。从单 Key 版本升级会保留既有凭证为 **主 Key**（id 固定为 `00000000-0000-0000-0000-000000000001`），客户端无需改动即可继续鉴权。主 Key 与额外子 Key 共用 `access_keys` 表：未删除子 Key 最多 64 把，删除为软删除，保留名称用于日志归因并清除明文。

已有（非空）库会先规范迁移到 v26，再由 v27 重写把主 Key 与全部 `sub_gateway_keys` 行复制进 `access_keys` 表，删除 `sub_gateway_keys`，并删除 `accounts` 上遗留的五列 `usage_sync_*`。任何 v27 写入前，库会得到同级快照 `data.sqlite.pre-v3.<timestamp>.bak` 及 SHA-256 sidecar。全新空数据目录直接创建 schema v49，不写这些副本。快照只是回滚点，不能替代完整备份：恢复前先校验 sidecar。旧版程序无法打开已迁移的数据库——单 Key 时代不识额外 Key，已撤销的值也不会因降级复活。

v29 从目录中移除 SCNet Token Plans，并在迁移期间删除所有现有 SCNet 账号行。每次启动时，历史 Command Code GOAT 验证状态都会统一为 `not_required`，因为公开目录不是 Key 验证；Custom API 的 enabled 状态保留。OpenCode Go、Zen Free 与未知 provider 身份不受影响。

v30 将 Custom API 的 `account_custom_configs` 从单一 `upstream_protocol` 列扩展为 JSON `upstream_protocols` 集合，按旧值回填每个现有 Custom 账号。Custom 配置/能力编辑保持账号启用，但将 `verification_status` 重置为 `pending`。

v31 新增 `provider_contract_model_protocol_overrides` 表以支持按模型/按协议启用，并停止读取已弃用的 `provider_contract_scopes` 开关列。

v32 将 Custom API 的 base URL、协议集合与可配置鉴权收敛为一个完整推理 Endpoint 和一个上游协议。历史 Custom 行按 Chat Completions → Responses → Messages 选择协议，追加对应标准推理后缀，并置为 disabled/pending 供管理员复核；非所选协议状态在同一事务中移除。

v33 新增 `account_model_capabilities.upstream_model`。历史映射行会以此前的公开 `model_id` 回填，因此升级后既有路由保持不变；新建 Custom 行可使用不同的公开名称与上游名称。

v35 在已知 v34 配对的 fail-closed 预检后，把 Provider/Plan 身份收敛为仅 `provider_id`，并把类型化用户定义供应商保存在 `dynamic_providers` / `dynamic_provider_models`。非空 v34 库会先写 `data.sqlite.pre-v35.<timestamp>.bak`。

v36 曾加入尚未发布的 Ollama Cookie 用量状态；v37 删除该表，并新增账号级 Ollama Cloud 计费档位（Pro/Max/Team）。

v38 新增 `platform_accounts` / `platform_links`，承载账号归属的平台 Key（New API / Sub2API 母账号）。v39 记录供应商预设来源（`preset_id`）；v40 新增按模型 `upstream_override`；v41 新增 MiniMax/Kimi 按模型首选协议表。

v42 把用户定义供应商与密封内置目录统一为同一对 `providers` / `provider_models` 表。非空 v41 库会先写 `data.sqlite.pre-v42.<timestamp>.bak` 及 SHA-256 sidecar。

v43 允许 Responses 作为已保存的首选协议；v44 新增不含秘密的 `dashboard_operations` 台账，支撑面板 V4 写入幂等。v45 新增身份、凭据、绑定与配额池卫星表，并按既有账号回填；v46 在凭据绑定上新增已保存的端点与 Origin 授权。v47 在 `providers` 行上持久化 onboarding 草稿标记。

v48 删除失效的协议开关列与 `free_alias_enabled` 列，以及空的旧动态供应商表。非空 v47 库会先写 `data.sqlite.pre-v48.<timestamp>.bak` 及 SHA-256 sidecar，非空遗留表会 fail-closed 拒绝迁移而不是被删除。v49 新增 `unpublished_public_models`（增量变更，不做迁移前备份）。

### 节点备份载荷

节点备份当前导出 payload V6（含 `providerId` 与身份快照），并包含已保存的用户定义供应商定义。V4/V5 备份仍可导入，沿用旧的冷却仅本机行为；payload V1–V3 备份会被明确的不支持版本错误拒绝——那不是密码错误，也不是文件损坏。

## 备份

1. 停止所有会写数据的进程：从桌面托盘选择 **退出**，用 Ctrl+C 或服务管理器停止 CLI，Docker 则执行 `docker compose stop`。
2. 复制 **整个** GUI 数据目录、CLI 数据目录；桌面账号的 `browser-profiles/` 已包含在 GUI 数据目录中。Docker 必须同时备份 `ocg-data` 与 `ocg-browser-profiles` 两个敏感卷。已停止的 Docker 容器可分别执行 `docker compose cp ocg-manager:/data/. ../ocg-data-backup` 和 `docker compose cp ocg-manager:/browser-profiles/. ../ocg-browser-profiles-backup`。
3. 备份必须放在仓库外，并确认其中有 `data.sqlite`，以及适用时的 `.encryption-key`。浏览器 Profile 含长期 Cookie 和登录状态，不由 Open Console Gateway 加密，必须按账号 Key 与数据库同等级保护。

## 恢复

1. 先停进程，把现有数据移到别处，再把完整备份放回原目录或空的 Docker 卷。
2. 启动相同或更新的版本。

注意事项：

- Docker `/data` 中的文件必须继续允许 UID/GID `10001` 写入。
- Docker `/browser-profiles` 中的文件也必须继续允许 UID/GID `10001` 写入。
- Windows GUI 的混淆信息绑定 Windows 用户与机器，换机后不能直接恢复账号 Key 或密码；请在新机器创建全新数据并重新录入凭据。
- macOS/Linux GUI、CLI 与 Docker 恢复时必须保留 `.encryption-key`，或原来显式传入的 `--encryption-key` / `OCG_MANAGER_ENCRYPTION_KEY` 值。
- 请用相同或更新的版本打开已迁移的数据库。

## 恢复 Docker 备份到全新卷

先确认备份有效，并确认 `.env` 固定到原版本或更新版本。下面 `docker compose down -v` 会永久删除当前全部命名卷，必须先把两类持久数据另行保存：

```bash
docker compose down -v
docker compose run --rm --no-deps --user root \
  --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER \
  --entrypoint sh \
  --volume ../ocg-data-backup:/backup/data:ro \
  --volume ../ocg-browser-profiles-backup:/backup/browser-profiles:ro \
  ocg-manager \
  -c 'cp -a /backup/data/. /data/ && \
      cp -a /backup/browser-profiles/. /browser-profiles/ && \
      chown -R 10001:10001 /data /browser-profiles && \
      find /data /browser-profiles -type d -exec chmod 700 {} + && \
      find /data /browser-profiles -type f -exec chmod 600 {} +'
docker compose --profile browser up -d --no-build
docker compose ps
```

原部署如果使用了 `OCG_MANAGER_ENCRYPTION_KEY`，恢复前先把同一个秘密值写回 `.env`。在管理面板、账号和一次真实 Gateway 请求都验证通过前，请保留备份。

## 分运行方式的升级与卸载

应用内升级不可用时，按下面方式直接覆盖安装。

- **Windows GUI**：退出托盘程序并运行新版安装包，安装器会原地替换已有副本。从 Windows **已安装的应用** 卸载。只有在确认页勾选 **删除应用数据目录** 时才会删除 `%USERPROFILE%\.ocg-mgr`。卸载时未删除数据目录的，重装后会沿用原配置。
- **macOS GUI**：用新版 DMG 中的应用替换 **Applications** 里的旧应用。删除应用即可卸载；只有确定也要删除数据时才另行删除 `~/.ocg-mgr`。
- **Linux GUI**：用新版 `.deb` 覆盖安装，或替换 AppImage。卸载软件包或删除 AppImage 后，数据仍保留在 `~/.ocg-mgr`，除非手动删除。
- **CLI**：整体替换解压目录，保持可执行文件、`dist/` 与 `LICENSE` 同级。删除该目录即可卸载；数据仍保留在 `~/.ocg-mgr-cli` 或自定义 `--data-dir`。
- **Docker**：备份后依次执行 `docker compose pull` 和 `docker compose up -d --no-build`。如果启用了 browser profile，应改用 `docker compose --profile browser pull` 和 `docker compose --profile browser up -d --no-build`，确保两个镜像同步升级。生产部署建议把 `OCG_IMAGE` 与 `OCG_BROWSER_IMAGE` 固定到完整版本标签。`docker compose down` 只删容器、保留 `ocg-data` 与 `ocg-browser-profiles`；`docker compose down -v` 会永久删除这些卷，只能在确认双卷备份有效且确实要重置时使用。切换到旧镜像不等于回滚数据库；需要数据库回滚时，应同时恢复该旧版本升级前制作的完整备份。

---

[用户指南索引](../USER.zh-CN.md) · [English](upgrade-backup.md) · [文档索引](../README.zh-CN.md)
