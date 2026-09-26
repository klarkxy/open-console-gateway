import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { newRunId, parseArgv } from "./common.mjs";
import { smokeCliHelp, withRuntime } from "./harness.mjs";
import { createLab } from "./lab.mjs";
import { loadProfile } from "./profile.mjs";
import { defaultCliPath, inspectBinary, installInterruptCleanup } from "./process.mjs";
import { createCollector, resultsMarkdown, writeReportDir } from "./report.mjs";
import { probePolicyApi } from "./policy-api.mjs";
import {
  EVIDENCE,
  LIVE_MAX_REMOTE,
  LIVE_OUTBOUND_MODEL,
  POLICY_HELP,
  RESPONSIBILITY,
  SCENARIO,
  companionRustRows,
  policyExitCode,
} from "./policy-contract.mjs";
import { attachLabFromPath, call, labStats, snapshotLiveArmed } from "./policy-lab-attach.mjs";
import { recordHttpNotRun, runPolicyScenarios } from "./policy-scenarios.mjs";

export { POLICY_HELP };

function replayCommand({ cliPath, suite, artifacts, labRuntime }) {
  const parts = [
    "node tools/gateway-lab/policy-verify.mjs",
    `--cli "${cliPath}"`,
    `--suite ${suite}`,
    `--artifacts "${artifacts}"`,
  ];
  if (labRuntime) parts.push(`--lab-runtime "${labRuntime}"`);
  return parts.join(" ");
}

function scoped(collector, responsibility) {
  return {
    pass(label, extra = {}) {
      return collector.pass(label, { ...extra, responsibility });
    },
    fail(label, error, extra = {}) {
      return collector.fail(label, error, { ...extra, responsibility });
    },
    notRun(label, reason, extra = {}) {
      return collector.notRun(label, reason, { ...extra, responsibility });
    },
  };
}

function companionMarkdown(rows) {
  const lines = ["", "## Companion Rust (not scored by this HTTP runner)", ""];
  for (const row of rows || []) {
    lines.push(`- ${row.status} ${row.label} — ${row.reason}`);
  }
  lines.push("");
  return lines.join("\n");
}

export function resolvePolicyVerifyInput(argv) {
  const { flags, positional } = parseArgv(argv);
  if (flags.help || flags.h) return { help: true, flags, positional };
  const suite = String(flags.suite || "").trim();
  if (!suite) {
    const error = new Error("missing --suite local|live");
    error.code = "usage";
    throw error;
  }
  if (suite !== "local" && suite !== "live") {
    const error = new Error(`unknown suite ${suite}`);
    error.code = "usage";
    throw error;
  }
  const artifacts = flags.artifacts;
  if (!artifacts || artifacts === true) {
    const error = new Error("missing --artifacts <path>");
    error.code = "usage";
    throw error;
  }
  const cli = flags.cli || positional[0];
  if (!cli || cli === true) {
    const error = new Error("missing --cli <fresh ocg-manager-cli>");
    error.code = "usage";
    throw error;
  }
  const labRuntime = flags["lab-runtime"];
  if (suite === "live" && (!labRuntime || labRuntime === true)) {
    const error = new Error("live suite requires --lab-runtime <runtime.json>; refusing to use a local fixture as live");
    error.code = "usage";
    throw error;
  }
  if (suite === "local" && labRuntime && labRuntime !== true) {
    const error = new Error("local suite creates its own simulator; do not pass --lab-runtime");
    error.code = "usage";
    throw error;
  }
  return {
    help: false,
    suite,
    artifacts: path.resolve(artifacts),
    cliPath: path.resolve(cli),
    labRuntime: labRuntime ? path.resolve(labRuntime) : null,
    flags,
  };
}

export async function runPolicyVerify(options) {
  const suite = options.suite;
  const cliPath = path.resolve(options.cliPath || options.cli || defaultCliPath());
  const artifactDir = path.resolve(options.artifacts);
  const labRuntimePath = options.labRuntime ? path.resolve(options.labRuntime) : null;
  await mkdir(artifactDir, { recursive: true });
  const collector = createCollector();
  const harness = scoped(collector, RESPONSIBILITY.HARNESS);
  const live = scoped(collector, RESPONSIBILITY.LIVE);
  const startedAt = new Date().toISOString();
  const runId = newRunId();
  const profile = await loadProfile("default");
  let cleanupFn = async () => ({ verified: true });
  const uninstallInterrupt = installInterruptCleanup(() => cleanupFn());
  let binary = existsSync(cliPath) ? await inspectBinary(cliPath) : null;
  let runtime = null;
  let attached = null;
  let ownedLab = null;
  let liveArmedSaved = null;
  let remoteCallsBaseline = 0;
  const replay = replayCommand({ cliPath, suite, artifacts: artifactDir, labRuntime: labRuntimePath });

  async function restoreLiveArmed() {
    if (!attached?.lab || liveArmedSaved == null) return;
    await call(attached.lab, "armLive", liveArmedSaved);
  }

  async function finish(extra = {}) {
    const counts = collector.counts();
    let stats = { remoteCalls: extra.remoteCalls ?? null };
    try {
      if (runtime?.lab) stats = await labStats(runtime.lab);
      else if (ownedLab) stats = await labStats(ownedLab);
      else if (attached?.lab) stats = await labStats(attached.lab);
    } catch {
      /* keep prior stats */
    }
    const remoteCalls = stats.remoteCalls ?? extra.remoteCalls ?? null;
    const baseline = extra.remoteCallsBaseline ?? remoteCallsBaseline;
    const remoteCallsDelta = remoteCalls == null ? null : remoteCalls - baseline;
    const liveConfigured =
      extra.liveConfigured ??
      attached?.started?.liveConfigured ??
      (typeof runtime?.lab?.runtime === "function" ? runtime.lab.runtime().liveConfigured : false);
    const companionRust = companionRustRows();
    const exitCode = extra.exitCode ?? policyExitCode(collector.results);
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      runId,
      suite,
      tool: "policy-verify",
      evidenceClass: binary?.evidenceClass,
      binary,
      replay,
      liveOutboundModel: LIVE_OUTBOUND_MODEL,
      remoteCalls,
      remoteCallsBaseline: baseline,
      remoteCallsDelta,
      localArrivals: stats.receipts,
      liveConfigured: Boolean(liveConfigured),
      attachedLab: Boolean(attached),
      liveArmedRestored: extra.liveArmedRestored,
      counts,
      results: collector.results,
      companionRust,
      exitCode,
      ...extra,
      remoteCalls,
      remoteCallsBaseline: baseline,
      remoteCallsDelta,
      companionRust,
      exitCode,
    };
    const markdown = `${resultsMarkdown(report, { title: "Temporary unavailability policy verify" })}${companionMarkdown(companionRust)}`;
    await writeReportDir(artifactDir, {
      "report.json": report,
      "RESULTS.md": markdown,
    });
    return { report, counts, exitCode };
  }

  try {
    if (!existsSync(cliPath)) {
      const missingReason = `CLI binary not found: ${cliPath}. Need a freshly built ocg-manager-cli after the kernel/control-plane lands; this runner does not start cargo.`;
      harness.notRun("fresh CLI binary", missingReason, { scenarioId: "policy.cli.binary", evidenceKind: EVIDENCE.GATEWAY });
      if (suite === "local") {
        ownedLab = createLab({ profile, runId, host: profile.host });
        cleanupFn = async () => {
          await ownedLab.close();
          return { verified: true, ownedLabClosed: true };
        };
        await ownedLab.start();
        const stats = ownedLab.stats();
        remoteCallsBaseline = stats.remoteCalls || 0;
        if (stats.remoteCalls === 0) {
          harness.pass("local remoteCalls=0", {
            scenarioId: SCENARIO.LOCAL_REMOTE_ZERO,
            evidenceKind: EVIDENCE.LAB,
            remoteCalls: 0,
            remoteCallsDelta: 0,
          });
        } else {
          harness.fail("local remoteCalls=0", new Error(`remoteCalls=${stats.remoteCalls}`), {
            scenarioId: SCENARIO.LOCAL_REMOTE_ZERO,
            evidenceKind: EVIDENCE.LAB,
            remoteCalls: stats.remoteCalls,
          });
        }
        await ownedLab.close();
        cleanupFn = async () => ({ verified: true, ownedLabClosed: true });
      } else {
        attached = await attachLabFromPath(labRuntimePath);
        if (!attached.started.liveConfigured) {
          throw new Error("attached runtime liveConfigured=false; refusing to treat a local simulator as live");
        }
        liveArmedSaved = await snapshotLiveArmed(attached.lab);
        const statsBefore = await labStats(attached.lab);
        remoteCallsBaseline = statsBefore.remoteCalls || 0;
        try {
          harness.pass("attached coordinator live-lab runtime", {
            scenarioId: "policy.live.attach",
            evidenceKind: EVIDENCE.LIVE,
            control: attached.started.control?.url,
            liveConfigured: true,
            remoteCallsBaseline,
            remoteCalls: statsBefore.remoteCalls,
            localArrivals: statsBefore.receipts,
          });
          live.pass("live lab is not a local fixture", {
            scenarioId: SCENARIO.LIVE_NOT_FAKE,
            evidenceKind: EVIDENCE.LIVE,
            liveConfigured: true,
            liveEnabled: Boolean(attached.started.liveEnabled),
          });
          live.notRun(
            "live-suite local fault: arrivals increment, remoteCalls do not",
            "fresh CLI binary is missing; attach-only, no gateway send",
            {
              scenarioId: SCENARIO.LIVE_FAULT_ZERO_REMOTE,
              evidenceKind: EVIDENCE.LIVE,
              remoteCallsBaseline,
              remoteCalls: statsBefore.remoteCalls,
              localArrivals: statsBefore.receipts,
            },
          );
        } finally {
          await restoreLiveArmed();
        }
        const statsAfter = await labStats(attached.lab);
        const health = await attached.lab.health();
        const delta = (statsAfter.remoteCalls || 0) - remoteCallsBaseline;
        if (!health?.ok) {
          harness.fail("cleanup preserves external lab", new Error("attached lab health failed after attach.close"));
        } else if (delta !== 0) {
          harness.fail(
            "cleanup preserves external lab",
            new Error(`attach-only run changed remoteCalls ${remoteCallsBaseline} -> ${statsAfter.remoteCalls}`),
          );
        } else {
          harness.pass("cleanup preserves external lab", {
            attachedHealth: health,
            evidenceKind: EVIDENCE.LIVE,
            remoteCallsDelta: 0,
            remoteCallsBaseline,
            remoteCalls: statsAfter.remoteCalls,
            liveArmedRestored: liveArmedSaved,
          });
        }
      }
      recordHttpNotRun(collector, missingReason, { suite });
      const { report, exitCode } = await finish({
        fatal: "missing CLI",
        liveConfigured: Boolean(attached?.started?.liveConfigured),
        remoteCallsBaseline,
        liveArmedRestored: liveArmedSaved,
      });
      uninstallInterrupt();
      process.exitCode = exitCode;
      console.log(`report: ${path.join(artifactDir, "report.json")}`);
      return report;
    }

    if (suite === "live") {
      attached = await attachLabFromPath(labRuntimePath);
      if (!attached.started.liveConfigured) {
        throw new Error("attached runtime liveConfigured=false; refusing to treat a local simulator as live");
      }
      liveArmedSaved = await snapshotLiveArmed(attached.lab);
      const statsBefore = await labStats(attached.lab);
      remoteCallsBaseline = statsBefore.remoteCalls || 0;
      await call(attached.lab, "armLive", false);
      harness.pass("attached coordinator live-lab runtime", {
        scenarioId: "policy.live.attach",
        evidenceKind: EVIDENCE.LIVE,
        control: attached.started.control?.url,
        listeners: (attached.started.listeners || []).map((item) => item.id),
        liveConfigured: true,
        liveEnabled: liveArmedSaved,
        remoteCallsBaseline,
        remoteCalls: statsBefore.remoteCalls,
      });
    }

    runtime = await withRuntime({
      cliPath,
      profile,
      artifactDir,
      live: null,
      runId,
      existingLab: attached ? attached.lab : null,
      onCleanup(fn) {
        cleanupFn = fn;
      },
    });
    if (!attached) ownedLab = runtime.lab;
    binary = runtime.binary;
    cleanupFn = runtime.cleanup;
    if (suite === "local") {
      remoteCallsBaseline = ((await labStats(runtime.lab)).remoteCalls || 0);
    }

    try {
      const helpPid = await smokeCliHelp(cliPath, runtime.logDir);
      harness.pass("cli --help smoke", { scenarioId: SCENARIO.CLI_HELP, evidenceKind: EVIDENCE.GATEWAY, pid: helpPid });
    } catch (error) {
      harness.fail("cli --help smoke", error, { scenarioId: SCENARIO.CLI_HELP, evidenceKind: EVIDENCE.GATEWAY });
    }

    const apiProbe = await probePolicyApi(runtime.gatewayBase);
    const liveSendsAllowed = suite === "live" && apiProbe.ready;
    try {
      await runPolicyScenarios({
        runtime,
        collector,
        suite,
        apiProbe,
        liveSendsAllowed,
        remoteCallsBaseline,
        remoteMaxDelta: suite === "live" ? LIVE_MAX_REMOTE : 0,
      });
    } finally {
      await restoreLiveArmed().catch(() => {});
    }

    const proof = await runtime.cleanup();
    cleanupFn = async () => proof;
    if (suite === "live") {
      proof.labPreserved = true;
      const health = await attached.lab.health();
      const statsAfter = await labStats(attached.lab).catch(() => ({ remoteCalls: remoteCallsBaseline }));
      const delta = (statsAfter.remoteCalls || 0) - remoteCallsBaseline;
      if (!health?.ok) {
        harness.fail("cleanup preserves external lab", new Error("attached lab health failed after cleanup"), proof);
      } else if (!proof.verified) {
        harness.fail("cleanup verification", new Error(JSON.stringify(proof)), proof);
      } else {
        harness.pass("cleanup preserves external lab", {
          ...proof,
          attachedHealth: health,
          evidenceKind: EVIDENCE.LIVE,
          remoteCallsBaseline,
          remoteCallsDelta: delta,
          liveArmedRestored: liveArmedSaved,
        });
      }
    } else if (!proof.verified) {
      harness.fail("cleanup verification", new Error(JSON.stringify(proof)), proof);
    } else {
      harness.pass("cleanup verification", proof);
    }

    const stats = await labStats(attached?.lab || runtime.lab).catch(() => ({ remoteCalls: null }));
    const labRuntime = attached?.lab?.runtime() || runtime.lab.runtime();
    const { report, exitCode } = await finish({
      cleanup: proof,
      gateway: { pid: runtime.gatewayPid, url: runtime.gatewayBase, port: runtime.gatewayPort, host: "127.0.0.1" },
      control: labRuntime.control,
      listeners: runtime.started.listeners,
      dataDir: runtime.dataDir,
      apiProbe: { status: apiProbe.status, code: apiProbe.code, reason: apiProbe.reason },
      remoteCalls: stats.remoteCalls,
      remoteCallsBaseline,
      liveConfigured: Boolean(labRuntime.liveConfigured),
      liveArmedRestored: liveArmedSaved,
    });
    uninstallInterrupt();
    process.exitCode = exitCode;
    console.log(`report: ${path.join(artifactDir, "report.json")}`);
    return report;
  } catch (error) {
    harness.fail("orchestrator", error);
    try {
      await restoreLiveArmed();
    } catch {
      /* preserve original armed state if possible */
    }
    if (runtime) {
      error.cleanup = await runtime.cleanup().catch((cleanupError) => ({ verified: false, error: String(cleanupError) }));
    } else if (ownedLab) {
      await call(ownedLab, "close").catch(() => {});
    }
    const { report, exitCode } = await finish({
      fatal: error instanceof Error ? error.message : String(error),
      cleanup: error.cleanup,
      remoteCallsBaseline,
      liveArmedRestored: liveArmedSaved,
      exitCode: 1,
    });
    uninstallInterrupt();
    process.exitCode = exitCode;
    console.error(error instanceof Error ? error.message : error);
    return report;
  }
}
