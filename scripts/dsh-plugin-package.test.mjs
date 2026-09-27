import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const sourceRoot = new URL("../integrations/dsh-plugin/", import.meta.url);

async function writePackage(root, name, files) {
  const packageRoot = join(root, "node_modules", ...name.split("/"));
  await mkdir(packageRoot, { recursive: true });
  await writeFile(
    join(packageRoot, "package.json"),
    JSON.stringify({ name, version: "0.0.0", type: "module" }),
  );
  for (const [relative, contents] of Object.entries(files)) {
    const path = join(packageRoot, relative);
    await mkdir(join(path, ".."), { recursive: true });
    await writeFile(path, contents);
  }
}

async function writePluginRuntime(root) {
  await writePackage(root, "@earendil-works/pi-ai", {
    "dist/index.js": `
      export class InMemoryCredentialStore {}
      export function createProvider(input) {
        if (!input.auth?.apiKey || input.models.some((model) => model.provider !== input.id || !model.api || !model.baseUrl)) {
          throw new Error("invalid pi-ai provider contract");
        }
        return input;
      }
    `,
    "dist/api/openai-completions.lazy.js": `
      export function openAICompletionsApi() { return {}; }
    `,
  });
  await writePackage(root, "@deepseek-ai/dsh-llm", {
    "lib/index.js": `
      export class LlmError extends Error { constructor(message, code) { super(message); this.code = code; } }
      export function assertUsableApiKey(value) { return value; }
      export function resolveRetryPolicy() { return { mode: "normal", maxRetries: 0 }; }
    `,
  });
  await writePackage(root, "@deepseek-ai/dsh-llm-pi-ai", {
    "lib/index.js": `
      export class PiAiAdapter {
        constructor(config) { this.config = config; }
        async listModels(provider) {
          const profile = this.config.profiles().get(provider);
          return profile.piProvider.models.map((model) => ({ provider, id: model.id, name: model.name }));
        }
        async resolveModel(provider, model) {
          const profile = this.config.profiles().get(provider);
          const failure = profile.modelErrors.get(model);
          if (failure !== undefined) throw new Error(failure);
          return { provider, id: model, name: model };
        }
        async prepareCall(provider, model) { return { model: await this.resolveModel(provider, model) }; }
      }
    `,
  });
}

async function writeRenderedPlugin(root, bootstrap) {
  const template = await readFile(new URL("index.js", sourceRoot), "utf8");
  const rendered = template
    .replaceAll("__OCG_GATEWAY_V1_URL__", "http://127.0.0.1:9042/v1")
    .replaceAll(
      "__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__",
      JSON.stringify(bootstrap),
    );
  const plugin = join(root, "plugin.mjs");
  await writeFile(plugin, rendered);
  await writeFile(join(root, "model-catalog.js"), await readFile(new URL("model-catalog.js", sourceRoot)));
  return plugin;
}

function fileUrl(path) {
  return new URL(`file:///${path.replaceAll("\\", "/")}`).href;
}

async function runEntry(root, source) {
  const entry = join(root, "entry.mjs");
  await writeFile(entry, source);
  const { stdout, stderr } = await execFileAsync(process.execPath, [entry], {
    cwd: root,
    windowsHide: true,
  });
  assert.equal(stderr, "");
  return JSON.parse(stdout);
}

function claimPath(bootstrap, token) {
  return `${bootstrap}.claimed-${token}`;
}

function applyOnlySource(plugin, bootstrap, extra = "") {
  return `
    import { readdir, readFile, writeFile } from "node:fs/promises";
    import { basename, dirname } from "node:path";
    let stored;
    globalThis.fetch = async () => ({ ok: true, status: 200, async json() { return { object: "list", data: [] }; } });
    ${extra}
    const ctx = {
      get(name) { return name === "credentials" ? credentials : undefined; },
      llm: { registerAdapter() {} },
    };
    const plugin = await import(${JSON.stringify(fileUrl(plugin))});
    let applyError = "";
    try {
      await plugin.apply(ctx);
    } catch (error) {
      applyError = error instanceof Error ? error.message : String(error);
    }
    const names = await readdir(${JSON.stringify(dirname(bootstrap))});
    const prefix = ${JSON.stringify(`${basename(bootstrap)}.claimed-`)};
    let live = null;
    try { live = await readFile(${JSON.stringify(bootstrap)}, "utf8"); } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    process.stdout.write(JSON.stringify({
      stored,
      live,
      claims: names.filter((name) => name.startsWith(prefix)).sort(),
      applyError,
    }));
  `;
}

test("generated DSH plugin imports its one-time Key and prepares every live OCG model", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(bootstrap, "ocg-test-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      `
        import { access } from "node:fs/promises";
        let stored;
        let adapter;
        globalThis.fetch = async (url, init) => ({
          ok: true,
          status: 200,
          async json() {
            return { object: "list", data: [{ id: "model-a" }, { id: "org/model-b" }] };
          },
          requested: { url, init },
        });
        const credentials = {
          async set(ref, value) { stored = { ref, value }; },
          async resolve(ref) { return ref === stored?.ref ? { value: stored.value } : undefined; },
        };
        const ctx = {
          get(name) { return name === "credentials" ? credentials : undefined; },
          llm: { registerAdapter(_providers, value) { adapter = value; } },
        };
        const plugin = await import(${JSON.stringify(fileUrl(plugin))});
        await plugin.apply(ctx);
        const models = await adapter.listModels("open-console-gateway");
        const prepared = await adapter.prepareCall("open-console-gateway", "model-a");
        let bootstrapExists = true;
        try { await access(${JSON.stringify(bootstrap)}); } catch { bootstrapExists = false; }
        process.stdout.write(JSON.stringify({ stored, models, prepared, bootstrapExists }));
      `,
    );
    assert.deepEqual(result.stored, {
      ref: "OCG_GATEWAY_KEY",
      value: "ocg-test-key",
    });
    assert.deepEqual(
      result.models.map(({ id }) => id),
      ["model-a", "org/model-b"],
    );
    assert.equal(result.prepared.model.ocg.status, "legacy");
    const { ocg: _metadata, ...preparedModel } = result.prepared.model;
    assert.deepEqual(preparedModel, {
      provider: "open-console-gateway",
      id: "model-a",
      name: "model-a",
    });
    assert.equal(result.bootstrapExists, false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("an older consumer does not delete a newer handoff written during credentials.set", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-race-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(bootstrap, "older-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      `
        import { readdir, writeFile, readFile } from "node:fs/promises";
        import { dirname } from "node:path";
        let stored;
        globalThis.fetch = async () => ({ ok: true, status: 200, async json() { return { object: "list", data: [] }; } });
        const credentials = {
          async set(ref, value) {
            stored = { ref, value };
            await writeFile(${JSON.stringify(bootstrap)}, "newer-key");
          },
          async resolve() { return undefined; },
        };
        const ctx = {
          get(name) { return name === "credentials" ? credentials : undefined; },
          llm: { registerAdapter() {} },
        };
        const plugin = await import(${JSON.stringify(fileUrl(plugin))});
        await plugin.apply(ctx);
        const live = await readFile(${JSON.stringify(bootstrap)}, "utf8");
        const names = await readdir(${JSON.stringify(dirname(bootstrap))});
        process.stdout.write(JSON.stringify({
          stored,
          live,
          claims: names.filter((name) => name.includes(".claimed-")),
        }));
      `,
    );
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "older-key" });
    assert.equal(result.live, "newer-key");
    assert.deepEqual(result.claims, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("credential storage failure restores the claimed handoff for retry", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-retry-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(bootstrap, "retry-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      `
        import { access, readdir, readFile } from "node:fs/promises";
        import { dirname } from "node:path";
        let stored;
        let attempts = 0;
        let firstError = "";
        globalThis.fetch = async () => ({ ok: true, status: 200, async json() { return { object: "list", data: [] }; } });
        const credentials = {
          async set(ref, value) {
            attempts += 1;
            if (attempts === 1) throw new Error("credential storage failed");
            stored = { ref, value };
          },
          async resolve() { return stored; },
        };
        const ctx = {
          get(name) { return name === "credentials" ? credentials : undefined; },
          llm: { registerAdapter() {} },
        };
        const plugin = await import(${JSON.stringify(fileUrl(plugin))});
        try {
          await plugin.apply(ctx);
        } catch (error) {
          firstError = error instanceof Error ? error.message : String(error);
        }
        const afterFailure = await readFile(${JSON.stringify(bootstrap)}, "utf8");
        const namesAfterFailure = await readdir(dirname(${JSON.stringify(bootstrap)}));
        const prefix = ${JSON.stringify(`${basename(bootstrap)}.claimed-`)};
        await plugin.apply(ctx);
        let bootstrapExists = true;
        try { await access(${JSON.stringify(bootstrap)}); } catch { bootstrapExists = false; }
        const namesAfterRetry = await readdir(${JSON.stringify(dirname(bootstrap))});
        process.stdout.write(JSON.stringify({
          firstError,
          afterFailure,
          stored,
          attempts,
          bootstrapExists,
          claimsAfterFailure: namesAfterFailure.filter((name) => name.startsWith(prefix)),
          claimsAfterRetry: namesAfterRetry.filter((name) => name.startsWith(prefix)),
        }));
      `,
    );
    assert.equal(result.firstError, "credential storage failed");
    assert.equal(result.afterFailure, "retry-key");
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "retry-key" });
    assert.equal(result.attempts, 2);
    assert.equal(result.bootstrapExists, false);
    assert.deepEqual(result.claimsAfterFailure, []);
    assert.deepEqual(result.claimsAfterRetry, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a claim-only crash remnant is consumed and leaves no claims", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-claim-only-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(claimPath(bootstrap, "0000000000001000-aa"), "crash-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      applyOnlySource(
        plugin,
        bootstrap,
        `const credentials = { async set(ref, value) { stored = { ref, value }; }, async resolve() { return stored; } };`,
      ),
    );
    assert.equal(result.applyError, "");
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "crash-key" });
    assert.equal(result.live, null);
    assert.deepEqual(result.claims, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("live plus a stale claim consumes the live Key and leaves no claims", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-live-stale-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(bootstrap, "authoritative-key");
    await writeFile(claimPath(bootstrap, "0000000000001000-aa"), "stale-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      applyOnlySource(
        plugin,
        bootstrap,
        `const credentials = { async set(ref, value) { stored = { ref, value }; }, async resolve() { return stored; } };`,
      ),
    );
    assert.equal(result.applyError, "");
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "authoritative-key" });
    assert.equal(result.live, null);
    assert.deepEqual(result.claims, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("multiple stale claims consume the newest and leave no claims", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-multi-claim-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(claimPath(bootstrap, "0000000000001000-aa"), "older-stale");
    await writeFile(claimPath(bootstrap, "0000000000002000-bb"), "newest-stale");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const result = await runEntry(
      root,
      applyOnlySource(
        plugin,
        bootstrap,
        `const credentials = { async set(ref, value) { stored = { ref, value }; }, async resolve() { return stored; } };`,
      ),
    );
    assert.equal(result.applyError, "");
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "newest-stale" });
    assert.equal(result.live, null);
    assert.deepEqual(result.claims, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
