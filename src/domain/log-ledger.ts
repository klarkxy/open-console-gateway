import type { MessageKey } from "../i18n/index.ts";
import type {
  OperationLogQuery,
  OperationOutcome,
  OperationSource,
  RequestLogQuery,
  RequestLogSummary,
} from "../api/log-ledger-types.ts";

/**
 * Semantic label keys. Callers translate the key. Tests lock the mapping,
 * not the rendered sentence.
 */
export const OPERATION_OUTCOME_KEYS = {
  pending: "尚无最终回执",
  success: "成功",
  rejected: "已拒绝",
  failed: "失败",
  partial: "部分完成",
  compensated: "已补偿",
} as const satisfies Record<OperationOutcome, MessageKey>;

export const OPERATION_SOURCE_KEYS = {
  dashboard: "来自控制台",
  cli: "来自命令行",
  desktop: "来自桌面应用",
} as const satisfies Record<OperationSource, MessageKey>;

export const OPERATION_ACTION_KEYS = {
  "account.create": "创建账号",
  "account.update": "更新账号",
  "account.delete": "删除账号",
  "account.reorder": "调整账号顺序",
  "account.toggle": "切换账号开关",
  "key.create": "创建接入 Key",
  "key.update": "更新接入 Key",
  "key.delete": "删除接入 Key",
  "key.regenerate": "重新生成接入 Key",
  "settings.update": "更新设置",
  "account.create_managed": "创建托管账号",
  "account.browser.open": "打开账号浏览器",
  "account.browser.reset": "重置账号浏览器",
  "account.custom_config": "更新自定义账号配置",
  "account.key.verify": "验证账号 Key",
  "account.model.test": "测试账号模型",
  "account.model_capabilities": "更新账号模型能力",
  "account.provider.usage.refresh": "刷新供应商用量",
  "account.reset_cooldown": "重置账号冷却",
  "account.setup": "更新账号配置步骤",
  "account.transfer.export": "导出账号",
  "account.transfer.import": "导入账号",
  "account.transfer.preview": "预览账号导入",
  "account.usage.refresh": "刷新账号用量",
  "account.usage.update": "更新账号用量",
  "account.verify": "验证账号",
  "account.enable": "启用账号",
  "account.disable": "停用账号",
  "account.ping": "测试账号连接",
  "application.configure": "配置应用",
  "application.install": "安装应用",
  "application.recover": "恢复应用配置",
  "application.remove": "移除应用配置",
  "application.uninstall": "卸载应用",
  "auth.login": "登录控制台",
  "auth.logout": "退出控制台",
  "auth.register": "设置控制台登录",
  "balance.refresh": "刷新余额",
  "billing.calibrate": "校准额度",
  "billing.configure": "配置额度",
  "billing.disable": "停用额度",
  "billing.grant": "增加额度",
  "binding.update": "更新凭证绑定",
  "credential.create": "创建凭证",
  "credential.quota.retry": "重试凭证额度恢复",
  "credential.rotate": "轮换凭证",
  "destination.delete": "删除目的地",
  "destination.update": "更新目的地",
  "gateway.start": "启动网关",
  "gateway.stop": "停止网关",
  "metadata.update": "更新模型元数据",
  "onboarding.commit": "保存连接与凭证",
  "policy.clear": "清除临时策略",
  "policy.update": "更新临时策略",
  "publication.update": "更新模型发布",
  "routing.replace": "更新路由规则",
  "settings.proxy.test": "测试代理连接",
  "skill.sync": "同步 OCG 技能",
  "app.update": "升级桌面应用",
  "desktop.update.install": "升级桌面应用",
  "desktop.update.check": "检查应用更新",
  "catalog.add": "添加模型目录",
  "catalog.edit": "编辑模型目录",
  "catalog.refresh": "刷新模型目录",
  "catalog.remove": "移除模型目录",
  "catalog.test": "测试模型目录",
  "catalog.update": "更新模型目录",
  "platform.create": "创建平台账号",
  "platform.delete": "删除平台账号",
  "platform.import": "导入平台账号",
  "platform.link": "关联平台账号",
  "platform.refresh": "刷新平台账号",
  "platform.unlink": "取消关联平台账号",
  "platform.update": "更新平台账号",
  "provider.create": "创建供应商",
  "provider.delete": "删除供应商",
  "provider.update": "更新供应商",
  "provider.discover": "发现模型供应商",
  "provider.test": "测试连接供应商",
  "provider.catalog.refresh": "刷新供应商目录",
  "provider.models.refresh": "刷新供应商模型",
  "provider.custom.protocol.update": "更新自定义供应商协议",
  "provider.protocol.probe": "探测供应商协议",
  "provider.protocol.reset": "重置供应商协议",
  "provider.protocol.update": "更新供应商协议",
  "provider.zen.refresh": "刷新 Zen Free 模型",
  "provider.zen.update": "更新 Zen Free 设置",
  "cpa.account.delete": "删除 CPA 账号",
  "cpa.account.reset": "重置 CPA 账号",
  "cpa.account.status": "更新 CPA 账号状态",
  "cpa.cli.import": "导入 CPA CLI 账号",
  "cpa.connection.test": "测试 CPA 连接",
  "cpa.integration.delete": "删除 CPA 集成",
  "cpa.integration.update": "更新 CPA 集成",
  "cpa.key.create": "创建 CPA Key",
  "cpa.key.delete": "删除 CPA Key",
  "cpa.key.rotate": "轮换 CPA Key",
  "cpa.models.refresh": "刷新 CPA 模型",
  "cpa.oauth.cancel": "取消 CPA 授权",
  "cpa.oauth.start": "开始 CPA 授权",
  "cpa.replace": "替换 CPA 凭证",
  "cpa.runtime.check": "检查 CPA 运行时",
  "cpa.runtime.install": "安装 CPA 运行时",
  "cpa.runtime.remove": "移除 CPA 运行时",
  "cpa.runtime.rollback": "回滚 CPA 运行时",
  "cpa.runtime.start": "启动 CPA 运行时",
  "cpa.runtime.stop": "停止 CPA 运行时",
  "cpa.runtime.update": "更新 CPA 运行时",
} as const satisfies Record<string, MessageKey>;

/** Wildcard families. An unknown verb under one of these stays readable as domain + verb. */
export const OPERATION_DOMAIN_KEYS = {
  provider: "供应商",
  catalog: "模型目录",
  alias: "别名",
  routing: "路由",
  platform: "平台账号",
  application: "应用",
  cpa: "CPA",
} as const satisfies Record<string, MessageKey>;

export const REQUEST_STATUS_KEYS = {
  success: "成功",
  error: "错误",
  client_error: "客户端错误",
  streaming: "流式未结束",
  outcome_unknown: "结果未知",
  cancelled: "已取消",
} as const satisfies Record<string, MessageKey>;

export const UNRECOGNIZED_OPERATION_KEY = "未登记的操作：{action}" as const satisfies MessageKey;

const OPERATION_DOMAINS = new Set<string>(Object.keys(OPERATION_DOMAIN_KEYS));

export type OperationActionLabel =
  | { kind: "mapped"; key: MessageKey }
  | { kind: "domain"; domainKey: MessageKey; verb: string }
  | { kind: "fallback"; key: typeof UNRECOGNIZED_OPERATION_KEY; action: string };

export function operationOutcomeKey(outcome: string): MessageKey | null {
  if (!Object.hasOwn(OPERATION_OUTCOME_KEYS, outcome)) return null;
  return OPERATION_OUTCOME_KEYS[outcome as OperationOutcome];
}

export function operationSourceKey(source: string): MessageKey | null {
  if (!Object.hasOwn(OPERATION_SOURCE_KEYS, source)) return null;
  return OPERATION_SOURCE_KEYS[source as OperationSource];
}

export function requestStatusKey(status: string): MessageKey | null {
  const logical = logicalRequestStatus(status);
  if (!Object.hasOwn(REQUEST_STATUS_KEYS, logical)) return null;
  return REQUEST_STATUS_KEYS[logical as keyof typeof REQUEST_STATUS_KEYS];
}

/**
 * Latest-attempt status, matching the server projection.
 * Every `success_*` variant is success. Streaming and outcome_unknown stay unresolved.
 */
export function logicalRequestStatus(status: string): string {
  if (status === "success" || status.startsWith("success_")) return "success";
  return status;
}

function isCodePart(value: string): boolean {
  return /^[a-z][a-z0-9]*$/.test(value);
}

export function describeOperationAction(action: string): OperationActionLabel {
  const mapped = Object.hasOwn(OPERATION_ACTION_KEYS, action)
    ? OPERATION_ACTION_KEYS[action as keyof typeof OPERATION_ACTION_KEYS] : undefined;
  if (mapped) return { kind: "mapped", key: mapped };
  const dot = action.indexOf(".");
  if (dot > 0) {
    const domain = action.slice(0, dot);
    const verb = action.slice(dot + 1);
    if (OPERATION_DOMAINS.has(domain) && isCodePart(verb) && !verb.includes(".")) {
      return {
        kind: "domain",
        domainKey: OPERATION_DOMAIN_KEYS[domain as keyof typeof OPERATION_DOMAIN_KEYS],
        verb,
      };
    }
  }
  return { kind: "fallback", key: UNRECOGNIZED_OPERATION_KEY, action };
}

export interface RequestUsageTotals {
  totalRequests: number;
  totalAttempts: number;
  inputTokens: number;
  outputTokens: number;
  cachedTokens: number;
  /** Input + output. Cached tokens are already inside input. */
  totalTokens: number;
}

export function requestUsageTotals(summary: RequestLogSummary): RequestUsageTotals {
  return {
    totalRequests: summary.totalRequests,
    totalAttempts: summary.totalAttempts,
    inputTokens: summary.promptTokens,
    outputTokens: summary.completionTokens,
    cachedTokens: summary.cachedTokens,
    totalTokens: summary.promptTokens + summary.completionTokens,
  };
}

/** Skeleton only before the first successful load. Revalidation keeps prior rows. */
export function showResourceSkeleton(loading: boolean, loaded: boolean): boolean {
  return loading && !loaded;
}

function queryPart(value: string | number | null | undefined): string {
  if (value === null || value === undefined) return "";
  return String(value);
}

/** Identity of one request page. Offset and request id participate so detail cache cannot cross them. */
export function requestLogQueryKey(query: RequestLogQuery): string {
  return [
    queryPart(query.limit),
    queryPart(query.offset),
    queryPart(query.status),
    queryPart(query.providerId),
    queryPart(query.accountId),
    queryPart(query.routeAccountId),
    queryPart(query.credentialAccountId),
    queryPart(query.keyId),
    queryPart(query.model),
    queryPart(query.startTime),
    queryPart(query.endTime),
    queryPart(query.requestId),
  ].join("\n");
}

export function operationLogQueryKey(query: OperationLogQuery): string {
  return [
    queryPart(query.limit),
    queryPart(query.offset),
    queryPart(query.action),
    queryPart(query.source),
    queryPart(query.outcome),
    queryPart(query.subjectType),
    queryPart(query.subjectId),
    queryPart(query.startTime),
    queryPart(query.endTime),
  ].join("\n");
}

export function attemptPanelId(requestKey: string): string {
  return `request-attempts-${encodeURIComponent(requestKey)}`;
}
