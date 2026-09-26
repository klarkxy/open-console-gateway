import assert from "node:assert/strict";
import test from "node:test";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import {
  BUILTIN_GOAT_ID,
  DEFAULT_INITIAL_SECONDS,
  DEFAULT_MAX_SECONDS,
  MAX_CONFIGURED_RULES,
  MAX_ENABLED_PER_DESTINATION,
  MAX_MATCH_ALTERNATIVES,
  MAX_MATCH_STRING_BYTES,
  TEMPORARY_POLICY_BUILTIN_KEYS,
  TEMPORARY_POLICY_CLIENT_ERROR_KEYS,
  TEMPORARY_POLICY_EMPTY_KEYS,
  TEMPORARY_POLICY_HINT_KEYS,
  TEMPORARY_POLICY_ISSUE_KEYS,
  TEMPORARY_POLICY_RESTRICTION_SOURCE_KEYS,
  TEMPORARY_POLICY_RESTRICTION_STATE_KEYS,
  TEMPORARY_POLICY_SCOPE_KEYS,
  allocateCustomRuleId,
  catalogAsRules,
  connectionDisplayName,
  credentialDisplayName,
  customRuleDraftFrom,
  effectiveBuiltinEnabled,
  effectiveRules,
  emptyCustomRuleDraft,
  enabledEffectiveCount,
  knownBuiltinIds,
  localOverride,
  maskInheritedCustomRule,
  overlayRules,
  parseCustomRuleDraft,
  parseDelimitedList,
  remainingProbeSeconds,
  removeRule,
  restrictionSnapshotEmptyCode,
  restrictionTableEmpty,
  upsertBuiltinOverride,
  upsertRule,
  utf8ByteLength,
  validateConfiguredRules,
  visibleCustomRules,
  type PolicyBuiltin,
  type PolicyDraftIssue,
  type PolicyRule,
} from "./temporary-policy.ts";

const goat: PolicyBuiltin = {
  id: BUILTIN_GOAT_ID,
  scope: "credential_model",
  backoff: { initialSeconds: DEFAULT_INITIAL_SECONDS, maxSeconds: DEFAULT_MAX_SECONDS },
};

function custom(overrides: Partial<Extract<PolicyRule, { kind: "custom" }>> = {}): Extract<PolicyRule, { kind: "custom" }> {
  return {
    kind: "custom",
    id: "custom.lab",
    destinationId: null,
    enabled: true,
    scope: "credential_model",
    match: { statusCodes: [429] },
    backoff: { initialSeconds: 30, maxSeconds: 300 },
    ...overrides,
  };
}

function draft(overrides: Partial<ReturnType<typeof emptyCustomRuleDraft>> = {}) {
  return {
    ...emptyCustomRuleDraft(),
    id: "custom.lab",
    statusCodes: "429",
    ...overrides,
  };
}

test("draft parsing rejects malformed values, empty matchers, and list boundaries", () => {
  assert.deepEqual(parseCustomRuleDraft(draft({ id: "  " }), null), { ok: false, issues: ["missing_id"] });
  assert.deepEqual(
    parseCustomRuleDraft(draft({ id: BUILTIN_GOAT_ID }), null),
    { ok: false, issues: ["reserved_builtin_id"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ scope: "" }), null),
    { ok: false, issues: ["invalid_scope"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", errorCodes: "", errorTypes: "", messageContains: "" }), null),
    { ok: false, issues: ["empty_match"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: " , " }), null),
    { ok: false, issues: ["empty_status_codes"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "399" }), null),
    { ok: false, issues: ["invalid_status_code"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "600" }), null),
    { ok: false, issues: ["invalid_status_code"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "429.5" }), null),
    { ok: false, issues: ["invalid_status_code"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", errorCodes: "\n," }), null),
    { ok: false, issues: ["empty_error_codes"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", errorTypes: " , " }), null),
    { ok: false, issues: ["empty_error_types"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", messageContains: "\n" }), null),
    { ok: false, issues: ["empty_message_contains"] },
  );

  const tooMany = Array.from({ length: MAX_MATCH_ALTERNATIVES + 1 }, (_, index) => `e${index}`).join(",");
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", errorCodes: tooMany }), null),
    { ok: false, issues: ["too_many_alternatives"] },
  );

  const tooLong = "a".repeat(MAX_MATCH_STRING_BYTES + 1);
  assert.equal(utf8ByteLength(tooLong), MAX_MATCH_STRING_BYTES + 1);
  assert.deepEqual(
    parseCustomRuleDraft(draft({ statusCodes: "", messageContains: tooLong }), null),
    { ok: false, issues: ["match_string_too_long"] },
  );

  assert.deepEqual(
    parseCustomRuleDraft(draft({ initialSeconds: "0" }), null),
    { ok: false, issues: ["invalid_backoff_initial"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ maxSeconds: "86401" }), null),
    { ok: false, issues: ["invalid_backoff_max"] },
  );
  assert.deepEqual(
    parseCustomRuleDraft(draft({ initialSeconds: "40", maxSeconds: "30" }), null),
    { ok: false, issues: ["backoff_max_lt_initial"] },
  );
});

test("draft parsing accepts AND-of-fields matchers and dedupes list tokens", () => {
  const parsed = parseCustomRuleDraft(draft({
    statusCodes: "429, 429, 503",
    errorCodes: "insufficient_quota\ninsufficient_quota",
    errorTypes: "insufficient_quota",
    messageContains: "no credits",
  }), "dest-1");
  assert.equal(parsed.ok, true);
  if (!parsed.ok) return;
  assert.deepEqual(parsed.rule.match.statusCodes, [429, 503]);
  assert.deepEqual(parsed.rule.match.errorCodes, ["insufficient_quota"]);
  assert.equal(parsed.rule.destinationId, "dest-1");
  assert.equal(parsed.rule.scope, "credential_model");
});

test("status-only matchers are valid without a parsed error body", () => {
  const parsed = parseCustomRuleDraft(draft({
    errorCodes: "",
    errorTypes: "",
    messageContains: "",
    statusCodes: "400",
  }), null);
  assert.equal(parsed.ok, true);
  if (!parsed.ok) return;
  assert.deepEqual(parsed.rule.match, { statusCodes: [400] });
});

test("configured lists reject duplicates, unknown builtins, and enabled-count limits", () => {
  assert.deepEqual(
    validateConfiguredRules([custom(), custom()], [goat]),
    ["duplicate_rule"],
  );
  assert.deepEqual(
    validateConfiguredRules([
      { kind: "builtin_override", id: "builtin.unknown", destinationId: null, enabled: false },
    ], [goat]),
    ["unknown_builtin"],
  );
  const tooMany = Array.from({ length: MAX_CONFIGURED_RULES + 1 }, (_, index) => custom({
    id: `custom.${index}`,
    enabled: false,
  }));
  assert.deepEqual(validateConfiguredRules(tooMany, [goat]), ["too_many_rules"]);

  const enabled = Array.from({ length: MAX_ENABLED_PER_DESTINATION }, (_, index) => custom({
    id: `custom.${index}`,
    destinationId: "dest-1",
  }));
  assert.equal(enabledEffectiveCount(enabled, [goat], "dest-1"), MAX_ENABLED_PER_DESTINATION + 1);
  assert.deepEqual(
    validateConfiguredRules(enabled, [goat]),
    ["too_many_enabled_for_destination"],
  );
});

test("same-id destination rules replace globals, and disabled entries mask inheritance", () => {
  const globalRule = custom({ id: "shared", match: { statusCodes: [429] } });
  const replacement = custom({
    id: "shared",
    destinationId: "dest-1",
    match: { errorCodes: ["x"] },
    enabled: false,
  });
  const effective = effectiveRules([globalRule, replacement], [goat], "dest-1");
  const shared = effective.find((rule) => rule.id === "shared");
  assert.equal(shared?.kind, "custom");
  if (shared?.kind !== "custom") return;
  assert.equal(shared.enabled, false);
  assert.deepEqual(shared.match, { errorCodes: ["x"] });
  assert.equal(effectiveBuiltinEnabled([
    { kind: "builtin_override", id: BUILTIN_GOAT_ID, destinationId: null, enabled: true },
    { kind: "builtin_override", id: BUILTIN_GOAT_ID, destinationId: "dest-1", enabled: false },
  ], [goat], "dest-1", BUILTIN_GOAT_ID), false);
  assert.equal(effectiveBuiltinEnabled([
    { kind: "builtin_override", id: BUILTIN_GOAT_ID, destinationId: null, enabled: false },
  ], [goat], "dest-2", BUILTIN_GOAT_ID), false);
  assert.equal(effectiveBuiltinEnabled([], [goat], null, BUILTIN_GOAT_ID), true);
});

test("visible custom rules keep inherited rows until a local same-id replacement exists", () => {
  const globalRule = custom({ id: "shared" });
  const other = custom({ id: "local-only", destinationId: "dest-1" });
  const rows = visibleCustomRules([globalRule, other], "dest-1");
  assert.deepEqual(rows.map((row) => [row.origin, row.rule.id]), [
    ["inherited", "shared"],
    ["local", "local-only"],
  ]);
  const masked = maskInheritedCustomRule(globalRule, "dest-1");
  assert.equal(masked.destinationId, "dest-1");
  assert.equal(masked.enabled, false);
  const afterMask = visibleCustomRules([globalRule, masked], "dest-1");
  assert.deepEqual(afterMask.map((row) => [row.origin, row.rule.id, row.rule.enabled]), [
    ["local", "shared", false],
  ]);
});

test("empty restriction tables do not become an upstream-health claim", () => {
  assert.equal(restrictionTableEmpty(0), "no_local_waits");
  assert.equal(restrictionTableEmpty(1), null);
});

test("restriction empty copy requires a successful diagnostics snapshot", () => {
  assert.equal(restrictionSnapshotEmptyCode(false, 0), null);
  assert.equal(restrictionSnapshotEmptyCode(false, 1), null);
  assert.equal(restrictionSnapshotEmptyCode(true, 0), "no_local_waits");
  assert.equal(restrictionSnapshotEmptyCode(true, 1), null);
});

test("probe countdown is a local snapshot remainder, not an upstream reset clock", () => {
  assert.equal(remainingProbeSeconds(30, 1_000, 1_000), 30);
  assert.equal(remainingProbeSeconds(30, 1_000, 11_000), 20);
  assert.equal(remainingProbeSeconds(5, 1_000, 10_000), 0);
  assert.equal(remainingProbeSeconds(0, 1_000, 1_000), 0);
  assert.equal(remainingProbeSeconds(null, 1_000, 1_000), 0);
});

test("friendly names use destination and credential labels and never read a Key field", () => {
  const destinations = [{ id: "dest-1", name: "Lab HTTP" }];
  const credentials = [{ id: "cred-1", name: "Office", key: "sk-secret", value: "ocg-secret" }];
  assert.equal(connectionDisplayName("dest-1", destinations), "Lab HTTP");
  assert.equal(connectionDisplayName("missing", destinations), null);
  assert.equal(credentialDisplayName("cred-1", credentials), "Office");
  assert.notEqual(credentialDisplayName("cred-1", credentials), "sk-secret");
  assert.notEqual(credentialDisplayName("cred-1", credentials), "ocg-secret");
});

test("delimited lists drop empties and preserve first-seen order", () => {
  assert.deepEqual(parseDelimitedList(" 429, 503\n429 "), ["429", "503"]);
  assert.deepEqual(parseDelimitedList("\n,"), []);
});

test("upsert/remove keep other destination rules and restore inheritance by deletion", () => {
  const globalRule = custom({ id: "shared" });
  const destRule = custom({ id: "shared", destinationId: "dest-1", enabled: false });
  const next = upsertRule([globalRule], destRule);
  assert.equal(next.length, 2);
  assert.equal(localOverride(next, "dest-1", "shared")?.enabled, false);
  assert.deepEqual(removeRule(next, "dest-1", "shared"), [globalRule]);
  const overridden = upsertBuiltinOverride([], null, BUILTIN_GOAT_ID, { enabled: false });
  assert.equal(overridden[0]?.kind, "builtin_override");
  assert.equal(overridden[0]?.enabled, false);
});

test("overlay and catalog helpers keep builtin identity sealed", () => {
  const catalog = catalogAsRules([goat]);
  assert.equal(catalog[0]?.id, BUILTIN_GOAT_ID);
  const overlay = overlayRules(catalog, [
    { kind: "builtin_override", id: BUILTIN_GOAT_ID, destinationId: null, enabled: false },
  ]);
  assert.equal(overlay.length, 1);
  assert.equal(overlay[0]?.enabled, false);
  assert.deepEqual(knownBuiltinIds([]), [BUILTIN_GOAT_ID]);
});

test("custom drafts round-trip matcher fields without introducing regex or raw-scan flags", () => {
  const rule = custom({
    match: {
      statusCodes: [429, 503],
      errorCodes: ["insufficient_quota"],
      messageContains: ["no credits"],
    },
  });
  const roundTrip = parseCustomRuleDraft(customRuleDraftFrom(rule), null);
  assert.equal(roundTrip.ok, true);
  if (!roundTrip.ok) return;
  assert.deepEqual(roundTrip.rule.match, rule.match);
  assert.equal("regex" in roundTrip.rule.match, false);
  assert.equal("script" in roundTrip.rule.match, false);
});

test("issue and presentation maps cover every semantic code without pinning copy", () => {
  const issues = Object.keys(TEMPORARY_POLICY_ISSUE_KEYS) as PolicyDraftIssue[];
  assert.equal(issues.length, 18);
  for (const code of issues) {
    const key = TEMPORARY_POLICY_ISSUE_KEYS[code];
    assert.ok(key in enUSMessages, code);
    assert.notEqual(enUSMessages[key], key);
  }
  for (const table of [
    TEMPORARY_POLICY_CLIENT_ERROR_KEYS,
    TEMPORARY_POLICY_RESTRICTION_STATE_KEYS,
    TEMPORARY_POLICY_RESTRICTION_SOURCE_KEYS,
    TEMPORARY_POLICY_SCOPE_KEYS,
    TEMPORARY_POLICY_EMPTY_KEYS,
    TEMPORARY_POLICY_BUILTIN_KEYS,
    TEMPORARY_POLICY_HINT_KEYS,
  ]) {
    for (const key of Object.values(table)) {
      assert.ok(key in enUSMessages);
      assert.notEqual(enUSMessages[key], key);
    }
  }
  assert.equal(allocateCustomRuleId("abc"), "custom.abc");
});
