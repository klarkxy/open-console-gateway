import { PRIMARY_KEY_ID } from "../api/dashboard-v3.ts";
import type { ConnectionInfo } from "../api/dashboard.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { DshApplication, DshApplicationStatus } from "../api/generated/dashboard-v4.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Pure state helpers for the Applications page. The page currently hosts a
 * single DSH child tab; the tab type and `app=` deep link are kept narrow on
 * purpose — this is not a plugin registry.
 */

export const APPLICATION_TABS = ["dsh"] as const;
export type ApplicationTab = (typeof APPLICATION_TABS)[number];
export const DEFAULT_APPLICATION_TAB: ApplicationTab = "dsh";

export function normalizeApplicationTab(raw: string | null | undefined): ApplicationTab | null {
  return (APPLICATION_TABS as readonly string[]).includes(raw ?? "")
    ? raw as ApplicationTab
    : null;
}

/** Reads the `app` deep link; unknown or missing values fall back to DSH. */
export function readApplicationTab(search: string): ApplicationTab {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  return normalizeApplicationTab(params.get("app")) ?? DEFAULT_APPLICATION_TAB;
}

export interface DshInstallKeyOption {
  id: string;
  name: string;
  kind: "primary" | "sub";
}

/**
 * Keys offered for a DSH install: the primary Key plus every enabled sub Key.
 * Options carry id and display name only — plaintext values never leave the
 * connection payload and are never sent in the install request.
 */
export function buildDshInstallKeyOptions(
  connection: Pick<ConnectionInfo, "primary_key" | "sub_keys"> | null | undefined,
): DshInstallKeyOption[] {
  if (!connection) return [];
  const options: DshInstallKeyOption[] = [];
  if (connection.primary_key) {
    options.push({ id: PRIMARY_KEY_ID, name: "", kind: "primary" });
  }
  for (const key of connection.sub_keys) {
    if (key.enabled) options.push({ id: key.id, name: key.name, kind: "sub" });
  }
  return options;
}

export type DshStatusTone = "default" | "info" | "success" | "warning" | "error";

export interface DshStatusPresentation {
  tone: DshStatusTone;
  labelKey: MessageKey;
  hintKey: MessageKey;
}

export function dshStatusPresentation(status: DshApplicationStatus): DshStatusPresentation {
  switch (status) {
    case "unsupported_runtime":
      return {
        tone: "default",
        labelKey: "当前环境不支持",
        hintKey: "DSH 安装可在桌面应用或同一台机器上的原生无头 CLI 中进行；官方 Docker 镜像暂不支持。",
      };
    case "not_detected":
      return {
        tone: "warning",
        labelKey: "未检测到 DSH",
        hintKey: "先安装 DSH 客户端，再刷新状态。",
      };
    case "ready":
      return {
        tone: "info",
        labelKey: "可安装",
        hintKey: "已检测到 DSH，可以把 OCG 网关注册进去。",
      };
    case "installed":
      return {
        tone: "success",
        labelKey: "已安装",
        hintKey: "OCG 网关已注册到 DSH。若上方提示需重启或启动 DSH，按提示操作即可。",
      };
    case "incompatible":
      return {
        tone: "error",
        labelKey: "版本不兼容",
        hintKey: "检测到的 DSH 与当前 OCG 版本不兼容；升级其中一方后刷新。",
      };
    case "conflict":
      return {
        tone: "warning",
        labelKey: "存在冲突",
        hintKey: "检测到同名或不完整的现有配置。先处理冲突项，再刷新状态。",
      };
  }
}

export type DshInstallAction = "install" | "reinstall" | "unavailable";

/**
 * The install action requires backend-reported support and a fingerprint to
 * pin the CAS precondition; an installed DSH offers reinstall instead.
 */
export function dshInstallAction(
  app: Pick<DshApplication, "installSupported" | "installed" | "fingerprint">,
): DshInstallAction {
  if (!app.installSupported || !app.fingerprint) return "unavailable";
  return app.installed ? "reinstall" : "install";
}

/**
 * Host-supplied diagnostic shown when the page cannot act, or when status
 * already names a blocked environment. Empty or redundant ready/installed
 * English summaries stay off the page.
 */
export function dshHostDetail(
  app: Pick<DshApplication, "detail" | "status" | "installSupported" | "installed" | "fingerprint"> | null | undefined,
): string | null {
  if (!app) return null;
  const detail = app.detail?.trim() ?? "";
  if (!detail) return null;
  if (dshInstallAction(app) === "unavailable") return detail;
  switch (app.status) {
    case "conflict":
    case "incompatible":
    case "unsupported_runtime":
    case "not_detected":
      return detail;
    default:
      return null;
  }
}

/** CAS precondition captured from the inspection that the user confirmed. */
export function dshInstallExpectation(
  app: Pick<DshApplication, "revision">,
): MutationExpectation {
  return {
    expectedRevision: app.revision.revision,
    processGeneration: app.revision.processGeneration,
  };
}
