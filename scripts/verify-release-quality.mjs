import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

function readGitHub(endpoint) {
  return JSON.parse(execFileSync("gh", ["api", endpoint], {
    encoding: "utf8",
    timeout: 60_000,
    windowsHide: true,
  }));
}

export function verifyReleaseQuality({ repository, sha }, request = readGitHub) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? "") || !/^[0-9a-f]{40}$/.test(sha ?? "")) {
    throw new Error("Release quality requires a repository and a full commit SHA.");
  }
  const query = new URLSearchParams({ branch: "main", event: "push", head_sha: sha, per_page: "1" });
  const response = request(`repos/${repository}/actions/workflows/quality.yml/runs?${query}`);
  // Query the latest run without a success filter: a newer failed or running
  // attempt must not be hidden by an older successful result.
  const run = response?.workflow_runs?.[0];
  if (!run || run.head_sha !== sha || run.head_branch !== "main" || run.event !== "push"
    || run.path !== ".github/workflows/quality.yml"
    || run.head_repository?.full_name?.toLowerCase() !== repository.toLowerCase()) {
    throw new Error(`No matching main Quality run for ${sha}. Push the candidate to main and let Quality finish before tagging.`);
  }
  if (run.status !== "completed" || run.conclusion !== "success") {
    throw new Error(`Main Quality for ${sha} is ${run.status}/${run.conclusion ?? "pending"}: ${run.html_url}`);
  }
  return run;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const sha = process.argv[2] ?? execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    const run = verifyReleaseQuality({ repository: process.env.GITHUB_REPOSITORY, sha });
    console.log(`Reusing main Quality for ${sha}: ${run.html_url}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
