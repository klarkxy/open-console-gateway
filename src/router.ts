import { defineAsyncComponent } from "vue";
import type { Component } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import type { RouteRecordRaw, Router } from "vue-router";
import RoutePending from "./components/RoutePending.vue";

// Hash history: the dashboard is served by the embedded host (and the Tauri
// webview) from a single fixed path, so path-based deep links would 404 on
// reload. Query strings live inside the hash route, never on the request URL.
type ViewLoader = () => Promise<{ default: Component }>;

const viewLoaders = {
  dashboard: () => import("./views/Dashboard.vue"),
  keys: () => import("./views/Keys.vue"),
  accounts: () => import("./views/Accounts.vue"),
  providers: () => import("./views/Providers.vue"),
  aliases: () => import("./views/Aliases.vue"),
  applications: () => import("./views/Applications.vue"),
  logs: () => import("./views/Logs.vue"),
  settings: () => import("./views/Settings.vue"),
  cpa: () => import("./views/Cpa.vue"),
  browser: () => import("./views/BrowserSession.vue"),
} satisfies Record<string, ViewLoader>;

function asyncView(loader: ViewLoader) {
  return defineAsyncComponent({ loader, loadingComponent: RoutePending, delay: 150 });
}

const routes: RouteRecordRaw[] = [
  { path: "/", redirect: { name: "dashboard" } },
  { path: "/dashboard", name: "dashboard", component: asyncView(viewLoaders.dashboard) },
  { path: "/keys", name: "keys", component: asyncView(viewLoaders.keys) },
  { path: "/accounts", name: "accounts", component: asyncView(viewLoaders.accounts) },
  { path: "/providers", name: "providers", component: asyncView(viewLoaders.providers) },
  { path: "/aliases", name: "aliases", component: asyncView(viewLoaders.aliases) },
  { path: "/applications", name: "applications", component: asyncView(viewLoaders.applications) },
  { path: "/logs", name: "logs", component: asyncView(viewLoaders.logs) },
  { path: "/settings", name: "settings", component: asyncView(viewLoaders.settings) },
  { path: "/cpa", name: "cpa", component: asyncView(viewLoaders.cpa) },
  // The remote browser takes over the whole window: no shell, no KeepAlive.
  { path: "/browser", name: "browser", component: asyncView(viewLoaders.browser), meta: { bare: true } },
  { path: "/:pathMatch(.*)*", redirect: { name: "dashboard" } },
];

// The noVNC browser view stays on demand: it is rare and its chunk is large.
const prefetchLoaders: ViewLoader[] = [
  viewLoaders.keys,
  viewLoaders.accounts,
  viewLoaders.providers,
  viewLoaders.aliases,
  viewLoaders.applications,
  viewLoaders.logs,
  viewLoaders.settings,
  viewLoaders.cpa,
];

/**
 * Once the shell is idle, warm the remaining view chunks so the first visit
 * to each page does not wait on the network.
 */
export function prefetchAppViews(): void {
  const schedule = window.requestIdleCallback
    ?? ((callback: () => void) => { window.setTimeout(callback, 2000); });
  schedule(() => {
    for (const loader of prefetchLoaders) void loader();
  });
}

export function createAppRouter(): Router {
  return createRouter({ history: createWebHashHistory(), routes });
}
