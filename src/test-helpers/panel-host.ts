import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import vue from "@vitejs/plugin-vue";
import { build, type Plugin } from "vite";
import type { Component } from "vue";

/**
 * Vite bundles of the real quota and billing panels for the custom Vue host.
 * Naive UI and icons are the host stand-ins. Vue and Pinia stay on the test
 * process copies so props, stores, and fetch recording stay live.
 * Build output lives under the OS temp directory; callers delete `directory`.
 */

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const naiveStub = path.join(projectRoot, "src/test-helpers/panel-naive-stub.ts");
const iconStub = path.join(projectRoot, "src/test-helpers/panel-icon-stub.ts");
export interface QuotaBundle {
  AccountUsageEditor: Component;
  ProviderQuotaSummary: Component;
}

export interface ControlPlaneStore {
  expectation(): { expectedRevision: number; processGeneration: number };
  sync(tokens: { revision: number; processGeneration: number }): void;
}

export interface BillingStore {
  load(accountId: string, binding: string): Promise<void>;
}

export interface RetirementBundle {
  BillingPanel: Component;
  CreditCalibrationEditor: Component;
  CreditMeterPanel: Component;
  OfficialApiPanel: Component;
  useBillingStore: () => BillingStore;
  useControlPlaneStore: () => ControlPlaneStore;
}

export interface LoadedBundle<T> {
  bundle: T;
  directory: string;
}

export interface RecordedCall {
  body: Record<string, unknown> | null;
  method: string;
  url: string;
}

export function installHostElementMethods(): void {
  const define = (name: string, value: (...args: unknown[]) => unknown) => {
    if (Object.prototype.hasOwnProperty(name)) return;
    Object.defineProperty(Object.prototype, name, { configurable: true, writable: true, value });
  };
  define("addEventListener", () => undefined);
  define("removeEventListener", () => undefined);
  define("dispatchEvent", () => true);
  define("getRootNode", () => ({}));
  define("querySelector", () => null);
  define("querySelectorAll", () => []);
  define("setAttribute", () => undefined);
}

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

export function installRecordingFetch(
  respond: (call: RecordedCall) => Response,
): RecordedCall[] {
  const calls: RecordedCall[] = [];
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const call = recordCall(input, init);
    calls.push(call);
    return respond(call);
  };
  return calls;
}

export function loadQuotaBundle(): Promise<LoadedBundle<QuotaBundle>> {
  return loadBundle<QuotaBundle>(`
    import AccountUsageEditor from ${specifier("src/components/AccountUsageEditor.vue")};
    import ProviderQuotaSummary from ${specifier("src/components/ProviderQuotaSummary.vue")};
    export { AccountUsageEditor, ProviderQuotaSummary };
  `);
}

export function loadRetirementBundle(): Promise<LoadedBundle<RetirementBundle>> {
  return loadBundle<RetirementBundle>(`
    import BillingPanel from ${specifier("src/components/BillingPanel.vue")};
    import CreditCalibrationEditor from ${specifier("src/components/CreditCalibrationEditor.vue")};
    import CreditMeterPanel from ${specifier("src/components/CreditMeterPanel.vue")};
    import OfficialApiPanel from ${specifier("src/components/OfficialApiPanel.vue")};
    export { useBillingStore } from ${specifier("src/stores/billing.ts")};
    export { useControlPlaneStore } from ${specifier("src/stores/controlPlane.ts")};
    export { BillingPanel, CreditCalibrationEditor, CreditMeterPanel, OfficialApiPanel };
  `);
}

function specifier(relativePath: string): string {
  return JSON.stringify(path.join(projectRoot, relativePath).replaceAll("\\", "/"));
}

function moduleUrl(name: string): string {
  return import.meta.resolve(name);
}

function isRuntimePackage(id: string): boolean {
  if (id === "vue" || id === "pinia") return true;
  const normalized = id.replaceAll("\\", "/");
  return normalized.includes("/node_modules/vue/") || normalized.includes("/node_modules/pinia/");
}

function hostAliasPlugin(): Plugin {
  return {
    name: "panel-host-alias",
    enforce: "pre",
    resolveId(source: string) {
      if (source === "naive-ui") return naiveStub;
      if (source === "@vicons/antd") return iconStub;
      return null;
    },
    load(id: string) {
      if (id.includes("type=style")) return "";
      return null;
    },
  };
}

function recordCall(input: RequestInfo | URL, init?: RequestInit): RecordedCall {
  const url = typeof input === "string"
    ? input
    : input instanceof URL
      ? input.href
      : input.url;
  const method = (init?.method ?? (typeof input === "string" || input instanceof URL ? "GET" : input.method)).toUpperCase();
  return { body: readBody(init?.body), method, url };
}

function readBody(body: BodyInit | null | undefined): Record<string, unknown> | null {
  if (typeof body !== "string" || body.trim() === "") return null;
  const parsed: unknown = JSON.parse(body);
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  return parsed as Record<string, unknown>;
}

async function loadBundle<T>(source: string): Promise<LoadedBundle<T>> {
  const directory = await mkdtemp(path.join(tmpdir(), "ocg-panel-"));
  const entry = path.join(directory, "entry.mjs");
  const outDir = path.join(directory, "out");
  const vueUrl = moduleUrl("vue");
  const piniaUrl = moduleUrl("pinia");
  try {
    await writeFile(entry, source, "utf8");
    await build({
      configFile: false,
      logLevel: "silent",
      root: projectRoot,
      plugins: [hostAliasPlugin(), vue()],
      cacheDir: path.join(directory, "vite-cache"),
      build: {
        emptyOutDir: true,
        minify: false,
        reportCompressedSize: false,
        target: "esnext",
        lib: {
          entry,
          fileName: () => "panel.mjs",
          formats: ["es"],
        },
        outDir,
        rollupOptions: {
          external: isRuntimePackage,
          output: {
            paths: (id: string) => {
              if (id === "pinia" || id.replaceAll("\\", "/").includes("/node_modules/pinia/")) return piniaUrl;
              if (id === "vue" || id.replaceAll("\\", "/").includes("/node_modules/vue/")) return vueUrl;
              return id;
            },
          },
        },
      },
    });
    const imported: unknown = await import(pathToFileURL(path.join(outDir, "panel.mjs")).href);
    return { bundle: imported as T, directory };
  } catch (error) {
    await rm(directory, { force: true, recursive: true });
    throw error;
  }
}
