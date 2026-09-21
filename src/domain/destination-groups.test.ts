import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  alignDestinationGroupsToAccountOrder,
  buildDestinationGroups,
  buildCredentialOrder,
  expandGroupOrder,
  filterGroupRows,
  includeCredentialRow,
  isSingleAccountGroup,
  moveWithinGroup,
} from "./destination-groups.ts";

test("global credential order preserves interleaved suppliers independently of grouping", () => {
  const rows = [credential("a", "a2", 2), credential("b", "b1", 1), credential("a", "a1", 0)];
  const ordered = buildCredentialOrder([destination("a"), destination("b")], rows);
  assert.deepEqual(ordered.map((group) => group.credentials[0].legacy_account_id), ["a1", "b1", "a2"]);
  assert.deepEqual(ordered.map((group) => group.id), [rows[2].id, rows[1].id, rows[0].id]);
  assert.deepEqual(expandGroupOrder(buildDestinationGroups([destination("a"), destination("b")], rows)), ["a1", "a2", "b1"]);
});

function destination(
  id: string,
  overrides: Partial<Destination> = {},
): Destination {
  return {
    adapter: "http",
    legacy: { kind: "builtin", id },
    auth_scheme: "bearer",
    base_url: null,
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: false,
      external_integration: false,
      identity_headers: false,
      managed_signup: false,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id,
    max_credentials: null,
    name: id,
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function credential(
  destinationId: string,
  accountId: string,
  routingRank: number,
): DestinationCredential {
  return {
    auth_state: "unknown",
    cooldowns: {
      five_hour_until: null,
      free_until: null,
      generic_until: null,
      month_until: null,
      week_until: null,
    },
    destination_id: destinationId,
    enabled: true,
    grants: { allowed_endpoint_ids: [], allowed_origins: [] },
    has_secret: true,
    id: `cred-${accountId}`,
    last_error: null,
    legacy_account_id: accountId,
    name: accountId,
    notes: null,
    onboarding_task: null,
    purchase_date: null,
    quota_pool_id: null,
    routing_rank: routingRank,
    scope: { kind: "all" },
  };
}

test("destination groups order by minimum routing_rank and keep credentials in rank order", () => {
  const groups = buildDestinationGroups(
    [
      destination("site"),
      destination("minimax"),
      destination("kimi"),
    ],
    [
      credential("minimax", "minimax", 0),
      credential("site", "k2", 2),
      credential("site", "k1", 1),
      credential("kimi", "kimi", 3),
    ],
  );
  assert.deepEqual(groups.map((group) => group.id), ["minimax", "site", "kimi"]);
  assert.deepEqual(groups[1]?.credentials.map((row) => row.legacy_account_id), ["k1", "k2"]);
  assert.deepEqual(expandGroupOrder(groups), ["minimax", "k1", "k2", "kimi"]);
});

test("empty destination groups append after every populated group in list order", () => {
  const groups = buildDestinationGroups(
    [destination("empty-b"), destination("go"), destination("empty-a")],
    [credential("go", "go", 4)],
  );
  assert.deepEqual(groups.map((group) => group.id), ["go", "empty-b", "empty-a"]);
  assert.deepEqual(groups[0]?.credentials.map((row) => row.legacy_account_id), ["go"]);
  assert.deepEqual(groups[1]?.credentials, []);
  assert.deepEqual(groups[2]?.credentials, []);
});

test("credentials still appear when the V3 account overlay is missing", () => {
  const groups = buildDestinationGroups(
    [destination("site"), destination("go")],
    [
      credential("site", "missing", 0),
      credential("site", "k1", 2),
      credential("go", "ghost", 1),
    ],
  );
  assert.deepEqual(groups.map((group) => group.id), ["site", "go"]);
  assert.deepEqual(
    groups[0]?.credentials.map((row) => row.legacy_account_id),
    ["missing", "k1"],
  );
  assert.deepEqual(groups[1]?.credentials.map((row) => row.legacy_account_id), ["ghost"]);
  assert.equal(
    includeCredentialRow(credential("site", "missing", 0), new Set(["k1"]), new Set(["k1"])),
    true,
  );
  assert.equal(
    includeCredentialRow(credential("site", "k1", 2), new Set(["k1"]), new Set(["k1"])),
    true,
  );
  assert.equal(
    includeCredentialRow(credential("site", "k2", 3), new Set(["k1"]), new Set(["k1", "k2"])),
    false,
  );
});

test("moving a Key inside a group keeps the surrounding order", () => {
  assert.deepEqual(
    moveWithinGroup(["go", "k1", "k2", "kimi"], ["k1", "k2"], "k2", -1),
    ["go", "k2", "k1", "kimi"],
  );
  assert.equal(moveWithinGroup(["go", "k1", "k2"], ["k1", "k2"], "k1", -1), null);
});

test("moving a Key inside an interleaved group keeps other destinations in place", () => {
  assert.deepEqual(
    moveWithinGroup(["a1", "b1", "a2"], ["a1", "a2"], "a2", -1),
    ["a2", "b1", "a1"],
  );
});

test("single-account groups follow max_credentials and platform-parent rules", () => {
  const custom = {
    destination: destination("custom", {
      legacy: { kind: "custom_account", id: "acct" },
      max_credentials: 1,
    }),
    credentials: [credential("custom", "acct", 0)],
    id: "custom",
  };
  const builtinOne = {
    destination: destination("go"),
    credentials: [credential("go", "go", 0)],
    id: "go",
  };
  const platformOne = {
    destination: destination("site", {
      legacy: { kind: "platform_parent", id: "site" },
      max_credentials: null,
    }),
    credentials: [credential("site", "k1", 0)],
    id: "site",
  };
  const platformSingleton = {
    destination: destination("solo-site", {
      legacy: { kind: "platform_parent", id: "solo-site" },
      max_credentials: 1,
    }),
    credentials: [credential("solo-site", "k1", 0)],
    id: "solo-site",
  };
  assert.equal(isSingleAccountGroup(custom), true);
  assert.equal(isSingleAccountGroup(builtinOne), true);
  assert.equal(isSingleAccountGroup(platformOne), false);
  assert.equal(isSingleAccountGroup(platformSingleton), true);
  assert.equal(isSingleAccountGroup({
    ...builtinOne,
    credentials: [credential("go", "a", 0), credential("go", "b", 1)],
  }), false);
});

test("filterGroupRows keeps groups with a visible row and does not mutate input", () => {
  const groups = [
    {
      destination: destination("a"),
      credentials: [credential("a", "a1", 0), credential("a", "a2", 1)],
      id: "a",
    },
    {
      destination: destination("b"),
      credentials: [credential("b", "b1", 0)],
      id: "b",
    },
    {
      destination: destination("c"),
      credentials: [credential("c", "c1", 0)],
      id: "c",
    },
  ];
  const snapshot = groups.map((group) => group.credentials.map((row) => row.legacy_account_id));
  const filtered = filterGroupRows(groups, new Set(["a2", "c1"]));
  assert.deepEqual(filtered.map((group) => group.id), ["a", "c"]);
  assert.deepEqual(filtered[0]?.credentials.map((row) => row.legacy_account_id), ["a2"]);
  assert.deepEqual(filtered[1]?.credentials.map((row) => row.legacy_account_id), ["c1"]);
  assert.deepEqual(
    groups.map((group) => group.credentials.map((row) => row.legacy_account_id)),
    snapshot,
  );
  assert.notEqual(filtered[0], groups[0]);
  assert.equal(filterGroupRows(groups, new Set()).length, 0);
});

test("aligning groups follows the live V3 account order", () => {
  const groups = buildDestinationGroups(
    [destination("late"), destination("early")],
    [credential("late", "late", 0), credential("early", "early", 1)],
  );
  assert.deepEqual(groups.map((group) => group.id), ["late", "early"]);
  const aligned = alignDestinationGroupsToAccountOrder(groups, ["early", "late"]);
  assert.deepEqual(aligned.map((group) => group.id), ["early", "late"]);
});

test("aligning groups can use credential ids when legacy ids are absent from the order", () => {
  const orphan = credential("solo", "ghost", 0);
  const groups = buildDestinationGroups(
    [destination("solo")],
    [orphan],
  );
  const aligned = alignDestinationGroupsToAccountOrder(groups, [orphan.id]);
  assert.deepEqual(aligned[0]?.credentials.map((row) => row.id), [orphan.id]);
});


test("observer credentials never enter account groups or global routing order", () => {
  const observer = credential("platform", "observer", -1);
  const destinations = [destination("platform", { observer_credential_id: observer.id }), destination("other")];
  const rows = [observer, credential("platform", "a1", 0), credential("other", "b1", 1), credential("platform", "a2", 2)];
  const order = buildCredentialOrder(destinations, rows);
  assert.deepEqual(expandGroupOrder(order), ["a1", "b1", "a2"]);
  assert.deepEqual(expandGroupOrder(buildDestinationGroups(destinations, rows)), ["a1", "a2", "b1"]);
  assert.deepEqual(moveWithinGroup(["a1", "b1", "a2"], expandGroupOrder(order), "a2", -1), ["a1", "a2", "b1"]);
});
