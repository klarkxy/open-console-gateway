import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  DESTINATION_LOAD_KEYS,
  DESTINATION_PROJECTION_REFRESH_KEYS,
  destinationFirstLoadFailed,
  groupsContainLegacyAccount,
  groupsFromDestinationSnapshot,
  refreshDestinationProjection,
} from "./destination-projection-refresh.ts";

function destination(id: string): Destination {
  return {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
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

test("refreshDestinationProjection returns a semantic code when load rejects", async () => {
  const createdAccountId = "new-account";
  const destinations = [destination("site")];
  const credentialsBefore = [credential("site", "existing", 0)];
  const created = { id: createdAccountId };

  const result = await refreshDestinationProjection(async () => {
    throw Object.assign(new Error("network"), { status: 500 });
  });

  assert.equal(result.ok, false);
  if (result.ok) throw new Error("expected refresh failure");
  assert.equal(result.code, "refresh_failed");
  assert.equal(
    Object.prototype.hasOwnProperty.call(DESTINATION_PROJECTION_REFRESH_KEYS, result.code),
    true,
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(DESTINATION_PROJECTION_REFRESH_KEYS, "created_refresh_failed"),
    true,
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(DESTINATION_PROJECTION_REFRESH_KEYS, "deleted_refresh_failed"),
    true,
  );
  assert.ok(created.id);

  const groups = groupsFromDestinationSnapshot(destinations, credentialsBefore);
  assert.equal(groupsContainLegacyAccount(groups, createdAccountId), false);
  assert.equal(groupsContainLegacyAccount(groups, "existing"), true);
});

test("refreshDestinationProjection succeeds and groups follow the updated snapshot", async () => {
  const createdAccountId = "new-account";
  let credentials = [credential("site", "existing", 0)];
  const destinations = [destination("site")];

  const result = await refreshDestinationProjection(async () => {
    credentials = [
      credential("site", "existing", 0),
      credential("site", createdAccountId, 1),
    ];
  });

  assert.equal(result.ok, true);
  const groups = groupsFromDestinationSnapshot(destinations, credentials);
  assert.equal(groupsContainLegacyAccount(groups, createdAccountId), true);
});

test("destinationFirstLoadFailed is true only before a snapshot exists and without refusals", () => {
  assert.equal(destinationFirstLoadFailed(false, "network", 0), true);
  assert.equal(destinationFirstLoadFailed(true, "network", 0), false);
  assert.equal(destinationFirstLoadFailed(false, "", 0), false);
  assert.equal(destinationFirstLoadFailed(false, "network", 1), false);
  assert.equal(
    Object.prototype.hasOwnProperty.call(DESTINATION_LOAD_KEYS, "load_failed"),
    true,
  );
});
