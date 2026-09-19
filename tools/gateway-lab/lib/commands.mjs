import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { HOST, assertLoopbackHost, isDirectRun, newRunId, repoRoot } from "./common.mjs";
import { createLab, selfCheck } from "./lab.mjs";
import { loadProfile } from "./profile.mjs";
import { createLiveClient, readLiveEnv } from "./live.mjs";
import { makeApi, readJsonResponse, request } from "./dashboard.mjs";
import { registerSlots } from "./harness.mjs";
import { runVerify } from "./verify.mjs";
import { runRoutingLab } from "./routing-compat.mjs";

async function readRuntime(runtimePath) {
  const text = await readFile(runtimePath, "utf8");
  return JSON.parse(text);
}

function controlUrl(runtime) {
  const url = runtime.control?.url;
  if (!url) throw new Error("runtime is missing control.url");
  return url;
}

export async function serveCommand(flags) {
  const profile = await loadProfile(flags.profile || "default");
  const runId = flags["run-id"] || newRunId();
  const artifactDir = path.resolve(flags.artifacts || path.join(repoRoot(), ".artifacts", "gateway-lab", runId));
  await mkdir(artifactDir, { recursive: true });
  const env = readLiveEnv();
  const live = flags.live && env.present ? createLiveClient({ url: env.url, key: env.key, model: profile.live.model }) : null;
  if (flags.live && !env.present) {
    throw new Error("serve --live requires OCG_LAB_REMOTE_CHAT_URL and OCG_LAB_REMOTE_KEY");
  }
  const host = flags.host || profile.host || HOST;
  assertLoopbackHost(host, "serve host");
  const lab = createLab({ host, profile, live, runId });
  const started = await lab.start();
  if (flags.live) lab.armLive(true);
  const runtimePath = path.join(artifactDir, "runtime.json");
  await writeFile(runtimePath, `${JSON.stringify({ ...started, artifactDir, pid: process.pid }, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify({ ...started, artifactDir, runtime: runtimePath }, null, 2)}\n`);
  const stop = async () => {
    await lab.close();
    process.exit(0);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  await new Promise(() => {});
}

export async function registerCommand(flags) {
  const gateway = flags.gateway;
  const runtimePath = flags.runtime;
  if (!gateway || !runtimePath) throw new Error("register requires --gateway and --runtime");
  const runtime = await readRuntime(runtimePath);
  const connection = await readJsonResponse(await request(gateway, "/dashboard/api/v4/connection"));
  if (connection.status !== 200 || !connection.body?.primaryKey) {
    throw new Error(`cannot read gateway connection: ${connection.status} ${connection.text?.slice(0, 300)}`);
  }
  const api = makeApi(gateway, { snapshot: () => [] }, () => connection.body.primaryKey);
  const slots = await registerSlots(api, runtime.slots);
  const receipt = {
    gateway,
    registered: slots.map((slot) => ({
      slot: slot.slot,
      connectionId: slot.connectionId,
      accountId: slot.accountId,
      credentialId: slot.credentialId,
      publicModel: slot.publicModel,
      upstreamModel: slot.model,
    })),
  };
  process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
  return receipt;
}

export async function scenarioCommand(flags) {
  const runtime = await readRuntime(flags.runtime);
  const name = flags.name;
  if (!name) throw new Error("scenario requires --name");
  if (!flags.listener && !flags.endpoint) {
    throw new Error("scenario requires --endpoint (or --listener) plus --model or --key; refusing unmatched empty isolation");
  }
  if (!flags.listener && !flags.model && !flags.key) {
    throw new Error("scenario requires --model or --key together with --endpoint");
  }
  const payload = {
    name,
    listener: flags.listener || undefined,
    endpointId: flags.endpoint || undefined,
    model: flags.model || undefined,
    keyFingerprint: flags.key || undefined,
  };
  const response = await fetch(`${controlUrl(runtime)}/scenario`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`scenario failed: ${response.status} ${text.slice(0, 400)}`);
  process.stdout.write(`${text}\n`);
}

export async function resetCommand(flags) {
  const runtime = await readRuntime(flags.runtime);
  const response = await fetch(`${controlUrl(runtime)}/reset`, { method: "POST" });
  const text = await response.text();
  if (!response.ok) throw new Error(`reset failed: ${response.status} ${text.slice(0, 400)}`);
  process.stdout.write(`${text}\n`);
}

export async function verifyCommand(flags, positional) {
  const suite = flags.suite || "local";
  const cli = flags.cli || positional[0];
  await runVerify({
    cli,
    suite,
    profile: flags.profile || "default",
    shadow: Boolean(flags.shadow),
  });
  return Number.isInteger(process.exitCode) ? process.exitCode : 0;
}

export const HELP = `Gateway Lab — development Gateway test CLI (no UI)

Usage:
  node tools/gateway-lab/cli.mjs serve [--profile default]
  node tools/gateway-lab/cli.mjs register --gateway <url> --runtime <runtime.json>
  node tools/gateway-lab/cli.mjs scenario --runtime <runtime.json> --name <scenario> --endpoint <id> --model <upstream>
  node tools/gateway-lab/cli.mjs reset --runtime <runtime.json>
  node tools/gateway-lab/cli.mjs verify --cli <ocg-manager-cli> [--suite local|live|all]

Default profile exposes independent Chat Completions, Responses, and Messages
endpoints plus shared public model lab-route. Local responses are synthetic.
Live calls OpenAI Chat Completions at OCG_LAB_REMOTE_CHAT_URL with
OCG_LAB_REMOTE_KEY and always sends model minimax-m3. Custom live models are
invalid, not configurable. Listeners bind loopback only (127.0.0.1 / ::1).
scenario requires --endpoint (or --listener) plus --model or --key; a missing
selector is a validation error, not an empty isolation key.

Reports: .artifacts/gateway-lab/<run-id>/
Compatibility: node scripts/routing-lab/run.mjs --cli <path>
`;

void isDirectRun;
void selfCheck;
void runRoutingLab;
