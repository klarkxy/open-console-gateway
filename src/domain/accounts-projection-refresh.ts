export const ACCOUNTS_PROJECTION_REFRESH_MS = 15_000;

/**
 * Scope of one billing revalidation pass: accounts whose row is on screen, or
 * whose display changes with time (cooldown / quota-retry countdown), need
 * fresh usage; everything else keeps its last committed usage. Explicit
 * refreshes and mutation receipts still update the skipped accounts.
 */
export function projectionUsageRevalidationScope<T>(input: {
  accounts: readonly T[];
  idOf: (account: T) => string;
  visibleIds: ReadonlySet<string>;
  hasCountdown: (account: T) => boolean;
}): T[] {
  return input.accounts.filter((account) => (
    input.visibleIds.has(input.idOf(account)) || input.hasCountdown(account)
  ));
}

export function canRefreshAccountsProjection(input: {
  viewActive: boolean;
  documentVisible: boolean;
  authenticated: boolean;
}): boolean {
  return input.viewActive && input.documentVisible && input.authenticated;
}

export interface AccountsProjectionRefreshHost {
  setInterval: (handler: () => void, ms: number) => number;
  clearInterval: (id: number) => void;
  addEventListener: (type: "visibilitychange", handler: () => void) => void;
  removeEventListener: (type: "visibilitychange", handler: () => void) => void;
  visibilityState: () => DocumentVisibilityState;
}

export function browserAccountsProjectionRefreshHost(): AccountsProjectionRefreshHost {
  return {
    setInterval: (handler, ms) => window.setInterval(handler, ms),
    clearInterval: (id) => window.clearInterval(id),
    addEventListener: (type, handler) => document.addEventListener(type, handler),
    removeEventListener: (type, handler) => document.removeEventListener(type, handler),
    visibilityState: () => document.visibilityState,
  };
}

/**
 * 15s local V4 destination/card revalidation while Accounts is the active
 * keep-alive view, the document is visible, and the session is authenticated.
 * Activate does not fire immediately (the view already loads on enter);
 * becoming visible again does. Logout and hide stop the timer.
 */
export function createAccountsProjectionRefresh(options: {
  intervalMs?: number;
  host: AccountsProjectionRefreshHost;
  isAuthenticated: () => boolean;
  refresh: () => void | Promise<void>;
}): {
  activate: () => void;
  deactivate: () => void;
  onSessionDropped: () => void;
} {
  const intervalMs = options.intervalMs ?? ACCOUNTS_PROJECTION_REFRESH_MS;
  let viewActive = false;
  let timer: number | undefined;
  let bound = false;

  function documentVisible(): boolean {
    return options.host.visibilityState() === "visible";
  }

  function allowed(): boolean {
    return canRefreshAccountsProjection({
      viewActive,
      documentVisible: documentVisible(),
      authenticated: options.isAuthenticated(),
    });
  }

  function tick(): void {
    if (!allowed()) return;
    void options.refresh();
  }

  function arm(): void {
    if (timer !== undefined) return;
    timer = options.host.setInterval(tick, intervalMs);
  }

  function disarm(): void {
    if (timer === undefined) return;
    options.host.clearInterval(timer);
    timer = undefined;
  }

  function onVisibility(): void {
    if (!viewActive) return;
    if (documentVisible() && options.isAuthenticated()) {
      arm();
      tick();
      return;
    }
    disarm();
  }

  function bind(): void {
    if (bound) return;
    options.host.addEventListener("visibilitychange", onVisibility);
    bound = true;
  }

  function unbind(): void {
    if (!bound) return;
    options.host.removeEventListener("visibilitychange", onVisibility);
    bound = false;
  }

  return {
    activate() {
      viewActive = true;
      bind();
      if (documentVisible() && options.isAuthenticated()) arm();
    },
    deactivate() {
      viewActive = false;
      disarm();
      unbind();
    },
    onSessionDropped() {
      disarm();
    },
  };
}
