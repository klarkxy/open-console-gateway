import type { Connection, LegacyConnectionKind } from "../api/connections.ts";
import type { Destination, LegacyDestinationRef } from "../api/destinations.ts";
import {
  connectionForLegacyProvider,
  isOnboardingDraftConnection,
} from "./connections.ts";

/** Catalog key for OpenCode Go. Not a reserved account id. */
export const DEFAULT_PROVIDER_ID = "opencode";
export const OLLAMA_PROVIDER_ID = "ollama";
export const ZEN_FREE_PROVIDER_ID = "opencode-zen-free";
export const CPA_PROVIDER_ID = "cpa";

const DESTINATION_TO_CONNECTION_KIND: Record<
  Exclude<LegacyDestinationRef["kind"], "platform_parent">,
  LegacyConnectionKind
> = {
  builtin: "builtin_provider",
  dynamic: "dynamic_provider",
  custom_account: "custom_account",
};

const CONNECTION_TO_DESTINATION_KIND: Record<
  LegacyConnectionKind,
  Exclude<LegacyDestinationRef["kind"], "platform_parent">
> = {
  builtin_provider: "builtin",
  dynamic_provider: "dynamic",
  custom_account: "custom_account",
};

function normalizeLegacyId(id: string): string {
  return id.trim().toLocaleLowerCase();
}

/** Join a destination to the V4 connection that still owns mutations. */
export function connectionForDestination(
  connections: readonly Connection[],
  destination: Pick<Destination, "legacy">,
): Connection | undefined {
  if (destination.legacy.kind === "platform_parent") return undefined;
  const kind = DESTINATION_TO_CONNECTION_KIND[destination.legacy.kind];
  const needle = normalizeLegacyId(destination.legacy.id);
  return connections.find((connection) => (
    connection.legacy.kind === kind
    && normalizeLegacyId(connection.legacy.id) === needle
  ));
}

/** Inverse of {@link connectionForDestination} for a Providers rail row. */
export function destinationForConnection(
  destinations: readonly Destination[],
  connection: Pick<Connection, "legacy">,
): Destination | undefined {
  const destKind = CONNECTION_TO_DESTINATION_KIND[connection.legacy.kind];
  const needle = normalizeLegacyId(connection.legacy.id);
  return destinations.find((destination) => (
    destination.legacy.kind === destKind
    && normalizeLegacyId(destination.legacy.id) === needle
  ));
}

/**
 * New API / Sub2API sites live on Accounts. Their parent destination is not a
 * Provider catalog row and must not appear in the Providers rail.
 */
export function isProvidersRailDestination(
  destination: Pick<Destination, "legacy">,
): boolean {
  return destination.legacy.kind !== "platform_parent";
}

export function filterDestinations(
  destinations: readonly Destination[],
  query: string,
): Destination[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...destinations];
  return destinations.filter((destination) => (
    destination.name.toLocaleLowerCase().includes(needle)
    || destination.legacy.id.toLocaleLowerCase().includes(needle)
    || (destination.brand_family ?? "").toLocaleLowerCase().includes(needle)
    || (destination.base_url ?? "").toLocaleLowerCase().includes(needle)
  ));
}

/**
 * Providers navigation key. Always the destination id.
 * Old `connection` and `provider` query params are converted once at page entry.
 * Writes that still need a connection id use `connectionForDestination`.
 */
export function railKeyForDestination(
  destination: Pick<Destination, "id">,
): string {
  return destination.id;
}

export function railKeyForDraftConnection(
  connection: Pick<Connection, "id">,
): string {
  return connection.id;
}

/**
 * Destination projection on the Providers rail. Empty last-success content is
 * `empty`; a primary failure with nothing retained is `failure`; an in-flight
 * first load is `not_loaded`. Do not treat those three as interchangeable.
 */
export type ProvidersDestinationProjectionState = "not_loaded" | "failure" | "empty" | "ready";

export function providersDestinationProjectionState(input: {
  loaded: boolean;
  loadFailed: boolean;
  railCount: number;
}): ProvidersDestinationProjectionState {
  if (input.loaded) return input.railCount > 0 ? "ready" : "empty";
  if (input.loadFailed) return "failure";
  return "not_loaded";
}

export type ProvidersRailItem =
  | { kind: "destination"; destination: Destination }
  | { kind: "draft_connection"; connection: Connection };

/**
 * Rail rows once the destination projection is ready or empty. Draft
 * connections that do not join a rail destination stay visible; configured
 * connections are not a fallback for an unloaded or failed destination list.
 */
export function providersRailItems(
  destinations: readonly Destination[],
  connections: readonly Connection[] | null,
): ProvidersRailItem[] {
  const railDestinations = destinations.filter(isProvidersRailDestination);
  const connectionRows = connections ?? [];
  const joinedConnectionIds = new Set(
    railDestinations
      .map((destination) => connectionForDestination(connectionRows, destination)?.id)
      .filter((id): id is string => Boolean(id)),
  );
  const unmatchedDrafts = connectionRows.filter((connection) => (
    isOnboardingDraftConnection(connection) && !joinedConnectionIds.has(connection.id)
  ));
  return [
    ...railDestinations.map((destination) => ({ kind: "destination" as const, destination })),
    ...unmatchedDrafts.map((connection) => ({ kind: "draft_connection" as const, connection })),
  ];
}

export interface ProvidersSelectionQuery {
  connection: string | null;
  provider: string | null;
  destination: string | null;
}

export interface ProvidersSelectionPrefer {
  connectionId?: string;
  providerId?: string;
}

export interface ProvidersSelectionCached {
  destinationId: string | null;
  connectionId: string | null;
}

export interface ProvidersSelection {
  destinationId: string | null;
  /** Joined connection, or a draft/unmatched connection when no destination resolves. */
  connectionId: string | null;
  fellBack: boolean;
}

function selectionFromConnection(
  destinations: readonly Destination[],
  connections: readonly Connection[],
  connectionId: string,
): Omit<ProvidersSelection, "fellBack"> | null {
  const connection = connections.find((row) => row.id === connectionId);
  if (!connection) return null;
  const destination = destinationForConnection(destinations, connection);
  if (destination) {
    return { destinationId: destination.id, connectionId: connection.id };
  }
  return { destinationId: null, connectionId: connection.id };
}

function selectionFromProvider(
  destinations: readonly Destination[],
  connections: readonly Connection[],
  providerId: string,
): Omit<ProvidersSelection, "fellBack"> | null {
  const connection = connectionForLegacyProvider(connections, providerId);
  if (!connection) return null;
  return selectionFromConnection(destinations, connections, connection.id);
}

function selectionFromDestination(
  destinations: readonly Destination[],
  connections: readonly Connection[],
  destinationId: string,
): Omit<ProvidersSelection, "fellBack"> | null {
  const destination = destinations.find((row) => (
    row.id === destinationId && isProvidersRailDestination(row)
  ));
  if (!destination) return null;
  return {
    destinationId: destination.id,
    connectionId: connectionForDestination(connections, destination)?.id ?? null,
  };
}

function defaultProvidersSelection(
  destinations: readonly Destination[],
  connections: readonly Connection[],
): Omit<ProvidersSelection, "fellBack"> {
  const rail = destinations.filter(isProvidersRailDestination);
  const firstDestination = rail[0];
  if (firstDestination) {
    return {
      destinationId: firstDestination.id,
      connectionId: connectionForDestination(connections, firstDestination)?.id ?? null,
    };
  }
  const draft = connections.find(isOnboardingDraftConnection);
  if (draft) return { destinationId: null, connectionId: draft.id };
  return { destinationId: null, connectionId: null };
}

/**
 * Providers selection: explicit navigation/action target, then destination id,
 * then cached selection, then the first rail destination (or an unmatched
 * draft connection). A leftover cached destination must not beat `provider=`.
 */
export function resolveProvidersSelection(input: {
  query: ProvidersSelectionQuery;
  prefer?: ProvidersSelectionPrefer;
  cached: ProvidersSelectionCached;
  destinations: readonly Destination[];
  connections: readonly Connection[] | null;
}): ProvidersSelection {
  const connections = input.connections ?? [];
  const connectionId = input.prefer?.connectionId ?? input.query.connection;
  const providerId = input.prefer?.providerId ?? input.query.provider;
  const destinationId = input.query.destination;
  const hadExplicit = Boolean(connectionId || providerId || destinationId);

  if (connectionId) {
    const resolved = selectionFromConnection(input.destinations, connections, connectionId);
    if (resolved) return { ...resolved, fellBack: false };
  }
  if (providerId) {
    const resolved = selectionFromProvider(input.destinations, connections, providerId);
    if (resolved) return { ...resolved, fellBack: false };
  }
  if (destinationId) {
    const resolved = selectionFromDestination(input.destinations, connections, destinationId);
    if (resolved) return { ...resolved, fellBack: false };
  }
  if (input.cached.destinationId) {
    const resolved = selectionFromDestination(
      input.destinations,
      connections,
      input.cached.destinationId,
    );
    if (resolved) return { ...resolved, fellBack: hadExplicit };
  }
  if (input.cached.connectionId) {
    const resolved = selectionFromConnection(
      input.destinations,
      connections,
      input.cached.connectionId,
    );
    if (resolved) return { ...resolved, fellBack: hadExplicit };
  }
  return { ...defaultProvidersSelection(input.destinations, connections), fellBack: hadExplicit };
}

/** Resources `Providers` loads together. Destinations are the rail primary. */
export const PROVIDERS_PAGE_NECESSARY_RESOURCES = [
  "destinations",
  "catalog",
  "connections",
  "contracts",
] as const;

export type ProvidersPageNecessaryResource = (typeof PROVIDERS_PAGE_NECESSARY_RESOURCES)[number];

export type ProvidersPageLoadResults = Record<
  ProvidersPageNecessaryResource | "accounts",
  PromiseSettledResult<unknown>
>;

export interface ProvidersPageLoadOutcome {
  ok: boolean;
  /**
   * Rewrite selection/URL only when every resource needed to resolve the
   * current target succeeded. A Destination-only success is not enough:
   * failed Connections cannot map `provider=` and would fall back.
   */
  applySelection: boolean;
  failedResource: ProvidersPageNecessaryResource | null;
  reason: unknown;
}

export function providersPageLoadOutcome(
  results: ProvidersPageLoadResults,
): ProvidersPageLoadOutcome {
  for (const resource of PROVIDERS_PAGE_NECESSARY_RESOURCES) {
    const result = results[resource];
    if (result.status === "rejected") {
      return {
        ok: false,
        applySelection: false,
        failedResource: resource,
        reason: result.reason,
      };
    }
  }
  return { ok: true, applySelection: true, failedResource: null, reason: undefined };
}

/** Last-success snapshots required before resolving a Providers deep link. */
export function providersSelectionProjectionReady(input: {
  destinationsLoaded: boolean;
  connectionsLoaded: boolean;
  catalogLoaded: boolean;
  contractsLoaded: boolean;
}): boolean {
  return input.destinationsLoaded
    && input.connectionsLoaded
    && input.catalogLoaded
    && input.contractsLoaded;
}

export type ProvidersQueryAction = "redirect-add" | "apply-selection" | "defer";

/**
 * popstate / loadAll action. Add/preset redirects never wait for Destinations.
 * A target already present in the current projection may apply immediately.
 * Fallback and URL rewrite for an unresolved explicit `provider=` /
 * `destination=` / `connection=` wait until a fresh necessary-resource load
 * has succeeded, so KeepAlive last-success data cannot erase the link.
 * A direct rail/mobile pick (`userSelection`) commits anyway and replaces
 * that pending URL target; add redirect and an unread projection still win.
 */
export function providersQueryAction(input: {
  add: boolean;
  projectionReady: boolean;
  unresolvedExplicitTarget: boolean;
  freshLoadSucceeded: boolean;
  /** Rail/mobile pick; writes even while a URL target is still unresolved. */
  userSelection?: boolean;
}): ProvidersQueryAction {
  if (input.add) return "redirect-add";
  if (!input.projectionReady) return "defer";
  if (input.userSelection) return "apply-selection";
  if (input.unresolvedExplicitTarget && !input.freshLoadSucceeded) return "defer";
  return "apply-selection";
}

export function providersRailItemName(item: ProvidersRailItem): string {
  return item.kind === "destination" ? item.destination.name : item.connection.name;
}
