import type {
  AuthSchemeDto,
  Destination,
  DestinationCredential,
  DestinationPatchInput,
  ProtocolDto,
} from "../api/destinations.ts";
import type { ConnectionEndpoint } from "../api/connections.ts";
import type { MessageKey } from "../i18n/index.ts";
import { customEndpointUrlIssue } from "./custom-account.ts";
import { MAX_HTTP_PROTOCOL_ROUTES } from "./destination-catalog.ts";
import { providerPresetRoutesForEndpoint, type ProviderPreset } from "./provider-presets.ts";

/**
 * Edit planning for configurable HTTP destinations (dynamic providers and
 * legacy Custom API connections). The destination PATCH is a full replacement
 * of the editable configuration; Keys stay account-owned and are only touched
 * through the explicit `authorizeCredentialIds` grant consent.
 */

export interface DestinationModelDraft {
  enabled?: boolean;
  public_model: string;
  upstream_model: string;
  /** Null inherits the connection endpoint/protocol. */
  upstream_override: { protocol: ProtocolDto; endpoint_url: string } | null;
  protocols?: ProtocolDto[];
  preferred?: ProtocolDto | null;
}

export interface DestinationProtocolRouteDraft {
  protocol: ProtocolDto | "";
  endpoint_url: string;
  auth_scheme: AuthSchemeDto;
}

export interface DestinationEditDraft {
  enabled?: boolean;
  name: string;
  /** First route is the legacy default. Extra routes are additional protocols. */
  protocol_routes: DestinationProtocolRouteDraft[];
  models: DestinationModelDraft[];
}

export type DestinationEditIssue =
  | "immutable_destination"
  | "missing_name"
  | "missing_endpoint_url"
  | "invalid_endpoint_url"
  | "endpoint_url_not_http"
  | "endpoint_url_with_credentials"
  | "missing_protocol"
  | "missing_mappings"
  | "duplicate_public_model"
  | "missing_public_model"
  | "missing_upstream_model"
  | "missing_override_endpoint"
  | "invalid_override_endpoint"
  | "override_endpoint_not_http"
  | "override_endpoint_with_credentials"
  | "missing_route_endpoint"
  | "invalid_route_endpoint"
  | "route_endpoint_not_http"
  | "route_endpoint_with_credentials"
  | "duplicate_protocol_route"
  | "too_many_protocol_routes";

export const DESTINATION_EDIT_ISSUE_KEYS = {
  immutable_destination: "此连接由系统托管，不能在此编辑",
  missing_name: "填写供应商名称",
  missing_endpoint_url: "填写 API 地址",
  invalid_endpoint_url: "Endpoint 格式无效",
  endpoint_url_not_http: "Endpoint 必须是 http:// 或 https:// URL",
  endpoint_url_with_credentials: "Endpoint 不能包含用户名或密码",
  missing_protocol: "选择上游协议",
  missing_mappings: "至少添加一个完整模型映射",
  duplicate_public_model: "对外模型名不能重复",
  missing_public_model: "填写对外模型名",
  missing_upstream_model: "填写上游模型 ID",
  missing_override_endpoint: "填写该模型覆盖的上游地址，或改回跟随供应商默认",
  invalid_override_endpoint: "覆盖的上游地址格式无效",
  override_endpoint_not_http: "覆盖的上游地址必须是 http:// 或 https:// URL",
  override_endpoint_with_credentials: "覆盖的上游地址不能包含用户名或密码",
  missing_route_endpoint: "填写协议地址",
  invalid_route_endpoint: "协议地址格式无效",
  route_endpoint_not_http: "协议地址必须是 http:// 或 https:// URL",
  route_endpoint_with_credentials: "协议地址不能包含用户名或密码",
  duplicate_protocol_route: "每个上游协议只能配置一条地址",
  too_many_protocol_routes: "最多三条协议地址",
} as const satisfies Record<DestinationEditIssue, MessageKey>;

export class DestinationEditError extends Error {
  readonly issue: DestinationEditIssue;

  constructor(issue: DestinationEditIssue) {
    super(issue);
    this.issue = issue;
  }
}

/** Only user-defined HTTP rows accept the destination PATCH. */
export function isDestinationEditable(
  destination: Pick<Destination, "adapter" | "capabilities">,
): boolean {
  return destination.adapter === "http"
    && !destination.capabilities.observer;
}

export function isDestinationCatalogRefreshable(
  destination: Pick<Destination, "adapter" | "capabilities">,
): boolean {
  return isDestinationEditable(destination) && destination.capabilities.discoverable_models;
}

/** The server refuses a delete while any Key still routes through the destination. */
export function isDestinationDeletable(
  destination: Pick<Destination, "adapter" | "capabilities" | "id">,
  credentials: readonly Pick<DestinationCredential, "destination_id">[],
): boolean {
  return isDestinationEditable(destination)
    && !credentials.some((credential) => credential.destination_id === destination.id);
}

export function destinationHasExplicitRoutes(
  destination: Pick<Destination, "protocol_routes">,
): boolean {
  return (destination.protocol_routes?.length ?? 0) > 0;
}

export function destinationDraftRoutes(
  destination: Destination,
): DestinationProtocolRouteDraft[] {
  if (destinationHasExplicitRoutes(destination)) {
    return destination.protocol_routes!.map((route) => ({
      protocol: route.protocol,
      endpoint_url: route.endpoint_url,
      auth_scheme: route.auth_scheme,
    }));
  }
  return [{
    protocol: destination.protocols[0] ?? "",
    endpoint_url: destination.base_url ?? "",
    auth_scheme: destination.auth_scheme,
  }];
}

export function destinationEditDraft(destination: Destination): DestinationEditDraft {
  return {
    enabled: destination.enabled,
    name: destination.name,
    protocol_routes: destinationDraftRoutes(destination),
    models: destination.catalog.map((model) => ({
      enabled: model.enabled,
      public_model: model.public_model,
      upstream_model: model.upstream_model,
      protocols: [...model.protocols],
      preferred: model.preferred,
      upstream_override: model.upstream_override
        ? { protocol: model.upstream_override.protocol, endpoint_url: model.upstream_override.endpoint_url }
        : null,
    })),
  };
}

function endpointIssue(value: string): DestinationEditIssue | null {
  const issue = customEndpointUrlIssue(value);
  if (issue === "empty") return "missing_endpoint_url";
  if (issue === "malformed") return "invalid_endpoint_url";
  if (issue === "not_http") return "endpoint_url_not_http";
  if (issue === "with_credentials") return "endpoint_url_with_credentials";
  return null;
}

function overrideIssue(
  override: DestinationModelDraft["upstream_override"],
): DestinationEditIssue | null {
  if (!override) return null;
  if (override.protocol !== "chat_completions"
    && override.protocol !== "responses"
    && override.protocol !== "messages") {
    return "missing_protocol";
  }
  const endpointUrl = override.endpoint_url.trim();
  if (!endpointUrl) return "missing_override_endpoint";
  const issue = customEndpointUrlIssue(endpointUrl);
  if (issue === "empty") return "missing_override_endpoint";
  if (issue === "malformed") return "invalid_override_endpoint";
  if (issue === "not_http") return "override_endpoint_not_http";
  if (issue === "with_credentials") return "override_endpoint_with_credentials";
  return null;
}

function routeEndpointIssue(value: string): DestinationEditIssue | null {
  const issue = customEndpointUrlIssue(value);
  if (issue === "empty") return "missing_route_endpoint";
  if (issue === "malformed") return "invalid_route_endpoint";
  if (issue === "not_http") return "route_endpoint_not_http";
  if (issue === "with_credentials") return "route_endpoint_with_credentials";
  return null;
}

function parsedProtocolRoutes(
  draft: DestinationEditDraft,
): { protocol: ProtocolDto; endpoint_url: string; auth_scheme: AuthSchemeDto }[] {
  if (draft.protocol_routes.length > MAX_HTTP_PROTOCOL_ROUTES) {
    throw new DestinationEditError("too_many_protocol_routes");
  }
  const seen = new Set<ProtocolDto>();
  return draft.protocol_routes.map((route, index) => {
    if (route.protocol !== "chat_completions"
      && route.protocol !== "responses"
      && route.protocol !== "messages") {
      throw new DestinationEditError("missing_protocol");
    }
    if (seen.has(route.protocol)) throw new DestinationEditError("duplicate_protocol_route");
    seen.add(route.protocol);
    const endpointUrl = route.endpoint_url.trim();
    const problem = index === 0 ? endpointIssue(endpointUrl) : routeEndpointIssue(endpointUrl);
    if (problem) throw new DestinationEditError(problem);
    return {
      protocol: route.protocol,
      endpoint_url: endpointUrl,
      auth_scheme: route.auth_scheme,
    };
  });
}

/** Validate the draft and build the full-replacement PATCH body (sans CAS pair). */
export function buildDestinationPatch(
  destination: Destination,
  draft: DestinationEditDraft,
): DestinationPatchInput {
  if (!isDestinationEditable(destination)) throw new DestinationEditError("immutable_destination");
  const name = draft.name.trim();
  if (!name) throw new DestinationEditError("missing_name");
  const protocolRoutes = parsedProtocolRoutes(draft);
  const first = protocolRoutes[0];
  if (!first) throw new DestinationEditError("missing_protocol");
  const seen = new Set<string>();
  const models = draft.models.map((model) => {
    const publicModel = model.public_model.trim();
    const upstreamModel = model.upstream_model.trim();
    if (!publicModel) throw new DestinationEditError("missing_public_model");
    if (!upstreamModel) throw new DestinationEditError("missing_upstream_model");
    const overrideProblem = overrideIssue(model.upstream_override);
    if (overrideProblem) throw new DestinationEditError(overrideProblem);
    const key = publicModel.toLocaleLowerCase();
    if (seen.has(key)) throw new DestinationEditError("duplicate_public_model");
    seen.add(key);
    const previous = destination.catalog.find((entry) => entry.public_model.toLocaleLowerCase() === key);
    const available = model.upstream_override
      ? [model.upstream_override.protocol]
      : protocolRoutes.map((route) => route.protocol);
    const previouslyAvailable = previous?.upstream_override
      ? [previous.upstream_override.protocol]
      : destinationDraftRoutes(destination).map((route) => route.protocol);
    let protocols = model.protocols?.filter((protocol) => available.includes(protocol));
    if (protocols) {
      for (const protocol of available) {
        if (!previouslyAvailable.includes(protocol) && !protocols.includes(protocol)) protocols.push(protocol);
      }
      if (model.enabled && !previous?.enabled && protocols.length === 0) protocols = [...available];
    }
    const enabled = model.enabled === undefined ? undefined : model.enabled && (protocols?.length ?? available.length) > 0;
    const preferredChoices = enabled ? (protocols ?? available) : available;
    const preferred = model.preferred && preferredChoices.includes(model.preferred)
      ? model.preferred : preferredChoices[0];
    return {
      publicModel,
      ...(enabled === undefined ? {} : { enabled }),
      ...(protocols ? { protocols } : {}),
      ...(model.preferred || model.protocols ? { preferred } : {}),
      upstreamModel,
      upstreamOverride: model.upstream_override
        ? {
          protocol: model.upstream_override.protocol,
          endpointUrl: model.upstream_override.endpoint_url.trim(),
        }
        : null,
    };
  });
  if (models.length === 0) throw new DestinationEditError("missing_mappings");
  const extraRoutes = protocolRoutes.length > 1;
  const sendRoutes = extraRoutes || destinationHasExplicitRoutes(destination);
  return {
    ...(draft.enabled === undefined ? {} : { enabled: draft.enabled }),
    authScheme: first.auth_scheme,
    endpointUrl: first.endpoint_url,
    models,
    name,
    upstreamProtocol: first.protocol,
    ...(sendRoutes
      ? {
        protocolRoutes: protocolRoutes.map((route) => ({
          protocol: route.protocol,
          endpointUrl: route.endpoint_url,
          authScheme: route.auth_scheme,
        })),
      }
      : {}),
  };
}

function originOf(value: string | null): string | null {
  if (!value) return null;
  try {
    return new URL(value.trim()).origin;
  } catch {
    return null;
  }
}

/** Every upstream origin the PATCH would route Key material towards. */
export function destinationPatchOrigins(input: DestinationPatchInput): string[] {
  const origins = new Set<string>();
  const add = (value: string | null | undefined) => {
    const origin = originOf(value ?? null);
    if (origin) origins.add(origin);
  };
  add(input.endpointUrl);
  for (const route of input.protocolRoutes ?? []) add(route.endpointUrl);
  for (const model of input.models) {
    add(model.upstreamOverride?.endpointUrl ?? null);
  }
  return [...origins];
}

interface DestinationRoute {
  protocol: ProtocolDto;
  url: string;
}

function normalizedRouteUrl(value: string | null): string | null {
  if (!value) return null;
  try {
    return new URL(value.trim()).toString();
  } catch {
    return null;
  }
}

function routeKey(route: DestinationRoute): string {
  return `${route.protocol}\n${route.url}`;
}

/** Exact protocol+URL routes whose endpoint identities the PATCH will use. */
export function destinationPatchRoutes(input: DestinationPatchInput): DestinationRoute[] {
  const routes: DestinationRoute[] = [];
  const seen = new Set<string>();
  const add = (protocol: ProtocolDto, value: string | null) => {
    const url = normalizedRouteUrl(value);
    if (!url) return;
    const route = { protocol, url };
    const key = routeKey(route);
    if (seen.has(key)) return;
    seen.add(key);
    routes.push(route);
  };
  if (input.protocolRoutes && input.protocolRoutes.length > 0) {
    for (const route of input.protocolRoutes) add(route.protocol, route.endpointUrl);
  } else {
    add(input.upstreamProtocol, input.endpointUrl);
  }
  for (const model of input.models) {
    if (model.upstreamOverride) {
      add(model.upstreamOverride.protocol, model.upstreamOverride.endpointUrl);
    }
  }
  return routes;
}

function destinationCurrentRoutes(destination: Destination): DestinationRoute[] {
  const protocol = destination.protocols[0];
  const firstUrl = destination.protocol_routes?.[0]?.endpoint_url ?? destination.base_url;
  if (!protocol && (destination.protocol_routes?.length ?? 0) === 0) return [];
  if (!firstUrl && (destination.protocol_routes?.length ?? 0) === 0) return [];
  return destinationPatchRoutes({
    authScheme: destination.auth_scheme,
    endpointUrl: destination.base_url ?? firstUrl ?? "",
    name: destination.name,
    upstreamProtocol: protocol ?? destination.protocol_routes?.[0]?.protocol ?? "chat_completions",
    protocolRoutes: destination.protocol_routes?.map((route) => ({
      protocol: route.protocol,
      endpointUrl: route.endpoint_url,
      authScheme: route.auth_scheme,
    })),
    models: destination.catalog.map((model) => ({
      publicModel: model.public_model,
      upstreamModel: model.upstream_model,
      upstreamOverride: model.upstream_override
        ? {
          protocol: model.upstream_override.protocol,
          endpointUrl: model.upstream_override.endpoint_url,
        }
        : null,
    })),
  });
}

/**
 * A route/origin change is any edit that moves traffic or credentials to a
 * exact protocol+URL endpoint the current configuration did not expose.
 * Endpoint ids distinguish paths even on the same origin, so comparing only
 * origins would save a route that existing Keys cannot use.
 */
export function destinationRouteChanged(
  destination: Destination,
  input: DestinationPatchInput,
): boolean {
  const current = new Set(destinationCurrentRoutes(destination).map(routeKey));
  const next = new Set(destinationPatchRoutes(input).map(routeKey));
  if (current.size !== next.size) return true;
  for (const origin of next) {
    if (!current.has(origin)) return true;
  }
  return false;
}

export interface DestinationGrantCandidate {
  id: string;
  name: string;
  enabled: boolean;
  /**
   * False when the Key's persisted grants do not cover every origin the PATCH
   * routes towards — these are the affected Keys the user must decide on.
   */
  covered: boolean;
}

/**
 * The Keys on this destination with their grant coverage against the PATCH.
 * Nothing here is auto-authorized: the view renders this list, the user
 * selects ids, and only the selection lands in `authorizeCredentialIds`.
 */
export function destinationGrantCandidates(
  destination: Destination,
  credentials: readonly DestinationCredential[],
  input: DestinationPatchInput,
  endpoints: readonly ConnectionEndpoint[] = [],
): DestinationGrantCandidate[] {
  const origins = destinationPatchOrigins(input);
  const routes = destinationPatchRoutes(input);
  return credentials
    .filter((credential) => credential.destination_id === destination.id)
    .map((credential) => ({
      id: credential.id,
      name: credential.name,
      enabled: credential.enabled,
      covered: origins.every((origin) => credential.grants.allowed_origins.includes(origin))
        && routes.every((route) => {
          const endpoint = endpoints.find((candidate) => (
            candidate.wire_protocol === route.protocol
              && normalizedRouteUrl(candidate.url) === route.url
          ));
          return Boolean(endpoint && credential.grants.allowed_endpoint_ids.includes(endpoint.id));
        }),
    }));
}

/** Attach the explicit grant consent; an empty selection is omitted from the digest. */
export function withAuthorizedCredentials(
  input: DestinationPatchInput,
  authorizeCredentialIds: readonly string[],
): DestinationPatchInput {
  return authorizeCredentialIds.length === 0
    ? input
    : { ...input, authorizeCredentialIds: [...authorizeCredentialIds] };
}

function isProtocol(value: string): value is ProtocolDto {
  return value === "chat_completions" || value === "responses" || value === "messages";
}

export function unusedDraftProtocol(draft: DestinationEditDraft): ProtocolDto | null {
  const used = new Set(draft.protocol_routes.map((route) => route.protocol));
  return (["chat_completions", "responses", "messages"] as const).find((protocol) => !used.has(protocol)) ?? null;
}

export function addDraftProtocolRoute(draft: DestinationEditDraft): boolean {
  if (draft.protocol_routes.length >= MAX_HTTP_PROTOCOL_ROUTES) return false;
  const protocol = unusedDraftProtocol(draft);
  if (!protocol) return false;
  const first = draft.protocol_routes[0];
  draft.protocol_routes.push({
    protocol,
    endpoint_url: "",
    auth_scheme: first?.auth_scheme ?? "bearer",
  });
  return true;
}

export function removeDraftProtocolRoute(draft: DestinationEditDraft, index: number): boolean {
  if (index <= 0 || index >= draft.protocol_routes.length) return false;
  draft.protocol_routes.splice(index, 1);
  return true;
}

/**
 * Fill draft routes from an official preset's declared protocolRoutes.
 * Does not persist; Save still has to authorize grants.
 */
export function applyPresetProtocolRoutesToDraft(
  draft: DestinationEditDraft,
  preset: Pick<ProviderPreset, "protocolRoutes"> & Partial<Pick<ProviderPreset, "id" | "endpointUrl">>,
): boolean {
  const routes = preset.endpointUrl === "" && preset.id
    ? providerPresetRoutesForEndpoint({
      id: preset.id,
      endpointUrl: preset.endpointUrl,
      protocolRoutes: preset.protocolRoutes,
    }, draft.protocol_routes[0]?.endpoint_url ?? "")
    : preset.protocolRoutes;
  if (!routes || routes.length === 0) return false;
  draft.protocol_routes = routes.slice(0, MAX_HTTP_PROTOCOL_ROUTES).map((route) => ({
    protocol: route.protocol,
    endpoint_url: route.endpointUrl,
    auth_scheme: route.authScheme,
  }));
  const first = draft.protocol_routes[0];
  return Boolean(first && isProtocol(first.protocol));
}
