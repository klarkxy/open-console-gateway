import { computed, nextTick, ref } from "vue";
import type { Ref } from "vue";
import { useMessage } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard";
import type { Account, PricingLimits, UsageWindow } from "../api/dashboard";
import { providerApi } from "../api/providers.ts";
import type {
  ProviderCatalogEntry,
  ProviderQuotaWindow,
  ProviderUsageResponse,
} from "../api/providers.ts";
import {
  defaultResetsInMinutes,
  isUsageLimitReached,
  mergeCalibratedProviderUsage,
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
import { accountIsReady } from "./account-display.ts";
import { findPlanDefinition } from "./plans.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { mapWithConcurrency } from "../utils/async.ts";

export type AccountUsageEdits = Record<UsageKey, UsageEditState>;

export type UsageLimitView = { key: UsageKey; label: string; limit: number };

/**
 * Quota window state for the account list: OpenCode Go pricing limits,
 * per-account usage snapshots, the manual calibration drafts, and the
 * official-usage refresh flow (including 429 throttle handling). GOAT and
 * paid Ollama Cloud have no machine-readable usage endpoint, so their
 * windows project locally priced OCG request logs and allow an explicit
 * manual correction. Ollama accounts without a billing row skip the meter.
 */
export function useAccountUsage(
  accounts: Ref<Account[]>,
  now: Ref<number>,
  catalog: Ref<ProviderCatalogEntry[] | null>,
) {
  const message = useMessage();

  const quotaLimits = ref<PricingLimits | null>(null);
  const quotaLimitsLoading = ref(false);
  const quotaLimitsError = ref("");
  const providerUsageLimits = ref<Record<string, UsageLimitView[]>>({});
  const usageLimits = computed<UsageLimitView[]>(() => {
    const limits = quotaLimits.value;
    if (!limits) return [];
    return [
      { key: "window_5h", label: t("5小时"), limit: limits.window_5h },
      { key: "window_week", label: t("本周"), limit: limits.window_week },
      { key: "window_month", label: t("本月"), limit: limits.window_month },
    ];
  });

  function usageLimitsFor(account: Account): UsageLimitView[] {
    const providerLimits = providerUsageLimits.value[account.id];
    if (providerLimits?.length) return providerLimits;
    const surface = findPlanDefinition(account.provider_id, catalog.value);
    const limits = surface?.legacy && account.provider_id === "opencode"
      ? quotaLimits.value
      : null;
    if (!limits) return [];
    return [
      { key: "window_5h", label: t("5小时"), limit: limits.window_5h },
      { key: "window_week", label: t("本周"), limit: limits.window_week },
      { key: "window_month", label: t("本月"), limit: limits.window_month },
    ];
  }

  function usageCapabilities(account: Account): {
    providerWindows: boolean;
    refresh: boolean;
    manual: boolean;
  } {
    const surface = findPlanDefinition(account.provider_id, catalog.value);
    const manual = surface?.manual_usage_calibration === true;
    const refresh = surface?.usage_availability === "available";
    return { providerWindows: refresh || manual, refresh, manual };
  }

  function limitsFromProviderWindows(windows: ProviderQuotaWindow[]): UsageLimitView[] {
    const byKind = new Map(windows.map((window) => [window.window_kind, window]));
    const definitions: Array<[UsageKey, string, string]> = [
      ["window_5h", "five_hours", t("5小时")],
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

  const usageMap = ref<Record<string, UsageWindow>>({});
  const providerUsageMap = ref<Record<string, ProviderUsageResponse>>({});
  const usageEdits = ref<Record<string, AccountUsageEdits>>({});
  const usageLoading = ref<Record<string, boolean>>({});
  const usageLoadErrors = ref<Record<string, string | null>>({});
  const usageRefreshLoading = ref<Record<string, boolean>>({});

  function blankUsage(accountId: string): UsageWindow {
    return {
      account_id: accountId,
      window_5h: 0,
      window_week: 0,
      window_month: 0,
      resets_in_5h: null,
      resets_in_week: null,
      resets_in_month: null,
    };
  }

  function getUsage(accountId: string): UsageWindow {
    return usageMap.value[accountId] || blankUsage(accountId);
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
    if (usageLoading.value[account.id] || usageLoadErrors.value[account.id]) return false;
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
    const edit = usageEdits.value[accountId]?.[key];
    if (!edit || edit.saving) return;
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
      usageMap.value[accountId] = {
        ...getUsage(accountId),
        [key]: usage[key],
        ...(key === "window_5h" ? { resets_in_5h: usage.resets_in_5h } : {}),
        ...(key === "window_week" ? { resets_in_week: usage.resets_in_week } : {}),
        ...(key === "window_month" ? { resets_in_month: usage.resets_in_month } : {}),
      };
      const patchedProviderUsage = mergeCalibratedProviderUsage(
        providerUsageMap.value[accountId],
        key,
        usage,
        new Date().toISOString(),
      );
      if (patchedProviderUsage) {
        providerUsageMap.value = {
          ...providerUsageMap.value,
          [accountId]: patchedProviderUsage,
        };
      }
      const saved = usagePercentFromCost(usage[key], usageLimit(accountId, key));
      edit.draft = saved;
      edit.saved = saved;
      edit.resets_at_saved = windowResetsAt(usage, key);
      edit.resets_in_minutes_draft = defaultResetsInMinutes(usage, key);
      edit.resets_dirty = false;
    } catch (error) {
      edit.error = dashboardErrorDetail(error);
      message.error(t("用量保存失败: {error}", { error: edit.error }));
    } finally {
      edit.saving = false;
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
    if (usageRefreshLoading.value[accountId] || usageLoading.value[accountId]) {
      return;
    }
    usageRefreshLoading.value = { ...usageRefreshLoading.value, [accountId]: true };
    try {
      const result = await providerApi.refreshProviderUsage(accountId);
      providerUsageMap.value = { ...providerUsageMap.value, [accountId]: result };
      providerUsageLimits.value = {
        ...providerUsageLimits.value,
        [accountId]: limitsFromProviderWindows(result.quota_windows),
      };
      patchAccountUsageSync(accountId, {
        usage_sync_last_success_at: result.sync_state?.last_success_at ?? null,
        usage_sync_next_allowed_at: result.sync_state?.next_eligible_at ?? null,
      });
      if (usageCapabilities(account).manual) {
        const usage = await dashboardApi.getAccountUsage(accountId);
        usageMap.value[accountId] = usage;
        syncUsageEdits(accountId, usage);
      }
      message.success(t("成功"));
    } catch (error) {
      if (error instanceof DashboardRequestError && error.status === 429) {
        const nextAllowed = error.nextAllowedAt;
        if (nextAllowed) {
          patchAccountUsageSync(accountId, { usage_sync_next_allowed_at: nextAllowed });
        }
        const seconds = error.retryAfterSeconds;
        message.warning(
          seconds
            ? t("请稍后再试（约 {seconds} 秒）", { seconds: String(seconds) })
            : t("刷新额度失败: {error}", { error: dashboardErrorDetail(error) }),
        );
      } else {
        message.error(t("刷新额度失败: {error}", { error: dashboardErrorDetail(error) }));
      }
    } finally {
      usageRefreshLoading.value = { ...usageRefreshLoading.value, [accountId]: false };
    }
  }

  async function loadQuotaLimits(): Promise<boolean> {
    quotaLimitsLoading.value = true;
    quotaLimitsError.value = "";
    try {
      quotaLimits.value = (await dashboardApi.getPricing()).limits;
      return true;
    } catch (error) {
      quotaLimits.value = null;
      quotaLimitsError.value = dashboardErrorDetail(error);
      return false;
    } finally {
      quotaLimitsLoading.value = false;
    }
  }

  async function loadAccountUsage(accountId: string) {
    usageLoading.value[accountId] = true;
    usageLoadErrors.value[accountId] = null;
    try {
      const account = accounts.value.find(({ id }) => id === accountId);
      if (!account) return;
      const capabilities = usageCapabilities(account);
      if (!capabilities.providerWindows && !capabilities.manual) return;
      const [providerUsage, usage] = await Promise.all([
        capabilities.providerWindows
          ? providerApi.getProviderUsage(accountId)
          : Promise.resolve(null),
        capabilities.manual
          ? dashboardApi.getAccountUsage(accountId)
          : Promise.resolve(null),
      ]);
      if (providerUsage) {
        providerUsageMap.value = { ...providerUsageMap.value, [accountId]: providerUsage };
        providerUsageLimits.value = {
          ...providerUsageLimits.value,
          [accountId]: limitsFromProviderWindows(providerUsage.quota_windows),
        };
      }
      if (usage) {
        usageMap.value[accountId] = usage;
        syncUsageEdits(accountId, usage);
      } else {
        usageMap.value[accountId] = blankUsage(accountId);
      }
    } catch (error) {
      usageLoadErrors.value[accountId] = dashboardErrorDetail(error);
    } finally {
      usageLoading.value[accountId] = false;
    }
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

  return {
    quotaLimits,
    quotaLimitsLoading,
    quotaLimitsError,
    usageLimits,
    usageLimitsFor,
    usageMap,
    providerUsageMap,
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
    retryQuotaLimits,
  };
}
