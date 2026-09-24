/**
 * Hand-written Dashboard V4 endpoint client for the additive
 * `/dashboard/api/v4` contract (schema/dashboard-api-v4.schema.json).
 *
 * Transport, error classes, and CAS token publishing are shared with V3.
 * Generated types stay in `generated/dashboard-v4.ts` (types only).
 */

import { requestV4, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  BindingPatchRequest,
  BindingPatchResult,
  ConnectionList,
  CredentialList,
  DestinationList,
  AliasPublication,
  AliasPublicationUpdate,
  CatalogModelsRemoveRequest,
  CatalogModelsRemoveResult,
  CpaCatalog,
  CpaCatalogUpdate,
  CredentialRotateRequest,
  CredentialRotateResult,
  DestinationCatalogModelUpdate,
  DestinationCatalogUpdate as DestinationCatalogUpdateDto,
  DestinationDeleteResult,
  DestinationModelTestResult,
  DestinationPatchRequest,
  DestinationPatchResult,
  DestinationCatalogRefreshResult,
  DshApplication,
  DshApplicationInstallRequest,
  HttpProtocolRouteDto,
  IdentityCredentialCreateRequest,
  IdentityCredentialCreateResult,
  IdentityList,
  OnboardingCommitRequest,
  OnboardingCommitResult,
  ProtocolDto,
  QuotaRecoveryDto,
  QuotaRetryResult,
  RoutingCardList,
  RoutingCardUpdate,
  RoutingClientProtocol,
  RoutingExplanation,
  TemplateList,
} from "./generated/dashboard-v4.ts";

export type {
  DestinationCatalogModelUpdate,
  DestinationModelTestResult,
  HttpProtocolRouteDto,
  QuotaRecoveryDto,
  QuotaRetryResult,
};

/**
 * Catalog write body without CAS. Callers pass this business input; the
 * client attaches `expectedRevision` and `processGeneration` per attempt.
 */
export type DestinationCatalogUpdate = WithoutExpectation<DestinationCatalogUpdateDto>;

export const dashboardV4 = {
  getTemplates: () => requestV4<TemplateList>("/templates"),
  getConnections: () => requestV4<ConnectionList>("/connections"),
  getAccounts: () => requestV4<IdentityList>("/accounts"),
  getDestinations: () => requestV4<DestinationList>("/destinations"),
  getCredentials: () => requestV4<CredentialList>("/credentials"),
  refreshDestinationCatalog: (id: string, expectation: MutationExpectation) =>
    requestV4<DestinationCatalogRefreshResult>(
      `/destinations/${encodeURIComponent(id)}/catalog/refresh`,
      { method: "POST", body: withExpectation({}, expectation) },
    ),
  updateDestinationCatalog: (id: string, input: WithoutExpectation<DestinationCatalogUpdate>, expectation: MutationExpectation) =>
    requestV4<DestinationPatchResult>(
      `/destinations/${encodeURIComponent(id)}/catalog`,
      { method: "PUT", body: withExpectation(input, expectation) },
    ),
  testDestinationModel: (
    id: string,
    publicModel: string,
    protocol: ProtocolDto,
    expectation: MutationExpectation,
  ) =>
    requestV4<DestinationModelTestResult>(
      `/destinations/${encodeURIComponent(id)}/model-tests`,
      { method: "POST", body: withExpectation({ publicModel, protocol }, expectation) },
    ),
  getRoutingCards: () => requestV4<RoutingCardList>("/routing/cards"),
  putRoutingCards: (
    input: WithoutExpectation<RoutingCardUpdate>,
    expectation: MutationExpectation,
  ) => requestV4<RoutingCardList>("/routing/cards", {
    method: "PUT",
    body: withExpectation(input, expectation),
  }),
  patchDestination: (
    id: string,
    input: WithoutExpectation<DestinationPatchRequest>,
    expectation: MutationExpectation,
  ) => requestV4<DestinationPatchResult>(
    `/destinations/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      body: withExpectation(input, expectation),
    },
  ),
  deleteDestination: (id: string, expectation: MutationExpectation) =>
    requestV4<DestinationDeleteResult>(
      `/destinations/${encodeURIComponent(id)}`,
      {
        method: "DELETE",
        body: withExpectation({}, expectation),
      },
    ),
  explainRouting: (model: string, clientProtocol: RoutingClientProtocol) =>
    requestV4<RoutingExplanation>(
      `/routing/explain?model=${encodeURIComponent(model)}&clientProtocol=${encodeURIComponent(clientProtocol)}`,
    ),
  commitOnboarding: (
    input: WithoutExpectation<OnboardingCommitRequest>,
    expectation: MutationExpectation,
  ) => requestV4<OnboardingCommitResult>("/onboarding/commit", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  rotateCredential: (
    id: string,
    input: WithoutExpectation<CredentialRotateRequest>,
    expectation: MutationExpectation,
  ) => requestV4<CredentialRotateResult>(
    `/credentials/${encodeURIComponent(id)}/rotate`,
    {
      method: "POST",
      body: withExpectation(input, expectation),
    },
  ),
  retryCredentialQuota: (
    id: string,
    expectation: MutationExpectation,
  ) => requestV4<QuotaRetryResult>(
    `/credentials/${encodeURIComponent(id)}/quota-retry`,
    {
      method: "POST",
      body: withExpectation({}, expectation),
    },
  ),
  patchBinding: (
    id: string,
    input: WithoutExpectation<BindingPatchRequest>,
    expectation: MutationExpectation,
  ) => requestV4<BindingPatchResult>(
    `/bindings/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      body: withExpectation(input, expectation),
    },
  ),
  createIdentityCredential: (
    id: string,
    input: WithoutExpectation<IdentityCredentialCreateRequest>,
    expectation: MutationExpectation,
  ) => requestV4<IdentityCredentialCreateResult>(
    `/identities/${encodeURIComponent(id)}/credentials`,
    {
      method: "POST",
      body: withExpectation(input, expectation),
    },
  ),
  getCpaCatalog: () => requestV4<CpaCatalog>("/cpa/models"),
  putCpaCatalog: (
    input: WithoutExpectation<CpaCatalogUpdate>,
    expectation: MutationExpectation,
  ) => requestV4<CpaCatalog>("/cpa/models", {
    method: "PUT",
    body: withExpectation(input, expectation),
  }),
  getAliasPublication: () => requestV4<AliasPublication>("/alias-publication"),
  patchAliasPublication: (
    input: WithoutExpectation<AliasPublicationUpdate>,
    expectation: MutationExpectation,
  ) => requestV4<AliasPublication>("/alias-publication", {
    method: "PATCH",
    body: withExpectation(input, expectation),
  }),
  removeCatalogModels: (
    scopeKind: "provider" | "custom_endpoint",
    scopeId: string,
    input: WithoutExpectation<CatalogModelsRemoveRequest>,
    expectation: MutationExpectation,
  ) => requestV4<CatalogModelsRemoveResult>(
    `/provider-contracts/${encodeURIComponent(scopeKind)}/${encodeURIComponent(scopeId)}/catalog/remove`,
    {
      method: "POST",
      body: withExpectation(input, expectation),
    },
  ),
  getDshApplication: () => requestV4<DshApplication>("/applications/dsh"),
  installDshApplication: (
    input: WithoutExpectation<DshApplicationInstallRequest>,
    expectation: MutationExpectation,
  ) => requestV4<DshApplication>("/applications/dsh", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
};
