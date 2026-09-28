import type { MessageKey } from "../i18n/messages/en-US.ts";
import type { PlatformAccount } from "../api/platform-accounts.ts";
import { watch } from "vue";

export type AccountRefreshState = "queued" | "running";
export const ACCOUNT_REFRESH_STATE_KEYS: Record<AccountRefreshState, MessageKey> = {
  queued: "排队中",
  running: "刷新中",
};

/** Observation revisions advance on refresh; only a different target invalidates waiting work. */
export function platformRefreshBinding(parent: PlatformAccount | undefined): string | null {
  return parent ? JSON.stringify([parent.id, parent.kind, parent.baseUrl]) : null;
}

/** Wait on reactive write locks, without polling or discarding the queued action. */
export function waitForAccountRefreshIdle(blocked: () => boolean, current: () => boolean): Promise<boolean> {
  if (!current() || !blocked()) return Promise.resolve(current());
  return new Promise(resolve => {
    const stop = watch(() => [blocked(), current()] as const, ([busy, valid]) => {
      if (!busy || !valid) {
        stop();
        resolve(valid);
      }
    }, { flush: "sync" });
  });
}

/** The Accounts view's manual and automatic refreshes share one serial lane. */
export function createAccountRefreshQueue(
  changed: (id: string, state: AccountRefreshState | null) => void,
) {
  type Job = {
    id: string;
    current: () => boolean;
    run: (isCurrent: () => boolean) => Promise<void>;
    promise: Promise<void>;
    resolve: () => void;
    reject: (error: unknown) => void;
  };
  const jobs = new Map<string, Job>();
  const pending: Job[] = [];
  let running = false;

  async function drain() {
    if (running) return;
    running = true;
    try {
      while (pending.length) {
        const job = pending.shift()!;
        const isCurrent = () => jobs.get(job.id) === job && job.current();
        try {
          if (isCurrent()) {
            changed(job.id, "running");
            await job.run(isCurrent);
          }
          job.resolve();
        } catch (error) {
          job.reject(error);
        } finally {
          if (jobs.get(job.id) === job) {
            jobs.delete(job.id);
            changed(job.id, null);
          }
        }
      }
    } finally {
      running = false;
    }
  }

  return {
    enqueue(id: string, current: () => boolean, run: Job["run"]): Promise<void> {
      const existing = jobs.get(id);
      if (existing) return existing.promise;
      let resolve!: Job["resolve"];
      let reject!: Job["reject"];
      const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
      const job = { id, current, run, promise, resolve, reject };
      jobs.set(id, job);
      pending.push(job);
      changed(id, "queued");
      void drain();
      return promise;
    },
    reset() {
      for (const id of jobs.keys()) changed(id, null);
      jobs.clear();
      for (const job of pending.splice(0)) job.resolve();
      // Let active I/O finish; its identity can no longer commit or clear a new job.
    },
  };
}
