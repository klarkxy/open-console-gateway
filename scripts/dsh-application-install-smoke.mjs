#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import {
  access,
  cp,
  mkdtemp,
  mkdir,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(repo, "integrations", "dsh-plugin");
const packageName = "@open-console-gateway/dsh-plugin";
const secret = "ocg-isolated-smoke-key";

function dshBin() {
  if (process.env.OCG_DSH_SMOKE_BIN) return resolve(process.env.OCG_DSH_SMOKE_BIN);
  const appData = process.env.APPDATA;
  if (!appData) throw new Error("APPDATA is unavailable; cannot locate the installed DSH CLI");
  return join(appData, "npm", "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
}

async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

async function runNode(args, options) {
  const result = await execFileAsync(process.execPath, args, {
    encoding: "utf8",
    timeout: 180_000,
    windowsHide: true,
    maxBuffer: 2 * 1024 * 1024,
    ...options,
  });
  return result;
}

async function main() {
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-smoke-"));
  if (/\s/.test(root)) throw new Error(`isolated smoke path contains whitespace: ${root}`);
  const home = join(root, "home");
  const plugin = join(root, "plugin");
  const bootstrap = join(root, "credential-handoff");
  const store = join(root, "pnpm-store");
  const cache = join(root, "cache");
  const bin = dshBin();
  if (!(await exists(bin))) throw new Error(`DSH CLI is missing: ${bin}`);
  await Promise.all([mkdir(home), mkdir(store), mkdir(cache), cp(source, plugin, { recursive: true })]);

  const server = createServer(async (request, response) => {
    if (request.url === "/v1/models") {
      assert.equal(request.headers.authorization, `Bearer ${secret}`);
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({
        object: "list",
        data: [{ id: "smoke-model-a", ocg: {
          schemaVersion: 1, contextWindow: 262144, maxOutputTokens: 32768,
          inputModalities: ["text", "image"], reasoning: true,
          reasoningEfforts: { low: "low", high: "high", xhigh: "max" },
        } }, { id: "org/smoke-model-b" }],
      }));
      return;
    }
    if (request.url === "/v1/chat/completions" && request.method === "POST") {
      assert.equal(request.headers.authorization, `Bearer ${secret}`);
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      assert.equal(body.model, "smoke-model-a");
      assert.equal(body.stream, true);
      assert.equal(body.reasoning_effort, "max");
      response.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
      });
      response.end([
        `data: ${JSON.stringify({
          id: "chatcmpl-smoke",
          object: "chat.completion.chunk",
          created: 0,
          model: "smoke-model-a",
          choices: [{ index: 0, delta: { role: "assistant", content: "smoke-ok" }, finish_reason: null }],
        })}\n\n`,
        `data: ${JSON.stringify({
          id: "chatcmpl-smoke",
          object: "chat.completion.chunk",
          created: 0,
          model: "smoke-model-a",
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
          usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
        })}\n\n`,
        "data: [DONE]\n\n",
      ].join(""));
      return;
    }
    response.writeHead(404).end();
  });
  await new Promise((resolveReady, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveReady);
  });
  const address = server.address();
  assert.equal(typeof address, "object");
  const gateway = `http://127.0.0.1:${address.port}/v1`;

  try {
    const indexPath = join(plugin, "index.js");
    const rendered = (await readFile(indexPath, "utf8"))
      .replaceAll("__OCG_GATEWAY_V1_URL__", gateway)
      .replaceAll(
        "__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__",
        JSON.stringify(bootstrap),
      );
    await writeFile(indexPath, rendered);
    await writeFile(bootstrap, secret);

    const env = {
      ...process.env,
      DSH_HOME: home,
      PNPM_HOME: join(root, "pnpm-home"),
      npm_config_store_dir: store,
      XDG_CACHE_HOME: cache,
      XDG_DATA_HOME: join(root, "data"),
      ELECTRON_RUN_AS_NODE: undefined,
    };
    const version = (await runNode([bin, "--version"], { env })).stdout.trim();
    assert.ok(version.length > 0);
    await runNode(
      [
        bin,
        "plugin",
        "--profile",
        "web",
        "add",
        plugin,
        "--config.auto-install-peers=true",
      ],
      { env },
    );

    const manifest = JSON.parse(await readFile(join(home, "profiles", "web", "package.json"), "utf8"));
    assert.ok(manifest.dependencies?.[packageName]);
    assert.ok(manifest.dsh?.profile?.bundles?.includes(packageName));
    const dump = (await runNode([bin, "--profile", "web", "--dump-config"], { env })).stdout;
    assert.match(dump, /id:\s*open-console-gateway/);
    assert.match(dump, /@open-console-gateway\/dsh-plugin/);

    const installedIndex = join(
      home,
      "profiles",
      "web",
      "node_modules",
      "@open-console-gateway",
      "dsh-plugin",
      "index.js",
    );
    const runner = join(root, "runtime-check.mjs");
    await writeFile(
      runner,
      `
        process.argv[1] = ${JSON.stringify(bin)};
        let stored;
        let adapter;
        const credentials = {
          async set(ref, value) { stored = { ref, value }; },
          async resolve(ref) { return ref === stored?.ref ? { value: stored.value } : undefined; },
        };
        const ctx = {
          get(name) { return name === "credentials" ? credentials : undefined; },
          llm: { registerAdapter(_providers, value) { adapter = value; } },
        };
        const plugin = await import(${JSON.stringify(pathToFileURL(installedIndex).href)});
        await plugin.apply(ctx);
        const models = await adapter.listModels("open-console-gateway");
        const prepared = await adapter.prepareCall("open-console-gateway", "smoke-model-a");
        if (prepared.model.context.contextWindow !== 262144) throw new Error("context metadata did not reach DSH");
        if (JSON.stringify(prepared.model.reasoning.efforts.map((effort) => effort.id)) !== JSON.stringify(["low", "high", "xhigh"])) throw new Error("reasoning tiers did not reach DSH");
        const streamChunks = [];
        for await (const chunk of prepared.stream({
          provider: "open-console-gateway",
          model: "smoke-model-a",
          reasoningEffort: "xhigh",
          messages: [{
            id: "smoke-user-message",
            role: "user",
            content: [{ type: "text", text: "Reply with smoke-ok" }],
            source: { kind: "user" },
          }],
        })) streamChunks.push(chunk);
        process.stdout.write(JSON.stringify({
          storedRef: stored?.ref,
          storedValueMatches: stored?.value === ${JSON.stringify(secret)},
          models: models.map(({ id }) => id),
          preparedModel: prepared.model.id,
          streamChunks,
        }));
      `,
    );
    const runtime = JSON.parse((await runNode([runner], { env })).stdout);
    assert.deepEqual(runtime, {
      storedRef: "OCG_GATEWAY_KEY",
      storedValueMatches: true,
      models: ["smoke-model-a", "org/smoke-model-b"],
      preparedModel: "smoke-model-a",
      streamChunks: runtime.streamChunks,
    });
    assert.ok(
      runtime.streamChunks.some((chunk) => chunk.type === "text-delta" && chunk.text === "smoke-ok"),
      JSON.stringify(runtime.streamChunks),
    );
    assert.ok(
      runtime.streamChunks.some((chunk) => chunk.type === "finish" && chunk.reason?.kind === "stop"),
      JSON.stringify(runtime.streamChunks),
    );
    assert.equal(await exists(bootstrap), false);

    process.stdout.write(JSON.stringify({
      status: "pass",
      dshVersion: version,
      profile: "web",
      packageName,
      installed: true,
      modelIds: runtime.models,
      credentialImported: runtime.storedValueMatches,
      modelCallPrepared: runtime.preparedModel === "smoke-model-a",
      chatStreamCompleted: true,
      contextAndReasoningTiersVerified: true,
      reasoningWireMappingVerified: true,
      realUserHomeTouched: false,
    }, null, 2));
    process.stdout.write("\n");
  } finally {
    await new Promise((resolveClose) => server.close(resolveClose));
    await rm(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack : String(error));
  process.exitCode = 1;
});
