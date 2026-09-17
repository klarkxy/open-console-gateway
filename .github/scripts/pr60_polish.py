"""Finish targeted resolutions on the prepared maintainer branch."""
from pathlib import Path
import subprocess

def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f'Unexpected resolution context in {path}: {old[:120]}')
    p.write_text(text.replace(old, new, 1))

def replace_range(path, start, end, replacement):
    p = Path(path)
    text = p.read_text()
    if text.count(start) != 1:
        raise RuntimeError(f'Unexpected paragraph in {path}')
    a = text.index(start)
    b = text.index(end, a)
    p.write_text(text[:a] + replacement + text[b:])

replace('crates/ocg-core/tests/dashboard_v3_usage.rs',
        'assert_eq!(provider["availability"], "local_state");',
        'assert_eq!(provider["availability"], "available");')
replace_range('docs/user/accounts.md', 'GOAT cards show a clearly\n', 'Paid Ollama Cloud cards',
    'GOAT cards offer **Refresh quota** to calibrate the `$14 / $35 / $70`\n'
    'windows from Command Code first-party account usage. The endpoint is used\n'
    'by the official CLI but is not documented in the public Provider API.\n'
    'Between snapshots, priced OCG request logs continue accumulating locally;\n'
    'manual baseline correction remains available. The monthly reset still\n'
    'uses the configured purchase date, not an upstream monthly-reset timestamp.\n')
replace_range('docs/user/accounts.zh-CN.md', 'GOAT 卡片显示的是明确标注的本地估算：\n', '付费档 Ollama Cloud 卡片',
    'GOAT 卡片可通过 **刷新额度** 从 Command Code 第一方账号用量校准 `$14 / $35 / $70` 三个窗口。'
    '该端点由官方 CLI 使用，但未列入公开 Provider API 文档。两次快照之间继续本地累计 OCG 内已定价请求日志，'
    '并保留手工校准。月窗口重置时间仍由已配置的购买日期推导，不是上游返回的月重置时间。')
replace_range('docs/maintainer/runtime-invariants.md',
    'Command Code exposes no machine-readable account-usage endpoint,',
    'All persistence mutation paths still reject',
    'The fixed first-party `/alpha/billing/credits` endpoint used by the official CLI supplies manual calibration evidence; it is not documented in the public Provider API. '
    'GOAT catalog usage availability is `available`, with manual refresh and local correction enabled, but automatic sync and authoritative quota gating remain disabled. '
    'Explicit refresh validates the `$14 / $35 / $70` plan and atomically calibrates the local windows. Priced OCG logs accumulate after calibration; the monthly reset remains purchase-date-derived. '
    'Neither official usage failure nor displayed fullness writes inference cooldown or changes routing. ')
replace_range('docs/maintainer/runtime-invariants.zh-CN.md',
    'Command Code 没有可机读的账号用量端点，',
    '所有持久化变更路径仍会',
    '官方 CLI 使用的固定第一方 `/alpha/billing/credits` 端点提供手工校准证据，但公开 Provider API 文档未列出。'
    'GOAT 目录的用量可用性为 `available`，支持显式官方刷新与本地手工校准，但自动同步和权威额度路由限制仍关闭。'
    '刷新会校验 `$14 / $35 / $70` 套餐并原子校准本地三个窗口，随后继续累计 OCG 内已定价日志；月重置时间仍由购买日期推导。'
    '官方用量失败或显示用量已满都不会写入推理冷却或改变路由。')
replace('docs/maintainer/runtime-invariants.md',
    'Command Code GOAT dispatches the same V3 refresh route to the isolated',
    'Command Code GOAT dispatches both V3 `POST /accounts/{id}/provider-usage` and `POST /accounts/{id}/usage/refresh` to the same isolated')
replace('docs/maintainer/runtime-invariants.zh-CN.md',
    'Command Code GOAT 将同一个 V3 刷新路由分派到独立的',
    'Command Code GOAT 将 V3 `POST /accounts/{id}/provider-usage` 和 `POST /accounts/{id}/usage/refresh` 都分派到同一份独立的')

subprocess.run(['cargo', 'fmt', '--all'], check=True)
subprocess.run(['git', 'add', '-A'], check=True)
subprocess.run(['git', 'diff', '--cached', '--check'], check=True)
subprocess.run(['git', 'commit', '-m', 'test(goat): align catalog regression and current-main documentation'], check=True)
print(subprocess.check_output(['git', 'show', '--stat', '--oneline', 'HEAD'], text=True))
