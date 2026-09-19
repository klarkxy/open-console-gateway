"""Synchronize only reviewed recovery documentation, preserving unrelated sections."""
from pathlib import Path
import os
import subprocess

BRANCH = 'maintainer/unified-restrictions'
assert os.environ['GITHUB_REPOSITORY'] == 'klarkxy/open-console-gateway'
assert os.environ['GITHUB_REF'] == 'refs/heads/' + BRANCH

def once(s, old, new):
    assert s.count(old) == 1, (old[:180], s.count(old))
    return s.replace(old, new, 1)

def section(s, start, end, text):
    assert s.count(start) == 1 and s.count(end) == 1, (start, end)
    a, b = s.index(start), s.index(end, s.index(start) + len(start))
    return s[:a] + text.rstrip() + '\n\n' + s[b:]

p = Path('docs/user/routing.md'); s = p.read_text()
s = section(s, '### GOAT transient 429 versus account exhaustion', '## Account Selection And Failover', '''### Upstream rejection, quota reset, and local recovery

The current source uses one failure-facts and recovery policy. Static service-specific decoders identify cause, affected resource, quota window, upstream reset, and rule evidence; they do not each own a retry state machine. An unknown or transient 429 without a usable retry constraint only excludes the account from this request. It does not invent a five-minute account cooldown, quota exhaustion, or a replacement for an otherwise available global sticky target. Custom/dynamic, MiniMax, Kimi, Ollama, and CPA generic HTTP errors cannot borrow Go or GOAT quota phrases.

`Retry-After` is a separate not-before constraint, not evidence of an empty account or a quota reset. Without a known quota scope it is enforced for the exact endpoint, upstream model, protocol, and route; swapping Keys does not bypass that same-resource wait. A recognized plan-window reset is stored independently. Both constraints must be satisfied, even when Retry-After is longer. Invalid values do not invent a deadline; valid zero or past values add no future delay. Unrepresentably distant numeric waits remain blocked locally rather than being shortened.

These are source behavior notes, not a claim that an installed binary contains this work. The historical [2.4.2 candidate](../releases/v2.4.2.md) does not include PR #73's cross-request recovery.''')
s = once(s, '''Keys that share a **declared quota pool** also share cooldown: exhausting the
pool through one Key blocks the others. Matching names do not create a shared
pool. Switching Keys on the same identity does not invent a fresh pool.''', '''Keys in a **declared quota pool** share restrictions only when decoded evidence identifies that pool. Known quota-window cooldown applies across its members. Insufficient-credit waiting is conservative and model-specific within the pool; success on another model does not certify recovery. Unknown status-only 429s do not prove shared-pool exhaustion. Matching names or URLs do not create a pool, and new independent credentials remain independent.''')
s = once(s, '''A `429` with a recognized `Resets in …` phrase writes `cooldown_until` and
the gateway tries the next account.''', '''Only the matching service decoder may interpret quota-window reset evidence and persist the corresponding cooldown. Recognized exhaustion with unknown recovery instead uses process-local waiting and foreground reprobes. Admission skips a waiting resource without sending upstream, then tries the next compatible account within the shared request budget.''')
s = section(s, 'The gateway does not replay `408`', '## Cost Accounting', '''The gateway does not replay `408`, `5xx`, post-connect send failures, or response-body timeouts. Ambiguous outcomes are reported as `upstream_outcome_unknown` and logged as `outcome_unknown`, because quota may already have been consumed. The existing narrow stream exception remains: an interrupted or incomplete stream before any downstream SSE output may retry the same account once, within the remaining budget. After output starts, no account replay or stream splicing is allowed.

All accounts and same-account retries share at most 32 attempt slots and one deadline. Locally rejected candidates also consume an attempt slot, but are not billed as upstream sends. Non-stream calls use the configured non-stream timeout; streams use the stream idle setting as their pre-output deadline. After stream handoff, the normal idle timer applies, so a healthy long stream can outlive the non-stream timeout. Known upstream error statuses are retained even if the bounded error-body read times out. A budget exhausted before send returns 503 without an additional request.

When no candidate can be used, a known persisted quota cooldown can produce 429 with its reset time. With only local waits and no known quota reset, the gateway returns 503 rather than manufacturing `resets_at`. Recovery waits are visible in request diagnostics as `resource_wait`, with no upstream status or cost for a skipped send.''')
s = once(s, '''calibration or official refresh. A real inference `429` only writes an
  independent cooldown and affects account selection; it does not rewrite the
  usage baseline, but it does schedule a later official reconciliation.''', '''calibration or official refresh. A real inference `429` is decoded into scoped restriction facts; it does not rewrite the usage baseline. Eligible OpenCode Go inference 429s additionally schedule the existing later official reconciliation, not an inline fetch.''')
s = once(s, '''  the request while the gateway lost the response; the request is not
  retried and its local cost stays unknown.''', '''  the request while the gateway lost the response; its local cost stays unknown. The only stream retry exception is the bounded pre-output case described above.''')
s = section(s, '## True And False Circuit Breakers', '## Zen Free models', '''## What Actually Stops Traffic

**A full local estimate is not a routing prohibition.** The gateway keeps using that account unless a separate authorization or upstream restriction prevents it. Usage calibration is not recovery verification and does not clear local recovery waiting.

**A known upstream quota reset is persistent evidence.** A recognized window and valid reset write the existing matching cooldown without changing the usage baseline. Its dashboard bar is forced to 100% until the persisted cooldown expires or is explicitly reset. Unknown 429s no longer get an invented five-minute cooldown.

**Known exhaustion without a reset is local waiting.** The first eligible retry is scheduled around 30 seconds later, with small jitter. Repeated failed probes back off exponentially, capped at 300 seconds. This is permission to check, not a promise that upstream quota has recovered. Only a real client request performs the single-flight probe; there is no new background or synthetic paid request. Other requests skip the waiting or probing resource. A complete protocol-valid response confirms only its leased restriction; malformed 200 responses, cancellation, and incomplete streams do not.

Local waiting clears on process restart; already stored quota deadlines remain. A credential/binding generation change prevents old credit state from attaching to a replacement Key. The existing CAS cooldown-reset operation also clears associated local waits and fences late replies, but an account reset does not reset the anonymous Free pool. No new waiting badge or recovery button is added in this change; historical generic cooldown rows are not automatically rewritten.''')
s = once(s, '''them in. Its promo quota is shared per egress IP, so a Free `429` cools the
whole Free channel and does not rotate keys. Routing then continues to later
compatible account cards in saved order; a Free-only model returns the shared
cooldown.''', '''them in. Its promo quota is shared per egress IP, so a Free `429` restricts the anonymous Free channel rather than rotating Keys. A known reset is persisted; unknown recovery uses the same local single-flight waiting policy. Routing continues to later compatible cards in saved order. A Free-only request with no known reset returns local unavailability rather than a fictitious quota deadline.''')
s = section(s, '### GOAT credit rejection without a reset time', '---\n', '''### GOAT credit rejection without a reset time

An exact GOAT `400` / `BAD_REQUEST` / `invalid_request_error` declaring insufficient credits is a resource rejection, not a malformed prompt. It enters the common model-specific quota-pool waiting path and the current request tries another eligible account. New requests skip that resource until one foreground reprobe is admitted; a healthy sticky A with a transient 429 can still return on the next request while a credit-exhausted H is skipped. Logs retain H's real 400 for the send and `resource_wait` for subsequent local skips. No account disablement, authentication error, or invented quota reset is written. Ordinary 400s (context, model, reasoning validation), 413s, and lookalike messages from another service do not gain retry permission.''')
p.write_text(s)

p = Path('docs/user/routing.zh-CN.md'); s = p.read_text()
s = section(s, '### GOAT 临时 429 与账号额度耗尽', '## 账号选择与切换', '''### 上游拒绝、额度重置与本地恢复

当前源码使用统一的错误事实与恢复策略。静态供应商解码器负责识别原因、影响资源、额度窗口、上游重置时刻和规则证据，不各自维护重试状态机。没有有效重试约束的未知或临时 429，只在本次请求排除账号，不编造五分钟账号冷却、额度耗尽，也不替换原本仍可用的全局粘性目标。Custom/动态供应商、MiniMax、Kimi、Ollama、CPA 的通用 HTTP 错误不会套用 Go 或 GOAT 的额度文案规则。

`Retry-After` 是独立的最早重试约束，不是余额耗尽或额度重置的证明。没有明确额度范围时，它约束精确端点、上游模型、协议与出站路由；换 Key 不会绕过同一资源的等待。已识别的套餐窗口重置单独持久化，两项约束必须同时满足，即使 Retry-After 更长也不能提前重试。无效值不产生截止时间；合法的零值或过去日期不增加未来等待；无法表示的超长数字等待保留为本地阻止状态，而不是截短。

这里描述的是当前源码，不代表已安装二进制包含该能力。历史 [2.4.2 候选包](../releases/v2.4.2.zh-CN.md)不包含 PR #73 的跨请求恢复。''')
s = once(s, '共用**已声明额度池**的 Key 也共用冷却：用其中一把 Key 把池耗尽，其他成员同样不可用。名称相同不会自动变成共享池。在同一身份上换 Key 也不会另起一个新池。', '只有解码证据指向**已声明额度池**时，限制才在池成员间共享。已知额度窗口冷却作用于成员；余额不足等待则保守地限定为池内同一模型，其他模型成功不能证明它已恢复。仅有未知 429 不能证明共享池耗尽。名称或 URL 相同不建立共享关系，新增独立凭据仍保持独立。')
s = once(s, '带有可识别 `Resets in …` 时间短语的 `429` 写入 `cooldown_until`，然后尝试下一个账号。', '只有相应供应商解码器才能解释额度窗口及重置证据，并写入匹配冷却。已确认耗尽但恢复未知时改为进程内等待和前台重探。准入会在不发送上游请求的情况下跳过等待资源，再在统一预算内尝试其他兼容账号。')
s = section(s, '`408`、`5xx`、建连后的传输失败', '## 用量估算', '''`408`、`5xx`、建连后的发送失败和响应体超时不会重放。无法确认上游结果时，以 `upstream_outcome_unknown` 返回并记为 `outcome_unknown`，因为请求可能已经消耗额度。保留原有的狭窄流式例外：尚未向下游输出任何 SSE 数据时，中断或不完整流可在剩余预算内对同一账号重试一次；已经输出后，禁止换号重放或拼接流。

不同账号和同账号重试共用最多 32 个尝试位置及同一个截止时间。本地拒绝的候选同样占用尝试位置，但不算上游发送或费用。非流式使用非流式超时；流式以空闲超时设置作为首输出前的总时限。流交付下游后仍使用原有空闲计时，因此正常长流可以超过非流式超时。已经收到的上游错误状态不会因为限时读取错误正文而丢失；发送前预算耗尽返回 503，不追加请求。

没有候选可用时，已知持久化额度冷却可以返回带重置时刻的 429；如果只有本地等待、没有已知额度重置，则返回 503，不虚构 `resets_at`。本地跳过在请求诊断中记为 `resource_wait`，上游状态与费用均为空。''')
s = once(s, '真实推理 `429` 只写入独立冷却并影响选择器，不重写用量基线，但会调度稍后的官方对账。', '真实推理 `429` 被解码为带范围的限制事实，不重写用量基线；合格 OpenCode Go 推理 429 还会调度既有的稍后官方对账，不在当前请求内拉取。')
s = once(s, '这类请求不会自动重试，本地额度消耗保持未知。', '本地额度消耗保持未知；唯一的流式重试例外是前文描述的有限首输出前重试。')
s = section(s, '## 真熔断与假熔断', '## Zen Free 模型', '''## 什么真正阻止流量

**本地估算满格不是路由禁令。** 除非另有鉴权或上游限制，Gateway 仍会使用账号。用量校准也不是恢复验证，不会清除本地等待。

**上游明确额度重置属于持久化证据。** 只有已识别窗口及有效重置时间才写匹配冷却，不改用量基线。面板对应窗口在冷却到期或显式解除前强制显示 100%。未知 429 不再被默认写成五分钟冷却。

**已知耗尽、恢复未知时进入本地等待。** 首次约 30 秒后允许重探，并带小幅抖动；失败重探按指数退避，最长 300 秒。这是允许检查的时间，不是上游承诺恢复的时间。只有真实客户端请求承担单飞重探，不新增后台或合成收费请求；其他请求跳过等待中或探测中的资源。完整且协议有效的响应只确认它实际持有的限制租约；畸形 200、取消和不完整流都不能宣告恢复。

本地等待在进程重启后清空，已持久化的额度截止时刻仍保留。凭据或绑定代际变更防止旧余额状态附到新 Key 上。既有 CAS 解除冷却操作还会清除关联本地等待，并隔离晚到响应；账号级解除不会重置匿名 Free 池。本次未增加等待状态标签或恢复按钮，也不会自动改写历史通用冷却记录。''')
s = once(s, '促销额度按出口 IP 共享； Free `429` 会冷却整条 Free 通道，不换 Key，并继续尝试账号顺序中后续兼容卡片；只有 Free 映射的模型则返回共享冷却。', '促销额度按出口 IP 共享；Free `429` 限制匿名 Free 通道，而不是切换 Key。已知重置会持久化，恢复未知则进入同一套本地单飞等待。路由继续尝试顺序中后续兼容卡片；仅有 Free 映射且没有已知重置时，返回本地不可用，而不是虚构额度截止时间。')
s = section(s, '### GOAT 未给重置时间的余额不足', '---\n', '''### GOAT 未给重置时间的余额不足

GOAT 精确匹配的 `400` / `BAD_REQUEST` / `invalid_request_error` 余额不足声明属于资源拒绝，不是提示词格式错误。它进入统一的池内同模型等待路径，当前请求继续尝试其他合格账号。新请求会跳过该资源，直到允许一次前台重探；因此正常粘性 A 遇到临时 429 后，下次仍可回到 A，而余额耗尽的 H 在等待期间被跳过。日志保留 H 真实发送的 400，后续本地跳过记录为 `resource_wait`。不会禁用账号、写鉴权错误或编造额度重置。上下文、模型、reasoning 参数等普通 400、413，以及其他服务的相似文案均不会获得重试权限。''')
p.write_text(s)

p = Path('docs/user/accounts.md'); s = p.read_text()
s = once(s, '''  still writes the existing cooldown/selector state and additionally schedules''', '''  follows the common scoped restriction policy and additionally schedules''')
s = section(s, '- **GOAT inference cooldown.**', '- **Identity and credentials.**', '''- **GOAT inference restrictions.** A recognized 5-hour, weekly, or monthly plan reset writes the exact matching cooldown. `Retry-After` is enforced independently, never substituted for a quota reset. Unknown/transient 429s without a retry constraint are request-local. Exact insufficient-credit 400s enter model-specific waiting for the declared quota pool and foreground single-flight reprobes; this does not disable the Key or create an authentication error. See [routing and recovery](routing.md).''')
s = once(s, '''- **Cooldown reset.** You can reset a cooldown manually from this view. The bar
  snaps back to its local estimate as soon as the cooldown is cleared.''', '''- **Cooldown reset.** The existing visible cooldown action restores the bar to its local estimate. Its CAS backend operation also clears associated process-local waiting and fences late replies. Account reset does not reset anonymous Free restrictions. This change adds no new waiting badge or button for resources that have only local waiting; inspect `resource_wait` request diagnostics instead. Restart clears local waiting, not persisted quota deadlines, and no historical generic cooldown rows are migrated.''')
p.write_text(s)

p = Path('docs/user/accounts.zh-CN.md'); s = p.read_text()
s = once(s, '  仍写入现有冷却/选择器状态，并额外在约 1–2 分钟后调度一次官方对账；官方失败或', '  按统一的范围限制策略处理，并额外在约 1–2 分钟后调度一次官方对账；官方失败或')
s = section(s, '- **GOAT 推理冷却**', '- **标识与凭据**', '''- **GOAT 推理限制**：已识别的 5 小时、周或月套餐重置写入精确匹配的冷却。`Retry-After` 独立约束重试，不替代额度重置。没有重试约束的未知或临时 429 只影响本次请求；精确匹配的余额不足 400 则进入已声明额度池内同模型等待和前台单飞重探，不停用 Key，也不写鉴权错误。详见[路由与恢复](routing.zh-CN.md)。''')
s = once(s, '- **解除冷却**：冷却也可以在这个视图手动解除。解除后，进度条会立刻回到本地估算值。', '- **解除冷却**：现有可见的冷却操作会让进度条恢复本地估算值；其 CAS 后端操作还会清除关联进程内等待，并隔离晚到响应。账号级解除不重置匿名 Free 限制。本次没有为仅本地等待的资源新增标签或按钮，可查看请求诊断中的 `resource_wait`。重启清空本地等待，但保留持久化额度截止时刻；历史通用冷却记录不会迁移。')
p.write_text(s)

p = Path('docs/maintainer/runtime-invariants.md'); s = p.read_text()
s = once(s, 'A cooldown/429 written for one member fans out to the other members.', 'A recognized pool quota-window cooldown fans out to members; model-specific credit waiting also uses declared membership. A bare unknown 429 does not establish pool exhaustion.')
s = once(s, 'generic provider cooldown on 429, and unpriced forwarding.', 'the generic HTTP error profile with common scoped recovery on 429, and unpriced forwarding.')
s = once(s, 'Actual upstream 429 keeps the existing generic cooldown/fallback.', 'Actual upstream 429 uses the generic HTTP failure profile and common scoped recovery; no fixed account cooldown is invented, and a header-only retry constraint belongs to the endpoint/model.')
s = once(s, 'Real inference 429s still write the existing cooldown/selector and additionally schedule an official reconciliation ~1–2 minutes later (not inline);', 'Real inference 429s follow the common scoped restriction policy and additionally schedule an official reconciliation ~1–2 minutes later (not inline);')
s = once(s, 'after a 429 the entire Free channel cools down without swapping Keys, and routing continues trying subsequent compatible cards, returning shared cooldown only when only Free candidates remain.', 'a 429 restricts the anonymous Free channel without swapping Keys. Known reset evidence persists the Free cooldown; unknown recovery uses local single-flight waiting. Routing continues to later compatible cards; a Free-only local wait without a known reset yields 503 rather than a fabricated quota deadline.')
a = s.index('A real GOAT inference `429` remains the routing authority:')
b = s.index('\n', a)
s = s[:a] + 'A recognized GOAT inference plan-window reset persists the exact 5-hour, weekly, or monthly deadline. Retry-After is a separate constraint, not a replacement or proof of account exhaustion. Unknown/transient 429s without a retry constraint stay request-local; exact insufficient-credit 400s use the common model-specific quota-pool waiting policy described below. Usage refresh does not certify recovery or clear local waiting.' + s[b:]
s = once(s, '## Zen Free\n', '''## Failure Facts, Recovery, And Request Budgets

- Static error profiles only decode bounded upstream evidence into `FailureFacts` (cause, scope, window, upstream reset, retry constraint, rule id/version). `failure.rs` owns the common decision. `recovery.rs` owns process-local admission, not the selector or usage ledger. Generic/unknown 429s do not invent five-minute cooldown; provider lookalike phrases outside the matching dialect or error-message field are not quota evidence.
- `Retry-After` accepts delay-seconds and supported HTTP dates. It never overwrites a quota-window reset; both constraints apply. Zero/past means no extra future wait. Valid but unrepresentably large delays stay `Unbounded`, not capped into an early retry. Endpoint constraints include exact upstream URL, model, protocol, route and an explicit proxy's identity, and are shared across Keys to that resource. Known quota windows use the explicit pool generation; credit waiting is additionally model-specific. Anonymous Free uses the existing process-wide shared-egress contract, not account-owned Key quota.
- Waiting begins around 30 seconds with small jitter and failed probes back off exponentially to 300 seconds. Only an arriving real client request performs a due single-flight probe; no background timer emits paid inference. Healthy requests remain concurrent. The map is bounded at 4096 resource slots and fails locally on capacity instead of discarding active evidence. `resource_wait` logs carry no upstream HTTP status or cost. All locally waiting candidates with no known quota reset return 503, not a fabricated reset time.
- A complete protocol-valid success can clear only its leased probe at the captured restriction revision. Invalid JSON/200, incomplete streams, unrelated errors and cancellation do not certify recovery. Late observations are checked against live authorization, explicit pool members and credential/binding generation; operator reset fences in-flight replies. The existing CAS account cooldown reset clears associated local resources but not anonymous Free. Restart clears local waits while persisted quota deadlines survive. No V3/V4 schema, waiting-state UI or historical cooldown migration is introduced.
- One logical request shares 32 attempt slots and one deadline across account fallback and same-account retries. Local skips consume a slot without sending. Non-stream uses `non_stream_timeout_secs`; stream pre-output uses `stream_idle_timeout_secs`. The deadline is applied inside send/body/pre-output phases so actual account and known HTTP status survive timeout finalization. Partial SSE frames cannot refresh the pre-output deadline indefinitely. After downstream handoff, the existing idle timeout applies, not a non-stream lifetime cap. No 408/5xx or ambiguous send/body replay is added. The existing incomplete/interrupted pre-output stream exception remains at most one same-account retry within budget; after output starts there is no replay or cross-account splicing.
- The public regressions in `gateway_resource_recovery.rs`, `gateway_request_budget.rs`, `custom_trusted_admin.rs` and `v3_runtime_invariants.rs` assert real call sequences, per-attempt logs, long-stream compatibility, reset/rotation, shared-pool isolation and deadlines. Clock sampling counts are not selection counts: recovery admission/observation also reads clocks. The two-request round-robin test proves a safe same-account retry does not advance selection.

## Zen Free
''')
p.write_text(s)

p = Path('docs/maintainer/runtime-invariants.zh-CN.md'); s = p.read_text()
s = once(s, '对一名成员写入的冷却/429 会扇出到其他成员。', '已识别的池额度窗口冷却会传播到成员；按模型隔离的余额等待也使用已声明成员关系。仅有未知 429 不证明共享池耗尽。')
s = once(s, '429 通用供应商冷却，以及 unpriced 转发。', '429 通用 HTTP 错误配置及统一范围恢复策略，以及 unpriced 转发。')
s = once(s, '真实上游 429 仍走既有通用冷却/回退。', '真实上游 429 使用通用 HTTP 错误配置与统一范围恢复策略，不编造固定账号冷却；只有响应头的重试约束属于端点/模型。')
s = once(s, '真实推理 429 仍写入现有冷却/选择器，并额外安排约 1–2 分钟后官方对账（非内联）；', '真实推理 429 按统一范围限制策略处理，并额外安排约 1–2 分钟后官方对账（非内联）；')
s = once(s, '收到 429 后整个 Free 通道冷却，不切换 Key，路由继续尝试后续兼容卡片，仅当只剩 Free 候选者时才返回共享冷却。', '429 限制匿名 Free 通道而不切换 Key。已知重置证据持久化为 Free 冷却，恢复未知则进入本地单飞等待。路由继续尝试后续兼容卡片；仅有 Free 本地等待且无已知重置时返回 503，不虚构额度截止时间。')
a = s.index('真实 GOAT 推理 `429` 仍是路由冷却的权威来源：')
b = s.index('\n', a)
s = s[:a] + '已识别的 GOAT 推理套餐窗口重置会持久化精确的 5 小时、周或月截止时间。Retry-After 是单独约束，不替代额度重置，也不证明账号耗尽。没有重试约束的未知或临时 429 保持请求内影响；精确匹配的余额不足 400 使用下文的统一池内同模型等待策略。用量刷新不证明恢复，也不清除本地等待。' + s[b:]
s = once(s, '## Zen Free\n', '''## 错误事实、资源恢复与请求预算

- 静态错误配置只把受大小限制的上游证据解码为 `FailureFacts`：原因、范围、窗口、上游重置、重试约束、规则 id/版本。`failure.rs` 统一决策，`recovery.rs` 管理进程内准入，不代替选择器或用量账本。通用/未知 429 不再编造五分钟冷却；不属于相应供应商或错误消息字段的相似文案不是额度证据。
- `Retry-After` 支持延迟秒数和支持的 HTTP 日期，不覆盖额度窗口重置，两项约束同时适用。零值或过去时刻不增加未来等待；有效但无法表示的超长延迟保留为 `Unbounded`，不会截短为提前重试。端点限制包含精确上游 URL、模型、协议、路由和显式代理身份，同资源的不同 Key 共用约束。已知额度窗口使用显式池代际，余额等待额外按模型隔离。匿名 Free 使用既有进程级共享出口合约，不属于账号 Key 额度。
- 等待从约 30 秒及小幅抖动开始，失败重探指数退避至最长 300 秒。只有到来的真实客户端请求承担到期单飞重探，没有后台计时器发送收费推理。健康资源仍支持并发。映射最多 4096 个资源位置；容量不足本地拒绝，不丢弃活跃限制证据。`resource_wait` 日志没有上游 HTTP 状态或费用；全部本地等待且无已知额度重置时返回 503，不虚构恢复时间。
- 完整且协议有效的成功，只能清除所持探测租约及匹配限制版本。无效 JSON/200、不完整流、无关错误、取消均不证明恢复。晚到失败观察须核对实时授权、显式池成员和凭据/绑定代际；管理员重置隔离在途响应。既有 CAS 账号解除冷却会清除关联本地资源，但不重置匿名 Free。重启清空本地等待，持久化额度截止时间保留。本次不引入 V3/V4 schema 变更、等待状态 UI 或历史冷却迁移。
- 一个逻辑请求的不同账号回退及同账号重试共用 32 个尝试位置与一个截止时间；本地跳过占位置但不发送。非流式使用 `non_stream_timeout_secs`，流式首输出前使用 `stream_idle_timeout_secs`。截止时间下沉到发送、正文读取和首输出阶段，确保超时日志保留实际账号和已知 HTTP 状态。碎片 SSE 帧不能无限刷新首输出期限；交付下游后仍按空闲超时处理，不受非流式总时限截断。不增加 408/5xx 或发送/正文结果未知时的重放；保留既有中断/不完整流首输出前至多一次同账号重试，且受剩余预算约束，输出后不重放或跨账号拼接。
- `gateway_resource_recovery.rs`、`gateway_request_budget.rs`、`custom_trusted_admin.rs`、`v3_runtime_invariants.rs` 通过公开入口验证真实调用序列、逐尝试日志、长流兼容、重置/轮换、共享池隔离及截止时间。时钟读取次数不等于选择次数，恢复准入和观察也会采样。两次逻辑请求的轮询回归证明安全同账号重试不会推进选择器。

## Zen Free
''')
p.write_text(s)

# These notes describe the already built candidate, never the new PR source.
for path, marker, notice in [
    ('docs/releases/v2.4.2.md', '## Validation and packages', 'PR [#73](https://github.com/klarkxy/open-console-gateway/pull/73) adds the later source-level unified recovery implementation. It is not part of the candidate commit named above; these notes retain that candidate\'s request-local behavior and release boundary.\n\n'),
    ('docs/releases/v2.4.2.zh-CN.md', '## 验证与构建产物', '后续 PR [#73](https://github.com/klarkxy/open-console-gateway/pull/73) 在源码中加入统一恢复实现，但不属于前述已构建候选提交。本文仍描述该候选包的请求内行为和发布边界。\n\n'),
]:
    p = Path(path); p.write_text(once(p.read_text(), marker, notice + marker))

subprocess.run(['git', 'diff', '--check'], check=True)
for path in ['.workbench/recovery-docs.py', '.github/workflows/recovery-docs-validation.yml']:
    Path(path).unlink()
subprocess.run(['git', 'add', '-A'], check=True)
subprocess.run(['git', 'diff', '--cached', '--stat'], check=True)
subprocess.run(['git', '-c', 'user.name=github-actions[bot]', '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com', 'commit', '-m', 'docs(gateway): document unified recovery and preserve candidate release boundaries'], check=True)
subprocess.run(['git', 'push', 'origin', 'HEAD:refs/heads/' + BRANCH], check=True)
sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
with open(os.environ['GITHUB_OUTPUT'], 'a') as out: out.write('sha=' + sha + '\n')
print('SOURCE_COMMIT=' + sha)
