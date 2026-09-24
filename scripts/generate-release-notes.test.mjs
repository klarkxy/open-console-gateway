import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  PLATFORM_RELEASE_NOTES,
  PRERELEASE_WARNING,
  buildReleaseNotes,
  formatChangeLine,
  generateReleaseNotes,
  parseCommitSubject,
  selectPreviousTag,
} from "./generate-release-notes.mjs";

function runGit(cwd, args) {
  const result = spawnSync("git", args, {
    cwd,
    encoding: "utf8",
    windowsHide: true,
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: "ocg-test",
      GIT_AUTHOR_EMAIL: "ocg-test@example.com",
      GIT_COMMITTER_NAME: "ocg-test",
      GIT_COMMITTER_EMAIL: "ocg-test@example.com",
    },
  });
  if (result.status !== 0) {
    throw new Error(result.stderr?.trim() || `git ${args.join(" ")} failed`);
  }
  return result.stdout ?? "";
}

test("parseCommitSubject understands conventional commits and filters noise", () => {
  assert.deepEqual(parseCommitSubject("feat(macos): add Dock icon setting"), {
    kind: "section",
    type: "feat",
    scope: "macos",
    breaking: false,
    description: "add Dock icon setting",
    subject: "feat(macos): add Dock icon setting",
  });
  assert.equal(parseCommitSubject("style: rustfmt multi-protocol").kind, "excluded");
  assert.equal(parseCommitSubject("test: expand frontend coverage").kind, "excluded");
  assert.equal(parseCommitSubject("chore: prepare v1.5.6").kind, "excluded");
  assert.equal(parseCommitSubject("release: prepare v1.5.6").kind, "excluded");
  assert.equal(parseCommitSubject("🔧 chore(gitignore): ignore .kilo").type, "chore");
  assert.equal(parseCommitSubject("fix!: drop legacy path").breaking, true);
  assert.equal(parseCommitSubject("plain commit without type").type, "other");
});

test("buildReleaseNotes groups commits and always appends platform warnings", () => {
  const notes = buildReleaseNotes({
    tag: "v1.5.7",
    previousTag: "v1.5.6",
    subjects: [
      "feat: multi-protocol passthrough",
      "fix: harden sticky-global failover",
      "feat(settings): expose account routing controls",
      "style: rustfmt only",
      "test: unit coverage",
      "chore: prepare v1.5.7",
      "chore: add live routing smoke test script",
      "docs: mention release notes generation",
      "unscoped maintenance tweak",
    ],
  });

  assert.match(notes, /^# Open Console Gateway v1\.5\.7\n/);
  assert.match(notes, /## Changes since v1\.5\.6/);
  assert.match(notes, /### Features\n\n- multi-protocol passthrough\n- settings: expose account routing controls/);
  assert.match(notes, /### Fixes\n\n- harden sticky-global failover/);
  assert.match(notes, /### Documentation\n\n- mention release notes generation/);
  assert.match(notes, /### Maintenance\n\n- add live routing smoke test script/);
  assert.match(notes, /### Other\n\n- unscoped maintenance tweak/);
  assert.doesNotMatch(notes, /rustfmt only|unit coverage|prepare v1\.5\.7/);
  assert.ok(notes.trimEnd().endsWith(PLATFORM_RELEASE_NOTES));
});

test("empty ranges still produce a readable stub plus platform notes", () => {
  const notes = buildReleaseNotes({
    tag: "v1.0.0",
    previousTag: null,
    subjects: ["style: only formatting", "test: only tests"],
  });
  assert.match(notes, /## Changes since the beginning/);
  assert.match(notes, /No user-facing commits in this range/);
  assert.ok(notes.includes(PLATFORM_RELEASE_NOTES));
});

test("every prerelease note leads with the full Beta risk warning", () => {
  for (const tag of ["v1.5.8-beta.1", "v1.5.8-rc.2"]) {
    const notes = buildReleaseNotes({
      tag,
      previousTag: "v1.5.7",
      subjects: ["feat: managed account registration and isolated browser profiles"],
    });
    assert.ok(notes.includes(PRERELEASE_WARNING));
    assert.ok(notes.startsWith(`${PRERELEASE_WARNING}\n\n# Open Console Gateway ${tag}`));
  }
});

test("stable notes do not show the Beta warning", () => {
  const notes = buildReleaseNotes({ tag: "v1.5.8", previousTag: "v1.5.7", subjects: [] });
  assert.ok(!notes.includes(PRERELEASE_WARNING));
});

test("selectPreviousTag walks descending versions", () => {
  assert.equal(selectPreviousTag("v1.5.7", ["v1.5.7", "v1.5.6", "v1.5.5"]), "v1.5.6");
  assert.equal(selectPreviousTag("1.5.5", ["v1.5.7", "v1.5.6", "v1.5.5", "v1.4.2"]), "v1.4.2");
  assert.equal(selectPreviousTag("v1.0.0", ["v1.0.0"]), null);
  assert.equal(
    selectPreviousTag("v1.5.8-beta.2", ["v1.5.8-beta.2", "v1.5.8-beta.1", "v1.5.7"]),
    "v1.5.8-beta.1",
  );
  assert.equal(
    selectPreviousTag("v1.5.8-beta.10", ["v1.5.8-beta.2", "v1.5.8-beta.10", "v1.5.7"]),
    "v1.5.8-beta.2",
  );
  assert.equal(
    selectPreviousTag("v1.5.8", ["v1.5.8", "v1.5.8-beta.2", "v1.5.8-beta.1", "v1.5.7"]),
    "v1.5.7",
  );
  assert.throws(() => selectPreviousTag("v9.9.9", ["v1.0.0"]), /was not found/);
});

test("formatChangeLine keeps scope and breaking markers", () => {
  assert.equal(
    formatChangeLine({
      kind: "section",
      type: "fix",
      scope: "gateway",
      breaking: true,
      description: "rename route",
      subject: "fix(gateway)!: rename route",
    }),
    "- gateway: rename route **BREAKING**",
  );
});

test("generateReleaseNotes uses git helpers and previous-tag range", () => {
  const calls = [];
  const runGit = (args) => {
    calls.push(args);
    if (args[0] === "tag") return "v1.5.7\nv1.5.6\nv1.5.5\n";
    if (args[0] === "log") {
      assert.deepEqual(args.slice(0, 2), ["log", "v1.5.6..v1.5.7"]);
      return "feat: shipping notes\nstyle: ignored\n";
    }
    throw new Error(`unexpected git ${args.join(" ")}`);
  };

  const notes = generateReleaseNotes({ tag: "v1.5.7", runGit });
  assert.match(notes, /### Features\n\n- shipping notes/);
  assert.equal(calls.length, 2);
});

test("authored release notes take precedence and must match the tag", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "ocg-authored-release-notes-"));
  try {
    const folder = join(repoRoot, "docs", "releases");
    mkdirSync(folder, { recursive: true });
    const file = join(folder, "v1.5.7.md");
    const runGit = () => { throw new Error("authored notes should not query Git"); };
    writeFileSync(file, "# v1.5.7\n\nUpgrade details.\n", "utf8");
    assert.equal(generateReleaseNotes({ tag: "v1.5.7", repoRoot, runGit }), "# v1.5.7\n\nUpgrade details.");
    writeFileSync(file, "# v1.5.6\n\nStale notes.\n", "utf8");
    assert.throws(() => generateReleaseNotes({ tag: "v1.5.7", repoRoot, runGit }), /heading does not match/);
    writeFileSync(join(folder, "v1.5.8-beta.1.md"), "# v1.5.8-beta.1\n\nNo warning.\n", "utf8");
    const beta = generateReleaseNotes({
      tag: "v1.5.8-beta.1",
      previousTag: "v1.5.7",
      repoRoot,
      runGit: (args) => args[0] === "log" ? "feat: preview\n" : "",
    });
    assert.ok(beta.startsWith(PRERELEASE_WARNING));
  } finally {
    rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("stable generation uses the previous stable tag across same-version Betas", () => {
  const runGit = (args) => {
    if (args[0] === "tag") return "v1.5.8\nv1.5.8-beta.2\nv1.5.8-beta.1\nv1.5.7\n";
    if (args[0] === "log") {
      assert.deepEqual(args.slice(0, 2), ["log", "v1.5.7..v1.5.8"]);
      return "feat: preserve the complete stable feature scope\n";
    }
    throw new Error(`unexpected git ${args.join(" ")}`);
  };

  const notes = generateReleaseNotes({ tag: "v1.5.8", runGit });
  assert.match(notes, /## Changes since v1\.5\.7/);
  assert.match(notes, /preserve the complete stable feature scope/);
  assert.doesNotMatch(notes, /Beta preview/);
});

test("CLI writes notes for a local tag range without depending on checkout depth", () => {
  const script = fileURLToPath(new URL("./generate-release-notes.mjs", import.meta.url));
  const repoRoot = mkdtempSync(join(tmpdir(), "ocg-release-notes-"));
  try {
    runGit(repoRoot, ["init"]);
    runGit(repoRoot, ["checkout", "-b", "main"]);
    writeFileSync(join(repoRoot, "README.md"), "v1\n", "utf8");
    runGit(repoRoot, ["add", "README.md"]);
    runGit(repoRoot, ["commit", "-m", "chore: initial"]);
    runGit(repoRoot, ["tag", "v1.5.6"]);
    writeFileSync(join(repoRoot, "README.md"), "v2\n", "utf8");
    runGit(repoRoot, ["add", "README.md"]);
    runGit(repoRoot, ["commit", "-m", "feat: shipping notes"]);
    runGit(repoRoot, ["commit", "--allow-empty", "-m", "style: ignored formatting"]);
    runGit(repoRoot, ["tag", "v1.5.7"]);

    const result = spawnSync(
      process.execPath,
      [script, "--tag", "v1.5.7", "--repo-root", repoRoot],
      { encoding: "utf8", windowsHide: true },
    );
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stdout, /# Open Console Gateway v1\.5\.7/);
    assert.match(result.stdout, /## Changes since v1\.5\.6/);
    assert.match(result.stdout, /### Features\n\n- shipping notes/);
    assert.doesNotMatch(result.stdout, /ignored formatting/);
    assert.ok(result.stdout.includes(PLATFORM_RELEASE_NOTES));
  } finally {
    rmSync(repoRoot, { recursive: true, force: true });
  }
});
