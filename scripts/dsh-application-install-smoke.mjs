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
import {
  EFFORTS,
  FLIP_PATH,
  catalogPayload,
  protocolSse,
} from "./dsh-protocol-fixtures.mjs";

const execFileAsync = promisify(execFile);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(repo, "integrations", "dsh-plugin");
const packageName = "@open-console-gateway/dsh-plugin";
const secret = "ocg-isolated-smoke-key";
const installedDshVersion = "0.2.0-rc.2";
const installedPiAiVersion = "0.87.1";

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
  return execFileAsync(process.execPath, args, {
    encoding: "utf8",
    timeout: 180_000,
    windowsHide: true,
    maxBuffer: 4 * 1024 * 1024,
    ...options,
  });
}

function readBody(request) {
  return new Promise((resolveBody, reject) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => resolveBody(Buffer.concat(chunks).toString("utf8")));
    request.on("error", reject);
  });
}

function protocolOf(pathname) {
  if (pathname === "/ocg/v1/chat/completions") return "chat_completions";
  if (pathname === "/ocg/v1/responses") return "responses";
  if (pathname === "/ocg/v1/messages") return "messages";
  return undefined;
}

function wireFact(record) {
  const body = record.body ?? {};
  const input = Array.isArray(body.input) ? body.input : [];
  const messages = Array.isArray(body.messages) ? body.messages : [];
  return {
    method: record.method,
    path: `${record.pathname}${record.search}`,
    model: body.model ?? null,
    generation: record.generation ?? null,
    authorizationMatches: record.authorizationMatches,
    xApiKeyMatches: record.xApiKeyMatches,
    anthropicVersion: record.anthropicVersion,
    store: Object.hasOwn(body, "store") ? body.store : "absent",
    reasoningEffort: body.reasoning_effort ?? body.reasoning?.effort ?? "absent",
    thinking: Object.hasOwn(body, "thinking") ? body.thinking : "absent",
    outputConfig: Object.hasOwn(body, "output_config") ? body.output_config : "absent",
    previousResponseId: body.previous_response_id ?? body.prior_response_id ?? "absent",
    instructionRole: body.system !== undefined ? "system-field" : body.messages?.[0]?.role ?? input[0]?.role ?? "absent",
    toolReplay: messages.some((message) => message.role === "tool" || message.role === "assistant")
      || input.some((item) => item.type === "function_call" || item.type === "function_call_output" || item.type === "reasoning")
      || messages.some((message) => JSON.stringify(message.content ?? "").includes("tool_result")
        || JSON.stringify(message.content ?? "").includes("thinking")),
  };
}

function assertWire(records) {
  assert.equal(records.some((record) => record.pathname.includes("/v1/v1")), false);
  assert.equal(records.some((record) => record.body?.model === "legacy-model"), false);
  for (const record of records) {
    if (record.pathname === FLIP_PATH) continue;
    if (record.pathname === "/ocg/v1/messages") {
      assert.equal(record.xApiKeyMatches, true, record.pathname);
      assert.equal(record.anthropicVersion, "2023-06-01", record.pathname);
      continue;
    }
    assert.equal(record.authorizationMatches, true, record.pathname);
    assert.equal(record.xApiKeyMatches, false, record.pathname);
  }

  const catalogs = records.filter((record) => record.method === "GET" && record.pathname === "/ocg/v1/models");
  assert.ok(catalogs.length >= 2, `catalog fetches: ${catalogs.length}`);
  assert.ok(catalogs.some((record) => record.generation === 1));
  const flippedCatalog = catalogs.findLast((record) => record.generation === 2);
  assert.ok(flippedCatalog);

  const chat = records.filter((record) => record.pathname === "/ocg/v1/chat/completions");
  assert.equal(chat.length, 3);
  for (const record of chat) {
    assert.equal(record.body.model, "smoke-chat");
    assert.equal(record.body.stream, true);
    assert.equal(record.body.reasoning_effort, "max");
    assert.equal(Object.hasOwn(record.body, "store"), false);
    assert.equal(record.body.messages[0].role, "system");
    assert.match(JSON.stringify(record.body.messages[0].content), /ocg-system/);
    assert.match(JSON.stringify(record.body.tools), /echo/);
  }
  assert.equal(chat[0].body.messages.some((message) => message.role === "assistant"), false);
  const chatAssistant = chat[1].body.messages.find((message) => message.role === "assistant");
  assert.equal(chatAssistant.reasoning_content, "plan");
  assert.equal(chatAssistant.tool_calls[0].id, "call_echo");
  assert.equal(chatAssistant.tool_calls[0].function.name, "echo");
  const chatTool = chat[1].body.messages.find((message) => message.role === "tool");
  assert.equal(chatTool.tool_call_id, "call_echo");
  assert.equal(chatTool.content, "echoed");
  assert.equal(chat[2].body.messages.some((message) => message.role === "assistant"), false);
  assert.ok(records.indexOf(chat[2]) > records.indexOf(flippedCatalog));

  const responses = records.filter((record) => record.pathname === "/ocg/v1/responses");
  const reasoned = responses.filter((record) => record.body.model === "smoke-responses");
  const plain = responses.filter((record) => record.body.model === "smoke-responses-plain");
  assert.equal(reasoned.length, 2);
  assert.equal(plain.length, 2);
  for (const record of responses) {
    assert.equal(record.body.store, false);
    assert.equal(record.body.previous_response_id, undefined);
    assert.equal(record.body.prior_response_id, undefined);
    assert.equal(record.body.input[0].role, "developer");
    assert.match(JSON.stringify(record.body.input[0].content), /ocg-system/);
    assert.match(JSON.stringify(record.body.tools), /echo/);
  }
  assert.equal(reasoned[0].body.reasoning.effort, "max");
  assert.equal(reasoned[1].body.reasoning.effort, "max");
  assert.ok(reasoned[1].body.input.some((item) => item.role === "user"));
  assert.ok(reasoned[1].body.input.some((item) => item.type === "reasoning"));
  assert.ok(reasoned[1].body.input.some((item) => item.type === "function_call"
    && item.call_id === "call_echo" && item.id === "fc_echo" && item.name === "echo"));
  assert.ok(reasoned[1].body.input.some((item) => item.type === "function_call_output"
    && item.call_id === "call_echo" && item.output === "echoed"));
  assert.equal(Object.hasOwn(plain[0].body, "reasoning"), false);
  assert.equal(Object.hasOwn(plain[1].body, "reasoning"), false);
  assert.equal(JSON.stringify(plain[0].body.input).includes("portable-note"), false);
  assert.equal(JSON.stringify(plain[1].body.input).includes("portable-note"), true);

  const messages = records.filter((record) => record.pathname === "/ocg/v1/messages");
  assert.equal(messages.length, 2);
  for (const record of messages) {
    assert.equal(record.search, "?beta=true");
    assert.equal(record.body.model, "smoke-messages");
    assert.equal(Object.hasOwn(record.body, "thinking"), false);
    assert.equal(Object.hasOwn(record.body, "output_config"), false);
    assert.equal(Object.hasOwn(record.body, "store"), false);
    assert.equal(Object.hasOwn(record.body, "reasoning_effort"), false);
    assert.match(JSON.stringify(record.body.system), /ocg-system/);
    assert.match(JSON.stringify(record.body.tools), /echo/);
  }
  const messagesAssistant = messages[1].body.messages.find((message) => message.role === "assistant");
  assert.ok(messagesAssistant.content.some((block) => block.type === "thinking"
    && block.signature === "sig-1" && block.thinking === "plan"));
  assert.ok(messagesAssistant.content.some((block) => block.type === "tool_use" && block.id === "toolu_echo"));
  assert.ok(messages[1].body.messages.some((message) => JSON.stringify(message).includes("toolu_echo")
    && JSON.stringify(message).includes("echoed")));
  assert.equal(records.some((record) => record.pathname === "/ocg/v1/messages" && record.body?.model === "smoke-chat"), false);
}

function runnerSource(bin, installedIndex, flipUrl) {
  return `
    process.argv[1] = ${JSON.stringify(bin)};
    const echo = [{
      name: "echo",
      description: "Echo text",
      parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
    }];
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
    function user(id, text) {
      return { id, role: "user", content: [{ type: "text", text }], source: { kind: "user" } };
    }
    async function collect(stream) {
      const chunks = [];
      try {
        for await (const chunk of stream) chunks.push(chunk);
      } catch (error) {
        return {
          thrown: { code: error?.code ?? null, message: error instanceof Error ? error.message : String(error) },
          finishKind: null,
          finishMessage: null,
          blocks: [],
          replayState: null,
        };
      }
      const finish = chunks.find((chunk) => chunk.type === "finish");
      return {
        thrown: null,
        finishKind: finish?.reason?.kind ?? null,
        finishMessage: finish?.reason?.failure?.message ?? null,
        finishCode: finish?.reason?.failure?.code ?? null,
        blocks: chunks.filter((chunk) => chunk.type === "block-end").map((chunk) => chunk.block),
        replayState: finish?.replayState ?? null,
      };
    }
    function expectKind(label, turn, kind) {
      if (turn.thrown) throw new Error(label + " threw " + turn.thrown.code + ": " + turn.thrown.message);
      if (turn.finishKind !== kind) throw new Error(label + " finish " + turn.finishKind + ": " + turn.finishMessage);
      return turn;
    }
    function assistant(id, model, turn) {
      return {
        id,
        role: "assistant",
        content: turn.blocks,
        source: { kind: "model", provider: "ocg", model, replayState: turn.replayState },
      };
    }
    function toolMessage(turn) {
      const call = turn.blocks.find((block) => block.type === "tool-call");
      if (!call) throw new Error("missing tool call");
      return {
        id: "tool-" + call.id,
        role: "tool",
        toolCallId: call.id,
        content: [{ type: "text", text: "echoed" }],
        source: { kind: "tool", callId: call.id },
      };
    }
    function outcome(turn) {
      return { code: turn.thrown?.code ?? turn.finishCode ?? null, message: turn.thrown?.message ?? turn.finishMessage ?? null };
    }
    function envelope(api, provider, model, blocks) {
      return {
        response: { kind: "pi-ai", version: 2, api, provider, model, stopReason: "stop" },
        blocks,
      };
    }
    function replayed(id, provider, model, content, replayState) {
      return {
        id,
        role: "assistant",
        content,
        source: { kind: "model", provider, model, replayState },
      };
    }
    function carriesOpaque(state) {
      return Array.isArray(state?.blocks) && state.blocks.some((block) => block?.redacted === true
        || (typeof block?.thinkingSignature === "string" && block.thinkingSignature.trim() !== "")
        || (typeof block?.thoughtSignature === "string" && block.thoughtSignature.trim() !== ""));
    }
    const plugin = await import(${JSON.stringify(pathToFileURL(installedIndex).href)});
    await plugin.apply(ctx);
    const listed = await adapter.listModels("ocg");
    async function prepare(id) { return adapter.prepareCall("ocg", id); }
    const chatPrepared = await prepare("smoke-chat");
    const responsesPrepared = await prepare("smoke-responses");
    const messagesPrepared = await prepare("smoke-messages");
    const plainPrepared = await prepare("smoke-responses-plain");
    async function turn(prepared, id, messages, reasoningEffort) {
      return collect(prepared.stream({
        provider: "ocg",
        model: id,
        system: "ocg-system",
        tools: echo,
        ...(reasoningEffort ? { reasoningEffort } : {}),
        messages,
      }));
    }
    const chatFirst = expectKind("chat tool", await turn(chatPrepared, "smoke-chat", [user("u-chat", "use echo")], "xhigh"), "tool-calls");
    const chatSecond = expectKind("chat final", await turn(chatPrepared, "smoke-chat", [
      user("u-chat", "use echo"),
      assistant("a-chat", "smoke-chat", chatFirst),
      toolMessage(chatFirst),
    ], "xhigh"), "stop");
    const responsesFirst = expectKind("responses tool", await turn(responsesPrepared, "smoke-responses", [user("u-resp", "use echo")], "xhigh"), "tool-calls");
    const responsesSecond = expectKind("responses final", await turn(responsesPrepared, "smoke-responses", [
      user("u-resp", "use echo"),
      assistant("a-resp", "smoke-responses", responsesFirst),
      toolMessage(responsesFirst),
    ], "xhigh"), "stop");
    const messagesFirst = expectKind("messages tool", await turn(messagesPrepared, "smoke-messages", [user("u-msg", "use echo")]), "tool-calls");
    const messagesSecond = expectKind("messages final", await turn(messagesPrepared, "smoke-messages", [
      user("u-msg", "use echo"),
      assistant("a-msg", "smoke-messages", messagesFirst),
      toolMessage(messagesFirst),
    ]), "stop");
    const plain = expectKind("responses plain", await turn(plainPrepared, "smoke-responses-plain", [user("u-plain", "say smoke-ok")]), "stop");
    const malformed = await turn(chatPrepared, "smoke-chat", [
      user("u-bad", "x"),
      replayed("a-bad", "ocg", "smoke-chat", [{ type: "text", text: "x" }], { kind: "nope" }),
    ], "xhigh");
    const badVersion = await turn(chatPrepared, "smoke-chat", [
      user("u-ver", "x"),
      replayed("a-ver", "ocg", "smoke-chat", [{ type: "text", text: "x" }], {
        response: { kind: "pi-ai", version: 1, api: "openai-completions", provider: "ocg", model: "smoke-chat", stopReason: "stop" },
        blocks: [{ type: "text" }],
      }),
    ], "xhigh");
    const misaligned = await turn(chatPrepared, "smoke-chat", [
      user("u-mis", "x"),
      replayed("a-mis", "ocg", "smoke-chat", [{ type: "text", text: "x" }], envelope("openai-completions", "ocg", "smoke-chat", [
        { type: "text" },
        { type: "reasoning", thinkingSignature: "sig" },
      ])),
    ], "xhigh");
    const differentModel = await turn(responsesPrepared, "smoke-responses", [
      user("u-model", "x"),
      replayed("a-model", "ocg", "smoke-chat", [{ type: "reasoning", text: "secret" }], envelope("openai-responses", "ocg", "smoke-chat", [
        { type: "reasoning", thinkingSignature: "sig" },
      ])),
    ], "xhigh");
    const differentProvider = await turn(chatPrepared, "smoke-chat", [
      user("u-prov", "x"),
      replayed("a-prov", "other", "smoke-chat", [{ type: "reasoning", text: "secret" }], envelope("openai-completions", "other", "smoke-chat", [
        { type: "reasoning", thinkingSignature: "sig" },
      ])),
    ], "xhigh");
    const directStream = await collect(adapter.stream({
      provider: "ocg",
      model: "smoke-chat",
      system: "ocg-system",
      tools: echo,
      reasoningEffort: "xhigh",
      messages: [
        user("u-direct", "x"),
        replayed("a-direct", "ocg", "smoke-chat", [{ type: "reasoning", text: "secret" }], {
          response: { kind: "foreign", version: 2, api: "openai-completions", provider: "ocg", model: "smoke-chat", stopReason: "stop" },
          blocks: [{ type: "reasoning", thinkingSignature: "sig" }],
        }),
      ],
    }));
    const plainNote = expectKind("portable text", await turn(plainPrepared, "smoke-responses-plain", [
      user("u-note", "continue"),
      replayed("a-note", "ocg", "smoke-responses", [{ type: "text", text: "portable-note" }], envelope("openai-responses", "ocg", "smoke-responses", [
        { type: "text" },
      ])),
    ]), "stop");
    let legacy = { threw: false, code: null, message: null };
    try { await prepare("legacy-model"); }
    catch (error) { legacy = { threw: true, code: error?.code ?? null, message: error instanceof Error ? error.message : String(error) }; }
    const messagesLevel = await turn(messagesPrepared, "smoke-messages", [user("u-level", "think")], "high");
    const flip = await fetch(${JSON.stringify(flipUrl)}, { method: "POST" });
    if (!flip.ok && flip.status !== 204) throw new Error("fixture flip failed: " + flip.status);
    const refreshed = await adapter.listModels("ocg");
    const frozen = expectKind("frozen chat", await turn(chatPrepared, "smoke-chat", [user("u-frozen", "fresh")], "xhigh"), "stop");
    const flipped = await prepare("smoke-chat");
    const replay = await turn(flipped, "smoke-chat", [
      user("u-replay", "again"),
      assistant("a-chat", "smoke-chat", chatFirst),
    ]);
    const replayText = replay.finishMessage ?? replay.thrown?.message ?? "";
    const rejects = {
      malformed: outcome(malformed),
      version: outcome(badVersion),
      misaligned: outcome(misaligned),
      differentModel: outcome(differentModel),
      differentProvider: outcome(differentProvider),
      directStream: outcome(directStream),
    };
    process.stdout.write(JSON.stringify({
      storedRef: stored?.ref,
      storedValueMatches: stored?.value === ${JSON.stringify(secret)},
      ids: listed.map((model) => model.id),
      chatContext: chatPrepared.model.context?.contextWindow ?? null,
      chatEfforts: chatPrepared.model.reasoning?.efforts?.map((effort) => effort.id) ?? null,
      messagesReasoning: messagesPrepared.model.ocg?.reasoning ?? null,
      messagesEfforts: messagesPrepared.model.reasoning?.efforts?.map((effort) => effort.id) ?? null,
      messagesDeclaresEfforts: Object.hasOwn(messagesPrepared.model.ocg ?? {}, "reasoningEfforts"),
      messagesEffortMap: messagesPrepared.model.ocg?.reasoningEfforts ?? null,
      plainReasoning: plainPrepared.model.ocg?.reasoning ?? null,
      plainEfforts: plainPrepared.model.reasoning?.efforts?.map((effort) => effort.id) ?? null,
      finishes: {
        chat: [chatFirst.finishKind, chatSecond.finishKind, frozen.finishKind],
        responses: [responsesFirst.finishKind, responsesSecond.finishKind, plain.finishKind, plainNote.finishKind],
        messages: [messagesFirst.finishKind, messagesSecond.finishKind],
      },
      legacy,
      messagesLevel: outcome(messagesLevel),
      chatReplayOpaque: carriesOpaque(chatFirst.replayState),
      rejects,
      flippedPreferred: refreshed.find((model) => model.id === "smoke-chat")?.ocg?.protocols?.preferred ?? null,
      replayCode: replay.thrown?.code ?? replay.finishCode ?? null,
      replayText,
    }));
  `;
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
  const dshRoot = dirname(dirname(bin));
  const dshPackage = JSON.parse(await readFile(join(dshRoot, "package.json"), "utf8"));
  const piPackage = JSON.parse(await readFile(join(dshRoot, "node_modules", "@earendil-works", "pi-ai", "package.json"), "utf8"));
  assert.equal(dshPackage.version, installedDshVersion);
  assert.equal(piPackage.version, installedPiAiVersion);
  await Promise.all([mkdir(home), mkdir(store), mkdir(cache), cp(source, plugin, { recursive: true })]);

  let generation = 1;
  const turns = new Map();
  const records = [];
  const server = createServer(async (request, response) => {
    try {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const text = request.method === "GET" || request.method === "HEAD" ? "" : await readBody(request);
    let body = null;
    if (text) body = JSON.parse(text);
    const record = {
      method: request.method,
      pathname: url.pathname,
      search: url.search,
      authorizationMatches: request.headers.authorization === `Bearer ${secret}`,
      xApiKeyMatches: request.headers["x-api-key"] === secret,
      anthropicVersion: typeof request.headers["anthropic-version"] === "string" ? request.headers["anthropic-version"] : null,
      body,
      generation: null,
    };
    records.push(record);
    if (request.method === "GET" && url.pathname === "/ocg/v1/models") {
      record.generation = generation;
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(catalogPayload(generation)));
      return;
    }
    if (request.method === "POST" && url.pathname === FLIP_PATH) {
      generation = 2;
      response.writeHead(204).end();
      return;
    }
    const protocol = protocolOf(url.pathname);
    if (request.method === "POST" && protocol) {
      const seen = turns.get(body.model) ?? 0;
      turns.set(body.model, seen + 1);
      const phase = body.model === "smoke-responses-plain" || seen > 0 ? "final" : "tool";
      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      response.end(protocolSse(protocol, phase, body.model));
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "unexpected", method: request.method, path: url.pathname }));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      records.push({ method: request.method, pathname: request.url ?? "", search: "", authorizationMatches: false, xApiKeyMatches: false, anthropicVersion: null, body: null, handlerError: message });
      if (!response.headersSent) response.writeHead(500, { "content-type": "text/plain" });
      response.end(message);
    }
  });
  await new Promise((resolveReady, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveReady);
  });
  const address = server.address();
  assert.equal(typeof address, "object");
  const origin = `http://127.0.0.1:${address.port}`;
  const gateway = `${origin}/ocg/v1`;

  try {
    const indexPath = join(plugin, "index.js");
    const rendered = (await readFile(indexPath, "utf8"))
      .replaceAll("__OCG_GATEWAY_V1_URL__", gateway)
      .replaceAll("__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__", JSON.stringify(bootstrap));
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
    assert.match(version, new RegExp(installedDshVersion.replaceAll(".", "\\.")));
    await runNode(
      [bin, "plugin", "--profile", "web", "add", plugin, "--config.auto-install-peers=true"],
      { env, timeout: 300_000 },
    );
    const manifest = JSON.parse(await readFile(join(home, "profiles", "web", "package.json"), "utf8"));
    assert.ok(manifest.dependencies?.[packageName]);
    assert.ok(manifest.dsh?.profile?.bundles?.includes(packageName));
    const dump = (await runNode([bin, "--profile", "web", "--dump-config"], { env })).stdout;
    assert.match(dump, /id:\s*open-console-gateway/);
    assert.match(dump, /@open-console-gateway\/dsh-plugin/);
    const installedIndex = join(home, "profiles", "web", "node_modules", "@open-console-gateway", "dsh-plugin", "index.js");
    assert.match(await readFile(installedIndex, "utf8"), /\/ocg\/v1/);
    const runner = join(root, "runtime-check.mjs");
    await writeFile(runner, runnerSource(bin, installedIndex, `${origin}${FLIP_PATH}`));
    const runtime = JSON.parse((await runNode([runner], { env })).stdout);
    assertWire(records);
    assert.equal(runtime.storedRef, "OCG_GATEWAY_KEY");
    assert.equal(runtime.storedValueMatches, true);
    assert.deepEqual(runtime.ids, [
      "smoke-chat", "smoke-responses", "smoke-messages", "smoke-responses-plain", "legacy-model",
    ]);
    assert.equal(runtime.chatContext, 262144);
    assert.deepEqual(runtime.chatEfforts, ["low", "high", "xhigh"]);
    assert.equal(runtime.messagesReasoning, true);
    assert.equal(runtime.messagesDeclaresEfforts, true);
    assert.deepEqual(runtime.messagesEffortMap, EFFORTS);
    assert.deepEqual(runtime.messagesEfforts, []);
    assert.equal(runtime.plainReasoning, true);
    assert.deepEqual(runtime.plainEfforts, []);
    assert.equal(runtime.legacy.threw, true);
    assert.match(runtime.legacy.message, /schema/i);
    assert.equal(runtime.messagesLevel.code, "UNSUPPORTED_REASONING_EFFORT");
    assert.equal(runtime.chatReplayOpaque, true);
    for (const key of ["malformed", "version", "misaligned", "directStream"]) {
      assert.equal(runtime.rejects[key].code, "OCG_REPLAY_METADATA_REJECTED", key);
      assert.equal(runtime.rejects[key].message?.length > 0, true, key);
    }
    for (const key of ["differentModel", "differentProvider"]) {
      assert.equal(runtime.rejects[key].code, "OCG_PROTOCOL_REPLAY_REFUSED", key);
      assert.match(runtime.rejects[key].message, /cannot replay/);
    }
    assert.equal(runtime.flippedPreferred, "messages");
    assert.equal(runtime.replayCode, "OCG_PROTOCOL_REPLAY_REFUSED");
    assert.match(runtime.replayText, /cannot replay/);
    assert.match(runtime.replayText, /openai-completions/);
    assert.match(runtime.replayText, /anthropic-messages/);
    assert.equal(await exists(bootstrap), false);

    process.stdout.write(`${JSON.stringify({
      status: "pass",
      dshVersion: dshPackage.version,
      dshCli: version,
      piAiVersion: piPackage.version,
      profile: "web",
      packageName,
      realUserHomeTouched: false,
      gatewayLoopback: true,
      wire: records.map(wireFact),
      runtime: {
        ids: runtime.ids,
        chatContext: runtime.chatContext,
        chatEfforts: runtime.chatEfforts,
        messagesReasoning: runtime.messagesReasoning,
        messagesDeclaresEfforts: runtime.messagesDeclaresEfforts,
        messagesEffortMap: runtime.messagesEffortMap,
        messagesEfforts: runtime.messagesEfforts,
        plainReasoning: runtime.plainReasoning,
        plainEfforts: runtime.plainEfforts,
        finishes: runtime.finishes,
        legacy: runtime.legacy,
        messagesLevel: runtime.messagesLevel,
        chatReplayOpaque: runtime.chatReplayOpaque,
        rejects: runtime.rejects,
        flippedPreferred: runtime.flippedPreferred,
        replayCode: runtime.replayCode,
        replayText: runtime.replayText,
        credentialImported: runtime.storedValueMatches,
      },
    }, null, 2)}\n`);
  } finally {
    await new Promise((resolveClose) => server.close(resolveClose));
    await rm(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  const stdout = error?.stdout ? `\n${error.stdout}` : "";
  const stderr = error?.stderr ? `\n${error.stderr}` : "";
  console.error(`${error instanceof Error ? error.stack : String(error)}${stdout}${stderr}`);
  process.exitCode = 1;
});
