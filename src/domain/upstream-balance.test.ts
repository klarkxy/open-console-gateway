import assert from "node:assert/strict";
import test from "node:test";
import { accountInferenceEndpointUrl, officialBalanceSupported } from "./upstream-balance.ts";
import type { Account } from "../api/dashboard.ts";
import type { Connection, ConnectionEndpoint } from "../api/connections.ts";
import type { Identity } from "../api/identities.ts";

const endpoint = (id: string, url: string, official_balance = false): ConnectionEndpoint => ({
  id, url, official_balance, connection_id: "connection", locked: false,
  operation: "chat_create", wire_protocol: "chat_completions", auth_scheme: "bearer",
});
const connection = (id: string, endpoints: ConnectionEndpoint[]) => ({ id, endpoints }) as Connection;
const identity = (rows: { id: string; connection: string; endpoints: ConnectionEndpoint[] }[]): Identity => ({
  legacy: { kind: "account", id: rows[0]?.id },
  credentials: rows.map((row) => ({
    legacy: { kind: "account", id: row.id }, credential: { purpose: "inference" },
    bindings: [{ connection_id: row.connection, allowed_endpoint_ids: row.endpoints.map((e) => e.id),
      allowed_origins: row.endpoints.map((e) => new URL(e.url!).origin) }],
  })),
}) as Identity;

test("balance availability consumes server capability instead of interpreting provider names", () => {
  const url = "https://new.example/v1";
  assert.equal(officialBalanceSupported(url, [connection("c", [endpoint("e", url, true)])]), true);
  assert.equal(officialBalanceSupported("https://api.stepfun.com/v1", null), false);
  assert.equal(officialBalanceSupported(url, [connection("c", [endpoint("e", url)])]), false);
});

test("current account selects its own credential even inside a shared identity", () => {
  const a = endpoint("a", "https://one.example/v1");
  const b = endpoint("b", "https://two.example/v1");
  const shared = identity([{ id: "a", connection: "ca", endpoints: [a] }, { id: "b", connection: "cb", endpoints: [b] }]);
  const connections = [connection("ca", [a]), connection("cb", [b])];
  assert.equal(accountInferenceEndpointUrl({ id: "b", custom_config: null }, shared, connections), b.url);
  assert.equal(accountInferenceEndpointUrl({ id: "missing", custom_config: null }, shared, connections), null);
});

test("ambiguous, revoked and stale-origin endpoints remain unknown", () => {
  const a = endpoint("a", "https://one.example/v1");
  const b = endpoint("b", "https://two.example/v1");
  const shared = identity([{ id: "a", connection: "c", endpoints: [a, b] }]);
  const connections = [connection("c", [a, b])];
  const account = { id: "a", custom_config: null };
  assert.equal(accountInferenceEndpointUrl(account, shared, connections), null);
  shared.credentials[0]!.bindings[0]!.allowed_endpoint_ids = ["b"];
  assert.equal(accountInferenceEndpointUrl(account, shared, connections), b.url);
  shared.credentials[0]!.bindings[0]!.allowed_origins = [];
  assert.equal(accountInferenceEndpointUrl(account, shared, connections), null);
  shared.credentials[0]!.bindings[0]!.allowed_endpoint_ids = [];
  assert.equal(accountInferenceEndpointUrl(account, shared, connections), null);
});

test("Custom's explicit base endpoint remains available without guessing a sibling", () => {
  const account: Pick<Account, "id" | "custom_config"> = { id: "custom", custom_config: {
    account_id: "custom", endpoint_url: "https://api.deepseek.com/v1", upstream_protocol: "chat_completions", created_at: "", updated_at: "",
  } };
  assert.equal(accountInferenceEndpointUrl(account, null, null), account.custom_config!.endpoint_url);
});

test("financial endpoint uses the same explicit-port grant semantics as binding editing", () => {
  const e = endpoint("e", "https://API.DEEPSEEK.COM:443/v1", true);
  const owner = identity([{id:"a", connection:"c", endpoints:[e]}]);
  owner.credentials[0]!.bindings[0]!.allowed_origins = ["https://api.deepseek.com:443"];
  assert.equal(accountInferenceEndpointUrl({id:"a", custom_config:null}, owner, [connection("c", [e])]), e.url);
});
