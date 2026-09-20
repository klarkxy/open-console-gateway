import type {
  RoutingChannel,
  RoutingClientProtocol,
  RoutingExclusionCode,
  RoutingExplanationView,
  RoutingMode,
  RoutingResolvedKind,
  RuntimeOnlyUncertainty,
} from "../api/destinations.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Presentation mapping for the read-only `GET /routing/explain` prediction.
 * Domain code returns semantic codes; views resolve copy through these tables.
 */

export const ROUTING_MODE_KEYS = {
  "strict-priority": "严格优先级",
  "sticky-global": "全局粘性",
  "round-robin": "轮询",
} as const satisfies Record<RoutingMode, MessageKey>;

export const ROUTING_CHANNEL_KEYS = {
  go: "Go 通道",
  free: "Free 通道",
} as const satisfies Record<RoutingChannel, MessageKey>;

export const ROUTING_RESOLVED_KIND_KEYS = {
  alias: "Alias 映射",
  pinned_raw: "固定上游 ID",
} as const satisfies Record<RoutingResolvedKind, MessageKey>;

export const ROUTING_CLIENT_PROTOCOL_KEYS = {
  chat_completions: "Chat Completions",
  responses: "Responses",
  messages: "Messages",
  gemini: "Gemini",
} as const satisfies Record<RoutingClientProtocol, MessageKey>;

export const ROUTING_EXCLUSION_KEYS = {
  mapping_protocol_incompatible: "映射与请求协议不兼容",
  credential_disabled: "Key 已停用",
  binding_disabled: "绑定已停用",
  model_scope_denied: "模型范围不允许",
  goat_not_eligible: "GOAT 不满足资格条件",
  goat_unverified: "GOAT 未完成验证",
  candidate_materialization_failed: "候选构造失败",
  production_route_unsupported: "生产路由不支持",
  account_disabled: "账号已停用",
  setup_not_ready: "账号设置未完成",
  channel_mismatch: "通道不匹配",
  credential_missing: "缺少可用 Key",
  auth_error: "鉴权失败",
  cooling_down: "冷却中",
  free_channel_unavailable: "Free 通道不可用",
} as const satisfies Record<RoutingExclusionCode, MessageKey>;

export const ROUTING_UNCERTAINTY_KEYS = {
  state_changed_after_snapshot: "快照后运行状态可能已变化",
  conversation_binding_not_evaluated: "未评估对话绑定",
  retry_exclusions_not_applied: "未应用重试排除",
  credential_recheck_pending: "Key 状态待复核",
  upstream_result_unknown: "上游结果未知",
} as const satisfies Record<RuntimeOnlyUncertainty, MessageKey>;

export type RoutingExplanationState = "routeable" | "excluded" | "unresolved";

/**
 * Headline eligibility for one explained model: at least one eligible
 * candidate, only exclusions, or no resolved mapping at all.
 */
export function routingExplanationState(
  explanation: Pick<RoutingExplanationView, "eligible" | "resolved">,
): RoutingExplanationState {
  if (explanation.eligible.length > 0) return "routeable";
  if (explanation.resolved.mappings.length === 0) return "unresolved";
  return "excluded";
}

export const ROUTING_EXPLANATION_STATE_KEYS = {
  routeable: "当前可路由",
  excluded: "当前全部被排除",
  unresolved: "未解析到映射",
} as const satisfies Record<RoutingExplanationState, MessageKey>;

/**
 * Candidate display order mirrors routing order: ascending global routing
 * rank. Returns a copy; the explanation snapshot is never reordered in place.
 */
export function routingEligibleOrdered(
  explanation: Pick<RoutingExplanationView, "eligible">,
): RoutingExplanationView["eligible"] {
  return [...explanation.eligible].sort((left, right) => left.routing_rank - right.routing_rank);
}
