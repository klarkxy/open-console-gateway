import { isVersionAtLeast } from "../api/dashboard.ts";
import type { UpdatePhase, UpdateStatus } from "../api/dashboard.ts";

export const UPDATE_TARGET_STORAGE_KEY = "ocg-update-target-version";

export type UpdateStatusDecision = "complete" | "busy" | "failed" | "idle";
export type InstallRequestFailureDecision = "observe" | "wait" | "fail";

interface UpdateTargetStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function normalizeUpdateTarget(value: string | null | undefined): string {
  const trimmed = value?.trim() ?? "";
  const match = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(trimmed);
  return match ? `${match[1]}.${match[2]}.${match[3]}` : "";
}

export function readUpdateTarget(storage: UpdateTargetStorage | null): string {
  if (!storage) return "";
  try {
    const raw = storage.getItem(UPDATE_TARGET_STORAGE_KEY);
    const target = normalizeUpdateTarget(raw);
    if (raw && !target) storage.removeItem(UPDATE_TARGET_STORAGE_KEY);
    return target;
  } catch {
    return "";
  }
}

export function writeUpdateTarget(
  storage: UpdateTargetStorage | null,
  value: string,
): string {
  const target = normalizeUpdateTarget(value);
  if (!target) return "";
  if (!storage) return target;
  try {
    storage.setItem(UPDATE_TARGET_STORAGE_KEY, target);
  } catch {
    // In-memory recovery and polling still work when storage is blocked.
  }
  return target;
}

export function clearUpdateTarget(storage: UpdateTargetStorage | null): void {
  if (!storage) return;
  try {
    storage.removeItem(UPDATE_TARGET_STORAGE_KEY);
  } catch {
    // A blocked session store must not prevent updater cleanup.
  }
}

export function isUpdatePhaseBusy(phase: UpdatePhase): boolean {
  return phase === "checking" || phase === "downloading" || phase === "installing";
}

export function decideUpdateStatus(
  status: Pick<UpdateStatus, "phase" | "current_version">,
  targetVersion: string,
): UpdateStatusDecision {
  const target = normalizeUpdateTarget(targetVersion);
  if (target && isVersionAtLeast(status.current_version, target)) return "complete";
  if (status.phase === "failed") return "failed";
  if (isUpdatePhaseBusy(status.phase)) return "busy";
  return "idle";
}

export function decideInstallRequestFailure(
  httpStatus: number | null,
  hasTarget: boolean,
): InstallRequestFailureDecision {
  if (httpStatus === 409) return "observe";
  if (httpStatus === null && hasTarget) return "wait";
  return "fail";
}
