import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const gatewayPort = (() => {
  const raw = process.env.OCG_GATEWAY_PORT?.trim();
  if (!raw) return 9042;
  if (!/^\d+$/.test(raw)) throw new Error("OCG_GATEWAY_PORT must be an integer from 1 to 65535");
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error("OCG_GATEWAY_PORT must be an integer from 1 to 65535");
  }
  return port;
})();

export default defineConfig({
  base: "/dashboard/",
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  server: {
    port: 30001,
    strictPort: true,
    host: "127.0.0.1",
    proxy: {
      "/dashboard/api": {
        target: `http://127.0.0.1:${gatewayPort}`,
        ws: true,
      },
    },
    watch: {
      ignored: ["**/target/**", "**/target-agent/**", "**/src-tauri/target/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  // @novnc/novnc@1.7 uses top-level await in core/util/browser.js.
  esbuild: {
    target: "es2022",
    supported: { "top-level-await": true },
  },
  optimizeDeps: {
    esbuildOptions: {
      target: "es2022",
      supported: { "top-level-await": true },
    },
  },
  build: {
    target: "es2022",
    minify: "esbuild",
    sourcemap: !!process.env.TAURI_DEBUG,
    rollupOptions: {
      output: {
        manualChunks(id) {
          // Locale message modules resolve to their own per-locale chunks via
          // the dynamic imports in src/i18n; do not merge them back into one.
          if (id.includes("/node_modules/@vicons/")) return "icons";
          if (id.includes("/node_modules/vue/") || id.includes("/node_modules/@vue/")) return "vue";
          // naive-ui is intentionally NOT grouped: the entry (App.vue) only
          // uses the shell components, and a forced single chunk made every
          // first screen preload the entire library (~1.7 MB). Letting Rollup
          // follow the import graph splits it across the lazy view chunks.
        },
      },
    },
  },
});
