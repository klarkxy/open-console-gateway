export const ACCOUNT_REFRESH_CONCURRENCY = 4;

export type AccountRefreshState = "queued" | "running";
export interface AccountRefreshJobOptions {
  /** Control-plane writes stay exclusive; quota observations may share the pool. */
  exclusive?: boolean;
  priority?: "manual" | "background";
}

/** Bounded observations, exclusive writes, and account-level single flight. */
export function createAccountRefreshScheduler(
  changed: (id: string, state: AccountRefreshState | null) => void,
  concurrency = ACCOUNT_REFRESH_CONCURRENCY,
) {
  if (!Number.isInteger(concurrency) || concurrency < 1) {
    throw new RangeError("refresh concurrency must be a positive integer");
  }
  type Job = {
    id: string;
    current: () => boolean;
    run: (isCurrent: () => boolean) => Promise<void>;
    exclusive: boolean;
    priority: "manual" | "background";
    promise: Promise<void>;
    resolve: () => void;
    reject: (error: unknown) => void;
  };
  const jobs = new Map<string, Job>();
  const pending: Job[] = [];
  const active = new Set<Job>();
  let draining = false;

  async function execute(job: Job): Promise<void> {
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
      active.delete(job);
      if (jobs.get(job.id) === job) {
        jobs.delete(job.id);
        changed(job.id, null);
      }
      drain();
    }
  }

  function drain(): void {
    if (draining) return;
    draining = true;
    try {
      while (pending.length && active.size < concurrency) {
        if ([...active].some(job => job.exclusive)) break;
        const manual = pending.findIndex(job => job.priority === "manual");
        const index = manual < 0 ? 0 : manual;
        const job = pending[index]!;
        // An exclusive job fences subsequent observations instead of starving
        // behind an endless stream of background quota requests.
        if (job.exclusive && active.size) break;
        pending.splice(index, 1);
        active.add(job);
        void execute(job);
      }
    } finally {
      draining = false;
    }
  }

  return {
    enqueue(
      id: string,
      current: () => boolean,
      run: Job["run"],
      options: AccountRefreshJobOptions = {},
    ): Promise<void> {
      const existing = jobs.get(id);
      if (existing) {
        if (options.priority !== "background") existing.priority = "manual";
        drain();
        return existing.promise;
      }
      let resolve!: Job["resolve"];
      let reject!: Job["reject"];
      const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
      const job: Job = {
        id, current, run, promise, resolve, reject,
        exclusive: options.exclusive ?? true,
        priority: options.priority ?? "manual",
      };
      jobs.set(id, job);
      pending.push(job);
      changed(id, "queued");
      drain();
      return promise;
    },
    reset(): void {
      for (const id of jobs.keys()) changed(id, null);
      jobs.clear();
      for (const job of pending.splice(0)) job.resolve();
      // Active I/O still counts against the limit, but cannot commit or clear
      // a replacement job after logout. Never pretend it was cancelled.
    },
  };
}
