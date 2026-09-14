import assert from "node:assert/strict";
import test from "node:test";
import { OPENCODE_GO_PLAN, type ProviderSurface } from "./plans.ts";
import {
  accountCreatePayloadErrorKey,
  AccountCreatePayloadError,
  accountCreateRequestInput,
  buildCreateAccountPayload,
} from "./account-create-payload.ts";
import { CUSTOM_ENDPOINT_URL_ISSUE_KEYS } from "./custom-account.ts";
import type { AccountCreatePayloadErrorCode } from "./account-create-payload.ts";

function surface(providerId: string, extra: Partial<ProviderSurface> = {}): ProviderSurface {
  return {
    ...OPENCODE_GO_PLAN,
    id: providerId,
    provider_id: providerId,
    display_name: providerId,
    label: providerId,
    legacy: false,
    managed_registration: false,
    ...extra,
  };
}

const goPlan = OPENCODE_GO_PLAN;
const goatPlan = surface("command-code", { kind: "api-key" });
const customPlan = surface("custom", { kind: "custom", offering: "api" });

test("Custom payload uses one API URL and expands every model to its protocol", () => {
  const payload = buildCreateAccountPayload(customPlan, {
    name: "Custom",
    key: "custom-key",
    endpoint_url: "https://api.example.com/v1/responses",
    upstream_protocol: "responses",
    model_capabilities: [
      { public_model: "m1", upstream_model: "provider/m1" },
      { public_model: "m2", upstream_model: "provider/m2" },
    ],
  });
  assert.deepEqual(payload.custom_config, {
    endpoint_url: "https://api.example.com/v1/responses",
    upstream_protocol: "responses",
  });
  assert.deepEqual(payload.model_capabilities, [
    { public_model: "m1", upstream_model: "provider/m1", protocol: "responses", source: "manual" },
    { public_model: "m2", upstream_model: "provider/m2", protocol: "responses", source: "manual" },
  ]);
});

test("Custom payload rejects missing or malformed Endpoint fields", () => {
  const base = {
    name: "Custom",
    key: "custom-key",
    upstream_protocol: "chat_completions" as const,
    model_capabilities: [{ public_model: "m", upstream_model: "provider/m" }],
  };
  const cases: Array<{ endpoint_url?: string; code: AccountCreatePayloadErrorCode }> = [
    { code: "missing_endpoint_url" },
    { endpoint_url: "not-a-url", code: "invalid_endpoint_url" },
    { endpoint_url: "ftp://api.example.com", code: "endpoint_url_not_http" },
    { endpoint_url: "https://user:pass@api.example.com", code: "endpoint_url_with_credentials" },
  ];
  for (const { endpoint_url, code } of cases) {
    assert.throws(
      () => buildCreateAccountPayload(customPlan, { ...base, endpoint_url }),
      (error) => error instanceof AccountCreatePayloadError && error.code === code,
    );
  }
});

test("Custom payload requires one upstream protocol and valid model IDs", () => {
  const base = {
    name: "Custom",
    key: "custom-key",
    endpoint_url: "http://localhost:3000/v1/messages",
    model_capabilities: [{ public_model: "m", upstream_model: "provider/m" }],
  };
  assert.throws(
    () => buildCreateAccountPayload(customPlan, base),
    (error) => error instanceof AccountCreatePayloadError && error.code === "missing_upstream_protocol",
  );
  assert.throws(
    () => buildCreateAccountPayload(customPlan, {
      ...base,
      upstream_protocol: "messages",
      model_capabilities: [
        { public_model: " model-a ", upstream_model: "provider/a" },
        { public_model: "MODEL-A", upstream_model: "provider/a" },
      ],
    }),
    (error) => error instanceof AccountCreatePayloadError && error.code === "duplicate_public_model",
  );
});

test("non-Custom plans reject Custom-only fields", () => {
  assert.throws(
    () => buildCreateAccountPayload(goatPlan, {
      name: "GOAT",
      key: "key",
      endpoint_url: "https://api.example.com/v1/messages",
      upstream_protocol: "messages",
    }),
    (error) => error instanceof AccountCreatePayloadError && error.code === "custom_fields_not_allowed",
  );
  const payload = buildCreateAccountPayload(goPlan, { name: "Go", key: "key" });
  assert.equal(payload.custom_config, undefined);
  assert.equal(payload.model_capabilities, undefined);
});

test("dynamic Provider accounts omit Endpoint/protocol/models and skip Key when none-auth", () => {
  const dynamicKeyed = {
    ...goatPlan,
    id: "11111111-1111-4111-8111-111111111111",
    provider_id: "11111111-1111-4111-8111-111111111111",
    label: "Lab",
    dynamic: true,
  };
  assert.throws(
    () => buildCreateAccountPayload(dynamicKeyed, {
      name: "Second",
      key: "sk-second",
      endpoint_url: "http://127.0.0.1:9",
      upstream_protocol: "chat_completions",
      model_capabilities: [{ public_model: "x", upstream_model: "y" }],
    }),
    (error) => error instanceof AccountCreatePayloadError && error.code === "custom_fields_not_allowed",
  );
  const payload = buildCreateAccountPayload(dynamicKeyed, { name: "Second", key: "sk-second" });
  assert.equal(payload.provider_id, dynamicKeyed.provider_id);
  assert.equal(payload.custom_config, undefined);
  const dynamicNone = { ...dynamicKeyed, credential_kind: "none" as const };
  const nonePayload = buildCreateAccountPayload(dynamicNone, { name: "Singleton", key: "" });
  assert.equal(nonePayload.key, "");
});

test("create request input keeps fields and coerces a missing Key to empty", () => {
  assert.deepEqual(
    accountCreateRequestInput({ name: "Lab", provider_id: "lab", key: "sk-lab" }),
    { name: "Lab", provider_id: "lab", key: "sk-lab" },
  );
  assert.equal(accountCreateRequestInput({ name: "Lab", key: "" }).key, "");
  assert.equal(accountCreateRequestInput({ name: "Lab", key: undefined as unknown as string }).key, "");
});

test("payload errors map to Endpoint keys and fall back for unknown failures", () => {
  const endpoint = accountCreatePayloadErrorKey(new AccountCreatePayloadError("missing_endpoint_url"));
  assert.equal(endpoint, CUSTOM_ENDPOINT_URL_ISSUE_KEYS.empty);
  assert.equal(
    accountCreatePayloadErrorKey(new Error("internal")),
    accountCreatePayloadErrorKey(new AccountCreatePayloadError("custom_fields_not_allowed")),
  );
});
