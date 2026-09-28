/**
 * Follow-up reads after a confirmed account save. The write already ended at
 * its receipt: these reads run off the page lock, last-good content stays
 * rendered, each failure reports itself, and nothing here rethrows into the
 * completed write. `isCurrent` is the view/session fence — once it flips, no
 * further read starts and no failure is reported into the new context.
 */
export interface AccountSaveFollowup {
  kind: "updated" | "created";
  accountId: string;
  hasUsageDisplay: boolean;
  usageReady: boolean;
  isCurrent: () => boolean;
  /** True when the projection reloaded; false reports through the notifier. */
  refreshProjection: (created: boolean) => Promise<boolean>;
  notifyProjectionFailure: () => void;
  loadConnections: () => Promise<unknown>;
  refreshCatalogForNewProvider: () => Promise<void>;
  loadUsage: (accountId: string) => Promise<void>;
}

export async function runAccountSaveFollowup(input: AccountSaveFollowup): Promise<void> {
  if (!input.isCurrent()) return;
  const reads: Array<() => Promise<unknown>> = [async () => {
    const refreshed = await input.refreshProjection(input.kind === "created").catch(() => false);
    if (input.isCurrent() && !refreshed) input.notifyProjectionFailure();
  }];
  if (input.kind === "created") reads.push(input.loadConnections, input.refreshCatalogForNewProvider);
  if (input.hasUsageDisplay && (input.kind === "updated" || input.usageReady)) {
    reads.push(() => input.loadUsage(input.accountId));
  }
  // These projections are independent: a slow catalog or card read must not
  // postpone current usage. Each owner retains its own error state.
  await Promise.allSettled(reads.map(async read => {
    if (input.isCurrent()) await read();
  }));
}
