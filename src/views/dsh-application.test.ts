import assert from "node:assert/strict";
import test from "node:test";
import { DashboardConflictError, DashboardRequestError } from "../api/dashboard-v3.ts";
import type { DshApplicationView } from "../api/dashboard-v4.ts";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import {
  DEFAULT_APPLICATION_TAB,
  DSH_APPLICATION_OUTCOME_KEYS,
  DSH_DEFAULT_KEY_NAME,
  DSH_INSTALL_FAILURE_KEYS,
  DSH_UNINSTALL_FAILURE_KEYS,
  dshDraftRuntimeFromInspection,
  dshHostDetail,
  dshInstallAction,
  dshInstallExpectation,
  dshLoadTargetsEqual,
  dshMutationFailureKind,
  dshMutationFeedback,
  dshNormalizedLoadTarget,
  dshStatusPresentation,
  dshUninstallAction,
  normalizeApplicationTab,
  readApplicationTab,
  suggestedRuntimeUrl,
} from "./dsh-application.ts";

test("mutation feedback requires a confirmed runtime outcome and matching final state", () => {
  const installed = dshApp({ installed: true, enabled: true, application: "applied" });
  assert.equal(dshMutationFeedback(installed, "install"), "success");
  assert.equal(dshMutationFeedback(installed, "uninstall"), "unconfirmed");
  for (const application of ["failed", "cancelled", "overridden", "restart-required"] as const) {
    assert.equal(dshMutationFeedback({ ...installed, application }, "install"), application);
  }
  assert.equal(dshMutationFeedback({ ...installed, application: null }, "install"), "unconfirmed");
  assert.equal(dshMutationFeedback({ ...installed, application: null, runtimeUrl: null }, "install"), "success");
  assert.equal(dshMutationFeedback({ ...installed, installed: false }, "uninstall"), "success");
});

function dshApp(overrides: Partial<DshApplicationView> = {}): DshApplicationView {
  return {
    selectedProfilePath: "C:\\Users\\author\\.dsh\\profiles\\web",
    status: "ready",
    detected: true,
    installed: false,
    installSupported: true,
    activationRequired: false,
    version: "1.2.3",
    detail: null,
    targetPaths: [],
    discoveredProfiles: [],
    fingerprint: "fp-1",
    revision: { revision: 7, processGeneration: 3, pricingRevision: "p" },
    runtimeUrl: "http://127.0.0.1:3080",
    uninstallSupported: false,
    enabled: false,
    application: null,
    ...overrides,
  };
}

test("application tab deep link stays narrow and defaults to DSH", () => {
  assert.equal(DEFAULT_APPLICATION_TAB, "dsh");
  assert.equal(normalizeApplicationTab("dsh"), "dsh");
  assert.equal(normalizeApplicationTab("cpa"), null);
  assert.equal(normalizeApplicationTab(""), null);
  assert.equal(normalizeApplicationTab(null), null);
  assert.equal(readApplicationTab("?view=applications&app=dsh"), "dsh");
  assert.equal(readApplicationTab("?view=applications&app=unknown"), "dsh");
  assert.equal(readApplicationTab("?view=applications"), "dsh");
  assert.equal(readApplicationTab(""), "dsh");
});

test("DSH uses a named ordinary Key rather than a picker", () => {
  assert.equal(DSH_DEFAULT_KEY_NAME, "dsh");
});

test("every DSH status has a label, hint, and a meaningful tone", () => {
  const tones = {
    unsupported_runtime: "default",
    not_detected: "warning",
    ready: "info",
    installed: "success",
    incompatible: "error",
    conflict: "warning",
  } as const;
  for (const [status, tone] of Object.entries(tones)) {
    const presentation = dshStatusPresentation(status as DshApplicationView["status"]);
    assert.equal(presentation.tone, tone, status);
    assert.ok(presentation.labelKey.length > 0, status);
    assert.ok(presentation.hintKey.length > 0, status);
  }
});

test("unsupported_runtime presentation keys resolve to localized catalog entries", () => {
  const presentation = dshStatusPresentation("unsupported_runtime");
  // labelKey/hintKey are i18n keys — zh-CN renders the key text itself. The
  // behavior under test is wiring: both keys exist in the catalog and are
  // actually translated, so no locale renders raw or missing text.
  assert.notEqual(presentation.labelKey, presentation.hintKey);
  assert.ok(presentation.labelKey in enUSMessages);
  assert.ok(presentation.hintKey in enUSMessages);
  assert.notEqual(enUSMessages[presentation.labelKey], presentation.labelKey);
  assert.notEqual(enUSMessages[presentation.hintKey], presentation.hintKey);
});

test("install action requires support and a fingerprint; installed offers reinstall", () => {
  assert.equal(dshInstallAction(dshApp()), "install");
  assert.equal(dshInstallAction(dshApp({ installed: true, status: "installed" })), "reinstall");
  assert.equal(dshInstallAction(dshApp({ installSupported: false })), "unavailable");
  assert.equal(dshInstallAction(dshApp({ fingerprint: null })), "unavailable");
  assert.equal(
    dshInstallAction(dshApp({ status: "unsupported_runtime", installSupported: false, fingerprint: null })),
    "unavailable",
  );
});

test("install expectation is captured from the confirmed inspection revision", () => {
  assert.deepEqual(dshInstallExpectation(dshApp()), { expectedRevision: 7, processGeneration: 3 });
});

test("uninstall action and suggested runtime URL stay semantic", () => {
  assert.equal(dshUninstallAction(dshApp()), "unavailable");
  assert.equal(dshUninstallAction(dshApp({ uninstallSupported: true })), "uninstall");
  assert.equal(dshUninstallAction(dshApp({ uninstallSupported: true, fingerprint: null })), "unavailable");
  assert.equal(suggestedRuntimeUrl("web"), "http://127.0.0.1:3080");
  assert.equal(suggestedRuntimeUrl("desktop"), "http://127.0.0.1:19387");
  assert.equal(suggestedRuntimeUrl("dsh-editor"), null);
  for (const [code, key] of Object.entries(DSH_APPLICATION_OUTCOME_KEYS)) {
    assert.ok(key.length > 0, code);
    assert.ok(key in enUSMessages, code);
  }
});

test("normalized load targets compare trimmed profile and runtime drafts", () => {
  assert.deepEqual(
    dshNormalizedLoadTarget("  C:\\web  ", "  http://127.0.0.1:3080  "),
    { profilePath: "C:\\web", runtimeUrl: "http://127.0.0.1:3080" },
  );
  assert.deepEqual(dshNormalizedLoadTarget("   ", ""), { profilePath: undefined, runtimeUrl: undefined });
  assert.equal(
    dshLoadTargetsEqual(
      dshNormalizedLoadTarget("p", "http://127.0.0.1:3080"),
      dshNormalizedLoadTarget(" p ", "  http://127.0.0.1:3080  "),
    ),
    true,
  );
  assert.equal(
    dshLoadTargetsEqual(
      dshNormalizedLoadTarget("p", "http://127.0.0.1:3080"),
      dshNormalizedLoadTarget("p", "http://127.0.0.1:9999"),
    ),
    false,
  );
});

test("draft runtime seed uses the cached inspection, not a live ref", () => {
  assert.equal(dshDraftRuntimeFromInspection(dshApp({ runtimeUrl: "http://127.0.0.1:9999" })), "http://127.0.0.1:9999");
  assert.equal(
    dshDraftRuntimeFromInspection(dshApp({
      runtimeUrl: null,
      selectedProfilePath: "C:\\Users\\author\\.dsh\\profiles\\web",
      discoveredProfiles: [{ home: "C:\\Users\\author\\.dsh", name: "web", path: "C:\\Users\\author\\.dsh\\profiles\\web" }],
    })),
    "http://127.0.0.1:3080",
  );
});

test("only a revision conflict is classified as state changed", () => {
  assert.equal(
    dshMutationFailureKind(new DashboardConflictError("revision", 4, 5)),
    "revision-changed",
  );
  assert.equal(
    dshMutationFailureKind(new DashboardRequestError("cache", 409, "conflict")),
    "conflict",
  );
  assert.equal(
    dshMutationFailureKind(new DashboardRequestError("payload", 409, "operationPayloadMismatch")),
    "conflict",
  );
  assert.equal(
    dshMutationFailureKind(new DashboardRequestError("missing", 412, "preconditionFailed")),
    "failed",
  );
  assert.equal(dshMutationFailureKind(new Error("network")), "failed");
  assert.equal(dshMutationFailureKind("cache"), "failed");
  for (const kind of ["revision-changed", "conflict", "failed"] as const) {
    assert.equal(typeof DSH_INSTALL_FAILURE_KEYS[kind], "string", kind);
    assert.ok(DSH_INSTALL_FAILURE_KEYS[kind] in enUSMessages, kind);
    assert.ok(DSH_UNINSTALL_FAILURE_KEYS[kind] in enUSMessages, kind);
  }
});

test("host detail is shown when the action is blocked and omitted for ready/installed summaries", () => {
  assert.equal(dshHostDetail(null), null);
  assert.equal(dshHostDetail(dshApp({ detail: "Ready to install the OCG provider into the DSH web profile" })), null);
  assert.equal(
    dshHostDetail(dshApp({
      status: "conflict",
      installSupported: false,
      fingerprint: null,
      detail: "DSH has a same-name package that is not an OCG-managed source",
    })),
    "DSH has a same-name package that is not an OCG-managed source",
  );
  assert.equal(
    dshHostDetail(dshApp({
      status: "incompatible",
      installSupported: false,
      detail: "DSH 0.1.4 is not a supported 0.1.5-rc.1 or 0.1.5-rc.2 build",
    })),
    "DSH 0.1.4 is not a supported 0.1.5-rc.1 or 0.1.5-rc.2 build",
  );
  assert.equal(
    dshHostDetail(dshApp({
      status: "unsupported_runtime",
      installSupported: false,
      fingerprint: null,
      detail: "DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
    })),
    "DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host",
  );
});
