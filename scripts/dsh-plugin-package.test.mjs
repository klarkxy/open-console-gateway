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
        const apis = input.api ?? {};
        const single = typeof apis.streamSimple === "function";
        if (!input.auth?.apiKey || single || !input.models.every((model) => model.provider === input.id && model.api && model.baseUrl)) {
          throw new Error("invalid pi-ai provider contract");
        }
        const provider = {
          ...input,
          getModels: () => input.models,
          streamSimple(model, context, options) {
            const implementation = apis[model.api];
            if (!implementation?.streamSimple) {
              const error = new Error("no API implementation for " + model.api);
              error.code = "NO_API";
              throw error;
            }
            return implementation.streamSimple(model, context, options);
          },
        };
        globalThis.__ocgProviders.push(provider);
        return provider;
      }
    `,
    "dist/api/openai-completions.lazy.js": `
      export function openAICompletionsApi() {
        return { stream() {}, streamSimple(model) { globalThis.__ocgSent.push(model.api); return { api: model.api }; } };
      }
    `,
    "dist/api/openai-responses.lazy.js": `
      export function openAIResponsesApi() {
        return { stream() {}, streamSimple(model) { globalThis.__ocgSent.push(model.api); return { api: model.api }; } };
      }
    `,
    "dist/api/anthropic-messages.lazy.js": `
      export function anthropicMessagesApi() {
        return { stream() {}, streamSimple(model) { globalThis.__ocgSent.push(model.api); return { api: model.api }; } };
      }
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
        providerInfo(provider) {
          const profile = this.config.profiles().get(provider);
          return { id: provider, name: profile?.displayName ?? provider };
        }
        async listModels(provider) {
          const profile = this.config.profiles().get(provider);
          globalThis.__ocgProfile = { api: profile.api ?? null, baseURL: profile.baseURL };
          return profile.piProvider.models.map((model) => ({ provider, id: model.id, name: model.name, api: model.api, inputModalities: model.input }));
        }
        modelInfo(provider, model) {
          const profile = this.config.profiles().get(provider);
          const resolved = profile.piProvider.models.find((item) => item.id === model);
          const levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"].filter((level) => {
            if (!resolved?.reasoning) return false;
            const mapped = resolved.thinkingLevelMap?.[level];
            if (mapped === null) return false;
            if (level === "xhigh" || level === "max") return mapped !== undefined;
            return true;
          });
          const reasoning = resolved?.reasoning ? {
            efforts: levels.map((level) => ({ id: level, name: level.charAt(0).toUpperCase() + level.slice(1) })),
          } : null;
          globalThis.__ocgPiModelInfo ??= {};
          globalThis.__ocgPiModelInfo[model] = reasoning;
          return {
            provider,
            id: model,
            name: resolved?.name ?? model,
            inputModalities: resolved?.input ? [...resolved.input] : ["text"],
            ...(resolved?.contextWindow ? { context: { contextWindow: resolved.contextWindow } } : {}),
            ...(reasoning ? { reasoning } : {}),
          };
        }
        async resolveModel(provider, model) {
          const profile = this.config.profiles().get(provider);
          const failure = profile.modelErrors.get(model);
          if (failure !== undefined) throw new Error(failure);
          return this.modelInfo(provider, model);
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

test("generated DSH plugin dispatches v2 protocols and does not send rejected models", async () => {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-plugin-"));
  try {
    const bootstrap = join(root, "credential-handoff");
    await writeFile(bootstrap, "ocg-test-key");
    const plugin = await writeRenderedPlugin(root, bootstrap);
    await writePluginRuntime(root);
    const catalog = {
      object: "list",
      data: [
        { id: 42 },
        { id: "legacy-model" },
        { id: "schema-1", ocg: { schemaVersion: 1, protocols: { preferred: "chat_completions", supported: ["chat_completions"] } } },
        {
          id: "model-a",
          ocg: {
            schemaVersion: 2,
            status: "declared",
            protocols: { preferred: "chat_completions", supported: ["chat_completions", "responses"] },
            inputModalities: ["text"],
          },
        },
        {
          id: "org/model-b",
          ocg: {
            schemaVersion: 2,
            protocols: { preferred: "responses", supported: ["responses"] },
            reasoning: true,
            reasoningEfforts: { high: "high" },
          },
        },
        {
          id: "mimo-v2.6-flash",
          ocg: {
            schemaVersion: 2,
            protocols: { preferred: "messages", supported: ["messages"] },
            inputModalities: ["text"],
            reasoning: true,
            reasoningEfforts: { high: "high" },
          },
        },
        {
          id: "minimax-m3.1",
          ocg: {
            schemaVersion: 2,
            protocols: { preferred: "messages", supported: ["messages"] },
            inputModalities: ["text"],
            reasoning: true,
            contextWindow: 204800,
          },
        },
      ],
    };
    const result = await runEntry(
      root,
      `
        import { access } from "node:fs/promises";
        let stored;
        let adapter;
        let registeredProviders;
        const requests = [];
        globalThis.__ocgSent = [];
        globalThis.__ocgProviders = [];
        globalThis.__ocgPiModelInfo = {};
        globalThis.fetch = async (url) => {
          requests.push(String(url));
          return { ok: true, status: 200, async json() { return ${JSON.stringify(catalog)}; } };
        };
        const credentials = {
          async set(ref, value) { stored = { ref, value }; },
          async resolve(ref) { return ref === stored?.ref ? { value: stored.value } : undefined; },
        };
        const ctx = {
          get(name) { return name === "credentials" ? credentials : undefined; },
          llm: { registerAdapter(providers, value) {
            registeredProviders = providers;
            adapter = value;
          } },
        };
        const plugin = await import(${JSON.stringify(fileUrl(plugin))});
        await plugin.apply(ctx);
        const routes = {};
        const prepareErrors = {};
        for (const provider of registeredProviders) {
          routes[provider] = {
            info: adapter.providerInfo(provider),
            models: await adapter.listModels(provider),
          };
          try {
            routes[provider].prepared = await adapter.prepareCall(provider, "model-a");
            routes[provider].responsesModel = await adapter.prepareCall(provider, "org/model-b");
            routes[provider].mimo = await adapter.prepareCall(provider, "mimo-v2.6-flash");
            routes[provider].minimax = await adapter.prepareCall(provider, "minimax-m3.1");
          } catch (error) {
            prepareErrors.modelA = error instanceof Error ? error.message : String(error);
          }
          try {
            await adapter.prepareCall(provider, "legacy-model");
            prepareErrors.legacy = "";
          } catch (error) {
            prepareErrors.legacy = error instanceof Error ? error.message : String(error);
          }
          try {
            await adapter.prepareCall(provider, "schema-1");
            prepareErrors.schema1 = "";
          } catch (error) {
            prepareErrors.schema1 = error instanceof Error ? error.message : String(error);
          }
        }
        const piProvider = globalThis.__ocgProviders.at(-1);
        const rejected = piProvider.getModels().find((model) => model.id === "legacy-model");
        const messages = piProvider.getModels().find((model) => model.id === "mimo-v2.6-flash");
        let rejectedStream = "";
        try {
          piProvider.streamSimple(rejected, { messages: [] }, {});
        } catch (error) {
          rejectedStream = error instanceof Error ? error.code ?? error.message : String(error);
        }
        const sentBeforeMessages = globalThis.__ocgSent.slice();
        const messagesCall = piProvider.streamSimple(messages, { messages: [] }, {});
        let messagesLevel = "";
        try {
          piProvider.streamSimple(messages, { messages: [] }, { reasoning: "high" });
        } catch (error) {
          messagesLevel = error instanceof Error ? error.code ?? error.message : String(error);
        }
        let bootstrapExists = true;
        try { await access(${JSON.stringify(bootstrap)}); } catch { bootstrapExists = false; }
        process.stdout.write(JSON.stringify({
          stored,
          registeredProviders,
          routes,
          prepareErrors,
          bootstrapExists,
          profile: globalThis.__ocgProfile,
          apiKeys: Object.keys(piProvider.api).sort(),
          apiIsSingleStream: typeof piProvider.api.streamSimple === "function",
          rejectedStream,
          sentBeforeMessages,
          messagesCall,
          messagesLevel,
          sent: globalThis.__ocgSent,
          requests,
          piMenus: globalThis.__ocgPiModelInfo,
        }));
      `,
    );
    assert.deepEqual(result.stored, { ref: "OCG_GATEWAY_KEY", value: "ocg-test-key" });
    assert.deepEqual(result.registeredProviders, ["ocg"]);
    assert.equal(result.profile.api, null);
    assert.equal(result.profile.baseURL, "http://127.0.0.1:9042/v1");
    assert.deepEqual(result.apiKeys, ["anthropic-messages", "openai-completions", "openai-responses"]);
    assert.equal(result.apiIsSingleStream, false);
    assert.equal(result.prepareErrors.modelA, undefined);
    assert.match(result.prepareErrors.legacy, /schema/i);
    assert.match(result.prepareErrors.schema1, /schema/i);
    assert.equal(result.rejectedStream, "NO_API");
    assert.deepEqual(result.sentBeforeMessages, []);
    assert.equal(result.messagesCall.api, "anthropic-messages");
    assert.equal(result.messagesLevel, "OCG_MESSAGES_REASONING_UNDECLARED");
    assert.deepEqual(result.sent, ["anthropic-messages"]);
    assert.ok(result.requests.every((url) => url.endsWith("/v1/models")));
    const models = result.routes.ocg.models;
    assert.deepEqual(models.map(({ id }) => id), [
      "legacy-model", "schema-1", "model-a", "org/model-b", "mimo-v2.6-flash", "minimax-m3.1",
    ]);
    assert.equal(models.find(({ id }) => id === "model-a").api, "openai-completions");
    assert.equal(models.find(({ id }) => id === "org/model-b").api, "openai-responses");
    assert.equal(models.find(({ id }) => id === "mimo-v2.6-flash").api, "anthropic-messages");
    assert.equal(models.find(({ id }) => id === "mimo-v2.6-flash").ocg.reasoning, true);
    assert.deepEqual(models.find(({ id }) => id === "mimo-v2.6-flash").ocg.reasoningEfforts, { high: "high" });
    assert.deepEqual(result.piMenus["mimo-v2.6-flash"].efforts, []);
    assert.deepEqual(result.piMenus["minimax-m3.1"].efforts, []);
    assert.equal(Object.hasOwn(result.routes.ocg.mimo.model, "reasoning"), false);
    assert.equal(result.routes.ocg.mimo.model.ocg.reasoning, true);
    assert.deepEqual(result.routes.ocg.mimo.model.ocg.reasoningEfforts, { high: "high" });
    assert.equal(Object.hasOwn(result.routes.ocg.minimax.model, "reasoning"), false);
    assert.equal(result.routes.ocg.minimax.model.ocg.reasoning, true);
    assert.equal(Object.hasOwn(result.routes.ocg.minimax.model.ocg, "reasoningEfforts"), false);
    assert.deepEqual(result.piMenus["org/model-b"].efforts, [{ id: "high", name: "High" }]);
    assert.deepEqual(result.routes.ocg.responsesModel.model.reasoning, result.piMenus["org/model-b"]);
    assert.deepEqual(models.find(({ id }) => id === "model-a").inputModalities, ["text"]);
    assert.equal(result.routes.ocg.prepared.model.ocg.status, "declared");
    assert.equal(result.routes.ocg.prepared.model.ocg.protocols.preferred, "chat_completions");
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
