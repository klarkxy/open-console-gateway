import assert from "node:assert/strict";
import test from "node:test";
import { CredentialEditorError } from "./account-credential.ts";
import { emptyProviderDefinitionDraft } from "./dynamic-provider.ts";
import {
  buildOnboardingCommitPayload,
  connectionHasSavedKey,
  destinationOriginFromEndpointUrl,
  expectationFromProviderDefinition,
  identityHasSavedMaterialForConnection,
  isUncertainOnboardingFailure,
  nextOnboardingOperationId,
  onboardingMutationExpectation,
  onboardingPayloadSignature,
  onboardingUnknownLockedError,
  shouldHydrateOnboardingForm,
  shouldOfferAuthorizeCurrentEndpoint,
  validateOnboardingDraft,
} from "./onboarding-draft.ts";
import { DashboardAuthError, DashboardRequestError } from "../api/dashboard-v3.ts";

function validDraft() {
  const draft = emptyProviderDefinitionDraft();
  draft.name = "Lab";
  draft.endpoint_url = "http://127.0.0.1:9/v1";
  draft.upstream_protocol = "chat_completions";
  draft.auth_kind = "bearer";
  draft.models = [{ public_model: "", upstream_model: "" }];
  return draft;
}

test("draft validation accepts name and URL without models or Key", () => {
  const draft = validDraft();
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), null);
  draft.key = "";
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), null);
  draft.name = "";
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), "missing_name");
  draft.name = "Lab";
  draft.endpoint_url = "";
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), "missing_endpoint_url");
  draft.endpoint_url = "not-a-url";
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), "invalid_endpoint_url");
});

test("draft validation still rejects a half-filled mapping row", () => {
  const draft = validDraft();
  draft.models = [{ public_model: "opus", upstream_model: "" }];
  assert.equal(validateOnboardingDraft(draft, { intent: "draft" }), "missing_upstream_model");
});

test("complete validation requires models and a Key unless a saved Key exists", () => {
  const draft = validDraft();
  assert.equal(validateOnboardingDraft(draft, { intent: "complete" }), "missing_mappings");
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  assert.equal(validateOnboardingDraft(draft, { intent: "complete" }), "missing_key");
  assert.equal(
    validateOnboardingDraft(draft, { intent: "complete", hasSavedKey: true }),
    null,
  );
  draft.key = "sk-lab";
  assert.equal(validateOnboardingDraft(draft, { intent: "complete" }), null);
  draft.auth_kind = "none";
  draft.key = "";
  assert.equal(validateOnboardingDraft(draft, { intent: "complete" }), null);
  draft.auth_kind = "bearer";
  assert.equal(
    validateOnboardingDraft(draft, { intent: "draft", previousAuthKind: "none" }),
    "missing_replacement_key",
  );
});

test("new draft payload omits Key, models, and the authorize flag", () => {
  const draft = validDraft();
  const payload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "draft",
  });
  assert.equal(payload.mode, "draft");
  assert.equal(payload.authorizeCurrentEndpoint, undefined);
  assert.equal(payload.authorization, undefined);
  assert.deepEqual(payload.targets, []);
  assert.equal(payload.connection.kind, "new");
  if (payload.connection.kind !== "new") throw new Error("expected new");
  assert.equal(payload.connection.name, "Lab");
  assert.equal(payload.connection.endpointUrl, "http://127.0.0.1:9/v1");
  assert.equal(payload.connection.templateId, "custom-http");
});

test("complete payload includes Key and models; authorize is omitted until checked", () => {
  const draft = validDraft();
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  draft.key = "sk-lab";
  draft.account_name = "Lab key";
  const payload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
    authorizeCurrentEndpoint: false,
  });
  assert.equal(payload.mode, "complete");
  assert.equal(payload.authorizeCurrentEndpoint, undefined);
  assert.deepEqual(payload.authorization, {
    kind: "api_key",
    secretInput: "sk-lab",
    accountLabel: "Lab key",
  });
  assert.deepEqual(payload.targets, [
    { publicModel: "opus", upstreamModel: "vendor/opus", upstreamOverride: null },
  ]);
});

test("resume uses the same existing connection id and replacement configuration", () => {
  const draft = validDraft();
  draft.preset_id = "openai";
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  const connectionId = "conn-lab-stable";
  const draftPayload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "draft",
    connectionId,
  });
  assert.equal(draftPayload.connection.kind, "existing");
  if (draftPayload.connection.kind !== "existing") throw new Error("expected existing");
  assert.equal(draftPayload.connection.connectionId, connectionId);
  assert.deepEqual(draftPayload.connection.configuration, {
    templateId: "openai",
    name: "Lab",
    endpointUrl: "http://127.0.0.1:9/v1",
    upstreamProtocol: "chat_completions",
    authKind: "bearer",
  });
  draft.key = "sk-rotate";
  const completePayload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
    connectionId,
    hasSavedKey: true,
    authorizeCurrentEndpoint: true,
  });
  assert.equal(completePayload.mode, "complete");
  assert.equal(completePayload.authorizeCurrentEndpoint, true);
  assert.equal(completePayload.connection.kind, "existing");
  if (completePayload.connection.kind !== "existing") throw new Error("expected existing");
  assert.equal(completePayload.connection.connectionId, connectionId);
  assert.equal(completePayload.authorization?.kind, "api_key");
});

test("first completion of an existing draft still requires a Key", () => {
  const draft = validDraft();
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  assert.throws(
    () => buildOnboardingCommitPayload({
      draft,
      operationId: "11111111-1111-4111-8111-111111111111",
      mode: "complete",
      connectionId: "conn-lab-stable",
      hasSavedKey: false,
    }),
    (error: unknown) => error instanceof Error && error.message === "missing_key",
  );
  draft.key = "sk-first";
  const payload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
    connectionId: "conn-lab-stable",
    hasSavedKey: false,
  });
  assert.equal(payload.connection.kind, "existing");
  assert.equal(payload.authorization?.kind, "api_key");
});

test("blank Key on resume omits authorization so saved material is retained", () => {
  const draft = validDraft();
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  draft.key = "  ";
  const payload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
    connectionId: "conn-lab-stable",
    hasSavedKey: true,
  });
  assert.equal(payload.authorization, undefined);
});

test("existing draft CAS uses the definition that filled the form, not a newer list or store pair", () => {
  const definition = { revision: 4, process_generation: 11 };
  assert.deepEqual(expectationFromProviderDefinition(definition), {
    expectedRevision: 4,
    processGeneration: 11,
  });
  assert.deepEqual(onboardingMutationExpectation({
    existingDefinition: definition,
    createListExpectation: { expectedRevision: 8, processGeneration: 11 },
    storeExpectation: { expectedRevision: 9, processGeneration: 11 },
  }), { expectedRevision: 4, processGeneration: 11 });
  assert.deepEqual(onboardingMutationExpectation({
    createListExpectation: { expectedRevision: 8, processGeneration: 11 },
    storeExpectation: { expectedRevision: 9, processGeneration: 11 },
  }), { expectedRevision: 8, processGeneration: 11 });
  assert.equal(onboardingMutationExpectation({
    storeExpectation: { expectedRevision: 9, processGeneration: 11 },
  }), null);
});

test("an open dirty or unknown form is not rehydrated from refreshed props", () => {
  assert.equal(shouldHydrateOnboardingForm({ visible: true, wasVisible: false }), true);
  assert.equal(shouldHydrateOnboardingForm({ visible: true, wasVisible: true }), false);
  assert.equal(shouldHydrateOnboardingForm({ visible: false, wasVisible: true }), false);
});

test("unknown retry keeps the same operation id and payload signature", () => {
  const draft = validDraft();
  draft.models = [{ public_model: "opus", upstream_model: "vendor/opus" }];
  draft.key = "sk-lab";
  const operationId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const payload = buildOnboardingCommitPayload({
    draft,
    operationId,
    mode: "complete",
  });
  const signature = onboardingPayloadSignature(payload);
  const retried = nextOnboardingOperationId({
    previousId: operationId,
    previousSignature: signature,
    nextSignature: signature,
    lastFailure: "uncertain",
  });
  assert.equal(retried, operationId);
  const again = buildOnboardingCommitPayload({
    draft,
    operationId: retried,
    mode: "complete",
  });
  assert.equal(onboardingPayloadSignature(again), signature);
  assert.equal(again.operationId, operationId);
  assert.throws(
    () => nextOnboardingOperationId({
      previousId: operationId,
      previousSignature: signature,
      nextSignature: "changed",
      lastFailure: "uncertain",
    }),
    (error: unknown) => (
      error instanceof CredentialEditorError
      && error.issue === "uncertain_payload_locked"
      && onboardingUnknownLockedError(error)
    ),
  );
});

test("network unknown is retryable; 409 and auth failures are not", () => {
  assert.equal(isUncertainOnboardingFailure(new TypeError("Failed to fetch")), true);
  assert.equal(isUncertainOnboardingFailure(new DashboardRequestError("boom", 500)), true);
  assert.equal(isUncertainOnboardingFailure(new DashboardRequestError("conflict", 409)), false);
  assert.equal(isUncertainOnboardingFailure(new DashboardRequestError("bad", 400)), false);
  assert.equal(isUncertainOnboardingFailure(new DashboardAuthError("auth")), false);
});

test("saved-key indicator never treats none-auth or missing material as a Key", () => {
  const keyed = { id: "conn-1", authorization: "unknown" as const };
  const noneAuth = { id: "conn-1", authorization: "not_required" as const };
  const missing = { id: "conn-1", authorization: "missing" as const };
  const identities = [{
    credentials: [{
      credential: { has_material: true, material_kind: "api_key" },
      bindings: [{ connection_id: "conn-1" }],
      legacy: { kind: "account", id: "acc-1" },
    }],
  }];
  assert.equal(connectionHasSavedKey(noneAuth, { authKind: "none", identities }), false);
  assert.equal(connectionHasSavedKey(missing, { authKind: "bearer", identities }), false);
  assert.equal(connectionHasSavedKey(keyed, { authKind: "bearer" }), false);
  assert.equal(connectionHasSavedKey(keyed, { authKind: "bearer", identities }), true);
  assert.equal(connectionHasSavedKey(keyed, {
    authKind: "bearer",
    identities,
    legacyAccountId: "acc-other",
  }), false);
  assert.equal(connectionHasSavedKey(keyed, {
    authKind: "bearer",
    identities,
    legacyAccountId: "acc-1",
  }), true);
  assert.equal(identityHasSavedMaterialForConnection(identities, "conn-other"), false);
  assert.equal(shouldOfferAuthorizeCurrentEndpoint({
    intent: "complete",
    hasSavedKey: true,
  }), true);
  assert.equal(shouldOfferAuthorizeCurrentEndpoint({
    intent: "draft",
    hasSavedKey: true,
  }), false);
  assert.equal(shouldOfferAuthorizeCurrentEndpoint({
    intent: "complete",
    hasSavedKey: false,
  }), false);
  assert.equal(
    destinationOriginFromEndpointUrl("https://API.Example.com:443/v1/chat"),
    "https://api.example.com:443",
  );
});
