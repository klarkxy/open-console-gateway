import type {
  Account as V3Account,
  AccountCreate as V3AccountCreate,
  AccountModelTestResponse as V3AccountModelTestResponse,
  AccountUpdate as V3AccountUpdate,
  BrowserCapabilities as V3BrowserCapabilities,
  BrowserOpen as V3BrowserOpen,
  ConnectionInfo as V3ConnectionInfo,
  DailyModelTokens as V3DailyModelTokens,
  DashboardSummary as V3DashboardSummary,
  DesktopUpdate as V3DesktopUpdate,
  ForwardLog as V3ForwardLog,
  ForwardLogs as V3ForwardLogs,
  GatewayLog as V3GatewayLog,
  PricingSnapshot as V3PricingSnapshot,
  ProviderPricingRefresh as V3ProviderPricingRefresh,
  ProxyTestResponse as V3ProxyTestResponse,
  Settings as V3Settings,
  SettingsUpdate as V3SettingsUpdate,
  UpdateCheck as V3UpdateCheck,
  UsageRefresh as V3UsageRefresh,
  UsageWindow as V3UsageWindow,
} from "./generated/dashboard-v3.ts";

export type AccountCredentialKind = "api_key" | "none";
export type AccountQuotaScope = "key" | "egress-ip";
export type AccountType = "key" | "managed";
export type AccountSetupStep =
  | "google_account"
  | "opencode_registration"
  | "payment"
  | "key_verification"
  | "ready";
export type AccountProtocol = "chat_completions" | "responses" | "messages";
export type AccountModelTestResponse = V3AccountModelTestResponse;

export interface AccountCustomConfig {
  account_id: string;
  endpoint_url: string;
  upstream_protocol: AccountProtocol;
  created_at: string;
  updated_at: string;
}

export interface AccountModelCapability {
  public_model: string;
  protocol: AccountProtocol;
  verified_at: string | null;
  source: string;
  upstream_model: string;
}

/** Explicit UI projection. Write-only credentials are always empty strings. */
export interface Account {
  id: string;
  name: string;
  username: string;
  password: string;
  key: string;
  enabled: boolean;
  account_type: AccountType;
  setup_step: AccountSetupStep;
  provider_id: string;
  credential_kind: AccountCredentialKind;
  quota_scope: AccountQuotaScope;
  revision?: number;
  purchase_date: string;
  expires_on: string;
  cooldown_until: string | null;
  cooldown_generic_until: string | null;
  cooldown_5h_until: string | null;
  cooldown_week_until: string | null;
  cooldown_month_until: string | null;
  cooldown_free_until: string | null;
  last_error: string | null;
  auth_error: string | null;
  notes: string;
  usage_sync_last_success_at: string | null;
  usage_sync_next_allowed_at: string | null;
  created_at: string;
  updated_at: string;
  verification_status: "not_required" | "pending" | "verified" | "failed";
  connection_verified_at: string | null;
  verification_error: string | null;
  plan_routable: boolean;
  custom_config?: AccountCustomConfig | null;
  model_capabilities: AccountModelCapability[];
  ollama_billing_tier?: "pro" | "max" | "team" | null;
}

export interface AccountCustomConfigInput {
  endpoint_url: string;
  upstream_protocol: AccountProtocol;
}

export interface AccountCustomConfigUpdateInput extends AccountCustomConfigInput {
  model_capabilities: AccountModelCapabilityInput[];
}

export interface AccountModelCapabilityInput {
  public_model: string;
  protocol: AccountProtocol;
  source?: string;
  upstream_model: string;
}

export interface AccountInput {
  name: string;
  username?: string;
  password?: string;
  key: string;
  provider_id?: string;
  purchase_date?: string;
  notes?: string;
  custom_config?: AccountCustomConfigInput;
  model_capabilities?: AccountModelCapabilityInput[];
  ollama_billing_tier?: "pro" | "max" | "team";
}

export interface AccountUpdate {
  name?: string;
  username?: string;
  password?: string;
  key?: string;
  enabled?: boolean;
  purchase_date?: string;
  notes?: string;
  ollama_billing_tier?: "pro" | "max" | "team";
}

export interface ManagedAccountInput {
  name: string;
  username?: string;
  notes?: string;
}

export interface CustomModelDiscoveryInput {
  endpoint_url: string;
  upstream_protocol: AccountProtocol;
  api_key?: string;
  account_id?: string;
}

export interface CustomModelDiscoveryResult {
  models: string[];
  truncated: boolean;
}

export type RoutingMode = "strict-priority" | "sticky-global" | "round-robin";
export type ProxyMode = "auto" | "manual" | "direct" | "list";
export type ProxyListDirection = "whitelist" | "blacklist";

export interface ProxySupportedModel {
  id: string;
  preferred_protocol: string;
  zen_free: boolean;
}

export interface AppConfig {
  revision: number;
  gateway_port: number;
  gateway_port_from_env: boolean;
  proxy_mode: ProxyMode;
  proxy_url: string;
  proxy_list_direction: ProxyListDirection;
  proxy_list_models: string[];
  proxy_supported_models: ProxySupportedModel[];
  opencode_invite_url: string;
  client_root_url: string;
  client_root_url_from_env: boolean;
  auto_start: boolean;
  auto_start_supported: boolean;
  show_dock_icon: boolean;
  dock_visibility_supported: boolean;
  connect_timeout_secs: number;
  non_stream_timeout_secs: number;
  stream_idle_timeout_secs: number;
  routing_mode: RoutingMode;
  conversation_sticky: boolean;
}

export interface ConnectionSubKey {
  id: string;
  name: string;
  enabled: boolean;
  value: string;
}

export interface ConnectionInfo {
  gateway_port: number;
  client_root_url: string;
  primary_key: string;
  sub_keys: ConnectionSubKey[];
  revision: number;
}

export type BrowserMode = "native" | "remote" | "unsupported";
export type BrowserTarget =
  | "google_signup"
  | "google_login"
  | "github_signup"
  | "github_login"
  | "invite"
  | "console";

export interface BrowserCapabilities {
  mode: BrowserMode;
  reason: string | null;
}

export interface BrowserLaunchResult {
  mode: BrowserMode;
  session_token: string | null;
}

export interface UpdateCheckResult {
  current_version: string;
  latest_version: string;
  update_available: boolean;
  install_supported: boolean;
  release_url: string;
}

export interface ProxyTestResult {
  proxy_mode: ProxyMode;
  status: number;
  latency_ms: number;
}

export type UpdatePhase = "idle" | "checking" | "downloading" | "installing" | "failed";

export interface UpdateStatus {
  phase: UpdatePhase;
  downloaded: number;
  total: number | null;
  error: string | null;
  current_version: string;
  install_supported: boolean;
}

export interface GatewayLog {
  id: number;
  level: string;
  category: string;
  message: string;
  created_at: string;
  request_id: string | null;
  attempt: number | null;
  error_source: string | null;
  error_stage: string | null;
  duration_ms: number | null;
  diagnostic: any;
}

export interface ForwardLog {
  id: number;
  timestamp: string;
  model: string;
  requested_model: string | null;
  resolved_alias: string | null;
  upstream_model: string | null;
  account_id: string;
  account_name: string;
  client_key_id: string | null;
  client_key_name: string | null;
  route_account_id: string | null;
  provider_id: string | null;
  credential_account_id: string | null;
  raw_cost_usd: number | null;
  quota_debit: number | null;
  effective_paid_cost_usd: number | null;
  native_cost_value: number | null;
  native_cost_unit: string | null;
  native_cost_currency: string | null;
  status: string;
  http_status: number | null;
  route: string;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cache_creation_tokens: number;
  cost: number | null;
  cost_state: string;
  pricing_revision_id: string | null;
  quota_multiplier: number | null;
  local_adjustment_multiplier: number | null;
  service_tier: string | null;
  error_message: string | null;
  request_id: string | null;
  attempt: number | null;
  error_source: string | null;
  error_stage: string | null;
  duration_ms: number | null;
  diagnostic: any;
}

export interface ForwardLogSummary {
  total_requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cost: number;
}

export interface ForwardLogPage {
  items: ForwardLog[];
  summary: ForwardLogSummary;
}

export interface ForwardLogQuery {
  limit?: number;
  offset?: number;
  status?: string | null;
  account_id?: string | null;
  model?: string | null;
  request_id?: string | null;
  key_id?: string | null;
  provider_id?: string | null;
  route_account_id?: string | null;
  credential_account_id?: string | null;
  start_time?: string | null;
  end_time?: string | null;
  sort_by?: string | null;
  sort_order?: string | null;
}

export interface ForwardLogClientKey {
  id: string;
  name: string;
}

export interface UsageWindow {
  account_id: string;
  window_5h: number;
  window_week: number;
  window_month: number;
  resets_in_5h: string | null;
  resets_in_week: string | null;
  resets_in_month: string | null;
}

export interface OfficialUsageRefreshResult {
  usage: UsageWindow;
  source: string;
  last_success_at: string;
  next_allowed_at: string;
}

export interface PricingLimits {
  window_5h: number;
  window_week: number;
  window_month: number;
}

export interface PricingAdjustment {
  label: string;
  multiplier: number;
  applies_to: string;
}

export interface PricingModel {
  model_id: string;
  display_name: string;
  input: number;
  output: number;
  cache_read: number | null;
  cache_write: number | null;
  usage: number;
  quota_multiplier: number;
  min_input_tokens?: number | null;
  max_input_tokens?: number | null;
  time_window?: "always" | "off_peak" | "peak" | null;
  adjustments: PricingAdjustment[];
}

export interface PricingSnapshot {
  revision: string;
  control_revision?: number;
  activated_at: string;
  document_updated_at: string | null;
  source_url: string;
  content_hash: string;
  adjustment_policy_version: string;
  limits: PricingLimits;
  models: PricingModel[];
}

export interface PricingMultiplierChange {
  model_id: string;
  current_multiplier: number;
  official_multiplier: number;
}

export interface ProviderPricingRefreshResult {
  provider_id: string;
  refresh_status: "success" | "unchanged" | "needs_confirmation" | "failed_no_change";
  multiplier_changes: PricingMultiplierChange[];
  official_content_hash: string | null;
  error: string | null;
  revision: number;
  process_generation: number;
  pricing_revision: string;
  provider_pricing_revision: string;
}

export interface ProviderPricingRefreshRequest {
  policy?: "keep_current" | "use_official";
  expected_provider_revision?: string;
  expected_official_content_hash?: string;
}

export interface PricingMultiplierUpdate {
  model_id: string;
  multiplier: number;
}

export interface DashboardSummary {
  total_accounts: number;
  available_accounts: number;
  today_cost: number;
  week_cost: number;
  month_cost: number;
  gateway_running: boolean;
}

export interface DailyModelTokens {
  date: string;
  model: string;
  tokens: number;
}

export function presentAccount(value: V3Account): Account {
  return {
    id: value.id,
    name: value.name,
    username: value.username ?? "",
    password: "",
    key: "",
    enabled: value.enabled,
    account_type: value.accountType,
    setup_step: value.setupStep,
    provider_id: value.providerId,
    credential_kind: value.credentialKind,
    quota_scope: value.quotaScope,
    revision: value.revision,
    purchase_date: value.purchaseDate,
    expires_on: value.expiresOn,
    cooldown_until: value.cooldownUntil,
    cooldown_generic_until: value.cooldownGenericUntil,
    cooldown_5h_until: value.cooldown5hUntil,
    cooldown_week_until: value.cooldownWeekUntil,
    cooldown_month_until: value.cooldownMonthUntil,
    cooldown_free_until: value.cooldownFreeUntil,
    last_error: value.lastError,
    auth_error: value.authError,
    notes: value.notes ?? "",
    usage_sync_last_success_at: value.usageSyncLastSuccessAt,
    usage_sync_next_allowed_at: value.usageSyncNextAllowedAt,
    created_at: value.createdAt,
    updated_at: value.updatedAt,
    verification_status: value.verificationStatus,
    connection_verified_at: value.connectionVerifiedAt,
    verification_error: value.verificationError,
    plan_routable: value.planRoutable,
    custom_config: value.customConfig === null ? null : {
      account_id: value.customConfig.accountId,
      endpoint_url: value.customConfig.endpointUrl,
      upstream_protocol: value.customConfig.upstreamProtocol,
      created_at: value.customConfig.createdAt,
      updated_at: value.customConfig.updatedAt,
    },
    model_capabilities: value.modelCapabilities.map((capability) => ({
      public_model: capability.publicModel,
      protocol: capability.protocol,
      verified_at: capability.verifiedAt,
      source: capability.source,
      upstream_model: capability.upstreamModel,
    })),
    ollama_billing_tier: value.ollamaBillingTier ?? null,
  };
}

export function accountCreateInput(value: AccountInput): Omit<V3AccountCreate, "expectedRevision" | "processGeneration"> {
  return {
    name: value.name,
    key: value.key,
    username: value.username,
    password: value.password,
    providerId: value.provider_id,
    purchaseDate: value.purchase_date,
    notes: value.notes,
    customConfig: value.custom_config ? {
      endpointUrl: value.custom_config.endpoint_url,
      upstreamProtocol: value.custom_config.upstream_protocol,
    } : undefined,
    modelCapabilities: value.model_capabilities?.map((capability) => ({
      publicModel: capability.public_model,
      protocol: capability.protocol,
      source: capability.source,
      upstreamModel: capability.upstream_model,
    })),
    ollamaBillingTier: value.ollama_billing_tier,
  };
}

export function accountUpdateInput(value: AccountUpdate): Omit<V3AccountUpdate, "expectedRevision" | "processGeneration"> {
  return {
    name: value.name,
    username: value.username,
    password: value.password,
    key: value.key,
    enabled: value.enabled,
    purchaseDate: value.purchase_date,
    notes: value.notes,
    ollamaBillingTier: value.ollama_billing_tier,
  };
}

export function presentSettings(value: V3Settings): AppConfig {
  return {
    revision: value.revision,
    gateway_port: value.gatewayPort,
    gateway_port_from_env: value.gatewayPortFromEnv,
    proxy_mode: value.proxyMode,
    proxy_url: value.proxyUrl,
    proxy_list_direction: value.proxyListDirection,
    proxy_list_models: [...value.proxyListModels],
    proxy_supported_models: value.proxySupportedModels.map((model) => ({
      id: model.id,
      preferred_protocol: model.preferredProtocol,
      zen_free: model.zenFree,
    })),
    opencode_invite_url: value.opencodeInviteUrl,
    client_root_url: value.clientRootUrl,
    client_root_url_from_env: value.clientRootUrlFromEnv,
    auto_start: value.autoStart ?? false,
    auto_start_supported: value.autoStartSupported,
    show_dock_icon: value.showDockIcon ?? false,
    dock_visibility_supported: value.dockVisibilitySupported,
    connect_timeout_secs: value.connectTimeoutSecs,
    non_stream_timeout_secs: value.nonStreamTimeoutSecs,
    stream_idle_timeout_secs: value.streamIdleTimeoutSecs,
    routing_mode: value.routingMode,
    conversation_sticky: value.conversationSticky,
  };
}

export function settingsUpdateInput(value: AppConfig): Omit<V3SettingsUpdate, "expectedRevision" | "processGeneration"> {
  const input: Omit<V3SettingsUpdate, "expectedRevision" | "processGeneration"> = {
    clientRootUrl: value.client_root_url,
    connectTimeoutSecs: value.connect_timeout_secs,
    conversationSticky: value.conversation_sticky,
    nonStreamTimeoutSecs: value.non_stream_timeout_secs,
    opencodeInviteUrl: value.opencode_invite_url,
    proxyListDirection: value.proxy_list_direction,
    proxyListModels: value.proxy_list_models,
    proxyMode: value.proxy_mode,
    proxyUrl: value.proxy_url,
    routingMode: value.routing_mode,
    streamIdleTimeoutSecs: value.stream_idle_timeout_secs,
  };
  if (value.auto_start_supported) input.autoStart = value.auto_start;
  if (value.dock_visibility_supported) input.showDockIcon = value.show_dock_icon;
  if (!value.gateway_port_from_env) input.gatewayPort = value.gateway_port;
  return input;
}

export function presentConnection(value: V3ConnectionInfo): ConnectionInfo {
  return {
    gateway_port: value.gatewayPort,
    client_root_url: value.clientRootUrl,
    primary_key: value.primaryKey,
    sub_keys: value.subKeys.map((key) => ({ id: key.id, name: key.name, enabled: key.enabled, value: key.value })),
    revision: value.revision,
  };
}

export function presentUsage(value: V3UsageWindow): UsageWindow {
  return {
    account_id: value.accountId,
    window_5h: value.window5h,
    window_week: value.windowWeek,
    window_month: value.windowMonth,
    resets_in_5h: value.resetsIn5h,
    resets_in_week: value.resetsInWeek,
    resets_in_month: value.resetsInMonth,
  };
}

export function presentUsageRefresh(value: V3UsageRefresh): OfficialUsageRefreshResult {
  return {
    usage: presentUsage(value.usage),
    source: value.source,
    last_success_at: value.lastSuccessAt,
    next_allowed_at: value.nextAllowedAt,
  };
}

export function presentPricing(value: V3PricingSnapshot): PricingSnapshot {
  return {
    revision: value.pricingRevision,
    control_revision: value.revision,
    activated_at: value.activatedAt,
    document_updated_at: value.documentUpdatedAt,
    source_url: value.sourceUrl,
    content_hash: value.contentHash,
    adjustment_policy_version: value.adjustmentPolicyVersion,
    limits: {
      window_5h: value.limits.window5h,
      window_week: value.limits.windowWeek,
      window_month: value.limits.windowMonth,
    },
    models: value.models.map((model) => ({
      model_id: model.modelId,
      display_name: model.displayName,
      input: model.input,
      output: model.output,
      cache_read: model.cacheRead,
      cache_write: model.cacheWrite,
      usage: model.usage,
      quota_multiplier: model.quotaMultiplier,
      min_input_tokens: model.minInputTokens,
      max_input_tokens: model.maxInputTokens,
      time_window: model.timeWindow,
      adjustments: model.adjustments.map((adjustment) => ({
        label: adjustment.label,
        multiplier: adjustment.multiplier,
        applies_to: adjustment.appliesTo,
      })),
    })),
  };
}

export function presentProviderPricingRefresh(
  value: V3ProviderPricingRefresh,
): ProviderPricingRefreshResult {
  return {
    provider_id: value.providerId,
    refresh_status: value.refreshStatus,
    multiplier_changes: value.multiplierChanges.map((change) => ({
      model_id: change.modelId,
      current_multiplier: change.currentMultiplier,
      official_multiplier: change.officialMultiplier,
    })),
    official_content_hash: value.officialContentHash,
    error: value.error,
    revision: value.revision,
    process_generation: value.processGeneration,
    pricing_revision: value.pricingRevision,
    provider_pricing_revision: value.providerPricingRevision,
  };
}

export function presentDashboardSummary(value: V3DashboardSummary): DashboardSummary {
  return {
    total_accounts: value.totalAccounts,
    available_accounts: value.availableAccounts,
    today_cost: value.todayCost,
    week_cost: value.weekCost,
    month_cost: value.monthCost,
    gateway_running: value.gatewayRunning,
  };
}

export function presentDailyModelTokens(value: V3DailyModelTokens): DailyModelTokens {
  return { date: value.date, model: value.model, tokens: value.tokens };
}

export function presentGatewayLog(value: V3GatewayLog): GatewayLog {
  return {
    id: value.id,
    level: value.level,
    category: value.category,
    message: value.message,
    created_at: value.createdAt,
    request_id: value.requestId,
    attempt: value.attempt,
    error_source: value.errorSource,
    error_stage: value.errorStage,
    duration_ms: value.durationMs,
    diagnostic: value.diagnostic,
  };
}

export function presentForwardLog(value: V3ForwardLog): ForwardLog {
  return {
    id: value.id,
    timestamp: value.timestamp,
    model: value.model,
    requested_model: value.requestedModel,
    resolved_alias: value.resolvedAlias,
    upstream_model: value.upstreamModel,
    account_id: value.accountId,
    account_name: value.accountName,
    client_key_id: value.clientKeyId,
    client_key_name: value.clientKeyName,
    route_account_id: value.routeAccountId,
    provider_id: value.providerId,
    credential_account_id: value.credentialAccountId,
    raw_cost_usd: value.rawCostUsd,
    quota_debit: value.quotaDebit,
    effective_paid_cost_usd: value.effectivePaidCostUsd,
    native_cost_value: value.nativeCostValue,
    native_cost_unit: value.nativeCostUnit,
    native_cost_currency: value.nativeCostCurrency,
    status: value.status,
    http_status: value.httpStatus,
    route: value.route,
    prompt_tokens: value.promptTokens,
    completion_tokens: value.completionTokens,
    cached_tokens: value.cachedTokens,
    cache_creation_tokens: value.cacheCreationTokens,
    cost: value.cost,
    cost_state: value.costState,
    pricing_revision_id: value.pricingRevisionId,
    quota_multiplier: value.quotaMultiplier,
    local_adjustment_multiplier: value.localAdjustmentMultiplier,
    service_tier: value.serviceTier,
    error_message: value.errorMessage,
    request_id: value.requestId,
    attempt: value.attempt,
    error_source: value.errorSource,
    error_stage: value.errorStage,
    duration_ms: value.durationMs,
    diagnostic: value.diagnostic,
  };
}

export function presentForwardLogs(value: V3ForwardLogs): ForwardLogPage {
  return {
    items: value.items.map(presentForwardLog),
    summary: {
      total_requests: value.summary.totalRequests,
      prompt_tokens: value.summary.promptTokens,
      completion_tokens: value.summary.completionTokens,
      cached_tokens: value.summary.cachedTokens,
      cost: value.summary.cost,
    },
  };
}

export function presentBrowserCapabilities(value: V3BrowserCapabilities): BrowserCapabilities {
  return { mode: value.mode, reason: value.reason };
}

export function presentBrowserOpen(value: V3BrowserOpen): BrowserLaunchResult {
  return { mode: value.mode, session_token: value.sessionToken };
}

export function presentUpdateCheck(value: V3UpdateCheck): UpdateCheckResult {
  return {
    current_version: value.currentVersion,
    latest_version: value.latestVersion,
    update_available: value.updateAvailable,
    install_supported: value.installSupported,
    release_url: value.releaseUrl,
  };
}

export function presentUpdateStatus(value: V3DesktopUpdate): UpdateStatus {
  return {
    phase: value.phase,
    downloaded: value.downloaded,
    total: value.total,
    error: value.error,
    current_version: value.currentVersion,
    install_supported: value.installSupported,
  };
}

export function presentProxyTest(value: V3ProxyTestResponse): ProxyTestResult {
  return { proxy_mode: value.proxyMode, status: value.status, latency_ms: value.latencyMs };
}
