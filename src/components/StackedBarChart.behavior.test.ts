import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { before, after, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { ssrContextKey, type Component } from "vue";
import { createTestWindow, createVueHostRenderer, installTestWindow, text, walkHostNodes, type HostNode } from "../test-helpers/vue-host-runtime.ts";
import { formatTokens } from "../utils/format.ts";

let dir = "";
let Chart: Component;
before(async () => {
  const artifacts = path.join(process.cwd(), ".artifacts", "dashboard-projection");
  await mkdir(artifacts, { recursive: true });
  dir = await mkdtemp(path.join(artifacts, "chart-"));
  await build({ configFile: false, logLevel: "silent", plugins: [vue(), {
    name: "chart-test-styles", load(id) { if (id.endsWith(".css") || id.includes("vue&type=style")) return ""; return null; },
  }], build: { target: "esnext", outDir: dir, emptyOutDir: true,
    lib: { entry: path.resolve("src/components/StackedBarChart.vue"), formats: ["es"], fileName: () => "chart.mjs" },
    rollupOptions: { external: ["vue"] },
  } });
  Chart = (await import(pathToFileURL(path.join(dir, "chart.mjs")).href)).default;
});
after(async () => { if (dir) await rm(dir, { recursive: true, force: true }); });

function classes(node: HostNode): string[] { return typeof node.props.class === "string" ? node.props.class.split(/\s+/) : []; }

test("chart uses server UTC days, daily totals, and shared model palette order despite browser date", () => {
  const window = createTestWindow(); installTestWindow(window);
  const renderer = createVueHostRenderer(); const root: HostNode = { type: "root", children: [], props: {} };
  const app = renderer.createApp(Chart, {
    days: 2, totalTokens: 999,
    modelTotals: [{ model: "b", tokens: 2 }, { model: "a", tokens: 1 }],
    series: [{ date: "2000-01-01", totalTokens: 0, models: [] },
      { date: "2000-01-02", totalTokens: 10, models: [{ model: "a", tokens: 1 }, { model: "b", tokens: 2 }] }],
  });
  app.provide(ssrContextKey, { modules: new Set<string>() }); app.mount(root);
  try {
    const nodes = walkHostNodes(root); const bars = nodes.filter(node => classes(node).includes("bar-col"));
    assert.equal(bars.length, 2); assert.equal(bars[0]!.props.tabindex, -1); assert.equal(bars[1]!.props.tabindex, 0);
    assert.equal(String(bars[1]!.props["aria-label"]).includes("2000"), true);
    assert.equal(String(bars[1]!.props["aria-label"]).includes(formatTokens(10)), true);
    const segments = walkHostNodes(bars[1]!).filter(node => classes(node).includes("bar-seg"));
    assert.equal(segments.length, 2);
    assert.match(String(segments[0]!.props.fill), /bar-grad-0-/); assert.match(String(segments[1]!.props.fill), /bar-grad-1-/);
    assert.equal(Number(segments[0]!.props.height), Number(segments[1]!.props.height) * 2);
    const desc = nodes.find(node => node.type === "desc"); assert.ok(desc); assert.equal(text(desc).includes(formatTokens(999)), true);
    const ticks = nodes.filter(node => node.type === "text" && node.props["text-anchor"] === "end");
    assert.equal(text(ticks[ticks.length - 1]!), formatTokens(10));
  } finally { app.unmount(); }
});
