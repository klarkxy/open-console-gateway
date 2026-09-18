import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  alignDestinationGroupsToAccountOrder,
  buildDestinationGroups,
  expandGroupOrder,
  filterGroupRows,
  isSingleAccountGroup,
  moveWithinGroup,
} from "./destination-groups.ts";

function account(id: string): Account {
  return { id } as Account;
}

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

test("destination groups order by minimum routing_rank and keep accounts in rank order", () => {
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
    new Map([
      ["minimax", account("minimax")],
      ["k1", account("k1")],
      ["k2", account("k2")],
      ["kimi", account("kimi")],
    ]),
  );
  assert.deepEqual(groups.map((group) => group.id), ["minimax", "site", "kimi"]);
  assert.deepEqual(groups[1]?.accounts.map((row) => row.id), ["k1", "k2"]);
  assert.deepEqual(expandGroupOrder(groups), ["minimax", "k1", "k2", "kimi"]);
});

test("empty destination groups append after every populated group in list order", () => {
  const groups = buildDestinationGroups(
    [destination("empty-b"), destination("go"), destination("empty-a")],
    [credential("go", "go", 4)],
    new Map([["go", account("go")]]),
  );
  assert.deepEqual(groups.map((group) => group.id), ["go", "empty-b", "empty-a"]);
  assert.deepEqual(groups[0]?.accounts.map((row) => row.id), ["go"]);
  assert.deepEqual(groups[1]?.accounts, []);
  assert.deepEqual(groups[2]?.accounts, []);
});

test("credentials whose V3 account is missing are skipped", () => {
  const groups = buildDestinationGroups(
    [destination("site"), destination("go")],
    [
      credential("site", "missing", 0),
      credential("site", "k1", 2),
      credential("go", "ghost", 1),
    ],
    new Map([["k1", account("k1")]]),
  );
  assert.deepEqual(groups.map((group) => group.id), ["site", "go"]);
  assert.deepEqual(groups[0]?.accounts.map((row) => row.id), ["k1"]);
  assert.deepEqual(groups[1]?.accounts, []);
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
    accounts: [account("acct")],
    id: "custom",
  };
  const builtinOne = {
    destination: destination("go"),
    accounts: [account("go")],
    id: "go",
  };
  const platformOne = {
    destination: destination("site", {
      legacy: { kind: "platform_parent", id: "site" },
      max_credentials: null,
    }),
    accounts: [account("k1")],
    id: "site",
  };
  const platformSingleton = {
    destination: destination("solo-site", {
      legacy: { kind: "platform_parent", id: "solo-site" },
      max_credentials: 1,
    }),
    accounts: [account("k1")],
    id: "solo-site",
  };
  assert.equal(isSingleAccountGroup(custom), true);
  assert.equal(isSingleAccountGroup(builtinOne), true);
  assert.equal(isSingleAccountGroup(platformOne), false);
  assert.equal(isSingleAccountGroup(platformSingleton), true);
  assert.equal(isSingleAccountGroup({ ...builtinOne, accounts: [account("a"), account("b")] }), false);
});

test("filterGroupRows keeps groups with a visible row and does not mutate input", () => {
  const groups = [
    { destination: destination("a"), accounts: [account("a1"), account("a2")], id: "a" },
    { destination: destination("b"), accounts: [account("b1")], id: "b" },
    { destination: destination("c"), accounts: [account("c1")], id: "c" },
  ];
  const snapshot = groups.map((group) => group.accounts.map((row) => row.id));
  const filtered = filterGroupRows(groups, new Set(["a2", "c1"]));
  assert.deepEqual(filtered.map((group) => group.id), ["a", "c"]);
  assert.deepEqual(filtered[0]?.accounts.map((row) => row.id), ["a2"]);
  assert.deepEqual(filtered[1]?.accounts.map((row) => row.id), ["c1"]);
  assert.deepEqual(groups.map((group) => group.accounts.map((row) => row.id)), snapshot);
  assert.notEqual(filtered[0], groups[0]);
  assert.equal(filterGroupRows(groups, new Set()).length, 0);
});

test("aligning groups follows the live V3 account order", () => {
  const groups = buildDestinationGroups(
    [destination("late"), destination("early")],
    [credential("late", "late", 0), credential("early", "early", 1)],
    new Map([["late", account("late")], ["early", account("early")]]),
  );
  assert.deepEqual(groups.map((group) => group.id), ["late", "early"]);
  const aligned = alignDestinationGroupsToAccountOrder(groups, ["early", "late"]);
  assert.deepEqual(aligned.map((group) => group.id), ["early", "late"]);
});
