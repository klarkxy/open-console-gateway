import assert from "node:assert/strict";
import test from "node:test";
import { verifyReleaseQuality } from "./verify-release-quality.mjs";

const candidate = { repository: "owner/gateway", sha: "a".repeat(40) };
const successfulRun = {
  head_sha: candidate.sha,
  head_branch: "main",
  event: "push",
  path: ".github/workflows/quality.yml",
  head_repository: { full_name: candidate.repository },
  status: "completed",
  conclusion: "success",
  html_url: "https://github.com/owner/gateway/actions/runs/1",
};

test("reuses only the latest main quality run for the exact release commit", () => {
  const result = verifyReleaseQuality(candidate, (endpoint) => {
    const url = new URL(endpoint, "https://api.github.com/");
    assert.equal(url.pathname, "/repos/owner/gateway/actions/workflows/quality.yml/runs");
    assert.deepEqual(Object.fromEntries(url.searchParams), {
      branch: "main", event: "push", head_sha: candidate.sha, per_page: "1",
    });
    return { workflow_runs: [successfulRun] };
  });
  assert.equal(result, successfulRun);
});

test("missing or mismatched quality evidence cannot authorize publication", () => {
  for (const response of [undefined, {}, { workflow_runs: [] }]) {
    assert.throws(() => verifyReleaseQuality(candidate, () => response), /No matching/);
  }
  for (const change of [
    { head_sha: "b".repeat(40) },
    { head_branch: "feature" },
    { event: "pull_request" },
    { path: ".github/workflows/other.yml" },
    { head_repository: { full_name: "someone/fork" } },
  ]) {
    assert.throws(() => verifyReleaseQuality(candidate, () => ({
      workflow_runs: [{ ...successfulRun, ...change }],
    })), /No matching/);
  }
});

test("a failed or unfinished latest run does not fall back to an older success", () => {
  for (const change of [
    { conclusion: "failure" },
    { conclusion: "cancelled" },
    { conclusion: "skipped" },
    { status: "in_progress", conclusion: null },
    { status: "queued", conclusion: null },
  ]) {
    assert.throws(() => verifyReleaseQuality(candidate, () => ({
      workflow_runs: [{ ...successfulRun, ...change }, successfulRun],
    })), /Main Quality .* is /);
  }
});

test("API errors fail closed and invalid candidate input never calls the API", () => {
  assert.throws(() => verifyReleaseQuality(candidate, () => { throw new Error("API unavailable"); }), /API unavailable/);
  for (const change of [{ repository: "" }, { repository: "owner/repo/extra" }, { sha: "abc123" }]) {
    assert.throws(() => verifyReleaseQuality({ ...candidate, ...change }, () => {
      assert.fail("invalid input must not request evidence");
    }), /requires a repository/);
  }
});
