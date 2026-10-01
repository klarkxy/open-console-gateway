import { computed, getCurrentScope, nextTick, onScopeDispose, ref, watch } from "vue";
import type { ComputedRef, Ref } from "vue";
import { useMessage } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard.ts";
import type { Account } from "../api/dashboard";
import type { BillingStatus, ProviderUsage } from "../api/billing.ts";
import type {
  ProviderCatalogEntry,
  ProviderQuotaWindow,
  ProviderUsageResponse,
} from "../api/providers.ts";
import { useBillingStore, type BillingSlot } from "../stores/billing.ts";
import {
  defaultResetsInMinutes,
  isUsageLimitReached,
  manualEditorWindowKeys,
  manualUsageEditorEnabled,
  mergeUsageEdit,
  normalizeUsagePercent,
  resetsFieldsToMinutes,
  resetsFirstFieldValue,
  resetsInMinutesForSave,
  resetsSecondFieldValue,
  WINDOW_FULL_MINUTES,
  windowResetsAt,
} from "./accounts-usage.ts";
import type {
  ManualCalibrationPlan,
  ObservedUsageWindow,
  UsageEditState,
  UsageKey,
} from "./accounts-usage.ts";
import { accountIsReady } from "./account-display.ts";
import {
  BILLING_ERROR_KEYS,
  billingBinding,
  billingManualCalibration,
  presentedUsageOf,
  usageWindowFromManualReceipt,
  usageWindowFromProviderUsage,
  type ManualQuotaReceipt,
} from "./billing.ts";
import { findPlanDefinition } from "./plans.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { mapWithConcurrency } from "../utils/async.ts";
import { ACCOUNT_AUTO_REFRESH_MS, billingObservedAt, timestampMs, type AccountRefreshTarget } from "./accounts-auto-refresh.ts";

const USAGE_LIMIT_ORDER = ["window_5h", "window_week", "window_month"] as const satisfies readonly UsageKey[];

const USAGE_LIMIT_LABELS = {
  window_5h: "5 小时",
  window_week: "本周",
  window_month: "本月",
} as const satisfies Record<UsageKey, "5 小时" | "本周" | "本月">;

export type AccountUsageEdits = Partial<Record<UsageKey, UsageEditState>>;

export type UsageLimitView = { key: UsageKey; label: string; limit: number };

/**
 * Account-list usage editors. Server snapshots live in useBillingStore;
 * this composable keeps calibration drafts, messages, and focus, and
 * projects usageMap from a full usage snapshot or a quota-only receipt.
 * providerUsageMap stays on the full snapshot. Whole-table computeds serve
 * list-level consumers; row-level consumers
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
    /** Manual companion work, including model-only accounts; covered by the refresh lock. */
    afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>;
    /** Loaded destination plan. Absent plans fall back to sealed manual windows. */
    calibrationPlanFor?: (accountId: string) => ManualCalibrationPlan | null | undefined;
  },
) {
  const message = options?.message ?? useMessage();
  const billing = useBillingStore();
  // The billing mutation flag ends before companion discovery. Operation
  // identities cover the entire action and cannot unlock a newer session.
  const refreshOperations = ref<Record<string, symbol>>({});
  const usageLoads = new Map<string, { binding: string; session: number; pending: Promise<void> }>();
  let disposed = false;
  if (getCurrentScope()) onScopeDispose(() => { disposed = true; });

  const providerUsageMap = computed(() => {
    const out: Record<string, ProviderUsageResponse> = {};
    for (const [id, slot] of Object.entries(billing.byId)) {
      const presented = presentedProjectionFor(id, slot.status);
      if (presented) out[id] = presented;
    }
    return out;
  });

  const usageMap = computed(() => {
    const out: Record<string, ObservedUsageWindow> = {};
    for (const [id, slot] of Object.entries(billing.byId)) {
      out[id] = observedUsageFor(id, slot);
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
    for (const id of Object.keys(refreshOperations.value)) out[id] = true;
    return out;
  });

  // Per-account projections, keyed by the slot's `usage` object identity.
  // The store replaces a slot wholesale but preserves `status`/`usage`
  // references across loading-only writes, so one account's begin/write cycle
  // never changes another account's projected reference — whole-table map
  // entries and row memos keyed on them stay valid.
  const usageProjections = new Map<string, { source: ProviderUsage | null; window: ObservedUsageWindow }>();
  const receiptProjections = new Map<string, { source: ManualQuotaReceipt; window: ObservedUsageWindow }>();
  const presentedProjections = new Map<string, { source: ProviderUsage | null; presented: ProviderUsageResponse | null }>();

  function usageProjectionFor(accountId: string, source: ProviderUsage | null): ObservedUsageWindow {
    const cached = usageProjections.get(accountId);
    if (cached && cached.source === source) return cached.window;
    const window = usageWindowFromProviderUsage(source, accountId);
    usageProjections.set(accountId, { source, window });
    return window;
  }

  function receiptProjectionFor(accountId: string, source: ManualQuotaReceipt): ObservedUsageWindow {
    const cached = receiptProjections.get(accountId);
    if (cached && cached.source === source) return cached.window;
    const window = usageWindowFromManualReceipt(source, accountId);
    receiptProjections.set(accountId, { source, window });
    return window;
  }

  function observedUsageFor(accountId: string, slot: BillingSlot | undefined): ObservedUsageWindow {
    const source = slot?.status?.usage ?? null;
    if (source) return usageProjectionFor(accountId, source);
    const receipt = slot?.manualReceipt ?? null;
    if (receipt && receipt.windows.length > 0) return receiptProjectionFor(accountId, receipt);
    return usageProjectionFor(accountId, null);
  }

  function presentedProjectionFor(
    accountId: string,
    status: BillingStatus | null,
  ): ProviderUsageResponse | null {
    // presentedUsageOf depends only on status.usage; keying on the usage
    // reference keeps the projection stable across status-only replacements.
    const source = status?.usage ?? null;
    const cached = presentedProjections.get(accountId);
    if (cached && cached.source === source) return cached.presented;
    const presented = presentedUsageOf(status);
    presentedProjections.set(accountId, { source, presented });
    return presented;
  }

  // Row-level selectors: subscribing to one account's slot keeps an update
  // for account X from invalidating rows that only render account Y. Each
  // selector is created once per account so repeated calls share one computed
  // instead of accumulating new ones, and the projection cache above keeps
  // its value referentially stable when the account's usage did not change.
  const providerUsageSelectors = new Map<string, ComputedRef<ProviderUsageResponse | null>>();
  const usageSelectors = new Map<string, ComputedRef<ObservedUsageWindow>>();
  const usageLoadingSelectors = new Map<string, ComputedRef<boolean>>();
  const usageLoadErrorSelectors = new Map<string, ComputedRef<string | null>>();
  const usageRefreshLoadingSelectors = new Map<string, ComputedRef<boolean>>();

  function providerUsageFor(accountId: string): ComputedRef<ProviderUsageResponse | null> {
    const cached = providerUsageSelectors.get(accountId);
    if (cached) return cached;
    const selector = computed(() => presentedProjectionFor(
      accountId,
      billing.slotFor(accountId).value?.status ?? null,
    ));
    providerUsageSelectors.set(accountId, selector);
    return selector;
  }

  function usageFor(accountId: string): ComputedRef<ObservedUsageWindow> {
    const cached = usageSelectors.get(accountId);
    if (cached) return cached;
    const selector = computed(() => observedUsageFor(
      accountId,
      billing.slotFor(accountId).value,
    ));
    usageSelectors.set(accountId, selector);
    return selector;
  }

  function usageLoadingFor(accountId: string): ComputedRef<boolean> {
    const cached = usageLoadingSelectors.get(accountId);
    if (cached) return cached;
    const selector = computed(() => billing.slotFor(accountId).value?.loading ?? false);
    usageLoadingSelectors.set(accountId, selector);
    return selector;
  }

  function usageLoadErrorFor(accountId: string): ComputedRef<string | null> {
    const cached = usageLoadErrorSelectors.get(accountId);
    if (cached) return cached;
    const selector = computed(() => {
      const error = billing.slotFor(accountId).value?.error;
      return error ? t(BILLING_ERROR_KEYS[error]) : null;
    });
    usageLoadErrorSelectors.set(accountId, selector);
    return selector;
  }

  function usageRefreshLoadingFor(accountId: string): ComputedRef<boolean> {
    const cached = usageRefreshLoadingSelectors.get(accountId);
    if (cached) return cached;
    const selector = computed(() => Boolean(refreshOperations.value[accountId])
      || (billing.slotFor(accountId).value?.mutating ?? false));
    usageRefreshLoadingSelectors.set(accountId, selector);
    return selector;
  }

  function calibrationPlan(account: Account): ManualCalibrationPlan | null {
    return options?.calibrationPlanFor?.(account.id) ?? null;
  }

  function usageLimitsFor(account: Account): UsageLimitView[] {
    const presented = providerUsageFor(account.id).value;
    const observed = presented ? limitsFromProviderWindows(presented.quota_windows) : [];
    if (!usageCapabilities(account).manual) return observed;
    const byKey = new Map(observed.map((limit) => [limit.key, limit]));
    for (const key of manualEditorWindowKeys(account.provider_id, calibrationPlan(account))) {
      if (byKey.has(key)) continue;
      byKey.set(key, { key, label: t(USAGE_LIMIT_LABELS[key]), limit: 100 });
    }
    return USAGE_LIMIT_ORDER.flatMap((key) => {
      const limit = byKey.get(key);
      return limit ? [limit] : [];
    });
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
    const slot = billing.slotFor(account.id).value;
    const status = slot?.boundVersion === bindingFor(account) ? slot.status : null;
    const plan = calibrationPlan(account);
    if (status) {
      return {
        providerWindows: Boolean(status.usage) || status.model === "quota",
        refresh: status.officialRefresh,
        manual: manualUsageEditorEnabled({
          providerId: account.provider_id,
          plan,
          reportedManual: billingManualCalibration(status),
          hasCreditMeter: Boolean(status.credits),
        }),
      };
    }
    const surface = findPlanDefinition(account.provider_id, catalog.value);
    const refresh = surface?.usage_availability === "available";
    const creditBalance = options?.officialBalanceFor?.(account) === true;
    const manual = manualUsageEditorEnabled({
      providerId: account.provider_id,
      plan,
      reportedManual: surface?.manual_usage_calibration === true || plan?.manual_calibration === true,
      hasCreditMeter: false,
    });
    return {
      providerWindows: refresh || manual || creditBalance,
      refresh: refresh || creditBalance,
      manual,
    };
  }

  function limitsFromProviderWindows(windows: ProviderQuotaWindow[]): UsageLimitView[] {
    const byKind = new Map(windows.map((window) => [window.window_kind, window]));
    const definitions: Array<[UsageKey, string]> = [
      ["window_5h", "five_hours"],
      ["window_week", "week"],
      ["window_month", "month"],
    ];
    return definitions.flatMap(([key, kind]) => {
      const window = kind === "month"
        ? byKind.get("month") ?? byKind.get("monthly")
        : byKind.get(kind);
      if (!window || !Number.isFinite(window.used)) return [];
      const stored = window.limit_value;
      const limit = typeof stored === "number" && Number.isFinite(stored) && stored > 0 ? stored : 100;
      return [{ key, label: t(USAGE_LIMIT_LABELS[key]), limit }];
    });
  }

  const usageEdits = ref<Record<string, AccountUsageEdits>>({});

  function getUsage(accountId: string): ObservedUsageWindow {
    return usageFor(accountId).value;
  }

  function observedDraftPercent(usage: ObservedUsageWindow, key: UsageKey): number | null {
    const value = usage[key];
    return typeof value === "number" && Number.isFinite(value) ? value : null;
  }

  function accountUsageLimitReached(account: Account, key: UsageKey): boolean {
    return isUsageLimitReached(account, key, now.value);
  }

  function hasAvailableUsageEditor(account: Account): boolean {
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

  function blankUsageEdit(): UsageEditState {
    return {
      draft: null,
      saved: null,
      saving: false,
      error: null,
      resets_in_minutes_draft: null,
      resets_at_saved: null,
      resets_dirty: false,
    };
  }

  function usageEditsFromWindow(usage: ObservedUsageWindow): AccountUsageEdits {
    const account = accounts.value.find(({ id }) => id === usage.account_id);
    const limits = account ? usageLimitsFor(account) : [];
    return Object.fromEntries(limits.map(({ key }) => {
      const percent = observedDraftPercent(usage, key);
      if (percent === null) return [key, blankUsageEdit()];
      return [key, {
        draft: percent,
        saved: percent,
        saving: false,
        error: null,
        resets_in_minutes_draft: defaultResetsInMinutes(usage, key, now.value),
        resets_at_saved: windowResetsAt(usage, key),
        resets_dirty: false,
      }];
    })) as AccountUsageEdits;
  }

  function syncUsageEdits(accountId: string, usage: ObservedUsageWindow) {
    const existing = usageEdits.value[accountId];
    if (!existing) {
      usageEdits.value[accountId] = usageEditsFromWindow(usage);
      return;
    }
    const account = accounts.value.find(({ id }) => id === accountId);
    const limits = account ? usageLimitsFor(account) : [];
    for (const { key } of limits) {
      const saved = observedDraftPercent(usage, key);
      const edit = existing[key];
      if (saved === null) {
        // Same-binding refresh only. A clean acknowledged editor follows a
        // snapshot that has no observation. An unsaved, reset-dirty, or
        // in-flight draft stays. A binding change replaces this record first.
        if (
          edit
          && !edit.saving
          && !edit.resets_dirty
          && edit.draft === edit.saved
          && edit.saved !== null
        ) {
          existing[key] = blankUsageEdit();
        } else if (!edit) {
          existing[key] = blankUsageEdit();
        }
        continue;
      }
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
    const account = accounts.value.find(({ id }) => id === accountId);
    const allowed = Boolean(account) && usageLimitsFor(account!).some((limit) => limit.key === key);
    let edit = usageEdits.value[accountId]?.[key];
    if (edit?.saving || (!edit && !allowed)) return;
    if (!edit) {
      const bucket = usageEdits.value[accountId] ?? (usageEdits.value[accountId] = {});
      edit = blankUsageEdit();
      bucket[key] = edit;
    }
    edit.draft = value === null || !Number.isFinite(value) ? null : normalizeUsagePercent(value);
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
    if (!account || !edit || edit.saving || edit.draft === null) return;
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
      const observedAt = new Date().toISOString();
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
        observedAt,
      );
      if (!isCurrent()) return;
      const saved = observedDraftPercent(usage, key);
      if (saved === null) return;
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

  async function refreshAccountUsage(accountId: string, automatic = false): Promise<void> {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account) return;
    const canRefreshUsage = usageCapabilities(account).refresh;
    if (!canRefreshUsage && (automatic || !options?.afterUsageRefresh)) return;
    if (usageRefreshLoadingFor(accountId).value || usageLoadingFor(accountId).value) return;
    const operation = Symbol(accountId);
    refreshOperations.value[accountId] = operation;
    const requestCurrent = requestStillCurrent(account);
    const isCurrent = () => requestCurrent() && refreshOperations.value[accountId] === operation;
    const slot = billing.slotFor(accountId).value;
    const status = slot?.boundVersion === bindingFor(account) ? slot.status : null;
    let refreshed = false;
    try {
      if (canRefreshUsage) {
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
        if (usageCapabilities(account).manual) syncUsageEdits(accountId, getUsage(accountId));
        refreshed = true;
      }
    } catch (error) {
      if (!isCurrent()) return;
      if (error instanceof DashboardRequestError && error.status === 429) {
        const nextAllowed = error.nextAllowedAt ?? (error.retryAfterSeconds
          ? new Date(Date.now() + error.retryAfterSeconds * 1000).toISOString() : null);
        if (nextAllowed) {
          patchAccountUsageSync(accountId, { usage_sync_next_allowed_at: nextAllowed });
        }
        const seconds = error.retryAfterSeconds;
        if (!automatic) message.warning(
          seconds
            ? t("稍后再试（约 {seconds} 秒）", { seconds: String(seconds) })
            : t("刷新额度失败：{error}", { error: dashboardErrorDetail(error) }),
        );
      } else if (!automatic) {
        message.error(t("刷新额度失败：{error}", { error: dashboardErrorDetail(error) }));
      }
    } finally {
      try {
        // A failed/rate-limited quota request must not start unrelated writes.
        // Manual model-only accounts do not issue an unsupported billing POST.
        if (!automatic && isCurrent() && (refreshed || !canRefreshUsage)) {
          await options?.afterUsageRefresh?.(accountId, isCurrent);
          if (refreshed && isCurrent()) message.success(t("成功"));
        }
      } catch {
        // Companion catalog refresh reports its own failure.
      } finally {
        if (refreshOperations.value[accountId] === operation) delete refreshOperations.value[accountId];
      }
    }
  }

  function automaticRefreshTarget(account: Account): AccountRefreshTarget | null {
    const slot = billing.slotFor(account.id).value;
    if (!account.enabled || !accountIsReady(account) || !usageCapabilities(account).refresh) return null;
    const binding = bindingFor(account);
    const status = slot?.boundVersion === binding ? slot.status : null;
    return {
      id: account.id,
      binding,
      observedAt: Math.max(billingObservedAt(status), timestampMs(account.usage_sync_last_success_at)),
      nextAllowedAt: Math.max(
        timestampMs(status?.usage?.syncState?.nextEligibleAt),
        timestampMs(account.usage_sync_next_allowed_at),
      ),
      busy: Boolean(slot?.loading || slot?.mutating || refreshOperations.value[account.id])
        || Object.values(usageEdits.value[account.id] ?? {}).some(edit => edit.saving || edit.resets_dirty || edit.draft !== edit.saved),
      refresh: async (allowed) => {
        const isCurrent = requestStillCurrent(account);
        // Every preceding refresh can advance global CAS. Read this row again
        // immediately before its mutation, preserving the existing CAS contract.
        await revalidateAccountUsage(account.id);
        if (!allowed() || !isCurrent()) return;
        const latest = automaticRefreshTarget(account);
        if (!latest || latest.busy || billing.slotFor(account.id).value?.error) return;
        if (latest.nextAllowedAt > Date.now() || latest.observedAt + ACCOUNT_AUTO_REFRESH_MS > Date.now()) return;
        await refreshAccountUsage(account.id, true);
      },
    };
  }

  function loadAccountUsage(accountId: string): Promise<void> {
    const account = accounts.value.find(({ id }) => id === accountId);
    if (!account) return Promise.resolve();
    const binding = bindingFor(account);
    const session = billing.sessionEpoch;
    const pending = usageLoads.get(accountId);
    const slot = billing.slotFor(accountId).value;
    if (pending?.binding === binding && pending.session === session
      && slot?.loading && slot.boundVersion === binding) return pending.pending;
    const isCurrent = requestStillCurrent(account);
    const request = (async () => {
      await billing.load(accountId, binding);
      if (!isCurrent()) return;
      if (usageCapabilities(account).manual) syncUsageEdits(accountId, getUsage(accountId));
    })().finally(() => {
      if (usageLoads.get(accountId)?.pending === request) usageLoads.delete(accountId);
    });
    usageLoads.set(accountId, { binding, session, pending: request });
    return request;
  }

  async function revalidateAccountUsage(accountId: string): Promise<void> {
    const slot = billing.slotFor(accountId).value;
    if (slot?.loading || slot?.mutating || refreshOperations.value[accountId]) return;
    await loadAccountUsage(accountId);
  }

  function forgetAccount(accountId: string): void {
    delete refreshOperations.value[accountId];
    usageLoads.delete(accountId);
    billing.remove(accountId);
    delete usageEdits.value[accountId];
    usageProjections.delete(accountId);
    receiptProjections.delete(accountId);
    presentedProjections.delete(accountId);
    providerUsageSelectors.delete(accountId);
    usageSelectors.delete(accountId);
    usageLoadingSelectors.delete(accountId);
    usageLoadErrorSelectors.delete(accountId);
    usageRefreshLoadingSelectors.delete(accountId);
  }

  watch(() => accounts.value.map(account => account.id), (ids, previousIds) => {
    const current = new Set(ids);
    for (const id of previousIds) {
      if (!current.has(id)) forgetAccount(id);
    }
  }, { flush: "sync" });

  // The account view owns automatic billing reads. A changed account/endpoint
  // invalidates its old evidence immediately, then reloads only that binding.
  // Rows without a slot are still covered by the bounded initial load pass.
  watch(() => accounts.value.map(account => ({ id: account.id, binding: bindingFor(account) })), rows => {
    const changed = rows.filter(({ id, binding }) => {
      const slot = billing.slotFor(id).value;
      return slot !== undefined && slot.boundVersion !== binding;
    });
    for (const { id } of changed) {
      billing.remove(id);
      // The slot is gone, so this seeds blank drafts for the new binding.
      // New edit objects fence an in-flight ack: its finally only touches
      // the editor it captured, and the removed draft is not kept for return.
      usageEdits.value[id] = usageEditsFromWindow(getUsage(id));
    }
    const session = billing.sessionEpoch;
    // Batch synchronous account/connection updates before reading their final
    // binding. An explicit owner load in this turn already covers the change.
    void Promise.resolve().then(() => mapWithConcurrency(changed, 4, async ({ id }) => {
      if (disposed || session !== billing.sessionEpoch) return;
      const account = accounts.value.find(account => account.id === id);
      if (!account) return;
      const slot = billing.slotFor(id).value;
      if (slot?.boundVersion === bindingFor(account)) return;
      await loadAccountUsage(id);
    }));
  }, { flush: "sync" });

  watch(() => billing.sessionEpoch, () => {
    refreshOperations.value = {};
    usageLoads.clear();
    usageEdits.value = {};
    // A new session must never serve projections cached from the old one.
    usageProjections.clear();
    receiptProjections.clear();
    presentedProjections.clear();
    providerUsageSelectors.clear();
    usageSelectors.clear();
    usageLoadingSelectors.clear();
    usageLoadErrorSelectors.clear();
    usageRefreshLoadingSelectors.clear();
  }, { flush: "sync" });

  function ensureAccountUsage(accountId: string): Promise<void> {
    const account = accounts.value.find(item => item.id === accountId);
    if (!account) return Promise.resolve();
    const slot = billing.slotFor(accountId).value;
    if (slot?.loaded && !slot.error && slot.boundVersion === bindingFor(account)) return Promise.resolve();
    return loadAccountUsage(accountId);
  }

  return {
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
    automaticRefreshTarget,
    loadAccountUsage,
    ensureAccountUsage,
    revalidateAccountUsage,
    forgetAccount,
  };
}
