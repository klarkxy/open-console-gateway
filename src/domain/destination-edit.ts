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

/**
 * Edit planning for configurable HTTP destinations (dynamic providers and
 * legacy Custom API connections). The destination PATCH is a full replacement
 * of the editable configuration; Keys stay account-owned and are only touched
 * through the explicit `authorizeCredentialIds` grant consent.
 */

export interface DestinationModelDraft {
  public_model: string;
  upstream_model: string;
  /** Null inherits the connection endpoint/protocol. */
  upstream_override: { protocol: ProtocolDto; endpoint_url: string } | null;
}

export interface DestinationEditDraft {
  name: string;
  endpoint_url: string;
  auth_scheme: AuthSchemeDto;
  upstream_protocol: ProtocolDto | "";
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
  | "override_endpoint_with_credentials";

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
  destination: Pick<Destination, "adapter" | "legacy">,
): boolean {
  return destination.adapter === "http"
    && (destination.legacy.kind === "dynamic" || destination.legacy.kind === "custom_account");
}

/** The server refuses a delete while any Key still routes through the destination. */
export function isDestinationDeletable(
  destination: Pick<Destination, "adapter" | "legacy" | "id">,
  credentials: readonly Pick<DestinationCredential, "destination_id">[],
): boolean {
  return isDestinationEditable(destination)
    && !credentials.some((credential) => credential.destination_id === destination.id);
}

export function destinationEditDraft(destination: Destination): DestinationEditDraft {
  return {
    name: destination.name,
    endpoint_url: destination.base_url ?? "",
    auth_scheme: destination.auth_scheme,
    upstream_protocol: destination.protocols[0] ?? "",
    models: destination.catalog.map((model) => ({
      public_model: model.public_model,
      upstream_model: model.upstream_model,
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

/** Validate the draft and build the full-replacement PATCH body (sans CAS pair). */
export function buildDestinationPatch(
  destination: Destination,
  draft: DestinationEditDraft,
): DestinationPatchInput {
  if (!isDestinationEditable(destination)) throw new DestinationEditError("immutable_destination");
  const name = draft.name.trim();
  if (!name) throw new DestinationEditError("missing_name");
  const endpointUrl = draft.endpoint_url.trim();
  const endpointProblem = endpointIssue(endpointUrl);
  if (endpointProblem) throw new DestinationEditError(endpointProblem);
  if (draft.upstream_protocol !== "chat_completions"
    && draft.upstream_protocol !== "responses"
    && draft.upstream_protocol !== "messages") {
    throw new DestinationEditError("missing_protocol");
  }
  const seen = new Set<string>();
  const models = draft.models.map((model) => {
    const publicModel = model.public_model.trim();
    const upstreamModel = model.upstream_model.trim();
    if (!publicModel && !upstreamModel) throw new DestinationEditError("missing_public_model");
    if (!publicModel) throw new DestinationEditError("missing_public_model");
    if (!upstreamModel) throw new DestinationEditError("missing_upstream_model");
    const overrideProblem = overrideIssue(model.upstream_override);
    if (overrideProblem) throw new DestinationEditError(overrideProblem);
    const key = publicModel.toLocaleLowerCase();
    if (seen.has(key)) throw new DestinationEditError("duplicate_public_model");
    seen.add(key);
    return {
      publicModel,
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
  return {
    authScheme: draft.auth_scheme,
    endpointUrl,
    models,
    name,
    upstreamProtocol: draft.upstream_protocol,
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
  const base = originOf(input.endpointUrl);
  if (base) origins.add(base);
  for (const model of input.models) {
    const override = originOf(model.upstreamOverride?.endpointUrl ?? null);
    if (override) origins.add(override);
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
  add(input.upstreamProtocol, input.endpointUrl);
  for (const model of input.models) {
    if (model.upstreamOverride) {
      add(model.upstreamOverride.protocol, model.upstreamOverride.endpointUrl);
    }
  }
  return routes;
}

function destinationCurrentRoutes(destination: Destination): DestinationRoute[] {
  const protocol = destination.protocols[0];
  if (!protocol || !destination.base_url) return [];
  return destinationPatchRoutes({
    authScheme: destination.auth_scheme,
    endpointUrl: destination.base_url,
    name: destination.name,
    upstreamProtocol: protocol,
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
