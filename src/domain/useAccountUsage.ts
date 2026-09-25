import { computed, getCurrentScope, nextTick, onScopeDispose, ref, watch } from "vue";
import type { ComputedRef, Ref } from "vue";
import { useMessage } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard.ts";
import type { Account, UsageWindow } from "../api/dashboard";
import type {
  ProviderCatalogEntry,
  ProviderQuotaWindow,
  ProviderUsageResponse,
} from "../api/providers.ts";
import { useBillingStore } from "../stores/billing.ts";
import {
  defaultResetsInMinutes,
  isUsageLimitReached,
  mergeUsageEdit,
  normalizeUsagePercent,
  resetsFieldsToMinutes,
  resetsFirstFieldValue,
  resetsInMinutesForSave,
  resetsSecondFieldValue,
  usagePercentFromCost,
  WINDOW_FULL_MINUTES,
  windowResetsAt,
} from "./accounts-usage.ts";
import type { UsageEditState, UsageKey } from "./accounts-usage.ts";
import { isLegacyGoFallbackPlan } from "./account-capabilities.ts";
import { accountIsReady } from "./account-display.ts";
import {
  BILLING_ERROR_KEYS,
  billingBinding,
  billingManualCalibration,
  presentedUsageOf,
  usageWindowFromProviderUsage,
} from "./billing.ts";
import { findPlanDefinition } from "./plans.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { mapWithConcurrency } from "../utils/async.ts";

export type AccountUsageEdits = Record<UsageKey, UsageEditState>;

export type UsageLimitView = { key: UsageKey; label: string; limit: number };

/**
 * Account-list usage editors. Server snapshots live in useBillingStore;
 * this composable keeps calibration drafts, messages, and focus, and
 * projects usageMap / providerUsageMap from BillingStatus.usage. The
 * whole-table computeds serve list-level consumers; row-level consumers
 * should subscribe to the per-account `*For(accountId)` selectors so one
 * account's update does not invalidate every row.
 */
export function useAccountUsage(
  accounts: Ref<Account[]>,
  now: Ref<number>,
  catalog: Ref<ProviderCatalogEntry[] | null>,
  options?: {
    message?: Pick<ReturnType<typeof useMessage>, "success" | "warning" | "error">;
    endpointUrlFor?: (account: Account) => string | null;
    officialBalanceFor?: (account: Account) => boolean;
    /** Runs after an attempted quota refresh (success or failure), while the button still spins. */
    afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>;
  },
) {
  const message = options?.message ?? useMessage();
  const billing = useBillingStore();
  let disposed = false;
  if (getCurrentScope()) onScopeDispose(() => { disposed = true; });

  const quotaLimits = computed(() => billing.pricingLimits);
  const quotaLimitsLoading = computed(() => billing.pricingLoading);
  const quotaLimitsError = computed(() => billing.pricingError);
  const usageLimits = computed<UsageLimitView[]>(() => {
    const limits = quotaLimits.value;
    if (!limits) return [];
    return [
      { key: "window_5h", label: t("5 小时"), limit: limits.window_5h },
      { key: "window_week", label: t("本周"), limit: limits.window_week },
      { key: "window_month", label: t("本月"), limit: limits.window_month },
    ];
  });

  const providerUsageMap = computed(() => {
    const out: Record<string, ProviderUsageResponse> = {};
    for (const [id, slot] of Object.entries(billing.byId)) {
      const presented = presentedUsageOf(slot.status);
      if (presented) out[id] = presented;
    }
    return out;
  });

  const usageMap = computed(() => {
    const out: Record<string, UsageWindow> = {};
    for (const [id, slot] of Object.entries(billing.byId)) {
      out[id] = usageWindowFromProviderUsage(slot.status?.usage ?? null, id);
    }
    return out;
  });

  const usageLoading = computed(() => {
    const out: Record<string, boolean> = {};
    for (const [id, slot] of Object.entries(billing.byId)) out[id] = slot.loading;
    return out;
  });

  const usageLoadErrors = computed(() => {
    const out: Record<string, string | null> = {};
    for (const [id, slot] of Object.entries(billing.byId)) {
      out[id] = slot.error ? t(BILLING_ERROR_KEYS[slot.error]) : null;
    }
    return out;
  });

  const usageRefreshLoading = computed(() => {
    const out: Record<string, boolean> = {};
    for (const [id, slot] of Object.entries(billing.byId)) out[id] = slot.mutating;
    return out;
  });

  // Row-level selectors: subscribing to one account's slot keeps an update
  // for account X from invalidating rows that only render account Y.
  function providerUsageFor(accountId: string): ComputedRef<ProviderUsageResponse | null> {
    return computed(() => presentedUsageOf(billing.slotFor(accountId).value?.status ?? null));
  }

  function usageFor(accountId: string): ComputedRef<UsageWindow> {
    return computed(() => usageWindowFromProviderUsage(
      billing.slotFor(accountId).value?.status?.usage ?? null,
      accountId,
    ));
  }

  function usageLoadingFor(accountId: string): ComputedRef<boolean> {
    return computed(() => billing.slotFor(accountId).value?.loading ?? false);
  }

  function usageLoadErrorFor(accountId: string): ComputedRef<string | null> {
    return computed(() => {
      const error = billing.slotFor(accountId).value?.error;
      return error ? t(BILLING_ERROR_KEYS[error]) : null;
    });
  }

  function usageRefreshLoadingFor(accountId: string): ComputedRef<boolean> {
    return computed(() => billing.slotFor(accountId).value?.mutating ?? false);
  }

  function usageLimitsFor(account: Account): UsageLimitView[] {
    const presented = providerUsageFor(account.id).value;
    if (presented?.quota_windows.length) return limitsFromProviderWindows(presented.quota_windows);
    const surface = findPlanDefinition(account.provider_id, catalog.value);
    const limits = surface && isLegacyGoFallbackPlan(surface, catalog.value)
      ? quotaLimits.value
      : null;
    if (!limits) return [];
    return [
      { key: "window_5h", label: t("5 小时"), limit: limits.window_5h },
      { key: "window_week", label: t("本周"), limit: limits.window_week },
      { key: "window_month", label: t("本月"), limit: limits.window_month },
    ];
  }

  function bindingFor(account: Account): string {
    return billingBinding(
      account.updated_at,
      options?.endpointUrlFor?.(account) ?? account.custom_config?.endpoint_url ?? null,
    );
  }

  function requestStillCurrent(account: Account): () => boolean {
    const session = billing.sessionEpoch;
    const binding = bindingFor(account);
    return () => {
      const current = accounts.value.find(({ id }) => id === account.id);
      return !disposed && session === billing.sessionEpoch
        && current !== undefined && bindingFor(current) === binding;
    };
  }

  function usageCapabilities(account: Account): {
    providerWindows: boolean;
    refresh: boolean;
    manual: boolean;
  } {
    const status = billing.slotFor(account.id).value?.status;
    if (status) {
      return {
        providerWindows: Boolean(status.usage) || status.model === "quota",
        refresh: status.officialRefresh,
        manual: billingManualCalibration(status),
      };
    }
    const surface = findPlanDefinition(account.provider_id, catalog.value);
    const manual = surface?.manual_usage_calibration === true;
    const refresh = surface?.usage_availability === "available";
    const creditBalance = options?.officialBalanceFor?.(account) === true;
    return {
      providerWindows: refresh || manual || creditBalance,
      refresh: refresh || creditBalance,
      manual,
    };
  }

  function limitsFromProviderWindows(windows: ProviderQuotaWindow[]): UsageLimitView[] {
    const byKind = new Map(windows.map((window) => [window.window_kind, window]));
    const definitions: Array<[UsageKey, string, string]> = [
      ["window_5h", "five_hours", t("5 小时")],
      ["window_week", "week", t("本周")],
      ["window_month", "month", t("本月")],
    ];
    return definitions.flatMap(([key, kind, label]) => {
      const limit = byKind.get(kind)?.limit_value;
      return typeof limit === "number" && Number.isFinite(limit) && limit > 0
        ? [{ key, label, limit }]
        : [];
    });
  }

  const usageEdits = ref<Record<string, AccountUsageEdits>>({});

  function getUsage(accountId: string): UsageWindow {
    return usageFor(accountId).value;
  }

  function usageLimit(accountId: string, key: UsageKey): number {
    const account = accounts.value.find(({ id }) => id === accountId);
    return account
      ? usageLimitsFor(account).find((limit) => limit.key === key)?.limit ?? 0
      : 0;
  }

  function accountUsageLimitReached(account: Account, key: UsageKey): boolean {
    return isUsageLimitReached(account, key, now.value);
  }

  function hasAvailableUsageEditor(account: Account): boolean {
    if (usageLoadingFor(account.id).value || usageLoadErrorFor(account.id).value) return false;
    return usageLimitsFor(account).some(({ key }) => !accountUsageLimitReached(account, key));
  }

  async function focusUsageEditor(accountId: string) {
    await nextTick();
    requestAnimationFrame(() => {
      const editor = Array.from(
        document.querySelectorAll<HTMLElement>(".usage-editor-popover"),
      ).find((element) => element.dataset.usageEditorAccountId === accountId);
      editor?.querySelector<HTMLInputElement>(".n-input-number input")?.focus();
    });
  }

  function usageEditsFromWindow(usage: UsageWindow): AccountUsageEdits {
    const account = accounts.value.find(({ id }) => id === usage.account_id);
    const limits = account ? usageLimitsFor(account) : [];
    return Object.fromEntries(limits.map(({ key, limit }) => {
      const percent = usagePercentFromCost(usage[key], limit);
      const resetsInMin = defaultResetsInMinutes(usage, key, now.value);
      return [key, {
        draft: percent,
        saved: percent,
        saving: false,
        error: null,
        resets_in_minutes_draft: resetsInMin,
        resets_at_saved: windowResetsAt(usage, key),
        resets_dirty: false,
      }];
    })) as AccountUsageEdits;
  }

  function syncUsageEdits(accountId: string, usage: UsageWindow) {
    const existing = usageEdits.value[accountId];
    if (!existing) {
      usageEdits.value[accountId] = usageEditsFromWindow(usage);
      return;
    }
    const account = accounts.value.find(({ id }) => id === accountId);
    const limits = account ? usageLimitsFor(account) : [];
    for (const { key, limit } of limits) {
      const saved = usagePercentFromCost(usage[key], limit);
      const edit = existing[key];
      const wasActuallyReset = account && isUsageLimitReached(account, key, now.value);
      if (!edit) {
        const created = mergeUsageEdit(undefined, saved, Boolean(wasActuallyReset));
        created.resets_in_minutes_draft = defaultResetsInMinutes(usage, key, now.value);
        created.resets_at_saved = windowResetsAt(usage, key);
        existing[key] = created;
        continue;
      }
      Object.assign(edit, mergeUsageEdit(edit, saved, Boolean(wasActuallyReset)));
      edit.resets_at_saved = windowResetsAt(usage, key);
      if (wasActuallyReset || (!edit.saving && !edit.resets_dirty)) {
        edit.resets_in_minutes_draft = defaultResetsInMinutes(usage, key, now.value);
        edit.resets_dirty = false;
      }
    }
  }

  function updateUsageDraft(accountId: string, key: UsageKey, value: number | null) {
    const edit = usageEdits.value[accountId]?.[key];
    if (!edit || edit.saving || value === null) return;
    edit.draft = normalizeUsagePercent(value);
  }

  function updateResetsFirstField(accountId: string, key: UsageKey, value: number | null) {
    const edit = usageEdits.value[accountId]?.[key];
    if (!edit || edit.saving) return;
    if (WINDOW_FULL_MINUTES[key] === null) return;
    const v = value === null ? 0 : Math.max(0, Math.round(value));
    const second = resetsSecondFieldValue(edit, key, now.value);
    const max = WINDOW_FULL_MINUTES[key] ?? 10080;
    edit.resets_in_minutes_draft = Math.min(max, resetsFieldsToMinutes(v, second, key));
    edit.resets_dirty = true;
  }

  function updateResetsSecondField(accountId: string, key: UsageKey, value: number | null) {
    const edit = usageEdits.value[accountId]?.[key];
    if (!edit || edit.saving) return;
    if (WINDOW_FULL_MINUTES[key] === null) return;
    const v = value === null ? 0 : Math.max(0, Math.round(value));
    const first = resetsFirstFieldValue(edit, key, now.value);
    const max = WINDOW_FULL_MINUTES[key] ?? 10080;
    edit.resets_in_minutes_draft = Math.min(max, resetsFieldsToMinutes(first, v, key));
    edit.resets_dirty = true;
  }

  async function saveUsage(accountId: string, key: UsageKey) {
    const account = accounts.value.find(({ id }) => id === accountId);
    const edit = usageEdits.value[accountId]?.[key];
    if (!account || !edit || edit.saving) return;
    const currentRequest = requestStillCurrent(account);
    const isCurrent = () => currentRequest() && usageEdits.value[accountId]?.[key] === edit;
    const binding = bindingFor(account);
    const percent = normalizeUsagePercent(edit.draft);
    edit.draft = percent;
    const resetsChanged = edit.resets_dirty;
    if (percent === edit.saved && !resetsChanged && !edit.error) return;
    edit.saving = true;
    edit.error = null;
    const resetsInMin = resetsInMinutesForSave(edit, key);
    try {
      const usage = await dashboardApi.updateAccountUsage(
        accountId,
        key,
        percent,
        resetsInMin,
      );
      if (!isCurrent()) return;
      billing.applyCalibratedUsage(
        accountId,
        binding,
        key,
        {
          ...getUsage(accountId),
          [key]: usage[key],
          ...(key === "window_5h" ? { resets_in_5h: usage.resets_in_5h } : {}),
          ...(key === "window_week" ? { resets_in_week: usage.resets_in_week } : {}),
          ...(key === "window_month" ? { resets_in_month: usage.resets_in_month } : {}),
        },
        new Date().toISOString(),
      );
      const saved = usagePercentFromCost(usage[key], usageLimit(accountId, key));
      edit.draft = saved;
      edit.saved = saved;
      edit.resets_at_saved = windowResetsAt(usage, key);
      edit.resets_in_minutes_draft = defaultResetsInMinutes(usage, key);
      edit.resets_dirty = false;
    } catch (error) {
      if (!isCurrent()) return;
      edit.error = dashboardErrorDetail(error);
      message.error(t("用量保存失败：{error}", { error: edit.error }));
    } finally {
      if (usageEdits.value[accountId]?.[key] === edit) edit.saving = false;
    }
  }

  function patchAccountUsageSync(
    accountId: string,
    patch: Partial<Pick<Account, "usage_sync_last_success_at" | "usage_sync_next_allowed_at">>,
  ): void {
    accounts.value = accounts.value.map((account) =>
      account.id === accountId ? { ...account, ...patch } : account,
    );
  }

  async function refreshAccountUsage(accountId: string): Promise<void> {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account || !usageCapabilities(account).refresh) return;
    const isCurrent = requestStillCurrent(account);
    if (usageRefreshLoadingFor(accountId).value || usageLoadingFor(accountId).value) {
      return;
    }
    const status = billing.slotFor(accountId).value?.status;
    try {
      if (status?.model === "cash") {
        await billing.refreshCash(accountId, bindingFor(account));
      } else {
        await billing.refreshUsage(accountId, bindingFor(account));
      }
      if (!isCurrent()) return;
      const presented = providerUsageFor(accountId).value;
      patchAccountUsageSync(accountId, {
        usage_sync_last_success_at: presented?.sync_state?.last_success_at ?? null,
        usage_sync_next_allowed_at: presented?.sync_state?.next_eligible_at ?? null,
      });
      if (usageCapabilities(account).manual) {
        syncUsageEdits(accountId, getUsage(accountId));
      }
      message.success(t("成功"));
    } catch (error) {
      if (!isCurrent()) return;
      if (error instanceof DashboardRequestError && error.status === 429) {
        const nextAllowed = error.nextAllowedAt;
        if (nextAllowed) {
          patchAccountUsageSync(accountId, { usage_sync_next_allowed_at: nextAllowed });
        }
        const seconds = error.retryAfterSeconds;
        message.warning(
          seconds
            ? t("稍后再试（约 {seconds} 秒）", { seconds: String(seconds) })
            : t("刷新额度失败：{error}", { error: dashboardErrorDetail(error) }),
        );
      } else {
        message.error(t("刷新额度失败：{error}", { error: dashboardErrorDetail(error) }));
      }
    } finally {
      try {
        if (isCurrent()) await options?.afterUsageRefresh?.(accountId, isCurrent);
      } catch {
        // Companion catalog refresh reports its own failure.
      }
    }
  }

  async function loadQuotaLimits(): Promise<boolean> {
    return billing.loadPricing();
  }

  async function loadAccountUsage(accountId: string) {
    const account = accounts.value.find(({ id }) => id === accountId);
    if (!account) return;
    const isCurrent = requestStillCurrent(account);
    await billing.load(accountId, bindingFor(account));
    if (!isCurrent()) return;
    if (usageCapabilities(account).manual) {
      syncUsageEdits(accountId, getUsage(accountId));
    }
  }

  async function revalidateAccountUsage(accountId: string): Promise<void> {
    const slot = billing.slotFor(accountId).value;
    if (slot?.loading || slot?.mutating) return;
    await loadAccountUsage(accountId);
  }

  function forgetAccount(accountId: string): void {
    billing.remove(accountId);
    delete usageEdits.value[accountId];
  }

  async function retryQuotaLimits() {
    if (!await loadQuotaLimits()) return;
    await mapWithConcurrency(
      accounts.value.filter((account) => (
        accountIsReady(account)
        && (usageCapabilities(account).providerWindows || usageCapabilities(account).manual)
      )),
      4,
      (account) => loadAccountUsage(account.id),
    );
  }

  watch(() => billing.sessionEpoch, () => {
    usageEdits.value = {};
  });

  return {
    quotaLimits,
    quotaLimitsLoading,
    quotaLimitsError,
    usageLimits,
    usageLimitsFor,
    usageMap,
    providerUsageMap,
    providerUsageFor,
    usageFor,
    usageLoadingFor,
    usageLoadErrorFor,
    usageRefreshLoadingFor,
    usageEdits,
    usageLoading,
    usageLoadErrors,
    usageRefreshLoading,
    getUsage,
    hasAvailableUsageEditor,
    focusUsageEditor,
    updateUsageDraft,
    updateResetsFirstField,
    updateResetsSecondField,
    saveUsage,
    refreshAccountUsage,
    loadQuotaLimits,
    loadAccountUsage,
    revalidateAccountUsage,
    retryQuotaLimits,
    forgetAccount,
  };
}
