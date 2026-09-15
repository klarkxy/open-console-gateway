import assert from "node:assert/strict";
import test from "node:test";
import { PRIMARY_KEY_ID } from "../api/dashboard-v3.ts";
import type { DshApplication } from "../api/generated/dashboard-v4.ts";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import {
  DEFAULT_APPLICATION_TAB,
  buildDshInstallKeyOptions,
  dshHostDetail,
  dshInstallAction,
  dshInstallExpectation,
  dshStatusPresentation,
  normalizeApplicationTab,
  readApplicationTab,
} from "./dsh-application.ts";

function dshApp(overrides: Partial<DshApplication> = {}): DshApplication {
  return {
    status: "ready",
    detected: true,
    installed: false,
    installSupported: true,
    activationRequired: false,
    version: "1.2.3",
    detail: null,
    targetPaths: [],
    fingerprint: "fp-1",
    revision: { revision: 7, processGeneration: 3, pricingRevision: "p" },
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

test("install key options offer the primary plus enabled sub Keys without plaintext", () => {
  const options = buildDshInstallKeyOptions({
    primary_key: "ocg-primary-secret",
    sub_keys: [
      { id: "sub-1", name: "Laptop", enabled: true, value: "ocg-sub-secret" },
      { id: "sub-2", name: "Old phone", enabled: false, value: "ocg-disabled-secret" },
    ],
  });
  assert.deepEqual(options, [
    { id: PRIMARY_KEY_ID, name: "", kind: "primary" },
    { id: "sub-1", name: "Laptop", kind: "sub" },
  ]);
  for (const option of options) {
    assert.deepEqual(Object.keys(option).sort(), ["id", "kind", "name"]);
  }
});

test("install key options degrade when the primary Key or connection is unavailable", () => {
  assert.deepEqual(buildDshInstallKeyOptions(null), []);
  assert.deepEqual(buildDshInstallKeyOptions({ primary_key: "", sub_keys: [] }), []);
  assert.deepEqual(
    buildDshInstallKeyOptions({
      primary_key: "",
      sub_keys: [{ id: "sub-1", name: "Laptop", enabled: true, value: "x" }],
    }),
    [{ id: "sub-1", name: "Laptop", kind: "sub" }],
  );
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
    const presentation = dshStatusPresentation(status as DshApplication["status"]);
    assert.equal(presentation.tone, tone, status);
    assert.ok(presentation.labelKey.length > 0, status);
    assert.ok(presentation.hintKey.length > 0, status);
  }
});

test("unsupported_runtime hint names the desktop app or native headless CLI and keeps Docker unsupported", () => {
  const presentation = dshStatusPresentation("unsupported_runtime");
  assert.equal(presentation.labelKey, "当前环境不支持");
  assert.equal(
    presentation.hintKey,
    "DSH 安装可在桌面应用或同一台机器上的原生无头 CLI 中进行；官方 Docker 镜像暂不支持。",
  );
  assert.equal(
    enUSMessages[presentation.hintKey],
    "DSH installation is available in the desktop app or a native headless CLI on the same machine; the official Docker image remains unsupported.",
  );
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
