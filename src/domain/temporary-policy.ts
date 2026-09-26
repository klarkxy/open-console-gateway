import type { MessageKey } from "../i18n/index.ts";
import type {
  TemporaryPolicyBackoff,
  TemporaryPolicyBuiltin,
  TemporaryPolicyMatch,
  TemporaryPolicyRestriction,
  TemporaryPolicyRule,
} from "../api/generated/dashboard-v4.ts";

/**
 * Drafts, issue codes, and presentation maps. Wire shapes alias generated V4
 * DTOs; this module does not own a second HTTP schema.
 */

export const BUILTIN_GOAT_ID = "builtin.goat.credits_rejection";
export const MAX_CONFIGURED_RULES = 128;
export const MAX_ENABLED_PER_DESTINATION = 32;
export const MAX_MATCH_ALTERNATIVES = 32;
export const MAX_MATCH_STRING_BYTES = 256;
export const MIN_BACKOFF_SECONDS = 1;
export const MAX_BACKOFF_SECONDS = 86_400;
export const DEFAULT_INITIAL_SECONDS = 30;
export const DEFAULT_MAX_SECONDS = 300;
export const MIN_STATUS_CODE = 400;
export const MAX_STATUS_CODE = 599;

export type PolicyBackoff = TemporaryPolicyBackoff;
export type PolicyMatch = TemporaryPolicyMatch;
export type PolicyRule = TemporaryPolicyRule;
export type PolicyBuiltin = TemporaryPolicyBuiltin;
export type PolicyRestriction = TemporaryPolicyRestriction;
export type PolicyCustomRule = Extract<PolicyRule, { kind: "custom" }>;
export type PolicyBuiltinOverride = Extract<PolicyRule, { kind: "builtin_override" }>;
export type PolicyScope = PolicyCustomRule["scope"];
export type PolicyRestrictionSource = PolicyRestriction["source"];
export type PolicyRestrictionState = PolicyRestriction["state"];

export interface CustomRuleDraft {
  id: string;
  enabled: boolean;
  scope: PolicyScope | "";
  statusCodes: string;
  errorCodes: string;
  errorTypes: string;
  messageContains: string;
  initialSeconds: string;
  maxSeconds: string;
}

export type PolicyDraftIssue =
  | "missing_id"
  | "reserved_builtin_id"
  | "invalid_scope"
  | "empty_match"
  | "empty_status_codes"
  | "invalid_status_code"
  | "empty_error_codes"
  | "empty_error_types"
  | "empty_message_contains"
  | "too_many_alternatives"
  | "match_string_too_long"
  | "invalid_backoff_initial"
  | "invalid_backoff_max"
  | "backoff_max_lt_initial"
  | "duplicate_rule"
  | "too_many_rules"
  | "too_many_enabled_for_destination"
  | "unknown_builtin";

export type PolicyClientError = "conflict" | "load_failed" | "save_failed" | "clear_failed";
export type RestrictionEmptyCode = "no_local_waits";
export type RestrictionOrigin = "local" | "inherited";

export const TEMPORARY_POLICY_ISSUE_KEYS = {
  missing_id: "填写规则标识",
  reserved_builtin_id: "不能使用内置规则标识",
  invalid_scope: "选择范围",
  empty_match: "至少填写一项匹配条件",
  empty_status_codes: "状态码列表不能为空",
  invalid_status_code: "状态码须为 400–599 的整数",
  empty_error_codes: "error.code 列表不能为空",
  empty_error_types: "error.type 列表不能为空",
  empty_message_contains: "消息包含列表不能为空",
  too_many_alternatives: "同一字段最多 32 项",
  match_string_too_long: "匹配字符串最长 256 字节",
  invalid_backoff_initial: "初始退避须为 1–86400 秒",
  invalid_backoff_max: "最大退避须为 1–86400 秒",
  backoff_max_lt_initial: "最大退避不能小于初始退避",
  duplicate_rule: "同一作用范围下规则标识不能重复",
  too_many_rules: "最多 128 条配置规则",
  too_many_enabled_for_destination: "同一连接最多 32 条生效规则",
  unknown_builtin: "未知内置规则",
} as const satisfies Record<PolicyDraftIssue, MessageKey>;

export const TEMPORARY_POLICY_CLIENT_ERROR_KEYS = {
  conflict: "配置已由其他操作更新，已重新加载，请再保存。",
  load_failed: "临时停调加载失败",
  save_failed: "临时停调保存失败",
  clear_failed: "清除本地等待失败",
} as const satisfies Record<PolicyClientError, MessageKey>;

export const TEMPORARY_POLICY_RESTRICTION_STATE_KEYS = {
  waiting: "等待中",
  ready: "可探测",
  probing: "探测中",
} as const satisfies Record<PolicyRestrictionState, MessageKey>;

export const TEMPORARY_POLICY_RESTRICTION_SOURCE_KEYS = {
  global: "全局",
  connection: "连接覆盖",
  builtin: "内置",
} as const satisfies Record<PolicyRestrictionSource, MessageKey>;

export const TEMPORARY_POLICY_SCOPE_KEYS = {
  credential: "仅凭证",
  credential_model: "凭证与模型",
} as const satisfies Record<PolicyScope, MessageKey>;

export const TEMPORARY_POLICY_EMPTY_KEYS = {
  no_local_waits: "当前没有本地等待，这不表示上游健康。",
} as const satisfies Record<RestrictionEmptyCode, MessageKey>;

export const TEMPORARY_POLICY_BUILTIN_KEYS = {
  [BUILTIN_GOAT_ID]: "GOAT 额度拒绝",
} as const satisfies Record<typeof BUILTIN_GOAT_ID, MessageKey>;

export const TEMPORARY_POLICY_HINT_KEYS = {
  matcher: "跨字段为 AND，同字段为 OR。只匹配字面量，不使用脚本、正则或原文扫描。",
  local_wait: "倒计时是本地策略等待，不是上游额度重置。",
  clear_local: "清除本地等待，不发送测试请求。",
  builtin_sealed: "内置匹配器不可编辑，不能改为脚本、正则或原文扫描。",
} as const satisfies Record<"matcher" | "local_wait" | "clear_local" | "builtin_sealed", MessageKey>;

export function emptyCustomRuleDraft(): CustomRuleDraft {
  return {
    id: "",
    enabled: true,
    scope: "credential_model",
    statusCodes: "",
    errorCodes: "",
    errorTypes: "",
    messageContains: "",
    initialSeconds: String(DEFAULT_INITIAL_SECONDS),
    maxSeconds: String(DEFAULT_MAX_SECONDS),
  };
}

export function allocateCustomRuleId(unique: string): string {
  return `custom.${unique}`;
}

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

export function parseDelimitedList(raw: string): string[] {
  const seen = new Set<string>();
  const values: string[] = [];
  for (const part of raw.split(/[\n,]/)) {
    const item = part.trim();
    if (!item || seen.has(item)) continue;
    seen.add(item);
    values.push(item);
  }
  return values;
}

function optionalList(raw: string): { omitted: true } | { omitted: false; values: string[] } {
  if (raw === "") return { omitted: true };
  return { omitted: false, values: parseDelimitedList(raw) };
}

function parseBackoffSeconds(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const value = Number(trimmed);
  if (!Number.isInteger(value) || value < MIN_BACKOFF_SECONDS || value > MAX_BACKOFF_SECONDS) {
    return null;
  }
  return value;
}

function parseStatusCodes(values: readonly string[]): { ok: true; codes: number[] } | { ok: false } {
  const codes: number[] = [];
  const seen = new Set<number>();
  for (const value of values) {
    if (!/^\d+$/.test(value)) return { ok: false };
    const code = Number(value);
    if (!Number.isInteger(code) || code < MIN_STATUS_CODE || code > MAX_STATUS_CODE) {
      return { ok: false };
    }
    if (seen.has(code)) continue;
    seen.add(code);
    codes.push(code);
  }
  return { ok: true, codes };
}

function matchStringIssues(values: readonly string[]): PolicyDraftIssue[] {
  const issues: PolicyDraftIssue[] = [];
  if (values.length > MAX_MATCH_ALTERNATIVES) issues.push("too_many_alternatives");
  if (values.some((value) => utf8ByteLength(value) > MAX_MATCH_STRING_BYTES)) {
    issues.push("match_string_too_long");
  }
  return issues;
}

export function knownBuiltinIds(builtins: readonly Pick<PolicyBuiltin, "id">[]): string[] {
  const ids = new Set<string>([BUILTIN_GOAT_ID]);
  for (const builtin of builtins) ids.add(builtin.id);
  return [...ids];
}

export function parseCustomRuleDraft(
  draft: CustomRuleDraft,
  destinationId: string | null,
  builtins: readonly Pick<PolicyBuiltin, "id">[] = [],
): { ok: true; rule: PolicyCustomRule } | { ok: false; issues: PolicyDraftIssue[] } {
  const issues: PolicyDraftIssue[] = [];
  const id = draft.id.trim();
  if (!id) issues.push("missing_id");
  else if (knownBuiltinIds(builtins).includes(id)) issues.push("reserved_builtin_id");

  if (draft.scope !== "credential" && draft.scope !== "credential_model") {
    issues.push("invalid_scope");
  }

  const statusField = optionalList(draft.statusCodes);
  const errorCodeField = optionalList(draft.errorCodes);
  const errorTypeField = optionalList(draft.errorTypes);
  const messageField = optionalList(draft.messageContains);
  const match: PolicyMatch = {};

  if (!statusField.omitted) {
    if (statusField.values.length === 0) issues.push("empty_status_codes");
    else {
      const parsed = parseStatusCodes(statusField.values);
      if (!parsed.ok) issues.push("invalid_status_code");
      else {
        match.statusCodes = parsed.codes;
        issues.push(...matchStringIssues(statusField.values));
      }
    }
  }
  if (!errorCodeField.omitted) {
    if (errorCodeField.values.length === 0) issues.push("empty_error_codes");
    else {
      match.errorCodes = errorCodeField.values;
      issues.push(...matchStringIssues(errorCodeField.values));
    }
  }
  if (!errorTypeField.omitted) {
    if (errorTypeField.values.length === 0) issues.push("empty_error_types");
    else {
      match.errorTypes = errorTypeField.values;
      issues.push(...matchStringIssues(errorTypeField.values));
    }
  }
  if (!messageField.omitted) {
    if (messageField.values.length === 0) issues.push("empty_message_contains");
    else {
      match.messageContains = messageField.values;
      issues.push(...matchStringIssues(messageField.values));
    }
  }
  if (
    match.statusCodes === undefined
    && match.errorCodes === undefined
    && match.errorTypes === undefined
    && match.messageContains === undefined
    && statusField.omitted
    && errorCodeField.omitted
    && errorTypeField.omitted
    && messageField.omitted
  ) {
    issues.push("empty_match");
  }

  const initialSeconds = parseBackoffSeconds(draft.initialSeconds);
  const maxSeconds = parseBackoffSeconds(draft.maxSeconds);
  if (initialSeconds === null) issues.push("invalid_backoff_initial");
  if (maxSeconds === null) issues.push("invalid_backoff_max");
  if (initialSeconds !== null && maxSeconds !== null && maxSeconds < initialSeconds) {
    issues.push("backoff_max_lt_initial");
  }

  if (issues.length > 0 || draft.scope === "") {
    return { ok: false, issues: [...new Set(issues)] };
  }
  return {
    ok: true,
    rule: {
      kind: "custom",
      id,
      destinationId,
      enabled: draft.enabled,
      scope: draft.scope,
      match,
      backoff: { initialSeconds: initialSeconds!, maxSeconds: maxSeconds! },
    },
  };
}

function ruleKey(destinationId: string | null, id: string): string {
  return `${destinationId ?? ""}\0${id}`;
}

export function overlayRules(base: readonly PolicyRule[], overlay: readonly PolicyRule[]): PolicyRule[] {
  const mapped = new Map<string, PolicyRule>();
  for (const rule of base) mapped.set(rule.id, rule);
  for (const rule of overlay) mapped.set(rule.id, rule);
  return [...mapped.values()];
}

export function catalogAsRules(builtins: readonly PolicyBuiltin[]): PolicyBuiltinOverride[] {
  return builtins.map((builtin) => ({
    kind: "builtin_override" as const,
    id: builtin.id,
    destinationId: null,
    enabled: true,
    backoff: builtin.backoff,
  }));
}

export function rulesForDestination(rules: readonly PolicyRule[], destinationId: string | null): PolicyRule[] {
  return rules.filter((rule) => rule.destinationId === destinationId);
}

export function effectiveRules(
  rules: readonly PolicyRule[],
  builtins: readonly PolicyBuiltin[],
  destinationId: string | null,
): PolicyRule[] {
  const catalog = catalogAsRules(builtins.length > 0 ? builtins : [{
    id: BUILTIN_GOAT_ID,
    scope: "credential_model",
    backoff: { initialSeconds: DEFAULT_INITIAL_SECONDS, maxSeconds: DEFAULT_MAX_SECONDS },
  }]);
  const global = overlayRules(catalog, rulesForDestination(rules, null));
  if (destinationId === null) return global;
  return overlayRules(global, rulesForDestination(rules, destinationId));
}

export function enabledEffectiveCount(
  rules: readonly PolicyRule[],
  builtins: readonly PolicyBuiltin[],
  destinationId: string | null,
): number {
  return effectiveRules(rules, builtins, destinationId).filter((rule) => rule.enabled).length;
}

export function validateConfiguredRules(
  rules: readonly PolicyRule[],
  builtins: readonly PolicyBuiltin[],
  extraDestinationIds: readonly (string | null)[] = [],
): PolicyDraftIssue[] {
  const issues: PolicyDraftIssue[] = [];
  if (rules.length > MAX_CONFIGURED_RULES) issues.push("too_many_rules");
  const seen = new Set<string>();
  const builtinIds = knownBuiltinIds(builtins);
  for (const rule of rules) {
    const key = ruleKey(rule.destinationId, rule.id);
    if (seen.has(key)) issues.push("duplicate_rule");
    seen.add(key);
    if (rule.kind === "builtin_override" && !builtinIds.includes(rule.id)) {
      issues.push("unknown_builtin");
    }
  }
  const destinations = new Set<string | null>([null, ...extraDestinationIds]);
  for (const rule of rules) destinations.add(rule.destinationId);
  for (const destinationId of destinations) {
    if (enabledEffectiveCount(rules, builtins, destinationId) > MAX_ENABLED_PER_DESTINATION) {
      issues.push("too_many_enabled_for_destination");
    }
  }
  return [...new Set(issues)];
}

export function upsertRule(rules: readonly PolicyRule[], next: PolicyRule): PolicyRule[] {
  const key = ruleKey(next.destinationId, next.id);
  let replaced = false;
  const updated = rules.map((rule) => {
    if (ruleKey(rule.destinationId, rule.id) !== key) return rule;
    replaced = true;
    return next;
  });
  return replaced ? updated : [...updated, next];
}

export function removeRule(
  rules: readonly PolicyRule[],
  destinationId: string | null,
  id: string,
): PolicyRule[] {
  const key = ruleKey(destinationId, id);
  return rules.filter((rule) => ruleKey(rule.destinationId, rule.id) !== key);
}

export function upsertBuiltinOverride(
  rules: readonly PolicyRule[],
  destinationId: string | null,
  id: string,
  patch: { enabled: boolean; backoff?: PolicyBackoff | null },
): PolicyRule[] {
  return upsertRule(rules, {
    kind: "builtin_override",
    id,
    destinationId,
    enabled: patch.enabled,
    backoff: patch.backoff,
  });
}

export function localOverride(
  rules: readonly PolicyRule[],
  destinationId: string | null,
  id: string,
): PolicyRule | null {
  return rules.find((rule) => rule.destinationId === destinationId && rule.id === id) ?? null;
}

export function effectiveBuiltinEnabled(
  rules: readonly PolicyRule[],
  builtins: readonly PolicyBuiltin[],
  destinationId: string | null,
  id: string,
): boolean {
  const effective = effectiveRules(rules, builtins, destinationId)
    .find((rule) => rule.id === id);
  return effective?.enabled ?? true;
}

export interface VisibleCustomRule {
  rule: PolicyCustomRule;
  origin: RestrictionOrigin;
}

export function visibleCustomRules(
  rules: readonly PolicyRule[],
  destinationId: string | null,
): VisibleCustomRule[] {
  const local = rulesForDestination(rules, destinationId).filter(
    (rule): rule is PolicyCustomRule => rule.kind === "custom",
  );
  if (destinationId === null) {
    return local.map((rule) => ({ rule, origin: "local" as const }));
  }
  const replaced = new Set(local.map((rule) => rule.id));
  const inherited = rulesForDestination(rules, null).filter(
    (rule): rule is PolicyCustomRule => (
      rule.kind === "custom" && !replaced.has(rule.id)
    ),
  );
  return [
    ...inherited.map((rule) => ({ rule, origin: "inherited" as const })),
    ...local.map((rule) => ({ rule, origin: "local" as const })),
  ];
}

export function maskInheritedCustomRule(
  inherited: PolicyCustomRule,
  destinationId: string,
): PolicyCustomRule {
  return { ...inherited, destinationId, enabled: false };
}

export function restrictionTableEmpty(count: number): RestrictionEmptyCode | null {
  return count === 0 ? "no_local_waits" : null;
}

/** Empty copy is only for a successful diagnostics snapshot, never an unknown GET. */
export function restrictionSnapshotEmptyCode(
  loaded: boolean,
  count: number,
): RestrictionEmptyCode | null {
  if (!loaded) return null;
  return restrictionTableEmpty(count);
}

/** Local policy countdown from the last restrictions snapshot; not an upstream reset. */
export function remainingProbeSeconds(
  nextProbeInSeconds: number | null,
  observedAt: number,
  now: number,
): number {
  if (nextProbeInSeconds == null || !Number.isFinite(nextProbeInSeconds) || nextProbeInSeconds <= 0) return 0;
  if (!Number.isFinite(observedAt) || !Number.isFinite(now)) return 0;
  const elapsed = Math.max(0, Math.floor((now - observedAt) / 1000));
  return Math.max(0, Math.ceil(nextProbeInSeconds) - elapsed);
}

export function connectionDisplayName(
  destinationId: string | null | undefined,
  destinations: readonly { id: string; name: string }[],
): string | null {
  if (!destinationId) return null;
  return destinations.find((destination) => destination.id === destinationId)?.name ?? null;
}

export function credentialDisplayName(
  credentialId: string | null | undefined,
  credentials: readonly { id: string; name: string }[],
): string | null {
  if (!credentialId) return null;
  return credentials.find((credential) => credential.id === credentialId)?.name ?? null;
}

export function customRuleDraftFrom(rule: PolicyCustomRule): CustomRuleDraft {
  return {
    id: rule.id,
    enabled: rule.enabled,
    scope: rule.scope,
    statusCodes: rule.match.statusCodes?.join(", ") ?? "",
    errorCodes: rule.match.errorCodes?.join("\n") ?? "",
    errorTypes: rule.match.errorTypes?.join("\n") ?? "",
    messageContains: rule.match.messageContains?.join("\n") ?? "",
    initialSeconds: String(rule.backoff.initialSeconds),
    maxSeconds: String(rule.backoff.maxSeconds),
  };
}
