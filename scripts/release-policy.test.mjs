import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  compareStableVersions,
  immutableTagDecision,
  isPrereleaseVersion,
  normalizeReleaseVersion,
  pairedChannelDecision,
  shouldAdvanceChannel,
  validateComposeVersion,
} from "./release-policy.mjs";

const digest = (character) => `sha256:${character.repeat(64)}`;
const repositoryRoot = fileURLToPath(new URL("../", import.meta.url));
const packageVersion = JSON.parse(
  readFileSync(new URL("../package.json", import.meta.url), "utf8"),
).version;

test("stable release channels advance monotonically", () => {
  assert.equal(shouldAdvanceChannel("v1.5.0", "v1.4.9"), true);
  assert.equal(shouldAdvanceChannel("1.5.0", "1.5.0"), false);
  assert.equal(shouldAdvanceChannel("v1.4.9", "v1.5.0"), false);
  assert.equal(shouldAdvanceChannel("v1.5.0", ""), true);
  assert.equal(compareStableVersions("v10.0.0", "v2.99.99"), 1);
  assert.throws(() => shouldAdvanceChannel("v1.5.0-beta.1", "v1.4.2"), /stable semantic version/);
});

test("prerelease classification preserves the stable-only latest policy", () => {
  assert.equal(isPrereleaseVersion("v1.5.8-beta.1"), true);
  assert.equal(isPrereleaseVersion("1.5.8-rc.2"), true);
  assert.equal(isPrereleaseVersion("v1.5.8"), false);
  assert.equal(normalizeReleaseVersion("v1.5.8-beta.1"), "1.5.8-beta.1");
  assert.throws(() => isPrereleaseVersion("v1.5.8-beta.01"), /semantic version/);
  assert.throws(() => isPrereleaseVersion("v1.5.8+build.1"), /semantic version/);
});

test("immutable image tags are created once or retained at the same digest", () => {
  assert.equal(immutableTagDecision({
    tag: "1.5.0",
    candidateDigest: digest("a"),
    existingDigest: "",
  }), "create");
  assert.equal(immutableTagDecision({
    tag: "sha-0123456789ab",
    candidateDigest: digest("a"),
    existingDigest: digest("a"),
  }), "keep");
  assert.throws(() => immutableTagDecision({
    tag: "1.5.0",
    candidateDigest: digest("a"),
    existingDigest: digest("b"),
  }), /Refusing to move immutable container tag/);
});

test("Compose header and default image must match the release version", () => {
  const valid = `# Pull-only Docker Compose example for Open Console Gateway v1.5.0.\n`
    + `image: \${OCG_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr:1.5.0}\n`
    + `browser: \${OCG_BROWSER_IMAGE:-ghcr.io/klarkxy/opencode-go-mgr-browser:1.5.0}\n`;
  assert.equal(validateComposeVersion(valid, "1.5.0"), "1.5.0");
  assert.throws(
    () => validateComposeVersion(valid.replace(/:1\.5\.0}/, ":1.4.2}"), "1.5.0"),
    /Compose version mismatch/,
  );
  assert.throws(
    () => validateComposeVersion(valid.replace(/browser:1\.5\.0}/, "browser:1.4.2}"), "1.5.0"),
    /Compose version mismatch/,
  );
  assert.throws(
    () => validateComposeVersion(valid.replace(/^browser:.*\n/m, ""), "1.5.0"),
    /exactly one/,
  );
  assert.throws(
    () => validateComposeVersion(`${valid}${valid}`, "1.5.0"),
    /exactly one/,
  );
  const prerelease = valid.replaceAll("1.5.0", "1.5.0-rc.1");
  assert.equal(validateComposeVersion(prerelease, "v1.5.0-rc.1"), "1.5.0-rc.1");
});

test("paired moving channels either converge at the candidate or remain aligned", () => {
  assert.deepEqual(pairedChannelDecision({
    candidate: "1.5.1",
    mainCurrent: "1.5.0",
    browserCurrent: "1.5.1",
  }), { mainAdvance: true, browserAdvance: false, version: "1.5.1" });
  assert.deepEqual(pairedChannelDecision({
    candidate: "1.5.0",
    mainCurrent: "1.5.1",
    browserCurrent: "1.5.1",
  }), { mainAdvance: false, browserAdvance: false, version: "1.5.1" });
  assert.throws(() => pairedChannelDecision({
    candidate: "1.5.0",
    mainCurrent: "1.5.1",
    browserCurrent: "1.5.2",
  }), /Refusing to leave paired container channel split/);
  assert.throws(() => pairedChannelDecision({
    candidate: "1.5.0",
    mainCurrent: "",
    browserCurrent: "1.5.1",
  }), /Refusing to leave paired container channel split/);
});

test("release preflight rejects a tag that does not match repository versions", () => {
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL("./release.mjs", import.meta.url)), "--check"],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        OCG_RELEASE_TAG: "v9.9.9",
        OCG_REQUIRE_UPDATER_ARTIFACTS: "0",
      },
    },
  );
  assert.notEqual(result.status, 0);
  const output = `${result.stdout}\n${result.stderr}`;
  assert.ok(
    output.includes(`Release tag v9.9.9 does not match version ${packageVersion}`),
    output,
  );
});
