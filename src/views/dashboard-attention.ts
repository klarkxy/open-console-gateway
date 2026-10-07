import type { DashboardAttentionItem, DashboardAttentionReason } from "../api/pages.ts";
import type { MessageKey } from "../i18n/index.ts";

export type AttentionItem = DashboardAttentionItem;
export type AttentionReason = DashboardAttentionReason;

/** Business priority and dates belong to the backend; this table owns copy. */
export const ATTENTION_REASON_KEYS: Record<AttentionReason, MessageKey> = {
  "auth-error": "不可用",
  expired: "已到期 {days} 天",
  cooling: "冷却中",
  "setup-incomplete": "注册中",
};

export function attentionTagType(reason: AttentionReason): "error" | "warning" | "info" {
  switch (reason) {
    case "auth-error": case "expired": return "error";
    case "cooling": return "warning";
    case "setup-incomplete": return "info";
  }
}
