import assert from "node:assert/strict";
import test from "node:test";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import { BUILTIN_GOAT_ID, TEMPORARY_POLICY_BUILTIN_KEYS } from "../domain/temporary-policy.ts";
import {
  CLEAR_LOCAL_WAIT_KEY,
  RESTRICTION_MODEL_KEYS,
  RESTRICTION_NAME_KEYS,
  builtinLabel,
  createLocalWaitTicker,
  restrictionConnectionLabel,
  restrictionCredentialLabel,
  restrictionEmptyCode,
  restrictionModelLabel,
  restrictionWaitLabel,
  type RestrictionRow,
} from "./temporary-policy.ts";

function row(overrides: Partial<RestrictionRow> = {}): RestrictionRow {
  return {
    id: "r1",
    ruleId: "custom.lab",
    ruleGeneration: 1,
    source: "connection",
    credentialId: "cred-1",
    destinationId: "dest-1",
    scope: "credential_model",
    upstreamModel: "minimax-m3",
    state: "waiting",
    nextProbeInSeconds: 12,
    probeInFlight: false,
    ...overrides,
  };
}

test("restriction labels never surface Key material and keep empty tables non-healthy", () => {
  const destinations = [{ id: "dest-1", name: "Lab HTTP" }];
  const credentials = [{ id: "cred-1", name: "Office", key: "sk-live", value: "ocg-secret" }];
  assert.deepEqual(restrictionConnectionLabel("dest-1", destinations), { kind: "named", name: "Lab HTTP" });
  assert.deepEqual(restrictionConnectionLabel("missing", destinations), { kind: "unknown_connection" });
  const credential = restrictionCredentialLabel("cred-1", credentials);
  assert.deepEqual(credential, { kind: "named", name: "Office" });
  assert.equal(JSON.stringify(credential).includes("sk-live"), false);
  assert.equal(JSON.stringify(credential).includes("ocg-secret"), false);
  assert.equal(restrictionEmptyCode([]), "no_local_waits");
  assert.equal(restrictionEmptyCode([row()]), null);
});

test("model labels distinguish credential scope from a concrete upstream model", () => {
  assert.deepEqual(
    restrictionModelLabel(row({ scope: "credential", upstreamModel: null })),
    { kind: "credential_scope" },
  );
  assert.deepEqual(
    restrictionModelLabel(row()),
    { kind: "model", model: "minimax-m3" },
  );
});

test("waiting countdown uses the local snapshot; ready and probing do not tick a wait", () => {
  assert.equal(restrictionWaitLabel(row({ nextProbeInSeconds: 20 }), 0, 5_000), 15);
  assert.equal(restrictionWaitLabel(row({ nextProbeInSeconds: null }), 0, 5_000), 0);
  assert.equal(restrictionWaitLabel(row({ state: "ready", nextProbeInSeconds: 20 }), 0, 5_000), 0);
  assert.equal(restrictionWaitLabel(row({ state: "probing", nextProbeInSeconds: 20 }), 0, 5_000), 0);
});

test("local wait ticker starts on activate and clears on destroy without a leftover interval", () => {
  const ticks: number[] = [];
  const intervals = new Map<number, () => void>();
  let nextId = 1;
  const ticker = createLocalWaitTicker((now) => ticks.push(now), {
    now: () => 1_000 + ticks.length,
    setInterval(handler) {
      const id = nextId++;
      intervals.set(id, handler);
      return id;
    },
    clearInterval(id) {
      intervals.delete(id);
    },
  });
  assert.equal(ticker.running(), false);
  ticker.start();
  assert.equal(ticker.running(), true);
  assert.equal(intervals.size, 1);
  assert.equal(ticks.length, 1);
  ticker.start();
  assert.equal(intervals.size, 1);
  ticker.stop();
  assert.equal(ticker.running(), false);
  assert.equal(intervals.size, 0);
});

test("clear action and builtin labels resolve through keys, not hardcoded English", () => {
  assert.equal(CLEAR_LOCAL_WAIT_KEY, "清除本地等待");
  assert.ok(CLEAR_LOCAL_WAIT_KEY in enUSMessages);
  assert.notEqual(enUSMessages[CLEAR_LOCAL_WAIT_KEY], CLEAR_LOCAL_WAIT_KEY);
  assert.deepEqual(builtinLabel(BUILTIN_GOAT_ID), {
    kind: "key",
    key: TEMPORARY_POLICY_BUILTIN_KEYS[BUILTIN_GOAT_ID],
  });
  assert.deepEqual(builtinLabel("builtin.other"), { kind: "id", id: "builtin.other" });
  assert.ok(RESTRICTION_MODEL_KEYS.credential_scope in enUSMessages);
  assert.ok(RESTRICTION_NAME_KEYS.unknown_connection in enUSMessages);
  assert.ok(RESTRICTION_NAME_KEYS.unknown_credential in enUSMessages);
});
