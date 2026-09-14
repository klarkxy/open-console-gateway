import assert from "node:assert/strict";
import test from "node:test";
import {
  CPA_LOG_TAIL_LINES,
  CPA_OAUTH_PROVIDERS,
  cpaAccountKey,
  cpaCliImportAlreadyPresent,
  cpaCliImportFilenameToken,
  cpaOAuthProviderForCliAccount,
  cpaClientKeysAvailable,
  cpaLogTail,
  cpaManagedRuntimeConfirmed,
  cpaRuntimeControls,
  cpaRuntimeMode,
  formatCpaQuota,
  groupCpaCatalogModels,
  isCpaOAuthSuccessStatus,
  isCpaOAuthTerminalStatus,
  isCpaPhaseBusy,
  partitionCpaRuntimeKeys,
} from "./cpa-runtime.ts";
import type { CpaIntegration, CpaRuntime, CpaRuntimeKey } from "../api/generated/dashboard-v3.ts";

function runtime(overrides: Partial<CpaRuntime> = {}): CpaRuntime {
  return {
    assetSha256: null,
    baseUrl: "http://127.0.0.1:8317",
    currentOperation: null,
    currentVersion: "1.0.0",
    error: null,
    installed: true,
    latestVersion: null,
    owned: true,
    phase: "idle",
    port: 8317,
    previousVersion: null,
    processGeneration: 1,
    revision: 1,
    running: true,
    supported: true,
    unavailableReason: null,
    updateAvailable: false,
    ...overrides,
  };
}

function integration(overrides: Partial<CpaIntegration> = {}): CpaIntegration {
  return {
    accountId: null,
    baseUrl: "http://127.0.0.1:8317",
    baseUrlReadOnly: false,
    configured: false,
    currentOperation: null,
    enabled: false,
    inferenceKeyConfigured: false,
    installedVersion: null,
    latestVersion: null,
    managementKeyConfigured: false,
    modelCount: 0,
    modelsRefreshedAt: null,
    processGeneration: 1,
    revision: 1,
    runtimeOwned: false,
    runtimeRunning: false,
    runtimeSupported: true,
    runtimeUnavailableReason: null,
    updateAvailable: false,
    ...overrides,
  };
}

test("Overview defaults to a discoverable managed install, yet keeps external connection selectable", () => {
  const freshRuntime = runtime({ installed: false, owned: false, running: false });
  assert.equal(cpaRuntimeMode(integration(), freshRuntime), "managed");
  assert.equal(cpaRuntimeMode(integration(), freshRuntime, "external"), "external");
  assert.equal(cpaRuntimeMode(integration({ configured: true }), freshRuntime), "external");
  assert.equal(cpaRuntimeMode(integration({ runtimeOwned: true }), runtime()), "managed");
  assert.equal(cpaRuntimeMode(integration({ runtimeSupported: false }), null, "managed"), "unsupported");
});

test("a missing runtime snapshot is not confirmed managed support", () => {
  assert.equal(cpaManagedRuntimeConfirmed(integration(), null), false);
  assert.equal(cpaManagedRuntimeConfirmed(integration(), runtime({ supported: false })), false);
  assert.ok(cpaManagedRuntimeConfirmed(integration(), runtime()));
  assert.equal(cpaRuntimeMode(integration(), null), "external");
  assert.equal(cpaRuntimeMode(integration(), null, "managed"), "external");
  assert.equal(cpaRuntimeMode(integration({ runtimeOwned: true }), null), "managed");
  assert.equal(cpaRuntimeMode(integration({ runtimeSupported: false, runtimeOwned: true }), null), "unsupported");
});

test("CPA catalog groups by source and keeps unknown sources last", () => {
  assert.deepEqual(
    groupCpaCatalogModels([
      { id: "claude-sonnet", ownedBy: "anthropic" },
      { id: "gpt-5", ownedBy: "openai" },
      { id: "mystery", ownedBy: null },
      { id: "o3", ownedBy: "openai" },
      { id: "  ", ownedBy: "  " },
    ]),
    [
      { source: "anthropic", models: [{ id: "claude-sonnet", ownedBy: "anthropic" }] },
      {
        source: "openai",
        models: [
          { id: "gpt-5", ownedBy: "openai" },
          { id: "o3", ownedBy: "openai" },
        ],
      },
      {
        source: "",
        models: [
          { id: "  ", ownedBy: "  " },
          { id: "mystery", ownedBy: null },
        ],
      },
    ],
  );
});

test("client keys exist only for an owned, installed, supported managed runtime", () => {
  assert.ok(cpaClientKeysAvailable(runtime()));
  assert.ok(!cpaClientKeysAvailable(null));
  assert.ok(!cpaClientKeysAvailable(runtime({ supported: false })));
  assert.ok(!cpaClientKeysAvailable(runtime({ owned: false })));
  assert.ok(!cpaClientKeysAvailable(runtime({ installed: false })));
});

test("busy phases are exactly the lifecycle phases that block controls", () => {
  const busy = ["checking", "downloading", "installing", "starting"] as const;
  for (const phase of busy) {
    assert.ok(isCpaPhaseBusy(phase), phase);
  }
  assert.ok(!isCpaPhaseBusy("idle"));
  assert.ok(!isCpaPhaseBusy("failed"));
});

test("control availability follows install/run/version state", () => {
  const updateCheck = {
    currentVersion: "1.0.0",
    latestVersion: "1.1.0",
    processGeneration: 1,
    releaseUrl: "https://example.com/release",
    revision: 1,
    updateAvailable: true,
  };
  const allOff = { install: false, start: false, stop: false, checkUpdate: false, update: false, rollback: false, remove: false };
  const blocked = [
    { label: "busy flag", args: { runtime: runtime(), busy: true, updateCheck } },
    { label: "downloading phase", args: { runtime: runtime({ phase: "downloading" }), busy: false, updateCheck } },
    { label: "unsupported", args: { runtime: runtime({ supported: false }), busy: false, updateCheck } },
    { label: "unowned installed", args: { runtime: runtime({ owned: false }), busy: false, updateCheck } },
    { label: "missing runtime", args: { runtime: null, busy: false, updateCheck } },
  ];
  for (const { label, args } of blocked) {
    assert.deepEqual(cpaRuntimeControls(args), allOff, label);
  }

  const noUpdate = { updateCheck: null, busy: false };
  const installedRunning = cpaRuntimeControls({ runtime: runtime(), ...noUpdate });
  assert.deepEqual(installedRunning, {
    install: false,
    start: false,
    stop: true,
    checkUpdate: true,
    update: false,
    rollback: false,
    remove: true,
  });

  const fresh = cpaRuntimeControls({ runtime: runtime({ installed: false, running: false }), ...noUpdate });
  assert.ok(fresh.install && !fresh.start && !fresh.stop && !fresh.remove);

  const freshChecked = cpaRuntimeControls({
    runtime: runtime({ installed: false, running: false }),
    busy: false,
    updateCheck: {
      currentVersion: null,
      latestVersion: "1.1.0",
      processGeneration: 1,
      releaseUrl: "https://example.com/release",
      revision: 1,
      updateAvailable: true,
    },
  });
  assert.ok(!freshChecked.update);

  const installedStopped = cpaRuntimeControls({
    runtime: runtime({ running: false, previousVersion: "0.9.0" }),
    ...noUpdate,
  });
  assert.ok(installedStopped.start && !installedStopped.stop && installedStopped.rollback);

  const withUpdate = cpaRuntimeControls({
    runtime: runtime(),
    busy: false,
    updateCheck: {
      currentVersion: "1.0.0",
      latestVersion: "1.1.0",
      processGeneration: 1,
      releaseUrl: "https://example.com/release",
      revision: 1,
      updateAvailable: true,
    },
  });
  assert.ok(withUpdate.update);

  const checkedCurrent = cpaRuntimeControls({
    runtime: runtime(),
    busy: false,
    updateCheck: {
      currentVersion: "1.1.0",
      latestVersion: "1.1.0",
      processGeneration: 1,
      releaseUrl: "https://example.com/release",
      revision: 1,
      updateAvailable: false,
    },
  });
  assert.ok(!checkedCurrent.update);
});

test("log tail is bounded, trailing blank lines are stripped, CRLF is normalized", () => {
  assert.equal(cpaLogTail(""), "");
  assert.equal(cpaLogTail("\n\n"), "");
  assert.equal(cpaLogTail("a\nb\nc\n"), "a\nb\nc");
  assert.equal(cpaLogTail("a\r\nb\r\n"), "a\nb");
  const many = Array.from({ length: CPA_LOG_TAIL_LINES + 50 }, (_, index) => `line-${index}`).join("\n");
  const tail = cpaLogTail(many).split("\n");
  assert.equal(tail.length, CPA_LOG_TAIL_LINES);
  assert.equal(tail[0], "line-50");
  assert.equal(tail[tail.length - 1], `line-${CPA_LOG_TAIL_LINES + 49}`);
  assert.equal(cpaLogTail(many, 3), `line-${CPA_LOG_TAIL_LINES + 47}\nline-${CPA_LOG_TAIL_LINES + 48}\nline-${CPA_LOG_TAIL_LINES + 49}`);
});

test("protected routing keys never mix with direct client keys", () => {
  const routing: CpaRuntimeKey = { fingerprint: "fp-routing", hint: "sk-…ocg", protected: true };
  const direct: CpaRuntimeKey = { fingerprint: "fp-direct", hint: "sk-…abc", protected: false };
  const { protectedKeys, directKeys } = partitionCpaRuntimeKeys([direct, routing]);
  assert.deepEqual(protectedKeys, [routing]);
  assert.deepEqual(directKeys, [direct]);
  assert.deepEqual(partitionCpaRuntimeKeys([]), { protectedKeys: [], directKeys: [] });
});

test("account identity is name plus optional authIndex", () => {
  assert.equal(cpaAccountKey({ name: "acc", authIndex: "0" }), "acc:0");
  assert.equal(cpaAccountKey({ name: "acc", authIndex: null }), "acc:");
});

test("quota hides empty CPA trackers and renders scalars or JSON", () => {
  assert.equal(formatCpaQuota("100/200"), "100/200");
  assert.equal(formatCpaQuota(42), "42");
  assert.equal(formatCpaQuota({ remaining: 5 }), '{"remaining":5}');
  assert.equal(formatCpaQuota({ signals: { gpt: { used: 1 } } }), '{"signals":{"gpt":{"used":1}}}');
  assert.equal(formatCpaQuota(null), null);
  assert.equal(formatCpaQuota(undefined), null);
  assert.equal(formatCpaQuota({}), null);
  assert.equal(formatCpaQuota({ signals: {} }), null);
  assert.equal(formatCpaQuota({ signals: { gpt: {} } }), null);
  const circular: Record<string, unknown> = {};
  circular.self = circular;
  assert.equal(formatCpaQuota(circular), null);
});

test("OAuth provider registry keeps the fixed five providers in order", () => {
  assert.deepEqual(
    CPA_OAUTH_PROVIDERS.map(({ id }) => id),
    ["codex", "anthropic", "antigravity", "kimi", "xai"],
  );
});

test("CLI import filename tokens match CPA account names", () => {
  assert.equal(cpaCliImportFilenameToken("codex"), "codex");
  assert.equal(cpaCliImportFilenameToken("anthropic"), "claude");
  assert.equal(cpaCliImportFilenameToken("kimi"), "kimi");
  const accounts = [
    { name: "ocg-cli-codex-a1b2c3.json" },
    { name: "ocg-cli-claude-f0e1d2.json" },
    { name: "chatgpt-oauth.json" },
  ];
  assert.ok(cpaCliImportAlreadyPresent("codex", accounts));
  assert.ok(cpaCliImportAlreadyPresent("anthropic", accounts));
  assert.ok(!cpaCliImportAlreadyPresent("kimi", accounts));
  assert.ok(!cpaCliImportAlreadyPresent("codex", [{ name: "codex-work.json" }]));
  assert.equal(cpaOAuthProviderForCliAccount({ name: "ocg-cli-claude-f0e1d2.json" }), "anthropic");
  assert.equal(cpaOAuthProviderForCliAccount({ name: "chatgpt-oauth.json" }), null);
});

test("OAuth polling stops on terminal statuses and refreshes accounts only on success", () => {
  for (const status of ["ok", "completed", "success", "cancelled", "failed", "expired", "error", "OK"]) {
    assert.ok(isCpaOAuthTerminalStatus(status), status);
  }
  assert.ok(!isCpaOAuthTerminalStatus("pending"));
  assert.ok(!isCpaOAuthTerminalStatus("waiting"));
  for (const status of ["ok", "success", "completed"]) {
    assert.ok(isCpaOAuthSuccessStatus(status), status);
  }
  for (const status of ["cancelled", "failed", "expired", "error", "pending"]) {
    assert.ok(!isCpaOAuthSuccessStatus(status), status);
  }
});
