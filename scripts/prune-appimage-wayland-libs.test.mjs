import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import {
  sanitizedEnv,
  assertNoSecretsInEnv,
  SECRET_ENV_PATTERNS,
  pruneAppImageWaylandLibs,
} from "./prune-appimage-wayland-libs.mjs";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function tmpDir() {
  const dir = join(process.cwd(), `.prune-test-${randomUUID()}`);
  mkdirSync(dir, { recursive: true });
  return dir;
}

// ---------------------------------------------------------------------------
// sanitizedEnv / assertNoSecretsInEnv
// ---------------------------------------------------------------------------

describe("sanitizedEnv", () => {
  it("includes standard safe variables from process.env", () => {
    const env = sanitizedEnv();
    assert.ok(typeof env.PATH === "string" || env.PATH === undefined,
      "PATH should be present if defined in process.env");
  });

  it("always includes ARCH and APPIMAGE_EXTRACT_AND_RUN", () => {
    const env = sanitizedEnv();
    assert.strictEqual(env.ARCH, "x86_64");
    assert.strictEqual(env.APPIMAGE_EXTRACT_AND_RUN, "1");
  });

  it("never includes known secret patterns", () => {
    for (const pattern of SECRET_ENV_PATTERNS) {
      const env = sanitizedEnv();
      assert.ok(!(pattern in env),
        `sanitizedEnv must not include ${pattern}`);
    }
  });

  it("does not include unrecognized env vars", () => {
    process.env.OCG_PRUNE_TEST_XYZ = "test-value";
    try {
      const env = sanitizedEnv();
      assert.ok(!("OCG_PRUNE_TEST_XYZ" in env),
        "unrecognized vars must not leak into sanitized env");
    } finally {
      delete process.env.OCG_PRUNE_TEST_XYZ;
    }
  });

  it("assertNoSecretsInEnv throws for each known secret", () => {
    for (const pattern of SECRET_ENV_PATTERNS) {
      assert.throws(
        () => assertNoSecretsInEnv({ [pattern]: "leaked" }),
        new RegExp(pattern),
      );
    }
  });

  it("assertNoSecretsInEnv passes for clean env", () => {
    assertNoSecretsInEnv({ PATH: "/usr/bin", HOME: "/home/user", ARCH: "x86_64" });
  });
});

// ---------------------------------------------------------------------------
// pruneAppImageWaylandLibs
// ---------------------------------------------------------------------------

describe("pruneAppImageWaylandLibs", () => {
  let runCalls;
  let capturedRunEnv;
  let runShouldThrow;
  let runSkipRepackOutput;

  /**
   * Build a mock `run` that simulates the expected side-effects of each call:
   *
   *   call 1 — extract: create squashfs-root/usr/lib with `waylandLibs`
   *   call 2 — repack:  create the pruned output file, capture env
   *   call 3 — verify:  create squashfs-root/usr/lib with `verificationLibs`
   *                      (null = empty, no libwayland-*)
   *
   * @param {object} opts
   * @param {string[]} [opts.waylandLibs] — libs present in the extracted AppImage
   * @param {string[]|null} [opts.verificationLibs] — libs present after repack
   */
  function mockRunFactory({ waylandLibs = [], verificationLibs = null } = {}) {
    let count = 0;
    return (cmd, args, runOpts = {}) => {
      count++;
      runCalls.push({ count, cmd, args, opts: { ...runOpts } });
      if (runOpts.env) capturedRunEnv = { ...runOpts.env };

      if (runShouldThrow) {
        throw new Error("mock run threw");
      }

      if (count === 1) {
        // Extraction — create squashfs-root at the cwd the function gave us.
        const extractCwd = runOpts.cwd;
        if (extractCwd) {
          const libDir = join(extractCwd, "squashfs-root", "usr", "lib");
          mkdirSync(libDir, { recursive: true });
          for (const lib of waylandLibs) {
            writeFileSync(join(libDir, lib), "");
          }
        }
      }

      if (count === 2 && !runSkipRepackOutput) {
        // Repack — create the output file so rename can proceed.
        const outputPath = args[1];
        if (outputPath) writeFileSync(outputPath, "mock-pruned-appimage");
      }

      if (count === 3) {
        // Verification extraction.
        const cwd = runOpts.cwd || process.cwd();
        const verifyLibDir = join(cwd, "squashfs-root", "usr", "lib");
        mkdirSync(verifyLibDir, { recursive: true });
        if (verificationLibs) {
          for (const lib of verificationLibs) {
            writeFileSync(join(verifyLibDir, lib), "");
          }
        }
      }
    };
  }

  beforeEach(() => {
    runCalls = [];
    capturedRunEnv = null;
    runShouldThrow = false;
    runSkipRepackOutput = false;
  });

  // -- failClosed: missing tool ---------------------------------------------

  it("missing appimagetool respects failClosed", () => {
    assert.throws(
      () => pruneAppImageWaylandLibs("/fake/appimage", {
        failClosed: true,
        appimagetoolPath: "",
      }),
      /appimagetool not found/,
    );
    assert.strictEqual(
      pruneAppImageWaylandLibs("/fake/appimage", {
        failClosed: false,
        appimagetoolPath: "",
      }),
      "/fake/appimage",
      "failClosed=false missing tool returns original path",
    );
  });

  // -- failClosed: no libraries found ---------------------------------------

  it("missing libwayland respects failClosed", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "mock-appimage");

    try {
      assert.throws(
        () => pruneAppImageWaylandLibs(appImage, {
          failClosed: true,
          appimagetoolPath: "/fake/appimagetool",
          run: mockRunFactory({ waylandLibs: [] }),
        }),
        /No bundled libwayland-/,
      );
      assert.strictEqual(
        pruneAppImageWaylandLibs(appImage, {
          failClosed: false,
          appimagetoolPath: "/fake/appimagetool",
          run: mockRunFactory({ waylandLibs: [] }),
        }),
        appImage,
        "failClosed=false with no libwayland-* returns original path",
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- success path ---------------------------------------------------------

  it("successfully prunes, verifies, and replaces the AppImage", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");

    try {
      const result = pruneAppImageWaylandLibs(appImage, {
        failClosed: true,
        appimagetoolPath: "/fake/appimagetool",
        run: mockRunFactory({
          waylandLibs: ["libwayland-client.so.0", "libwayland-egl.so.1"],
        }),
      });

      assert.strictEqual(result, appImage);
      assert.ok(existsSync(appImage), "pruned AppImage should exist at original path");
      assert.ok(!existsSync(`${appImage}.wayland-backup`), "backup should be removed");
      assert.strictEqual(runCalls.length, 3,
        `expected 3 run calls (extract, repack, verify), got ${runCalls.length}`);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- extraction failure ---------------------------------------------------

  it("throws when extraction does not produce squashfs-root", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "mock-appimage");

    // A mock that does NOT create squashfs-root on call 1.
    let count = 0;
    const noExtractRun = (cmd, args, runOpts) => { count++; };

    try {
      assert.throws(
        () => pruneAppImageWaylandLibs(appImage, {
          failClosed: true,
          appimagetoolPath: "/fake/appimagetool",
          run: noExtractRun,
        }),
        /did not produce squashfs-root/,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- repack failure -------------------------------------------------------

  it("throws when the run function throws", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");
    runShouldThrow = true;

    try {
      assert.throws(
        () => pruneAppImageWaylandLibs(appImage, {
          failClosed: true,
          appimagetoolPath: "/fake/appimagetool",
          run: mockRunFactory({
            waylandLibs: ["libwayland-client.so.0"],
          }),
        }),
      );
      assert.ok(existsSync(appImage),
        "original AppImage should be preserved on failure");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- post-pruning verification --------------------------------------------

  it("throws when verification finds remaining libwayland-* libraries", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");

    try {
      assert.throws(
        () => pruneAppImageWaylandLibs(appImage, {
          failClosed: true,
          appimagetoolPath: "/fake/appimagetool",
          run: mockRunFactory({
            waylandLibs: ["libwayland-client.so.0"],
            verificationLibs: ["libwayland-client.so.0"],
          }),
        }),
        /still contains bundled Wayland libraries/,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- env sanitization -----------------------------------------------------

  it("does not pass secrets to the appimagetool subprocess", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");

    const saved = {};
    for (const pattern of SECRET_ENV_PATTERNS) {
      saved[pattern] = process.env[pattern];
      process.env[pattern] = `fake-${pattern}`;
    }

    try {
      pruneAppImageWaylandLibs(appImage, {
        failClosed: true,
        appimagetoolPath: "/fake/appimagetool",
        run: mockRunFactory({
          waylandLibs: ["libwayland-client.so.0"],
        }),
      });

      assert.ok(capturedRunEnv, "should have captured run env from repack call");
      for (const pattern of SECRET_ENV_PATTERNS) {
        assert.ok(!(pattern in capturedRunEnv),
          `captured env must not contain ${pattern}`);
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
      for (const pattern of SECRET_ENV_PATTERNS) {
        if (saved[pattern] === undefined) {
          delete process.env[pattern];
        } else {
          process.env[pattern] = saved[pattern];
        }
      }
    }
  });

  it("repack env contains required AppImage vars", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");

    try {
      pruneAppImageWaylandLibs(appImage, {
        failClosed: true,
        appimagetoolPath: "/fake/appimagetool",
        run: mockRunFactory({
          waylandLibs: ["libwayland-client.so.0"],
        }),
      });

      assert.ok(capturedRunEnv, "should have captured run env");
      assert.strictEqual(capturedRunEnv.ARCH, "x86_64");
      assert.strictEqual(capturedRunEnv.APPIMAGE_EXTRACT_AND_RUN, "1");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // -- rollback on replacement failure --------------------------------------

  it("rolls back when replacement rename fails", () => {
    const dir = tmpDir();
    const appImage = join(dir, "test.AppImage");
    writeFileSync(appImage, "original-content");
    runSkipRepackOutput = true;

    try {
      assert.throws(
        () => pruneAppImageWaylandLibs(appImage, {
          failClosed: true,
          appimagetoolPath: "/fake/appimagetool",
          run: mockRunFactory({
            waylandLibs: ["libwayland-client.so.0"],
          }),
        }),
      );
      // Original should be preserved, backup cleaned
      assert.ok(existsSync(appImage),
        "original AppImage should be preserved on rename failure");
      assert.ok(!existsSync(`${appImage}.wayland-backup`),
        "backup should be cleaned on rename failure");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
