import assert from "node:assert/strict";
import test from "node:test";
import {
  customEndpointUrlIssue,
  customApiUrlSupportsModelDiscovery,
  customApiUrlNeedsManualModels,
  legacyCustomAccountDestinationId,
  normalizeCustomCapabilities,
  CustomCapabilityError,
} from "./custom-account.ts";

test("trusted Endpoint validation permits LAN, localhost, and HTTP", () => {
  for (const endpoint of [
    "http://192.168.1.10:8080/v1/chat/completions",
    "http://localhost:3000/responses",
    "http://[::1]:8080/messages",
    "https://api.example.com/v1/messages",
  ]) assert.equal(customEndpointUrlIssue(endpoint), null, endpoint);
  assert.equal(customEndpointUrlIssue("ftp://api.example.com"), "not_http");
  assert.equal(customEndpointUrlIssue("https://user:pass@api.example.com"), "with_credentials");
  assert.equal(customEndpointUrlIssue(""), "empty");
  assert.equal(customEndpointUrlIssue("   "), "empty");
});

test("common API bases and legacy standard paths enable model discovery", () => {
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com", "chat_completions"));
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com/v1", "chat_completions"));
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com/openai/v1/", "responses"));
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com/v1/chat/completions", "chat_completions"));
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com/v1/responses/", "responses"));
  assert.ok(customApiUrlSupportsModelDiscovery("https://api.example.com/v1/messages", "messages"));
  assert.ok(!customApiUrlSupportsModelDiscovery("https://api.example.com/custom/infer", "messages"));
  assert.ok(!customApiUrlSupportsModelDiscovery("https://api.example.com/v1/messages", "responses"));
  assert.ok(!customApiUrlNeedsManualModels("", "messages"));
  assert.ok(!customApiUrlNeedsManualModels("not a url", "messages"));
  assert.ok(!customApiUrlNeedsManualModels("https://api.example.com", "messages"));
  assert.ok(customApiUrlNeedsManualModels("https://api.example.com/custom/infer", "messages"));
});

test("one protocol normalizes each model once and rejects mismatched rows", () => {
  assert.deepEqual(normalizeCustomCapabilities([
    { public_model: "m1", upstream_model: "m1", protocol: "messages" },
    { public_model: "m2", upstream_model: "m2", protocol: "messages" },
  ], "messages"), [
    { public_model: "m1", upstream_model: "m1", protocol: "messages", source: "manual" },
    { public_model: "m2", upstream_model: "m2", protocol: "messages", source: "manual" },
  ]);
  assert.throws(
    () => normalizeCustomCapabilities([{ public_model: "m", upstream_model: "m", protocol: "responses" }], "messages"),
    (error) => error instanceof CustomCapabilityError && error.issue === "protocol_mismatch",
  );
});

test("public models are case-insensitively unique while upstream IDs are reusable", () => {
  assert.deepEqual(normalizeCustomCapabilities([
    { public_model: "chat", upstream_model: "vendor/shared", protocol: "messages" },
    { public_model: "reasoning", upstream_model: "vendor/shared", protocol: "messages" },
  ], "messages").map(({ public_model, upstream_model }) => ({ public_model, upstream_model })), [
    { public_model: "chat", upstream_model: "vendor/shared" },
    { public_model: "reasoning", upstream_model: "vendor/shared" },
  ]);
  assert.throws(
    () => normalizeCustomCapabilities([
      { public_model: "Chat", upstream_model: "vendor/a", protocol: "messages" },
      { public_model: "chat", upstream_model: "vendor/b", protocol: "messages" },
    ], "messages"),
    (error) => error instanceof CustomCapabilityError && error.issue === "duplicate_public_model",
  );
});

test("a legacy Custom account resolves its destination through the credential row", () => {
  const credentialsByLegacyAccountId = new Map([
    ["acc-1", { destination_id: "dest-shared" }],
  ]);
  const destinations = [
    { id: "dest-shared", legacy: { kind: "custom_account" as const, id: "acc-1" } },
    { id: "dest-other", legacy: { kind: "custom_account" as const, id: "acc-2" } },
  ];
  assert.equal(
    legacyCustomAccountDestinationId("acc-1", credentialsByLegacyAccountId, destinations),
    "dest-shared",
  );
});

test("destination resolution falls back to the account-owned Custom destination", () => {
  const destinations = [
    { id: "dest-builtin", legacy: { kind: "builtin" as const, id: "opencode" } },
    { id: "dest-owned", legacy: { kind: "custom_account" as const, id: "acc-9" } },
  ];
  assert.equal(legacyCustomAccountDestinationId("acc-9", new Map(), destinations), "dest-owned");
  assert.equal(legacyCustomAccountDestinationId("acc-missing", new Map(), destinations), null);
  assert.equal(legacyCustomAccountDestinationId("acc-9", new Map(), []), null);
});
