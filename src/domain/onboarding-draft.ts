import type { WithoutExpectation } from "../api/dashboard-v3.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { ProviderDefinitionView } from "../api/providers.ts";
import { PROVIDER_PRESETS } from "./provider-presets.ts";
import type {
  OnboardingCommitMode,
  OnboardingCommitRequest,
  OnboardingConnectionNew,
} from "../api/generated/dashboard-v4.ts";
import {
  CredentialEditorError,
  isUncertainCreateFailure,
  nextCreateOperationId,
  normalizeOrigin,
  type CredentialCreateFailureKind,
} from "./account-credential.ts";
import {
  dynamicAuthRequiresKey,
  normalizeDynamicMappings,
  validateProviderDefinitionDraft,
  type ProviderDefinitionDraft,
  type ProviderDefinitionDraftError,
} from "./dynamic-provider.ts";

export type OnboardingIntent = OnboardingCommitMode;
export type OnboardingCommitInput = WithoutExpectation<OnboardingCommitRequest>;
export type OnboardingFailureKind = CredentialCreateFailureKind;

type IdentitySavedKeyRow = {
  credentials: readonly {
    credential: { has_material: boolean; material_kind: string };
    bindings: readonly { connection_id: string }[];
    legacy: { kind: string; id: string };
  }[];
};

/**
 * A Key is claimed only from identity `has_material` on an api_key bound to
 * this exact connection. Callers that treat none-auth as never-saved (the
 * modal's `effectiveHasSavedKey`) must apply that gate themselves.
 */
export function identityHasSavedMaterialForConnection(
  identities: readonly IdentitySavedKeyRow[],
  connectionId: string,
  legacyAccountId?: string | null,
): boolean {
  const needle = connectionId.trim();
  if (!needle) return false;
  const accountId = legacyAccountId?.trim() ?? "";
  return identities.some((identity) => (
    identity.credentials.some((row) => {
      if (!row.credential.has_material || row.credential.material_kind !== "api_key") {
        return false;
      }
      if (accountId && (row.legacy.kind !== "account" || row.legacy.id !== accountId)) {
        return false;
      }
      return row.bindings.some((binding) => binding.connection_id === needle);
    })
  ));
}

export function expectationFromProviderDefinition(
  definition: Pick<ProviderDefinitionView, "revision" | "process_generation">,
): MutationExpectation {
  return {
    expectedRevision: definition.revision,
    processGeneration: definition.process_generation,
  };
}

/**
 * Existing draft/edit CAS must follow the definition that filled the form.
 * Create may use the connections list snapshot. A newer global/list pair
 * must not rebase an already-initialized existing form.
 */
export function onboardingMutationExpectation(args: {
  existingDefinition?: Pick<ProviderDefinitionView, "revision" | "process_generation"> | null;
  createListExpectation?: MutationExpectation | null;
  storeExpectation?: MutationExpectation | null;
}): MutationExpectation | null {
  if (args.existingDefinition) return expectationFromProviderDefinition(args.existingDefinition);
  return args.createListExpectation ?? null;
}

export function shouldHydrateOnboardingForm(args: {
  visible: boolean;
  wasVisible: boolean;
}): boolean {
  return args.visible && !args.wasVisible;
}

/**
 * Draft may omit models and Key. Any supplied URL or mapping still has to
 * pass the ordinary field rules. Complete needs mappings, and a Key when
 * keyed auth has no saved material.
 */
export function validateOnboardingDraft(
  draft: ProviderDefinitionDraft,
  options: {
    intent: OnboardingIntent;
    hasSavedKey?: boolean;
    previousAuthKind?: string | null;
  },
): ProviderDefinitionDraftError | null {
  const structural = validateProviderDefinitionDraft(draft, {
    mode: "create",
    requireKey: false,
  });
  if (
    options.previousAuthKind === "none"
    && dynamicAuthRequiresKey(draft.auth_kind)
    && !draft.key.trim()
  ) {
    return "missing_replacement_key";
  }
  if (options.intent === "draft") {
    if (structural === "missing_mappings") return null;
    return structural;
  }
  if (structural) return structural;
  if (
    dynamicAuthRequiresKey(draft.auth_kind)
    && !options.hasSavedKey
    && !draft.key.trim()
  ) {
    return "missing_key";
  }
  return null;
}

function connectionConfiguration(draft: ProviderDefinitionDraft): OnboardingConnectionNew {
  const preset = PROVIDER_PRESETS.find((entry) => entry.id === draft.preset_id);
  const protocolRoutes = preset?.endpointUrl === draft.endpoint_url.trim()
    && preset.protocol === draft.upstream_protocol && preset.authKind === draft.auth_kind
    ? preset.protocolRoutes?.map((route) => ({ ...route })) : undefined;
  return {
    templateId: draft.preset_id || "custom-http",
    name: draft.name.trim(),
    endpointUrl: draft.endpoint_url.trim(),
    upstreamProtocol: draft.upstream_protocol as OnboardingConnectionNew["upstreamProtocol"],
    authKind: draft.auth_kind as OnboardingConnectionNew["authKind"],
    ...(protocolRoutes?.length ? { protocolRoutes } : {}),
  };
}

function commitTargets(draft: ProviderDefinitionDraft, intent: OnboardingIntent) {
  const mappings = normalizeDynamicMappings(draft.models);
  if (typeof mappings === "string") {
    if (intent === "draft" && mappings === "missing_mappings") return [];
    throw new Error(mappings);
  }
  return mappings.map((mapping) => ({
    publicModel: mapping.public_model,
    upstreamModel: mapping.upstream_model,
    upstreamOverride: mapping.upstream_override
      ? {
        protocol: mapping.upstream_override.protocol,
        endpointUrl: mapping.upstream_override.endpoint_url,
      }
      : null,
  }));
}

export function buildOnboardingCommitPayload(args: {
  draft: ProviderDefinitionDraft;
  operationId: string;
  mode: OnboardingIntent;
  connectionId?: string | null;
  hasSavedKey?: boolean;
  previousAuthKind?: string | null;
  authorizeCurrentEndpoint?: boolean;
}): OnboardingCommitInput {
  const createError = validateOnboardingDraft(args.draft, {
    intent: args.mode,
    hasSavedKey: Boolean(args.hasSavedKey),
    previousAuthKind: args.previousAuthKind,
  });
  if (createError) throw new Error(createError);
  const configuration = connectionConfiguration(args.draft);
  const body: OnboardingCommitInput = {
    operationId: args.operationId,
    mode: args.mode,
    connection: args.connectionId
      ? {
        kind: "existing",
        connectionId: args.connectionId,
        configuration,
      }
      : {
        kind: "new",
        ...configuration,
      },
    targets: commitTargets(args.draft, args.mode),
  };
  if (args.authorizeCurrentEndpoint) body.authorizeCurrentEndpoint = true;
  if (args.draft.auth_kind === "none") {
    body.authorization = { kind: "none" };
  } else if (dynamicAuthRequiresKey(args.draft.auth_kind)) {
    const secretInput = args.draft.key.trim();
    if (secretInput) {
      const accountLabel = args.draft.account_name.trim();
      const notes = args.draft.notes.trim();
      body.authorization = {
        kind: "api_key",
        secretInput,
        ...(accountLabel ? { accountLabel } : {}),
        ...(notes ? { notes } : {}),
      };
    }
  }
  return body;
}

export function onboardingPayloadSignature(payload: OnboardingCommitInput): string {
  return JSON.stringify({
    mode: payload.mode ?? null,
    authorizeCurrentEndpoint: payload.authorizeCurrentEndpoint === true,
    connection: payload.connection,
    authorization: payload.authorization ?? null,
    targets: payload.targets,
  });
}

export function nextOnboardingOperationId(args: {
  previousId: string | null;
  previousSignature: string | null;
  nextSignature: string;
  lastFailure: OnboardingFailureKind;
}): string {
  return nextCreateOperationId(args);
}

export function onboardingUnknownLockedError(error: unknown): boolean {
  return error instanceof CredentialEditorError && error.issue === "uncertain_payload_locked";
}

export { isUncertainCreateFailure as isUncertainOnboardingFailure };

export function destinationOriginFromEndpointUrl(url: string): string | null {
  return normalizeOrigin(url);
}

export function shouldOfferAuthorizeCurrentEndpoint(args: {
  intent: OnboardingIntent;
  hasSavedKey: boolean;
}): boolean {
  return args.intent === "complete" && args.hasSavedKey;
}
