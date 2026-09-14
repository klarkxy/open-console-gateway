import type { AccountSetupStep } from "../api/dashboard";

export const DEFAULT_OPENCODE_INVITE_URL =
  "https://opencode.ai/go?ref=68XPB6NP8V";

export const MANAGED_SETUP_STEPS: readonly AccountSetupStep[] = [
  "google_account",
  "opencode_registration",
  "payment",
  "key_verification",
  "ready",
];

export function setupStepIndex(step: AccountSetupStep): number {
  return MANAGED_SETUP_STEPS.indexOf(step);
}

export function normalizeOpenCodeInviteUrl(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "";
  if (trimmed.length > 2048) throw new Error("邀请链接不能超过 2048 个字符");
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new Error("邀请链接格式无效");
  }
  if (url.protocol !== "https:") throw new Error("邀请链接必须使用 HTTPS");
  if (url.username || url.password) throw new Error("邀请链接不能包含用户名或密码");
  if (url.hostname !== "opencode.ai" && url.hostname !== "console.opencode.ai") {
    throw new Error("邀请链接域名必须是 opencode.ai 或 console.opencode.ai");
  }
  return url.toString();
}

export function browserViewUrl(currentUrl: string, sessionToken: string): string {
  const url = new URL(currentUrl);
  url.searchParams.set("view", "browser");
  url.searchParams.delete("session");
  url.hash = new URLSearchParams({ session: sessionToken }).toString();
  return url.toString();
}
