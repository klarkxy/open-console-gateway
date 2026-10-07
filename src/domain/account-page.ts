import type { AccountPageAction, AccountPageCard, AccountPageRow } from "../api/pages.ts";
import { ACCOUNT_MENU_LABEL_KEYS, cooldownRemainingUntil } from "./account-display.ts";
import { familyOf, PROVIDER_FAMILIES, type ProviderFamily } from "./provider-families.ts";
import type { MessageKey } from "../i18n/index.ts";
import { CPA_CARD_STATUS_KEYS, type CpaCardStatus } from "./cpa-runtime.ts";
import { PLATFORM_KIND_LABELS } from "./platform-accounts.ts";
import { billingBinding, manualReceiptQuotaView, presentedUsageOf, type ManualQuotaReceipt } from "./billing.ts";

/** Keep an acknowledged quota edit visible only on the exact account binding. */
export function accountPageQuotaReceipt(row: AccountPageRow,
  slot: { boundVersion: string; manualReceipt: ManualQuotaReceipt | null } | undefined) {
  if (!slot || slot.boundVersion !== billingBinding(row.account?.updatedAt ?? "", row.inferenceEndpointUrl)) return null;
  return manualReceiptQuotaView(slot.manualReceipt, accountPageRowId(row), presentedUsageOf(row.billing));
}

export const ACCOUNT_PAGE_STATUS_KEYS = {
  available: "已启用", enabled: "已启用", disabled: "已禁用", registering: "注册中",
  cooling: "冷却中", unavailable: "不可用", "auth-error": "不可用",
  draft: "待验证", pending: "待验证", unknown: "未知",
  "quota-waiting": "额度耗尽", "quota-ready": "等待请求验证", "quota-probing": "验证中",
} as const satisfies Record<string, MessageKey>;

export const ACCOUNT_PAGE_ACTION_KEYS: Record<string, MessageKey> = {
  ...ACCOUNT_MENU_LABEL_KEYS,
  toggle: "已启用", "refresh-usage": "刷新", refresh: "刷新", "refresh-models": "刷新模型目录",
  "test-connection": "测试连接", calibrate: "校准用量", "purchase-date": "购买日期",
  models: "获取模型", "add-key": "添加 Key", "import-keys": "从站点导入 Key",
  "link-existing": "关联已有 Key", "fetch-all-models": "获取全部模型", "refresh-parent": "刷新",
  "delete-group": "删除", "add-card": "新卡片", "remove-empty-card": "删除空卡片",
  "rotate-key": "轮换 Key", "edit-binding": "编辑绑定", "retry-quota": "重新尝试",
  "move-card-up": "上移", "move-card-down": "下移", "move-card-top": "移到顶部", "move-card-bottom": "移到底部",
};

/** Translate wire verbs to the existing operation handlers; no capability inference. */
export function accountPageActionKey(key: string): string {
  const kebab = key.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`).replaceAll("_", "-");
  return ({ "refresh": "refresh-usage", "test": "test-connection", "calibration": "calibrate", "manual-usage": "calibrate", "open-models": "models" } as Record<string, string>)[kebab] ?? kebab;
}

export function accountPageCardActionKey(key: string): string {
  return ({ "refresh": "refresh-parent", "link-key": "link-existing" } as Record<string, string>)[key] ?? accountPageActionKey(key);
}

export function accountPageCpaStatus(card: AccountPageCard & { cpaStatus?: string | null }): CpaCardStatus | null {
  const status = card.cpaStatus;
  return status && Object.hasOwn(CPA_CARD_STATUS_KEYS, status) ? status as CpaCardStatus : null;
}

export interface AccountRoutingPresentation {
  routingMode: import("../api/dashboard.ts").RoutingMode;
  conversationSticky: boolean;
  revision: { revision: number; processGeneration: number };
}

export function accountRoutingSource(page: AccountRoutingPresentation | null | undefined,
  settings: { revision: number; process_generation: number } | null | undefined, currentProcess: number | null): "page" | "settings" | "empty" {
  if (!settings) return page ? "page" : "empty";
  if (!page) return "settings";
  if (settings.process_generation === page.revision.processGeneration) return settings.revision >= page.revision.revision ? "settings" : "page";
  return currentProcess === settings.process_generation ? "settings" : "page";
}

export function pageAction(actions: readonly AccountPageAction[] | undefined, key: string): AccountPageAction | null {
  return actions?.find(action => accountPageActionKey(action.key) === key) ?? null;
}

export function accountPageStatus(status: string): keyof typeof ACCOUNT_PAGE_STATUS_KEYS {
  const normalized = status.replaceAll("_", "-");
  return normalized in ACCOUNT_PAGE_STATUS_KEYS ? normalized as keyof typeof ACCOUNT_PAGE_STATUS_KEYS : "unknown";
}

export function accountPageTagType(status: string): "success" | "warning" | "error" | "default" {
  const code = accountPageStatus(status);
  if (code === "available" || code === "enabled") return "success";
  if (code === "cooling" || code === "registering" || code === "draft" || code === "pending" || code.startsWith("quota-")) return "warning";
  if (code === "unavailable" || code === "auth-error") return "error";
  return "default";
}

export function accountPageFamily(card: AccountPageCard): ProviderFamily {
  if (card.platform) return { id: `platform:${card.platform.kind}`, label: PLATFORM_KIND_LABELS[card.platform.kind], tint: "#5F6068" };
  const known = PROVIDER_FAMILIES.find(family => family.label === card.destination.brandFamily || family.id === card.destination.brandFamily);
  if (known) return known;
  return familyOf({ id: card.destination.id, name: card.destination.name,
    family: card.destination.brandFamily ?? card.rows[0]?.account?.providerId ?? undefined });
}

export function accountPageRowId(row: AccountPageRow): string {
  return row.account?.id ?? row.credential.legacyAccountId;
}

export function accountPageRefreshErrorCodes(receipt: { outcome: string; errors?: readonly { code: string }[] }): string[] {
  return [...new Set(receipt.errors?.map(issue => issue.code) ?? [])];
}

/** Only format the server's cooldown fact; routing/status remains server-owned. */
export function accountPageCooldown(row: AccountPageRow, now: number) {
  return cooldownRemainingUntil(row.account?.cooldownUntil ?? null, now);
}

/** A bounded page is also the maximum browser demand set. Intersection is applied separately. */
export function accountPageDemandIds(cards: readonly AccountPageCard[], visible: ReadonlySet<string>, collapsed: ReadonlySet<string>): string[] {
  return [...new Set(cards.flatMap(card => collapsed.has(card.cardId) ? [] : card.rows
    .filter(row => row.refresh.supported && visible.has(row.credential.id)).map(accountPageRowId)))];
}

export function accountPageWallet(card: AccountPageCard) {
  return card.platform?.snapshot?.wallet ?? null;
}
