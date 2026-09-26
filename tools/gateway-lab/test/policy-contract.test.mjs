import assert from "node:assert/strict";
import test from "node:test";
import {
  BUILTIN_GOAT_ID,
  LIVE_OUTBOUND_MODEL,
  RESPONSIBILITY,
  SEQ_ALL_WAITING_ZERO,
  SEQ_FIRST_400_STAYS_A,
  SEQ_GOAT_FIRST_AB,
  SEQ_GOAT_NEXT_B,
  SEQ_LIVE_ABBA,
  SEQ_NEXT_AVOIDS_A_USES_B,
  SEQ_SINGLE_FLIGHT_A1_B1,
  LIVE_MAX_REMOTE,
  PHASE,
  assertChatCompletionProtocolValid,
  assertListenerCounts,
  assertListenerSequence,
  assertLiveSuccessReceipt,
  assertRemoteBudget,
  assertUpstreamCount,
  builtinOverride,
  companionRustRows,
  custom400Rule,
  httpFault,
  liveProbeInput,
  listenersOfHits,
  modelTriad,
  policyExitCode,
  statusOnlyRule,
} from "../lib/policy-contract.mjs";
import { normalizeScript } from "../lib/faults.mjs";

test("HTTP custom 400 expected sequence is A then B, not GOAT A,B,B", () => {
  assert.deepEqual(SEQ_FIRST_400_STAYS_A, ["alpha"]);
  assert.deepEqual(SEQ_NEXT_AVOIDS_A_USES_B, ["bravo"]);
  assert.notDeepEqual(SEQ_FIRST_400_STAYS_A, SEQ_GOAT_FIRST_AB);
  assertListenerSequence(["alpha"], SEQ_FIRST_400_STAYS_A, "first");
  assertListenerSequence(["bravo"], SEQ_NEXT_AVOIDS_A_USES_B, "next");
  assertListenerSequence([], SEQ_ALL_WAITING_ZERO, "zero");
  assert.throws(() => assertListenerSequence(["alpha", "bravo"], SEQ_FIRST_400_STAYS_A, "mismatch"), /mismatch/);
  assert.throws(() => assertUpstreamCount([{ listener: "alpha" }, { listener: "bravo" }], 1, "count"), /count/);
  assertUpstreamCount([{ listener: "alpha" }], 1, "one");
});

test("GOAT sequence stays distinct from generic HTTP", () => {
  assert.deepEqual(SEQ_GOAT_FIRST_AB, ["alpha", "bravo"]);
  assert.deepEqual(SEQ_GOAT_NEXT_B, ["bravo"]);
  assert.equal(BUILTIN_GOAT_ID.startsWith("builtin."), true);
  assert.equal(LIVE_OUTBOUND_MODEL, "minimax-m3");
});

test("rule builders keep destination overlay and empty-match invalid shapes distinguishable", () => {
  const globalRule = custom400Rule({ id: "custom.http400" });
  const overlay = statusOnlyRule({ id: "custom.status400", destinationId: "dest-alpha", enabled: false });
  const goatOff = builtinOverride({ enabled: false });
  assert.equal(globalRule.kind, "custom");
  assert.equal(globalRule.destinationId, null);
  assert.equal(globalRule.backoff.initialSeconds, 1);
  assert.equal(overlay.enabled, false);
  assert.equal(overlay.destinationId, "dest-alpha");
  assert.equal(goatOff.id, BUILTIN_GOAT_ID);
  assert.equal(goatOff.kind, "builtin_override");
  const hits = [{ listener: "alpha" }, { listener: "bravo" }];
  assert.deepEqual(listenersOfHits(hits), ["alpha", "bravo"]);
  const triad = modelTriad({ publicModel: "lab-route", model: "upstream-alpha" });
  assert.equal(triad.public, "lab-route");
  assert.equal(triad.upstream, "upstream-alpha");
  assert.equal(triad.live, "minimax-m3");
});

test("overlapping A then B is two sends, not a shared single hit", () => {
  const hits = [{ listener: "alpha" }, { listener: "bravo" }];
  assertListenerCounts(hits, SEQ_SINGLE_FLIGHT_A1_B1, "A1 B1");
  assert.throws(() => assertUpstreamCount(hits, 1, "wrong total"), /wrong total/);
  assert.deepEqual(SEQ_LIVE_ABBA, ["alpha", "bravo", "bravo", "alpha"]);
});

test("http delayMs stays on the same 400 script item", () => {
  const delayed = normalizeScript(httpFault(400, { error: { message: "x" } }, {}, 800));
  assert.equal(delayed.kind, "http");
  assert.equal(delayed.status, 400);
  assert.equal(delayed.delayMs, 800);
  const capped = normalizeScript({ kind: "http", status: 400, delayMs: 40_000 });
  assert.equal(capped.delayMs, 30_000);
  const omitted = normalizeScript({ kind: "http", status: 400, delayMs: 0 });
  assert.equal(omitted.delayMs, undefined);
});

test("live probe is protocol-valid without requiring LAB_OK text or tools", () => {
  const body = liveProbeInput("lab-route");
  assert.equal(body.tools, undefined);
  assert.equal(body.max_tokens, 32);
  assert.match(body.messages[0].content, /LAB_PROBE/);
  const parsed = assertChatCompletionProtocolValid(
    JSON.stringify({ choices: [{ message: { role: "assistant", content: "unrelated-not-ok" } }] }),
    "protocol",
  );
  assert.equal(parsed.choices[0].message.content, "unrelated-not-ok");
});

test("live success receipt requires liveModel minimax-m3", () => {
  assertLiveSuccessReceipt({ live: true, liveStatus: 200, liveModel: "minimax-m3" }, "ok");
  assert.throws(
    () => assertLiveSuccessReceipt({ live: true, liveStatus: 200, liveModel: "lab-route" }, "model"),
    /liveModel/,
  );
});

test("assertRemoteBudget is whole-run: simulate delta must stay 0 and live checks before send", () => {
  assert.equal(LIVE_MAX_REMOTE, 6);
  assert.equal(assertRemoteBudget({ phase: PHASE.SIMULATE, delta: 0, max: 6 }), 0);
  assert.throws(() => assertRemoteBudget({ phase: PHASE.SIMULATE, delta: 1, max: 6 }), /simulate-phase/);
  assert.throws(() => assertRemoteBudget({ phase: "http", delta: 2, max: 6 }), /simulate-phase/);
  assert.equal(assertRemoteBudget({ phase: PHASE.LIVE, delta: 5, max: 6, beforeSend: true }), 5);
  assert.throws(
    () => assertRemoteBudget({ phase: PHASE.LIVE, delta: 6, max: 6, beforeSend: true }),
    /before send/,
  );
  assert.equal(assertRemoteBudget({ phase: PHASE.LIVE, delta: 6, max: 6, beforeSend: false }), 6);
  assert.throws(
    () => assertRemoteBudget({ phase: PHASE.LIVE, delta: 7, max: 6, beforeSend: false }),
    /exceeded/,
  );
});

test("policyExitCode ignores companion Rust NOT_RUN and does not mark those rows PASS", () => {
  const rust = companionRustRows();
  assert.ok(rust.length >= 1);
  assert.ok(rust.every((row) => row.status === "NOT_RUN" && row.responsibility === RESPONSIBILITY.RUST));
  assert.ok(rust.some((row) => row.scenarioId.includes("stale-probe-success")));
  const httpPass = { responsibility: RESPONSIBILITY.HTTP, status: "PASS" };
  assert.equal(policyExitCode([httpPass, ...rust]), 0);
  assert.equal(policyExitCode([{ responsibility: RESPONSIBILITY.HTTP, status: "NOT_RUN" }, ...rust]), 2);
  assert.equal(policyExitCode([{ responsibility: RESPONSIBILITY.HTTP, status: "FAIL" }, ...rust]), 1);
  assert.equal(
    policyExitCode([
      httpPass,
      { responsibility: RESPONSIBILITY.LIVE, status: "NOT_RUN" },
      ...rust,
    ]),
    2,
  );
});

