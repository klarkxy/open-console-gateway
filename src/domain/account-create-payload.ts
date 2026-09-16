import type { AccountInput } from "../api/dashboard.ts";
import type { PlanDefinition } from "./plans.ts";
import {
  CustomCapabilityError,
  customEndpointUrlIssue,
  isCustomProtocol,
  normalizeCustomCapabilities,
} from "./custom-account.ts";

export type UpstreamProtocol = "chat_completions" | "responses" | "messages";

/** The form declares public-to-upstream model mappings; protocol is account-wide. */
export interface AccountCreateCapability {
  public_model: string;
  upstream_model: string;
}

export interface AccountCreateFormValues {
  name: string;
  username?: string;
  key: string;
  purchase_date?: string;
  notes?: string;
  endpoint_url?: string;
  upstream_protocol?: UpstreamProtocol;
  model_capabilities?: AccountCreateCapability[];
}

export type AccountCreatePayloadErrorCode =
  | "missing_name"
  | "missing_key"
  | "missing_endpoint_url"
  | "invalid_endpoint_url"
  | "endpoint_url_not_http"
  | "endpoint_url_with_credentials"
  | "missing_upstream_protocol"
  | "missing_model_capabilities"
  | "duplicate_public_model"
  | "public_model_too_long"
  | "public_model_has_control_character"
  | "upstream_model_too_long"
  | "upstream_model_has_control_character"
  | "capability_protocol_mismatch"
  | "custom_fields_not_allowed";

const ACCOUNT_CREATE_PAYLOAD_ERROR_KEYS = {
  missing_name: "名称不能为空",
  missing_key: "填写 API Key",
  missing_endpoint_url: "填写 API 地址",
  invalid_endpoint_url: "Endpoint 格式无效",
  endpoint_url_not_http: "Endpoint 必须是 http:// 或 https:// URL",
  endpoint_url_with_credentials: "Endpoint 不能包含用户名或密码",
  missing_upstream_protocol: "选择上游协议",
  missing_model_capabilities: "至少添加一个模型能力",
  duplicate_public_model: "对外模型名不能重复",
  public_model_too_long: "对外模型名最多 200 个字符",
  public_model_has_control_character: "对外模型名不能包含控制字符",
  upstream_model_too_long: "上游模型 ID 最多 200 个字符",
  upstream_model_has_control_character: "上游模型 ID 不能包含控制字符",
  capability_protocol_mismatch: "模型能力必须使用所选上游协议",
  custom_fields_not_allowed: "账号创建失败，请重试",
} as const satisfies Record<AccountCreatePayloadErrorCode, string>;

export class AccountCreatePayloadError extends Error {
  readonly code: AccountCreatePayloadErrorCode;

  constructor(code: AccountCreatePayloadErrorCode) {
    super(code);
    this.name = "AccountCreatePayloadError";
    this.code = code;
  }
}

export function accountCreatePayloadErrorKey(error: unknown) {
  return error instanceof AccountCreatePayloadError
    ? ACCOUNT_CREATE_PAYLOAD_ERROR_KEYS[error.code]
    : "账号创建失败，请重试";
}

function trimOptional(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : undefined;
}

/** Build the exact V3 create payload from the chosen plan and form values. */
export function buildCreateAccountPayload(
  plan: PlanDefinition,
  values: AccountCreateFormValues,
): AccountInput {
  if (!values.name.trim()) throw new AccountCreatePayloadError("missing_name");
  const isDynamic = plan.dynamic;
  const requiresKey = plan.credential_kind !== "none";
  if (requiresKey && !values.key.trim()) throw new AccountCreatePayloadError("missing_key");

  const isCustom = plan.kind === "custom";
  const payload: AccountInput = {
    name: values.name.trim(),
    provider_id: plan.provider_id,
    key: requiresKey ? values.key.trim() : "",
  };

  const username = trimOptional(values.username);
  if (username) payload.username = username;
  const purchaseDate = trimOptional(values.purchase_date);
  if (purchaseDate) payload.purchase_date = purchaseDate;
  const notes = trimOptional(values.notes);
  if (notes) payload.notes = notes;

  if (isCustom) {
    if (!values.endpoint_url?.trim()) throw new AccountCreatePayloadError("missing_endpoint_url");
    const endpointIssue = customEndpointUrlIssue(values.endpoint_url);
    if (endpointIssue === "malformed") throw new AccountCreatePayloadError("invalid_endpoint_url");
    if (endpointIssue === "not_http") throw new AccountCreatePayloadError("endpoint_url_not_http");
    if (endpointIssue === "with_credentials") throw new AccountCreatePayloadError("endpoint_url_with_credentials");
    if (!isCustomProtocol(values.upstream_protocol)) {
      throw new AccountCreatePayloadError("missing_upstream_protocol");
    }
    if (!values.model_capabilities || values.model_capabilities.length === 0) {
      throw new AccountCreatePayloadError("missing_model_capabilities");
    }
    payload.custom_config = {
      endpoint_url: values.endpoint_url.trim(),
      upstream_protocol: values.upstream_protocol,
    };
    try {
      payload.model_capabilities = normalizeCustomCapabilities(
        values.model_capabilities.map((capability) => ({
          public_model: capability.public_model,
          upstream_model: capability.upstream_model,
          protocol: values.upstream_protocol!,
        })),
        values.upstream_protocol,
      );
    } catch (error) {
      if (error instanceof CustomCapabilityError) {
        const code = ({
          missing: "missing_model_capabilities",
          duplicate_public_model: "duplicate_public_model",
          public_model_too_long: "public_model_too_long",
          public_model_has_control_character: "public_model_has_control_character",
          upstream_model_too_long: "upstream_model_too_long",
          upstream_model_has_control_character: "upstream_model_has_control_character",
          protocol_mismatch: "capability_protocol_mismatch",
        } as const)[error.issue];
        throw new AccountCreatePayloadError(code);
      }
      throw error;
    }
  } else if (
    isDynamic
    && (
      values.endpoint_url?.trim()
      || values.upstream_protocol
      || values.model_capabilities?.length
    )
  ) {
    throw new AccountCreatePayloadError("custom_fields_not_allowed");
  } else if (
    !isDynamic
    && (
      values.endpoint_url?.trim()
      || values.upstream_protocol
      || values.model_capabilities?.length
    )
  ) {
    throw new AccountCreatePayloadError("custom_fields_not_allowed");
  }

  return payload;
}

/**
 * Create-form emit → V3 create body. Empty or omitted Key becomes "".
 * Shared by Accounts and the Providers "Add Key" path.
 */
export function accountCreateRequestInput(payload: AccountInput): AccountInput {
  return {
    ...payload,
    key: payload.key || "",
  };
}
