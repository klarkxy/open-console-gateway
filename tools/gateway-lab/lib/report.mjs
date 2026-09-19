import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { redactDeep } from "./common.mjs";

export const STATUS = Object.freeze({
  PASS: "PASS",
  FAIL: "FAIL",
  UNSUPPORTED: "UNSUPPORTED",
  NOT_RUN: "NOT_RUN",
});

export function createCollector() {
  const results = [];

  function record(entry) {
    const status = entry.status || (entry.pass ? STATUS.PASS : STATUS.FAIL);
    const row = { ...entry, status, pass: status === STATUS.PASS };
    results.push(row);
    const extra = row.error ? `: ${row.error}` : "";
    console.log(redactDeep(`${status} ${row.label}${extra}`));
    return row;
  }

  return {
    results,
    record,
    pass(label, extra = {}) {
      const { status: httpStatus, ...rest } = extra;
      return record({ label, httpStatus, ...rest, status: STATUS.PASS });
    },
    fail(label, error, extra = {}) {
      const { status: httpStatus, ...rest } = extra;
      return record({
        label,
        httpStatus,
        ...rest,
        status: STATUS.FAIL,
        error: redactDeep(error instanceof Error ? error.message : String(error)),
      });
    },
    unsupported(label, reason, extra = {}) {
      return record({ label, ...extra, status: STATUS.UNSUPPORTED, error: redactDeep(reason) });
    },
    notRun(label, reason, extra = {}) {
      return record({ label, ...extra, status: STATUS.NOT_RUN, error: redactDeep(reason) });
    },
    counts() {
      const counts = { PASS: 0, FAIL: 0, UNSUPPORTED: 0, NOT_RUN: 0 };
      for (const item of results) counts[item.status] = (counts[item.status] || 0) + 1;
      return counts;
    },
  };
}

export function exitCodeFor(counts, { liveBlocked = false } = {}) {
  if ((counts.FAIL || 0) > 0) return 1;
  if (liveBlocked || (counts.UNSUPPORTED || 0) > 0 || (counts.NOT_RUN || 0) > 0) return 2;
  if ((counts.PASS || 0) === 0) return 2;
  return 0;
}

export async function writeReportDir(dir, files) {
  await mkdir(dir, { recursive: true });
  for (const [name, value] of Object.entries(files)) {
    const target = path.join(dir, name);
    const body = typeof value === "string" ? redactDeep(value) : JSON.stringify(redactDeep(value), null, 2);
    await writeFile(target, body.endsWith("\n") ? body : `${body}\n`);
  }
}

export function resultsMarkdown(report, { title = "Gateway lab results" } = {}) {
  const lines = [];
  lines.push(`# ${title}`);
  lines.push("");
  lines.push(`Generated: ${report.generatedAt}`);
  lines.push("");
  lines.push(`Evidence class: **${report.evidenceClass || "unknown"}**. This run exercised the on-disk CLI binary, not an unbuilt source tree.`);
  lines.push("");
  if (report.binary) {
    lines.push(`- CLI: \`${report.binary.path}\``);
    lines.push(`- SHA-256: \`${report.binary.sha256}\``);
    lines.push(`- mtime: ${report.binary.mtime}`);
    lines.push(`- bytes: ${report.binary.bytes}`);
    lines.push(`- stale relative to sampled dirty source: ${report.binary.staleRelativeToDirtySource}`);
  }
  lines.push("");
  const counts = report.counts || {};
  lines.push(`PASS ${counts.PASS || 0}, FAIL ${counts.FAIL || 0}, UNSUPPORTED ${counts.UNSUPPORTED || 0}, NOT_RUN ${counts.NOT_RUN || 0}.`);
  if (report.remoteCalls != null) lines.push(`Remote calls: ${report.remoteCalls}.`);
  lines.push("");
  lines.push("## Replay");
  lines.push("");
  lines.push("```powershell");
  lines.push(report.replay || "");
  lines.push("```");
  lines.push("");
  if (report.gateway) lines.push(`Gateway: pid ${report.gateway.pid} at ${report.gateway.url}`);
  if (report.control) lines.push(`Control: ${report.control.url}`);
  if (report.listeners) {
    lines.push("Listeners:");
    for (const listener of report.listeners) lines.push(`- ${listener.id} ${listener.url}`);
  }
  lines.push("");
  lines.push("## Scenarios");
  lines.push("");
  for (const item of report.results || []) {
    const note = item.error ? ` — ${item.error}` : "";
    lines.push(`- ${item.status || (item.pass ? "PASS" : "FAIL")} ${item.label}${note}`);
  }
  lines.push("");
  return redactDeep(`${lines.join("\n")}\n`);
}
