import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { labDir, redactDeep, repoRoot } from "./common.mjs";
import { makeApi } from "./dashboard.mjs";
import { loadProfile } from "./profile.mjs";
import { createCollector, resultsMarkdown, writeReportDir } from "./report.mjs";
import { defaultCliPath, inspectBinary, installInterruptCleanup } from "./process.mjs";
import { smokeCliHelp, withRuntime } from "./harness.mjs";
import { selfCheck } from "./lab.mjs";
import { runCompatibilityScenarios } from "./scenarios-routing.mjs";

function parseArgs(argv) {
  const args = { shadow: false, cli: null };
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token === "--shadow") args.shadow = true;
    else if (token === "--cli") args.cli = argv[++i];
    else if (token.startsWith("--cli=")) args.cli = token.slice("--cli=".length);
    else if (!token.startsWith("-") && !args.cli) args.cli = token;
  }
  return args;
}

function replayCommand(cliPath, shadow = false) {
  const quoted = `"${cliPath}"`;
  return `node scripts/routing-lab/run.mjs --cli ${quoted}${shadow ? " --shadow" : ""}`;
}

export async function runRoutingLab(argv = process.argv.slice(2), { artifactDir } = {}) {
  const args = parseArgs(argv);
  const cliPath = path.resolve(args.cli || process.env.OCG_ROUTING_LAB_CLI || defaultCliPath());
  const here = artifactDir || path.join(repoRoot(), ".artifacts", "routing-lab");
  const collector = createCollector();
  const startedAt = new Date().toISOString();
  const profile = await loadProfile("default");
  let runtime = null;
  let proof = null;
  let cleanupFn = async () => {};
  const uninstallInterrupt = installInterruptCleanup(() => cleanupFn());
  let binary = existsSync(cliPath) ? await inspectBinary(cliPath) : null;

  try {
    const serverPath = path.join(labDir(), "..", "..", "scripts", "routing-lab", "server.mjs");
    execFileSync(process.execPath, [serverPath, "--self-check"], { stdio: "pipe" });
    selfCheck();

    runtime = await withRuntime({
      cliPath,
      profile,
      artifactDir: here,
      shadow: args.shadow,
      encryptionKey: "routing-lab-dummy-not-a-real-secret",
      register: false,
      onCleanup(fn) {
        cleanupFn = fn;
      },
    });
    binary = runtime.binary;
    cleanupFn = runtime.cleanup;
    await mkdir(runtime.logDir, { recursive: true });
    const helpPid = await smokeCliHelp(cliPath, runtime.logDir);
    collector.pass("cli --help smoke", { pid: helpPid });

    const { lab, started, api, gatewayBase } = runtime;
    await writeFile(
      path.join(here, "runtime.json"),
      JSON.stringify(
        {
          generatedAt: startedAt,
          binary,
          gateway: { pid: runtime.gatewayPid, host: "127.0.0.1", port: runtime.gatewayPort, url: gatewayBase },
          listeners: started.listeners,
          dataDir: runtime.dataDir,
          oldLab: runtime.oldLab,
          shadow: args.shadow,
        },
        null,
        2,
      ),
    );

    const { registerSlots } = await import("./harness.mjs");
    await registerSlots(api, started.slots);
    await runCompatibilityScenarios(runtime, collector);

    proof = await runtime.cleanup();
    if (!proof.verified) {
      collector.fail("cleanup verification", new Error(`listenersClosed=${proof.listenersClosed} gatewayPortClosed=${proof.gatewayPortClosed} gatewayPidExited=${proof.gatewayPidExited}`), proof);
    }
    const counts = collector.counts();
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      evidenceClass: binary.evidenceClass,
      binary,
      replay: replayCommand(cliPath, args.shadow),
      shadow: args.shadow,
      gateway: { pid: runtime.gatewayPid, url: gatewayBase, port: runtime.gatewayPort, host: "127.0.0.1" },
      listeners: started.listeners,
      dataDir: runtime.dataDir,
      oldLab: runtime.oldLab,
      cleanup: proof,
      passed: counts.PASS,
      failed: counts.FAIL,
      counts,
      results: collector.results,
    };
    await writeFile(path.join(here, "journal.json"), JSON.stringify({ requests: lab.snapshot(), truncated: lab.truncated }, null, 2));
    await writeFile(path.join(here, "cleanup.json"), JSON.stringify({ generatedAt: new Date().toISOString(), ...proof }, null, 2));
    await writeFile(path.join(here, "report.json"), JSON.stringify(redactDeep(report), null, 2));
    await writeFile(path.join(here, "RESULTS.md"), resultsMarkdown(report, { title: "Routing lab results" }));
    uninstallInterrupt();
    if (counts.FAIL) process.exitCode = 1;
    else process.exitCode = counts.UNSUPPORTED || counts.NOT_RUN ? 2 : 0;
    return report;
  } catch (error) {
    collector.fail("orchestrator", error);
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      evidenceClass: binary?.evidenceClass ?? "unknown",
      binary,
      replay: replayCommand(cliPath, args.shadow),
      shadow: args.shadow,
      passed: collector.counts().PASS,
      failed: collector.counts().FAIL,
      counts: collector.counts(),
      results: collector.results,
      fatal: error instanceof Error ? error.message : String(error),
    };
    try {
      await writeFile(path.join(here, "journal.json"), JSON.stringify({ requests: runtime?.lab?.snapshot?.() || [] }, null, 2));
    } catch {
      /* ignore */
    }
    report.cleanup = runtime ? await runtime.cleanup().catch((cleanupError) => ({ verified: false, error: String(cleanupError) })) : error.cleanup || { verified: false };
    await writeReportDir(here, {
      "report.json": report,
      "RESULTS.md": resultsMarkdown(report, { title: "Routing lab results" }),
      "cleanup.json": { generatedAt: new Date().toISOString(), ...(report.cleanup || {}) },
    }).catch(() => {});
    uninstallInterrupt();
    process.exitCode = 1;
    console.error(error);
    return report;
  }
}

export { makeApi, parseArgs };
