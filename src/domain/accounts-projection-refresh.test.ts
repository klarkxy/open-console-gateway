import assert from "node:assert/strict";
import test from "node:test";
import {
  ACCOUNTS_PROJECTION_REFRESH_MS,
  canRefreshAccountsProjection,
  createAccountsProjectionRefresh,
  projectionUsageRevalidationScope,
  type AccountsProjectionRefreshHost,
} from "./accounts-projection-refresh.ts";

test("usage revalidation keeps visible rows and countdown rows, skips the rest", () => {
  const accounts = [
    { id: "visible" },
    { id: "countdown" },
    { id: "both" },
    { id: "stale" },
  ];
  const scoped = projectionUsageRevalidationScope({
    accounts,
    idOf: (account) => account.id,
    visibleIds: new Set(["visible", "both"]),
    hasCountdown: (account) => account.id === "countdown" || account.id === "both",
  });
  assert.deepEqual(scoped.map((account) => account.id), ["visible", "countdown", "both"]);
});

test("projection refresh runs only while Accounts is active, visible, and authenticated", () => {
  assert.equal(canRefreshAccountsProjection({
    viewActive: true,
    documentVisible: true,
    authenticated: true,
  }), true);
  assert.equal(canRefreshAccountsProjection({
    viewActive: false,
    documentVisible: true,
    authenticated: true,
  }), false);
  assert.equal(canRefreshAccountsProjection({
    viewActive: true,
    documentVisible: false,
    authenticated: true,
  }), false);
  assert.equal(canRefreshAccountsProjection({
    viewActive: true,
    documentVisible: true,
    authenticated: false,
  }), false);
});

function installHost(visibility: { state: DocumentVisibilityState }): {
  host: AccountsProjectionRefreshHost;
  fireVisibility: () => void;
  tick: () => void;
  intervalArmed: () => boolean;
} {
  let intervalHandler: (() => void) | undefined;
  let visibilityHandler: (() => void) | undefined;
  const host: AccountsProjectionRefreshHost = {
    setInterval: (handler) => {
      intervalHandler = handler;
      return 1;
    },
    clearInterval: () => {
      intervalHandler = undefined;
    },
    addEventListener: (_type, handler) => {
      visibilityHandler = handler;
    },
    removeEventListener: () => {
      visibilityHandler = undefined;
    },
    visibilityState: () => visibility.state,
  };
  return {
    host,
    fireVisibility: () => visibilityHandler?.(),
    tick: () => intervalHandler?.(),
    intervalArmed: () => intervalHandler !== undefined,
  };
}

test("activate arms the 15s timer without an immediate load", () => {
  const visibility = { state: "visible" as DocumentVisibilityState };
  const { host, tick, intervalArmed } = installHost(visibility);
  const refreshes: number[] = [];
  const controller = createAccountsProjectionRefresh({
    host,
    isAuthenticated: () => true,
    refresh: () => { refreshes.push(1); },
  });
  controller.activate();
  assert.equal(ACCOUNTS_PROJECTION_REFRESH_MS, 15_000);
  assert.equal(intervalArmed(), true);
  assert.deepEqual(refreshes, []);
  tick();
  assert.equal(refreshes.length, 1);
});

test("hidden documents stop the timer; becoming visible refreshes once and rearms", () => {
  const visibility = { state: "visible" as DocumentVisibilityState };
  const { host, fireVisibility, tick, intervalArmed } = installHost(visibility);
  const refreshes: number[] = [];
  const controller = createAccountsProjectionRefresh({
    host,
    isAuthenticated: () => true,
    refresh: () => { refreshes.push(1); },
  });
  controller.activate();
  visibility.state = "hidden";
  fireVisibility();
  assert.equal(intervalArmed(), false);
  tick();
  assert.deepEqual(refreshes, []);

  visibility.state = "visible";
  fireVisibility();
  assert.equal(intervalArmed(), true);
  assert.equal(refreshes.length, 1);
});

test("logout disarms the timer so later ticks cannot refresh", () => {
  const visibility = { state: "visible" as DocumentVisibilityState };
  let authenticated = true;
  const { host, tick, intervalArmed } = installHost(visibility);
  const refreshes: number[] = [];
  const controller = createAccountsProjectionRefresh({
    host,
    isAuthenticated: () => authenticated,
    refresh: () => { refreshes.push(1); },
  });
  controller.activate();
  authenticated = false;
  controller.onSessionDropped();
  assert.equal(intervalArmed(), false);
  tick();
  assert.deepEqual(refreshes, []);
});

test("deactivate unbinds visibility and ignores later ticks", () => {
  const visibility = { state: "visible" as DocumentVisibilityState };
  const { host, fireVisibility, tick, intervalArmed } = installHost(visibility);
  const refreshes: number[] = [];
  const controller = createAccountsProjectionRefresh({
    host,
    isAuthenticated: () => true,
    refresh: () => { refreshes.push(1); },
  });
  controller.activate();
  controller.deactivate();
  assert.equal(intervalArmed(), false);
  tick();
  visibility.state = "hidden";
  fireVisibility();
  visibility.state = "visible";
  fireVisibility();
  assert.deepEqual(refreshes, []);
});
