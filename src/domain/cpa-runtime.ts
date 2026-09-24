import type {
  CpaAccount,
  CpaIntegration,
  CpaModel,
  CpaOAuthProvider,
  CpaRuntime,
  CpaRuntimeCheck,
  CpaRuntimeKey,
  CpaRuntimePhase,
} from "../api/generated/dashboard-v3.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Pure state helpers for the CPA page. The external connection and the managed
 * installation are mutually exclusive modes derived from the integration flag
 * plus the runtime snapshot; every lifecycle control decision is a pure
 * function of (runtime, busy, last update check) so the view stays declarative.
 */

export type CpaRuntimeMode = "external" | "managed" | "unsupported";
export type CpaRuntimeModePreference = Exclude<CpaRuntimeMode, "unsupported"> | null;

/**
 * Pick a usable Overview mode. A fresh supported Desktop defaults to managed
 * install; an already configured external connection defaults to external.
 * A user's explicit choice wins while it remains supported.
 */
/** Confirmed Host support requires a successful runtime snapshot; null is not support. */
export function cpaManagedRuntimeConfirmed(
  integration: Pick<CpaIntegration, "runtimeSupported">,
  runtime: Pick<CpaRuntime, "supported"> | null,
): boolean {
  return integration.runtimeSupported === true && runtime?.supported === true;
}

export function cpaRuntimeMode(
  integration: Pick<CpaIntegration, "configured" | "runtimeOwned" | "runtimeSupported">,
  runtime: Pick<CpaRuntime, "owned" | "supported"> | null,
  preference: CpaRuntimeModePreference = null,
): CpaRuntimeMode {
  const managedSupported = cpaManagedRuntimeConfirmed(integration, runtime);
  if (preference === "managed") {
    if (managedSupported) return "managed";
    if (integration.runtimeSupported !== true) return "unsupported";
    if (runtime === null) return integration.runtimeOwned ? "managed" : "external";
    return "unsupported";
  }
  if (preference === "external") return "external";
  if (integration.runtimeOwned || runtime?.owned) {
    if (managedSupported) return "managed";
    if (runtime === null && integration.runtimeSupported === true) return "managed";
    return "unsupported";
  }
  if (integration.configured) return "external";
  return managedSupported ? "managed" : "external";
}

/** A fresh supported host may install; an installed runtime must be OCG-owned. */
function cpaRuntimeLifecycleEditable(
  runtime: Pick<CpaRuntime, "supported" | "owned" | "installed"> | null,
): boolean {
  return !!runtime && runtime.supported && (!runtime.installed || runtime.owned);
}

/** The Client Keys section exists only for an owned, installed managed runtime. */
export function cpaClientKeysAvailable(
  runtime: Pick<CpaRuntime, "supported" | "owned" | "installed"> | null,
): boolean {
  return !!runtime && runtime.supported && runtime.owned && runtime.installed;
}

/** Non-terminal phases reported while a lifecycle operation is in flight. */
const CPA_BUSY_PHASES: readonly CpaRuntimePhase[] = [
  "checking",
  "downloading",
  "installing",
  "starting",
];

export function isCpaPhaseBusy(
  phase: CpaRuntimePhase,
): phase is Exclude<CpaRuntimePhase, "idle" | "failed"> {
  return (CPA_BUSY_PHASES as readonly CpaRuntimePhase[]).includes(phase);
}

/**
 * Operational status for the Accounts CPA pool card. Routing enablement stays
 * on the existing Enabled tag; this is the managed process (or external
 * connection) the pool actually talks to.
 */
export type CpaCardStatus =
  | "checking"
  | "downloading"
  | "installing"
  | "starting"
  | "failed"
  | "running"
  | "stopped"
  | "not_installed"
  | "external";

export const CPA_RUNTIME_PHASE_KEYS = {
  idle: "空闲",
  checking: "检查中",
  downloading: "下载中",
  installing: "安装中",
  starting: "启动中",
  failed: "失败",
} as const satisfies Record<CpaRuntimePhase, MessageKey>;

export const CPA_CARD_STATUS_KEYS = {
  checking: "检查中",
  downloading: "下载中",
  installing: "安装中",
  starting: "启动中",
  failed: "失败",
  running: "运行中",
  stopped: "已停止",
  not_installed: "未安装",
  external: "外部连接",
} as const satisfies Record<CpaCardStatus, MessageKey>;

type CpaCardIntegration = Pick<
  CpaIntegration,
  "configured" | "runtimeOwned" | "runtimeRunning" | "installedVersion"
>;
type CpaCardRuntime = Pick<CpaRuntime, "installed" | "running" | "owned" | "phase">;

/**
 * Prefer an in-flight lifecycle phase. A managed install reports running /
 * stopped; an external connection is not an OCG-owned process.
 */
export function cpaCardStatus(
  integration: CpaCardIntegration | null,
  runtime: CpaCardRuntime | null,
): CpaCardStatus | null {
  if (!integration) return null;
  if (runtime && isCpaPhaseBusy(runtime.phase)) return runtime.phase;
  if (runtime?.phase === "failed") return "failed";
  const owned = integration.runtimeOwned || runtime?.owned === true;
  if (owned) {
    const installed = runtime?.installed ?? integration.installedVersion != null;
    if (!installed) return "not_installed";
    const running = runtime?.running ?? integration.runtimeRunning;
    return running ? "running" : "stopped";
  }
  if (integration.configured) return "external";
  return null;
}

export function cpaCardStatusTagType(
  status: CpaCardStatus,
): "success" | "warning" | "error" | "default" {
  switch (status) {
    case "running":
      return "success";
    case "failed":
      return "error";
    case "checking":
    case "downloading":
    case "installing":
    case "starting":
    case "not_installed":
      return "warning";
    default:
      return "default";
  }
}

/** A managed process that is not up yet should gray the Accounts pool card. */
export function cpaCardProcessDown(status: CpaCardStatus | null | undefined): boolean {
  return status != null && status !== "running" && status !== "external";
}

export type CpaRuntimeAction =
  | "install"
  | "start"
  | "stop"
  | "checkUpdate"
  | "update"
  | "rollback"
  | "remove";

export type CpaRuntimeControlState = {
  runtime: CpaRuntime | null;
  /** A lifecycle request is in flight from this client. */
  busy: boolean;
  /** Result of the last explicit check-update, if any. */
  updateCheck: CpaRuntimeCheck | null;
};

const ALL_DISABLED: Record<CpaRuntimeAction, boolean> = {
  install: false,
  start: false,
  stop: false,
  checkUpdate: false,
  update: false,
  rollback: false,
  remove: false,
};

/**
 * Control-availability matrix. Everything is disabled while any operation is
 * in flight (local request or a busy backend phase), on unsupported runtimes,
 * and on runtimes OCG does not own. `update` additionally requires a fresh check that
 * reported an available version, because its `expectedVersion` comes from it.
 */
export function cpaRuntimeControls(state: CpaRuntimeControlState): Record<CpaRuntimeAction, boolean> {
  const { runtime, busy, updateCheck } = state;
  if (!runtime || busy || isCpaPhaseBusy(runtime.phase)) return { ...ALL_DISABLED };
  if (!cpaRuntimeLifecycleEditable(runtime)) return { ...ALL_DISABLED };
  return {
    install: !runtime.installed,
    start: runtime.installed && !runtime.running,
    stop: runtime.installed && runtime.running,
    checkUpdate: true,
    update: runtime.installed && updateCheck?.updateAvailable === true,
    rollback: runtime.installed && !!runtime.previousVersion,
    remove: runtime.installed,
  };
}

/** Client-side bound for the rendered log tail; the backend tail is bounded too. */
export const CPA_LOG_TAIL_LINES = 200;

/** Keep only the last `maxLines` lines of a log payload; blank/empty input renders empty. */
export function cpaLogTail(text: string, maxLines: number = CPA_LOG_TAIL_LINES): string {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\n+$/u, "");
  if (!normalized) return "";
  const lines = normalized.split("\n");
  return lines.slice(Math.max(0, lines.length - maxLines)).join("\n");
}

export type CpaRuntimeKeyPartition = {
  /** OCG-owned routing keys (`protected`); the contract guarantees at most one today. */
  protectedKeys: CpaRuntimeKey[];
  /** Direct-client keys that may be deleted individually. */
  directKeys: CpaRuntimeKey[];
};

/** Protected routing keys never mix with direct-client keys. */
export function partitionCpaRuntimeKeys(keys: readonly CpaRuntimeKey[]): CpaRuntimeKeyPartition {
  const protectedKeys: CpaRuntimeKey[] = [];
  const directKeys: CpaRuntimeKey[] = [];
  for (const key of keys) {
    (key.protected ? protectedKeys : directKeys).push(key);
  }
  return { protectedKeys, directKeys };
}

export type CpaCatalogRow = {
  id: string;
  ownedBy?: string | null;
};

export type CpaCatalogGroup<T extends CpaCatalogRow = CpaModel> = {
  source: string;
  models: T[];
};

/** Group the persisted CPA snapshot by CPA-reported `ownedBy`, unknown last. */
export function groupCpaCatalogModels<T extends CpaCatalogRow>(
  models: readonly T[],
): CpaCatalogGroup<T>[] {
  const groups = new Map<string, T[]>();
  for (const model of models) {
    const source = model.ownedBy?.trim() ?? "";
    const rows = groups.get(source);
    if (rows) rows.push(model);
    else groups.set(source, [model]);
  }
  const known: CpaCatalogGroup<T>[] = [];
  let unknown: CpaCatalogGroup<T> | null = null;
  for (const [source, rows] of groups) {
    rows.sort((left, right) => left.id.localeCompare(right.id));
    const group = { source, models: rows };
    if (source) known.push(group);
    else unknown = group;
  }
  known.sort((left, right) => left.source.localeCompare(right.source));
  return unknown ? [...known, unknown] : known;
}

/** Stable row identity for account lists. */
export function cpaAccountKey(account: Pick<CpaAccount, "name" | "authIndex">): string {
  return `${account.name}:${account.authIndex ?? ""}`;
}

function isVacuousCpaQuota(value: unknown, seen = new WeakSet<object>()): boolean {
  if (value === null || value === undefined) return true;
  if (typeof value === "string") return value.trim() === "";
  if (typeof value === "number") return !Number.isFinite(value);
  if (typeof value !== "object") return true;
  if (seen.has(value)) return true;
  seen.add(value);
  if (Array.isArray(value)) return value.length === 0 || value.every((item) => isVacuousCpaQuota(item, seen));
  const entries = Object.values(value as Record<string, unknown>);
  return entries.length === 0 || entries.every((item) => isVacuousCpaQuota(item, seen));
}

/**
 * Quota payloads are opaque CPA trackers. Hide empty `{ signals: {} }` shells;
 * render scalars directly and JSON otherwise.
 */
export function formatCpaQuota(value: unknown): string | null {
  if (isVacuousCpaQuota(value)) return null;
  if (typeof value === "string") return value.trim();
  if (typeof value === "number") return String(value);
  try {
    return JSON.stringify(value);
  } catch {
    return null;
  }
}

/** CPA OAuth sign-in entry points, in display order. */
export const CPA_OAUTH_PROVIDERS: ReadonlyArray<{ id: CpaOAuthProvider; label: string }> = [
  { id: "codex", label: "Codex" },
  { id: "anthropic", label: "Claude" },
  { id: "antigravity", label: "Antigravity" },
  { id: "kimi", label: "Kimi" },
  { id: "xai", label: "xAI" },
];

/**
 * CPA account filename token for a CLI import. Anthropic logins are stored as
 * `claude`; every other OAuth provider keeps its id.
 */
export function cpaCliImportFilenameToken(provider: CpaOAuthProvider): string {
  return provider === "anthropic" ? "claude" : provider;
}

/** True when a CPA account was created by importing this provider's local CLI. */
export function cpaCliImportAlreadyPresent(
  provider: CpaOAuthProvider,
  accounts: readonly Pick<CpaAccount, "name">[],
): boolean {
  const prefix = `ocg-cli-${cpaCliImportFilenameToken(provider)}-`.toLowerCase();
  return accounts.some((account) => account.name.toLowerCase().startsWith(prefix));
}

/** Reverse of `cpaCliImportAlreadyPresent` for the account being deleted. */
export function cpaOAuthProviderForCliAccount(
  account: Pick<CpaAccount, "name">,
): CpaOAuthProvider | null {
  const name = account.name.toLowerCase();
  for (const { id } of CPA_OAUTH_PROVIDERS) {
    const prefix = `ocg-cli-${cpaCliImportFilenameToken(id)}-`.toLowerCase();
    if (name.startsWith(prefix)) return id;
  }
  return null;
}

const CPA_OAUTH_TERMINAL_STATUSES = ["ok", "completed", "success", "cancelled", "failed", "expired", "error"];
const CPA_OAUTH_SUCCESS_STATUSES = ["ok", "success", "completed"];

/** Polling stops on any terminal OAuth status. */
export function isCpaOAuthTerminalStatus(status: string): boolean {
  return CPA_OAUTH_TERMINAL_STATUSES.includes(status.toLowerCase());
}

/** Only a successful terminal status triggers an account-list refresh. */
export function isCpaOAuthSuccessStatus(status: string): boolean {
  return CPA_OAUTH_SUCCESS_STATUSES.includes(status.toLowerCase());
}
