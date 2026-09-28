import type { ByokApplicationView } from "../api/byok-applications.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Pure helpers for the Applications tabs: tab/deep-link handling, status
 * presentation, capability-gated actions, and the default harness Key name.
 * Copy lives in `*_KEYS` / presentation tables so tests never depend on
 * literal UI wording.
 */

export const BYOK_CLIENTS = ["codex", "kimi", "minimax", "zcode"] as const;
export type ByokClientId = (typeof BYOK_CLIENTS)[number];

/** Brand names are proper nouns and stay untranslated. */
export const BYOK_CLIENT_LABELS: Record<ByokClientId, string> = {
  codex: "Codex",
  kimi: "Kimi Code",
  minimax: "MiniMax Code",
  zcode: "ZCode",
};

export type ApplicationsTab = "dsh" | ByokClientId;
export const DEFAULT_APPLICATIONS_TAB: ApplicationsTab = "dsh";

/**
 * Ordinary Key each Applications harness uses. The host reuses or creates
 * this named sub-Key; the UI never selects a Key or model list.
 */
export const HARNESS_DEFAULT_KEY_NAMES: Record<ApplicationsTab, string> = {
  dsh: "dsh",
  codex: "codex",
  kimi: "kimi-code",
  minimax: "minimax-code",
  zcode: "zcode",
};

export function normalizeByokClient(raw: string | null | undefined): ByokClientId | null {
  return (BYOK_CLIENTS as readonly string[]).includes(raw ?? "")
    ? raw as ByokClientId
    : null;
}

/** Reads the `app` deep link; unknown or missing values fall back to DSH. */
export function readApplicationsTab(search: string): ApplicationsTab {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  const raw = params.get("app");
  return normalizeByokClient(raw) ?? (raw === "dsh" ? "dsh" : DEFAULT_APPLICATIONS_TAB);
}

export type ByokStatusTone = "default" | "info" | "success" | "warning" | "error";

export interface ByokStatusPresentation {
  tone: ByokStatusTone;
  labelKey: MessageKey;
  hintKey: MessageKey;
}

const BYOK_STATUS_PRESENTATIONS: Record<string, ByokStatusPresentation> = {
  unsupported_runtime: {
    tone: "default",
    labelKey: "当前环境不支持",
    hintKey: "当前环境不支持自动写入客户端配置（例如 Docker 或浏览器主机）；可按下方网关地址在 {client} 中手动配置。",
  },
  not_detected: {
    tone: "warning",
    labelKey: "未发现配置",
    hintKey: "未发现 {client} 的本地配置。核对目标路径后可创建配置，也可填写自定义 Profile 的完整路径。",
  },
  ready: {
    tone: "info",
    labelKey: "可配置",
    hintKey: "已检测到 {client} 的配置目标；确认后会写入默认 Key 及其全部已发布模型。",
  },
  configured: {
    tone: "success",
    labelKey: "已保存配置",
    hintKey: "OCG 配置已写入客户端文件。是否已生效以客户端为准：重启 {client} 或开始新会话后加载。",
  },
  conflict: {
    tone: "warning",
    labelKey: "存在冲突",
    hintKey: "检测到非 OCG 管理的同名配置，或托管配置被外部修改。OCG 不会覆盖；请先在客户端配置中手动处理，再刷新状态。",
  },
  incompatible: {
    tone: "error",
    labelKey: "版本不兼容",
    hintKey: "检测到未知或已损坏的配置格式；OCG 不会覆盖，请手动处理或从备份恢复。",
  },
  recovery_required: {
    tone: "error",
    labelKey: "写入中断",
    hintKey: "上一次写入未完成，配置可能处于中间状态。使用“恢复中断的写入”回滚；若文件已被手动修改，恢复会保留改动并报告冲突。",
  },
};

/** Unrecognized statuses stay honest: an explicit unknown, never a guess. */
export const BYOK_STATUS_FALLBACK: ByokStatusPresentation = {
  tone: "default",
  labelKey: "未知状态",
  hintKey: "状态未识别；请刷新后重试。",
};

export function byokStatusPresentation(status: string): ByokStatusPresentation {
  return BYOK_STATUS_PRESENTATIONS[status] ?? BYOK_STATUS_FALLBACK;
}

export type ByokConfigureAction = "configure" | "update" | "unavailable";

/**
 * Configure requires backend-reported support and a fingerprint to pin the
 * CAS precondition; an already-managed target offers an update instead.
 */
export function byokConfigureAction(
  view: Pick<ByokApplicationView, "configureSupported" | "fingerprint" | "configuredModelIds"> | null | undefined,
): ByokConfigureAction {
  if (!view?.configureSupported || !view.fingerprint) return "unavailable";
  return view.configuredModelIds.length > 0 ? "update" : "configure";
}

/** Remove needs no current Key or model: only backend support and a fingerprint. */
export function byokRemoveAvailable(
  view: Pick<ByokApplicationView, "removeSupported" | "fingerprint"> | null | undefined,
): boolean {
  return Boolean(view?.removeSupported && view.fingerprint);
}

export function byokRecoverAvailable(
  view: Pick<ByokApplicationView, "recoverySupported" | "fingerprint"> | null | undefined,
): boolean {
  return Boolean(view?.recoverySupported && view.fingerprint);
}

/** CAS precondition captured from the inspection that the user confirmed. */
export function byokMutationExpectation(
  view: Pick<ByokApplicationView, "revision">,
): MutationExpectation {
  return {
    expectedRevision: view.revision.revision,
    processGeneration: view.revision.processGeneration,
  };
}

/** The exact config file the operation targets, as resolved by the backend. */
export function byokDisplayTarget(
  view: Pick<ByokApplicationView, "configPath" | "targetPaths"> | null | undefined,
): string {
  return view?.configPath || view?.targetPaths[0] || "";
}

/** Captured before an awaited mutation so a later logout can skip the refresh. */
export interface ConnectionSessionGuard {
  captured: number;
  current: () => number;
}

/**
 * Reload connection after configure/install. A Key may already exist; a
 * failed reload must not rewrite the mutation outcome. Callers capture the
 * connection session token before the mutation and do not await this refresh
 * before releasing mutation UI. A changed session skips the fetch entirely.
 */
export async function refreshConnectionAfterHarnessMutation(
  reload: () => Promise<unknown>,
  session?: ConnectionSessionGuard,
): Promise<void> {
  if (session && session.captured !== session.current()) return;
  try {
    await reload();
  } catch {
    // Ignore: callers treat the mutation result as authoritative.
  }
}
