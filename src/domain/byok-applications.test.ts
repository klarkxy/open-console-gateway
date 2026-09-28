import assert from "node:assert/strict";
import test from "node:test";
import {
  BYOK_CLIENTS,
  BYOK_CLIENT_LABELS,
  BYOK_STATUS_FALLBACK,
  HARNESS_DEFAULT_KEY_NAMES,
  byokConfigureAction,
  byokDisplayTarget,
  byokMutationExpectation,
  byokRecoverAvailable,
  byokRemoveAvailable,
  byokStatusPresentation,
  normalizeByokClient,
  readApplicationsTab,
  refreshConnectionAfterHarnessMutation,
} from "./byok-applications.ts";

test("client ids are the fixed four and labels are brand names", () => {
  assert.deepEqual([...BYOK_CLIENTS], ["codex", "kimi", "minimax", "zcode"]);
  for (const client of BYOK_CLIENTS) {
    assert.equal(typeof BYOK_CLIENT_LABELS[client], "string");
    assert.equal(normalizeByokClient(client), client);
  }
  assert.equal(normalizeByokClient("dsh"), null);
  assert.equal(normalizeByokClient(""), null);
  assert.equal(normalizeByokClient(null), null);
});

test("each harness has a named ordinary Key and never a picker id", () => {
  assert.deepEqual(HARNESS_DEFAULT_KEY_NAMES, {
    dsh: "dsh",
    codex: "codex",
    kimi: "kimi-code",
    minimax: "minimax-code",
    zcode: "zcode",
  });
});

test("applications tab deep link: byok clients and dsh, unknown falls back to dsh", () => {
  assert.equal(readApplicationsTab("?view=applications&app=codex"), "codex");
  assert.equal(readApplicationsTab("?view=applications&app=zcode"), "zcode");
  assert.equal(readApplicationsTab("?view=applications&app=dsh"), "dsh");
  assert.equal(readApplicationsTab("?view=applications&app=nope"), "dsh");
  assert.equal(readApplicationsTab("?view=applications"), "dsh");
  assert.equal(readApplicationsTab(""), "dsh");
});

test("status presentations cover known statuses with tone buckets, unknown gets fallback", () => {
  assert.equal(byokStatusPresentation("configured").tone, "success");
  assert.equal(byokStatusPresentation("ready").tone, "info");
  assert.equal(byokStatusPresentation("recovery_required").tone, "error");
  assert.equal(byokStatusPresentation("incompatible").tone, "error");
  assert.equal(byokStatusPresentation("conflict").tone, "warning");
  assert.equal(byokStatusPresentation("not_detected").tone, "warning");
  assert.equal(byokStatusPresentation("unsupported_runtime").tone, "default");
  const unknown = byokStatusPresentation("something-new");
  assert.equal(unknown, BYOK_STATUS_FALLBACK);
  for (const status of ["configured", "ready", "recovery_required", "incompatible", "conflict", "not_detected", "unsupported_runtime", "something-new"]) {
    const presentation = byokStatusPresentation(status);
    assert.equal(typeof presentation.labelKey, "string");
    assert.equal(typeof presentation.hintKey, "string");
  }
});

test("configure action requires support and fingerprint; managed targets offer update", () => {
  assert.equal(byokConfigureAction(null), "unavailable");
  assert.equal(byokConfigureAction({ configureSupported: false, fingerprint: "fp", configuredModelIds: [] }), "unavailable");
  assert.equal(byokConfigureAction({ configureSupported: true, fingerprint: null, configuredModelIds: [] }), "unavailable");
  assert.equal(byokConfigureAction({ configureSupported: true, fingerprint: "fp", configuredModelIds: [] }), "configure");
  assert.equal(byokConfigureAction({ configureSupported: true, fingerprint: "fp", configuredModelIds: ["m1"] }), "update");
});

test("remove and recover need only backend support and a fingerprint", () => {
  assert.equal(byokRemoveAvailable({ removeSupported: true, fingerprint: "fp" }), true);
  assert.equal(byokRemoveAvailable({ removeSupported: true, fingerprint: null }), false);
  assert.equal(byokRemoveAvailable({ removeSupported: false, fingerprint: "fp" }), false);
  assert.equal(byokRecoverAvailable({ recoverySupported: true, fingerprint: "fp" }), true);
  assert.equal(byokRecoverAvailable({ recoverySupported: false, fingerprint: "fp" }), false);
});

test("mutation expectation pins the inspection CAS tokens", () => {
  const expectation = byokMutationExpectation({
    revision: { revision: 41, processGeneration: 7, pricingRevision: "p" },
  });
  assert.deepEqual(expectation, { expectedRevision: 41, processGeneration: 7 });
});

test("display target prefers the resolved config path", () => {
  assert.equal(byokDisplayTarget({ configPath: "/home/u/.codex/config.toml", targetPaths: ["/other"] }), "/home/u/.codex/config.toml");
  assert.equal(byokDisplayTarget({ configPath: "", targetPaths: ["/fallback"] }), "/fallback");
  assert.equal(byokDisplayTarget({ configPath: "", targetPaths: [] }), "");
  assert.equal(byokDisplayTarget(null), "");
});

test("connection reload failure after a harness mutation is ignored", async () => {
  await refreshConnectionAfterHarnessMutation(async () => {
    throw new Error("reload failed");
  });
  let ran = false;
  await refreshConnectionAfterHarnessMutation(async () => {
    ran = true;
  });
  assert.equal(ran, true);
});

test("connection reload skips when the captured session is no longer current", async () => {
  let ran = false;
  await refreshConnectionAfterHarnessMutation(async () => {
    ran = true;
  }, { captured: 1, current: () => 2 });
  assert.equal(ran, false);
});

test("connection reload still runs when the captured session is unchanged", async () => {
  let ran = false;
  await refreshConnectionAfterHarnessMutation(async () => {
    ran = true;
  }, { captured: 4, current: () => 4 });
  assert.equal(ran, true);
});
