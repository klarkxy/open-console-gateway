import type { ConnectionEndpoint } from "../api/connections.ts";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import type { ModelProtocolOverrideUpdate, ProviderProtocol } from "../api/providers.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";

export interface ProviderProtocolGrantCandidate {
  id: string;
  name: string;
  /** Force-on protocols for which this Key still lacks the exact endpoint grant. */
  missingProtocols: ProviderProtocol[];
}

/** Snapshot held while the operator decides whether to grant a Key. */
export interface ProviderProtocolGrantCapture {
  connectionId: string;
  destinationId: string;
  expectation: MutationExpectation;
  scopeKey: string;
}

/** A dialog decision is stale if its selection or CAS identity changed. */
export function providerProtocolGrantCaptureIsCurrent(
  capture: ProviderProtocolGrantCapture,
  current: ProviderProtocolGrantCapture,
): boolean {
  return capture.scopeKey === current.scopeKey
    && capture.destinationId === current.destinationId
    && capture.connectionId === current.connectionId
    && capture.expectation.expectedRevision === current.expectation.expectedRevision
    && capture.expectation.processGeneration === current.expectation.processGeneration;
}

function operationForProtocol(protocol: ProviderProtocol): ConnectionEndpoint["operation"] {
  switch (protocol) {
    case "chat_completions": return "chat_create";
    case "responses": return "response_create";
    case "messages": return "message_create";
  }
}

/**
 * Connections identify a callable endpoint by both its operation and wire
 * protocol. Keep that exact pairing: a same-origin sibling operation is not a
 * grant for a newly enabled protocol.
 */
export function providerProtocolEndpointId(
  endpoints: readonly ConnectionEndpoint[],
  protocol: ProviderProtocol,
): string | null {
  const operation = operationForProtocol(protocol);
  return endpoints.find((endpoint) => (
    endpoint.operation === operation && endpoint.wire_protocol === protocol
  ))?.id ?? null;
}

function forceOnProtocols(
  overrides: readonly ModelProtocolOverrideUpdate[],
): ProviderProtocol[] {
  const protocols: ProviderProtocol[] = [];
  for (const override of overrides) {
    if (override.state !== "force_on" || protocols.includes(override.protocol)) continue;
    protocols.push(override.protocol);
  }
  return protocols;
}

/**
 * Keys on a sealed provider destination that need explicit grants for this
 * matrix write. HTTP destinations manage route consent in their own editor;
 * observer rows do not expose inference endpoints and must never be guessed.
 */
export function providerProtocolGrantCandidates(
  destination: Pick<Destination, "adapter" | "capabilities" | "id">,
  credentials: readonly Pick<DestinationCredential,
    "destination_id" | "grants" | "has_secret" | "id" | "name">[],
  endpoints: readonly ConnectionEndpoint[],
  overrides: readonly ModelProtocolOverrideUpdate[],
): ProviderProtocolGrantCandidate[] {
  if (destination.adapter === "http" || destination.capabilities.observer) return [];
  const endpointByProtocol = new Map<ProviderProtocol, string>();
  for (const protocol of forceOnProtocols(overrides)) {
    const endpointId = providerProtocolEndpointId(endpoints, protocol);
    if (endpointId) endpointByProtocol.set(protocol, endpointId);
  }
  if (endpointByProtocol.size === 0) return [];

  return credentials
    .filter((credential) => credential.destination_id === destination.id && credential.has_secret)
    .map((credential) => ({
      id: credential.id,
      name: credential.name,
      missingProtocols: [...endpointByProtocol]
        .filter(([, endpointId]) => !credential.grants.allowed_endpoint_ids.includes(endpointId))
        .map(([protocol]) => protocol),
    }))
    .filter((candidate) => candidate.missingProtocols.length > 0);
}
