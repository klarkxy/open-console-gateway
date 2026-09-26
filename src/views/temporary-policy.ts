import type { MessageKey } from "../i18n/index.ts";
import {
  BUILTIN_GOAT_ID,
  TEMPORARY_POLICY_BUILTIN_KEYS,
  connectionDisplayName,
  credentialDisplayName,
  remainingProbeSeconds,
  restrictionTableEmpty,
  type PolicyRestriction,
} from "../domain/temporary-policy.ts";

export type RestrictionRow = PolicyRestriction;

export type NamedResource = { id: string; name: string };

export type RestrictionModelLabel =
  | { kind: "model"; model: string }
  | { kind: "credential_scope" };

export type RestrictionNameLabel =
  | { kind: "named"; name: string }
  | { kind: "unknown_connection" }
  | { kind: "unknown_credential" };

export const RESTRICTION_MODEL_KEYS = {
  credential_scope: "仅凭证范围",
} as const satisfies Record<RestrictionModelLabel["kind"] & "credential_scope", MessageKey>;

export const RESTRICTION_NAME_KEYS = {
  unknown_connection: "未知连接",
  unknown_credential: "未知凭证",
} as const satisfies Record<"unknown_connection" | "unknown_credential", MessageKey>;

export function restrictionModelLabel(row: Pick<RestrictionRow, "scope" | "upstreamModel">): RestrictionModelLabel {
  if (row.scope === "credential" || row.upstreamModel === null) {
    return { kind: "credential_scope" };
  }
  return { kind: "model", model: row.upstreamModel };
}

export function restrictionConnectionLabel(
  destinationId: string,
  destinations: readonly NamedResource[],
): RestrictionNameLabel {
  const name = connectionDisplayName(destinationId, destinations);
  return name ? { kind: "named", name } : { kind: "unknown_connection" };
}

export function restrictionCredentialLabel(
  credentialId: string,
  credentials: readonly NamedResource[],
): RestrictionNameLabel {
  const name = credentialDisplayName(credentialId, credentials);
  return name ? { kind: "named", name } : { kind: "unknown_credential" };
}

export function restrictionWaitLabel(row: Pick<RestrictionRow, "state" | "nextProbeInSeconds">, observedAt: number, now: number): number {
  if (row.state !== "waiting") return 0;
  return remainingProbeSeconds(row.nextProbeInSeconds, observedAt, now);
}

export function restrictionEmptyCode(rows: readonly unknown[]) {
  return restrictionTableEmpty(rows.length);
}

export type BuiltinLabel =
  | { kind: "key"; key: MessageKey }
  | { kind: "id"; id: string };

export function builtinLabel(id: string): BuiltinLabel {
  if (id === BUILTIN_GOAT_ID) {
    return { kind: "key", key: TEMPORARY_POLICY_BUILTIN_KEYS[BUILTIN_GOAT_ID] };
  }
  return { kind: "id", id };
}

export const CLEAR_LOCAL_WAIT_KEY = "清除本地等待" as const satisfies MessageKey;

export interface LocalWaitClock {
  now: () => number;
  setInterval: (handler: () => void, timeout: number) => number;
  clearInterval: (id: number) => void;
}

export interface LocalWaitTicker {
  start(): void;
  stop(): void;
  running(): boolean;
}

export function createLocalWaitTicker(
  onTick: (now: number) => void,
  clock: LocalWaitClock = {
    now: () => Date.now(),
    setInterval: (handler, timeout) => window.setInterval(handler, timeout),
    clearInterval: (id) => window.clearInterval(id),
  },
): LocalWaitTicker {
  let tick: number | undefined;
  return {
    start() {
      this.stop();
      onTick(clock.now());
      tick = clock.setInterval(() => onTick(clock.now()), 1_000);
    },
    stop() {
      if (tick === undefined) return;
      clock.clearInterval(tick);
      tick = undefined;
    },
    running() {
      return tick !== undefined;
    },
  };
}
