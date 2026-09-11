<template>
  <div class="accounts-view">
    <n-space vertical :size="16" class="accounts-content">
      <div class="accounts-toolbar">
        <div
          v-if="!accountListLoading && !accountListError && accounts.length > 0"
          class="accounts-filter-bar"
        >
          <div class="filter-field">
            <span class="filter-label">{{ t("按方案筛选") }}</span>
            <n-select
              v-model:value="planFilter"
              :options="planFilterOptions"
              :placeholder="t('按方案筛选')"
              :aria-label="t('按方案筛选')"
              :consistent-menu-width="false"
              size="small"
            />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("按状态筛选") }}</span>
            <n-select
              v-model:value="statusFilter"
              :options="statusFilterOptions"
              :placeholder="t('按状态筛选')"
              :aria-label="t('按状态筛选')"
              :consistent-menu-width="false"
              size="small"
            />
          </div>
        </div>
        <n-space wrap class="accounts-actions">
          <n-button @click="openTransfer('import')">{{ t("导入账号") }}</n-button>
          <n-button @click="openTransfer('export')">{{ t("导出账号") }}</n-button>
          <n-button type="primary" @click="openAddModal">
            <template #icon>
              <n-icon :component="PlusOutlined" />
            </template>
            {{ t("新增账号") }}
          </n-button>
        </n-space>
      </div>

      <span id="account-order-instructions" class="sr-only">
        {{ t("使用上下方向键调整优先级") }}
      </span>

      <div
        v-if="accountListLoading"
        class="account-list-state"
        role="status"
        aria-live="polite"
        :aria-label="t('加载中…')"
      >
        <n-spin size="small" />
      </div>

      <n-alert v-else-if="accountListError" type="error" :title="t('加载账号失败: {error}', { error: accountListError })">
        <n-button size="small" secondary @click="loadAccounts">{{ t("重试") }}</n-button>
      </n-alert>

      <n-alert
        v-if="catalogError"
        type="warning"
        :title="t('加载服务商目录失败: {error}', { error: catalogError })"
      >
        <n-button size="small" secondary :loading="catalogLoading" @click="loadProviderCatalog">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert v-if="quotaLimitsError" type="warning" :title="t('用量加载失败')">
        <n-button
          size="small"
          secondary
          :loading="quotaLimitsLoading"
          @click="retryQuotaLimits"
        >{{ t("重试") }}</n-button>
      </n-alert>

      <PlatformAccountsSection
        ref="platformSectionRef"
        :accounts="accounts"
        :catalog="providerCatalog"
        @changed="loadAccounts"
        @account-updated="replaceAccount"
        @links-change="onPlatformLinksChange"
      />

      <n-empty
        v-if="!accountListLoading && !accountListError && displayedAccounts.length === 0"
        :description="t('暂无账号')"
      >
        <template #extra>
          <n-button v-if="planFilter !== 'all' || statusFilter !== 'all'" @click="resetFilters">
            {{ t("重置") }}
          </n-button>
          <n-button v-else type="primary" @click="openAddModal">
            <template #icon>
              <n-icon :component="PlusOutlined" />
            </template>
            {{ t("新增账号") }}
          </n-button>
        </template>
      </n-empty>

      <div v-if="!accountListLoading && !accountListError && displayedAccounts.length > 0" class="account-list">
        <AccountCard
          v-for="account in displayedAccounts"
          :key="account.id"
          :account="account"
          :catalog="providerCatalog"
          :usage="getUsage(account.id)"
          :provider-usage="providerUsageMap[account.id] ?? null"
          :limits="usageLimitsFor(account)"
          :edits="usageEdits[account.id]"
          :now="now"
          :order-handle-disabled="orderSaving || busy || accounts.length < 2"
          :dragging="draggingAccountId === account.id"
          :usage-loading="!!usageLoading[account.id]"
          :usage-load-error="usageLoadErrors[account.id] ?? null"
          :usage-refresh-loading="!!usageRefreshLoading[account.id]"
          :purchase-date-saving="busy || !!purchaseDateSaving[account.id]"
          :quota-limits-failed="!!quotaLimitsError"
          :menu-options="accountMenuOptions(account, now)"
          @order-keydown="handleOrderKeydown($event, account.id)"
          @order-drag-start="startAccountDrag($event, account.id)"
          @toggle="toggleAccount(account.id)"
          @test-connection="openAccountTest(account.id)"
          @refresh-usage="refreshAccountUsage(account.id)"
          @update-purchase-date="updatePurchaseDate(account.id, $event)"
          @reload-usage="loadAccountUsage(account.id)"
          @open-wizard="openManagedWizard(account.id)"
          @menu-select="handleMenuSelect($event, account.id)"
          @usage-editor-open="focusUsageEditor(account.id)"
          @usage-update-draft="(key, value) => updateUsageDraft(account.id, key, value)"
          @usage-update-resets-first="(key, value) => updateResetsFirstField(account.id, key, value)"
          @usage-update-resets-second="(key, value) => updateResetsSecondField(account.id, key, value)"
          @usage-save="(key) => saveUsage(account.id, key)"
        />
      </div>

      <span class="sr-only" aria-live="polite" aria-atomic="true">{{ orderAnnouncement }}</span>
    </n-space>

    <AccountAddModal
      v-model:show="showAddModal"
      :catalog="providerCatalog"
      :catalog-loading="catalogLoading"
      :managed-available="managedRegistrationAvailable"
      :managed-reason="managedRegistrationReason"
      :invite-missing="!opencodeInviteUrl"
      :create-busy="busy"
      :platform-busy="platformMutating"
      :initial-option-id="addInitialOptionId"
      @register-managed="openManagedCreateModal"
      @open-invite-url="openInviteUrl"
      @save-account="onFormSave"
      @create-platform="handleCreatePlatform"
      @preset-saved="onPresetAccountSaved"
      @preset-conflict="onPresetAccountConflict"
    />

    <AccountFormModal
      :show="showModal"
      :account="editingAccount"
      :is-cooling="editingAccount ? isCooling(editingAccount, now) : false"
      :busy="busy"
      :catalog="providerCatalog"
      :endpoint-locked="!!editingPlatformLink"
      :endpoint-lock-hint="editingEndpointLockHint"
      @update:show="setAccountFormVisible"
      @save="onFormSave"
      @reset-cooldown="resetCooldown(editingAccount!.id)"
    />

    <AccountConnectionTestModal
      :show="!!testingAccount"
      :account="testingAccount"
      :catalog="providerCatalog"
      @update:show="setAccountTestVisible"
    />

    <n-modal
      :show="showManagedCreate"
      preset="card"
      :title="t('注册新账号')"
      class="account-managed-modal"
      style="width: 520px; max-width: calc(100vw - 32px)"
      :mask-closable="false"
      :close-on-esc="!busy"
      @update:show="setManagedCreateVisible"
    >
      <n-form label-placement="top" @submit.prevent="createManagedAccount">
        <n-form-item :label="t('名称')" required>
          <n-input
            v-model:value="managedDraft.name"
            autofocus
            :disabled="busy"
            :placeholder="t('例如：新账号 1')"
            :input-props="{ 'aria-label': t('名称') }"
          />
        </n-form-item>
        <n-form-item :label="t('邮箱备注（可选）')">
          <n-input
            v-model:value="managedDraft.username"
            :disabled="busy"
            :placeholder="t('仅作备注')"
            :input-props="{ 'aria-label': t('邮箱备注（可选）') }"
          />
        </n-form-item>
        <n-form-item
          :label="t('邀请链接')"
          required
          :show-feedback="true"
          :validation-status="managedInviteStatus"
          :feedback="managedInviteFeedback"
        >
          <n-input
            v-model:value="managedDraft.inviteUrl"
            :disabled="busy"
            class="mono"
            :placeholder="DEFAULT_OPENCODE_INVITE_URL"
            :input-props="{ 'aria-label': t('邀请链接') }"
            @blur="normalizeManagedInviteDraft"
          />
        </n-form-item>
      </n-form>
      <n-alert type="warning" :show-icon="false">
        {{ t("请确认邀请链接是你自己的（默认仅演示）。修改后会写入 OpenCode Go 供应商。草稿可随时继续。") }}
      </n-alert>
      <template #footer>
        <n-space justify="end">
          <n-button :disabled="busy" @click="setManagedCreateVisible(false)">
            {{ busy ? t("加载中…") : t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="busy"
            :disabled="!canCreateManagedDraft"
            @click="createManagedAccount"
          >{{ t("创建并开始") }}</n-button>
        </n-space>
      </template>
    </n-modal>

    <ManagedAccountWizard
      v-if="managedWizardAccount"
      v-model:show="showManagedWizard"
      :account="managedWizardAccount"
      :browser-capabilities="browserCapabilities"
      :opening-target="openingBrowserTarget"
      :busy="busy"
      @open-browser="openAccountBrowser(managedWizardAccount.id, $event)"
      @advance="advanceManagedSetup(managedWizardAccount.id, $event)"
      @verify-key="verifyManagedKey(managedWizardAccount.id, $event)"
    />

    <AccountTransferModal
      v-model:show="showTransfer"
      :mode="transferMode"
      @imported="handleAccountsImported"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NEmpty,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NModal,
  NSelect,
  NSpin,
  NSpace,
  useDialog,
  useMessage,
} from "naive-ui";
import { PlusOutlined } from "@vicons/antd";
import { DashboardRequestError, dashboardApi, isRevisionConflict } from "../api/dashboard";
import { providerApi } from "../api/providers.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type {
  Account,
  AccountInput,
  AccountSetupStep,
  AccountUpdate,
  BrowserCapabilities,
  BrowserTarget,
} from "../api/dashboard";
import { isCooling } from "../domain/accounts-usage.ts";
import { accountIsReady, accountMenuOptions } from "../domain/account-display.ts";
import { DEFAULT_PROVIDER_ID, isCommandCodeGoatAccount, isOllamaCloudAccount, isOfficialCnPlanAccount, isZenFreeAccount } from "../domain/account-providers.ts";
import {
  executeCustomAccountEdit,
  isCustomApiAccount,
} from "../domain/custom-account.ts";
import { linkForAccount } from "../domain/platform-accounts.ts";
import type { PlatformLink } from "../api/platform-accounts.ts";
import { useAccountUsage } from "../domain/useAccountUsage.ts";
import { useAccountOrder } from "./useAccountOrder.ts";
import {
  filterAccounts,
  plansInUse,
  type AccountPlanFilter,
  type AccountStatusFilter,
} from "./account-filters.ts";
import {
  PLAN_DEFINITIONS,
  dynamicPlanDefinition,
  planFamilyLabel,
} from "../domain/plans.ts";
import { isDynamicCatalogEntry } from "../domain/dynamic-provider.ts";
import { accountCreateRequestInput } from "../domain/account-create-payload.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { applyAppViewSearchParams, readAccountAddDeepLink, readAccountDeepLink } from "./app-navigation.ts";
import { mapWithConcurrency } from "../utils/async.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import {
  reconcileEditingAccount,
} from "./account-cas.ts";
import {
  DEFAULT_OPENCODE_INVITE_URL,
  browserViewUrl,
  normalizeOpenCodeInviteUrl,
} from "../domain/managed-account.ts";
import AccountAddModal from "../components/AccountAddModal.vue";
import AccountCard from "../components/AccountCard.vue";
import AccountConnectionTestModal from "../components/AccountConnectionTestModal.vue";
import AccountFormModal, { type AccountFormPayload } from "../components/AccountFormModal.vue";
import ManagedAccountWizard from "../components/ManagedAccountWizard.vue";
import AccountTransferModal from "../components/AccountTransferModal.vue";
import PlatformAccountsSection from "../components/PlatformAccountsSection.vue";
import type { PlatformAccountFormPayload } from "../components/PlatformAccountFormModal.vue";

const dialog = useDialog();
const message = useMessage();
const accountsStore = useAccountsStore();
const providersStore = useProvidersStore();
const accounts = ref<Account[]>([]);
const accountListLoading = ref(true);
const accountListError = ref("");
const testingAccountId = ref<string | null>(null);
const providerSettingsSaving = ref<Record<string, boolean>>({});
const purchaseDateSaving = ref<Record<string, boolean>>({});
const showModal = ref(false);
const showAddModal = ref(false);
/** One-shot chooser preselection from the `add` deep link; cleared on close. */
const addInitialOptionId = ref<string | null>(null);
const showTransfer = ref(false);
const transferMode = ref<"import" | "export">("import");
const showManagedCreate = ref(false);
const showManagedWizard = ref(false);
useLocalizedModalCloseLabel(showManagedCreate, "account-managed-modal");
const editingAccount = ref<Account | null>(null);
const testingAccount = computed(() => (
  testingAccountId.value
    ? accounts.value.find((account) => account.id === testingAccountId.value) ?? null
    : null
));
const managedWizardAccountId = ref<string | null>(null);
const managedDraft = ref({
  name: "",
  username: "",
  inviteUrl: "",
});
const opencodeInviteUrl = ref("");
const browserCapabilities = ref<BrowserCapabilities>({
  mode: "unsupported",
  reason: t("正在检测浏览器能力…"),
});
const openingBrowserTarget = ref<BrowserTarget | null>(null);
const busy = ref(false);
const now = ref(Date.now());
const planFilter = ref<AccountPlanFilter>("all");
const platformSectionRef = ref<InstanceType<typeof PlatformAccountsSection> | null>(null);
const platformLinks = ref<PlatformLink[]>([]);
const platformParents = ref<{ id: string; name: string }[]>([]);
const editingPlatformLink = computed(() => (
  editingAccount.value ? linkForAccount(platformLinks.value, editingAccount.value.id) : null
));
const editingEndpointLockHint = computed(() => {
  const link = editingPlatformLink.value;
  if (!link) return "";
  const parentName = platformParents.value.find((parent) => parent.id === link.platformAccountId)?.name;
  return parentName
    ? t("已关联平台账号 {name}，Endpoint 由平台托管", { name: parentName })
    : t("已关联平台账号，Endpoint 由平台托管");
});
const OLLAMA_WEBSITE_URL = "https://ollama.com";

const statusFilter = ref<AccountStatusFilter>("all");
const providerCatalog = ref<ProviderCatalogEntry[] | null>(null);
const catalogLoading = ref(false);
const catalogError = ref("");
const platformMutating = computed(() => Boolean(platformSectionRef.value?.mutating));

const {
  quotaLimits,
  quotaLimitsLoading,
  quotaLimitsError,
  usageLimitsFor,
  usageMap,
  providerUsageMap,
  usageEdits,
  usageLoading,
  usageLoadErrors,
  usageRefreshLoading,
  getUsage,
  focusUsageEditor,
  updateUsageDraft,
  updateResetsFirstField,
  updateResetsSecondField,
  saveUsage,
  refreshAccountUsage,
  loadQuotaLimits,
  loadAccountUsage,
  retryQuotaLimits,
} = useAccountUsage(accounts, now);

const {
  orderSaving,
  draggingAccountId,
  orderAnnouncement,
  startAccountDrag,
  handleOrderKeydown,
  revertActiveDrag,
} = useAccountOrder({
  accounts,
  busy,
  reloadAfterRevisionConflict: reloadAfterControlPlaneConflict,
});

const managedWizardAccount = computed(() => (
  accounts.value.find(({ id }) => id === managedWizardAccountId.value) ?? null
));
const managedRegistrationAvailable = computed(() => (
  browserCapabilities.value.mode !== "unsupported"
));
const managedRegistrationReason = computed(() => {
  if (browserCapabilities.value.mode === "unsupported") {
    return browserCapabilities.value.reason || t("当前环境不支持独立浏览器");
  }
  return "";
});
const managedInvitePreview = computed(() => {
  try {
    const normalized = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
    return {
      status: undefined as "error" | undefined,
      feedback: normalized
        ? t("将用于打开邀请页；与 OpenCode Go 供应商中的值不同时会写回。")
        : t("必填。仅接受 opencode.ai 官方 HTTPS 链接。"),
      normalized,
    };
  } catch (error) {
    return {
      status: "error" as const,
      feedback: error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"),
      normalized: "",
    };
  }
});
const managedInviteStatus = computed(() => managedInvitePreview.value.status);
const managedInviteFeedback = computed(() => managedInvitePreview.value.feedback);
const canCreateManagedDraft = computed(() => (
  Boolean(managedDraft.value.name.trim())
  && Boolean(managedInvitePreview.value.normalized)
  && !managedInvitePreview.value.status
));

const displayedAccounts = computed(() => (
  filterAccounts(accounts.value, planFilter.value, statusFilter.value, now.value)
));

const planFilterOptions = computed(() => [
  { value: "all", label: t("全部方案") },
  ...plansInUse(
    accounts.value,
    PLAN_DEFINITIONS,
    (providerCatalog.value ?? []).filter(isDynamicCatalogEntry).map(dynamicPlanDefinition),
  ).map((plan) => ({
    value: plan.id === "dynamic-http" ? plan.provider_id : plan.id,
    label: planFamilyLabel(plan, providerCatalog.value),
  })),
]);

const statusFilterOptions = computed(() => [
  { value: "all", label: t("全部状态") },
  { value: "available", label: t("已启用") },
  { value: "cooling", label: t("冷却中") },
  { value: "auth-error", label: t("不可用") },
  { value: "disabled", label: t("已禁用") },
  { value: "registering", label: t("注册中") },
]);



function handleMenuSelect(key: string | number, accountId: string) {
  if (key === "open-cpa") {
    openCpa();
  } else if (key === "open-console") {
    void openAccountBrowser(accountId, "console");
  } else if (key === "open-site") {
    window.open(OLLAMA_WEBSITE_URL, "_blank", "noopener,noreferrer");
  } else if (key === "continue-setup") {
    openManagedWizard(accountId);
  } else if (key === "edit") {
    openEditModal(accountId);
  } else if (key === "reset") {
    resetCooldown(accountId);
  } else if (key === "reset-profile") {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account) return;
    dialog.warning({
      title: t("重置官网登录状态"),
      content: accountIsReady(account)
        ? t("确定重置账号 {name} 的独立浏览器 Profile 吗？Google 与 OpenCode 登录状态会被清除，但 Key 不受影响。", { name: account.name })
        : t("确定重置账号 {name} 的独立浏览器 Profile 吗？登录状态会被清除，注册进度将回到 Google 账号步骤。", { name: account.name }),
      positiveText: t("重置"),
      negativeText: t("取消"),
      onPositiveClick: () => resetBrowserProfile(accountId),
    });
  } else if (key === "delete") {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account) return;
    dialog.warning({
      title: t("删除账号"),
      content: t("确定删除账号 {name} 吗？账号数据以及独立浏览器中的 Cookie 和 Profile 都会被删除。", { name: account.name }),
      positiveText: t("删除"),
      negativeText: t("取消"),
      onPositiveClick: () => deleteAccount(accountId),
    });
  }
}

function openCpa(): void {
  const url = new URL(window.location.href);
  url.searchParams.set("view", "cpa");
  url.searchParams.delete("account_id");
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function openAddModal(): void {
  // Add Account owns creation now; a stale edit target would turn the
  // chooser's save payload into an update of the previously edited account.
  editingAccount.value = null;
  addInitialOptionId.value = null;
  showAddModal.value = true;
}

/**
 * One-shot deep link (Suppliers Custom API row): open Add Account with the
 * requested chooser option preselected. The parameter is deleted before the
 * modal opens so a reload or close never replays it.
 */
function applyAccountAddDeepLink(): void {
  const optionId = readAccountAddDeepLink(window.location.search);
  if (!optionId) return;
  const url = new URL(window.location.href);
  url.searchParams.delete("add");
  window.history.replaceState(null, "", url);
  editingAccount.value = null;
  addInitialOptionId.value = optionId;
  showAddModal.value = true;
}

function openTransfer(mode: "import" | "export"): void {
  transferMode.value = mode;
  showTransfer.value = true;
}

async function handleAccountsImported(count: number): Promise<void> {
  await loadAccounts();
  message.success(t("节点配置迁移完成：处理 {count} 项账号。", { count }));
}

// The chooser's embedded platform form delegates the write to the section so
// validation, CAS conflict recovery, and the card reload stay in one place.
async function handleCreatePlatform(payload: PlatformAccountFormPayload): Promise<void> {
  const created = await platformSectionRef.value?.createPlatform(payload);
  if (created) showAddModal.value = false;
}

// The atomic create already saved supplier + first account; reload both lists
// so the account and the new user-defined choice appear.
async function onPresetAccountSaved(): Promise<void> {
  await Promise.allSettled([
    loadAccounts(),
    loadProviderCatalog(),
    providersStore.loadCatalog(),
    providersStore.loadConnections(),
  ]);
  message.success(t("账号已添加"));
  showAddModal.value = false;
}

async function onPresetAccountConflict(): Promise<void> {
  await Promise.allSettled([
    loadAccounts(),
    loadProviderCatalog(),
    providersStore.loadCatalog(),
    providersStore.loadConnections(),
  ]);
}

function resetFilters(): void {
  planFilter.value = "all";
  statusFilter.value = "all";
}

function onPlatformLinksChange(links: PlatformLink[], parents: { id: string; name: string }[]): void {
  platformLinks.value = links;
  platformParents.value = parents;
}

function openManagedCreateModal(): void {
  if (!managedRegistrationAvailable.value) return;
  showAddModal.value = false;
  managedDraft.value = {
    name: "",
    username: "",
    inviteUrl: opencodeInviteUrl.value || DEFAULT_OPENCODE_INVITE_URL,
  };
  showManagedCreate.value = true;
}

function normalizeManagedInviteDraft(): void {
  try {
    managedDraft.value.inviteUrl = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
  } catch {
    // Keep the raw value so the form can show validation feedback.
  }
}

async function ensureInviteUrlSaved(inviteUrl: string): Promise<void> {
  if (inviteUrl === opencodeInviteUrl.value) return;
  const settings = await dashboardApi.getSettings();
  await dashboardApi.updateSettings({
    ...settings,
    opencode_invite_url: inviteUrl,
  });
  opencodeInviteUrl.value = inviteUrl;
}

function setManagedCreateVisible(show: boolean): void {
  if (!show && busy.value) return;
  showManagedCreate.value = show;
}

function openManagedWizard(accountId: string): void {
  const account = accounts.value.find(({ id }) => id === accountId);
  if (!account || account.account_type !== "managed" || accountIsReady(account)) return;
  managedWizardAccountId.value = accountId;
  showManagedWizard.value = true;
}

function openInviteUrl(): void {
  showAddModal.value = false;
  const url = applyAppViewSearchParams(new URL(window.location.href), "providers", {
    provider: DEFAULT_PROVIDER_ID,
    tab: "settings",
  });
  url.searchParams.delete("session");
  url.hash = "";
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function openEditModal(id: string): void {
  editingAccount.value = accounts.value.find((account) => account.id === id) ?? null;
  showModal.value = true;
}

function clearAccountDeepLink(): void {
  const url = new URL(window.location.href);
  if (!url.searchParams.has("account_id")) return;
  url.searchParams.delete("account_id");
  window.history.replaceState(null, "", url);
}

function setAccountFormVisible(show: boolean): void {
  showModal.value = show;
}

function applyAccountDeepLink(): void {
  const accountId = readAccountDeepLink(window.location.search);
  if (!accountId) return;
  const account = accounts.value.find((item) => item.id === accountId);
  if (!account) {
    clearAccountDeepLink();
    message.warning(t("未找到指定账号，已清除链接参数"));
    return;
  }
  editingAccount.value = account;
  showModal.value = true;
}

function applyCachedAccountDeepLink(): void {
  const accountId = readAccountDeepLink(window.location.search);
  if (!accountId) return;
  const account = accountsStore.byId.get(accountId);
  if (!account) return;
  editingAccount.value = account;
  showModal.value = true;
}

watch(showModal, (show) => {
  if (!show) clearAccountDeepLink();
});

watch(showAddModal, (show) => {
  if (!show) addInitialOptionId.value = null;
});

async function createManagedAccount(): Promise<void> {
  const name = managedDraft.value.name.trim();
  if (!name || busy.value || !managedRegistrationAvailable.value || !canCreateManagedDraft.value) {
    return;
  }
  let inviteUrl = "";
  try {
    inviteUrl = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
  } catch (error) {
    message.error(error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"));
    return;
  }
  if (!inviteUrl) {
    message.error(t("请填写邀请链接"));
    return;
  }
  managedDraft.value.inviteUrl = inviteUrl;
  busy.value = true;
  try {
    await ensureInviteUrlSaved(inviteUrl);
    const username = managedDraft.value.username.trim();
    const created = await dashboardApi.createManagedAccount({
      name,
      ...(username ? { username } : {}),
    });
    addAccount(created);
    showManagedCreate.value = false;
    managedWizardAccountId.value = created.id;
    showManagedWizard.value = true;
    message.success(t("注册草稿已创建"));
  } catch (error) {
    if (await recoverAccountMutationConflict(error)) return;
    message.error(t("创建注册草稿失败: {error}", { error: dashboardErrorDetail(error) }));
  } finally {
    busy.value = false;
  }
}

async function advanceManagedSetup(accountId: string, setupStep: AccountSetupStep): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  try {
    const updated = await dashboardApi.advanceAccountSetup(accountId, setupStep);
    replaceAccount(updated);
    message.success(t("注册进度已保存"));
  } catch (error) {
    if (await recoverAccountMutationConflict(error)) return;
    await recoverManagedSetupConflict(accountId, error);
    message.error(t("保存注册进度失败: {error}", { error: dashboardErrorDetail(error) }));
  } finally {
    busy.value = false;
  }
}

async function verifyManagedKey(accountId: string, key: string): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  try {
    const updated = await dashboardApi.verifyManagedAccountKey(accountId, key);
    replaceAccount(updated);
    if (accountIsReady(updated)) {
      showManagedWizard.value = false;
      await loadAccountUsage(updated.id);
      message.success(isCooling(updated, now.value)
        ? t("Key 有效，账号已启用并按上游响应进入冷却")
        : t("Key 验证成功，账号已启用"));
    }
  } catch (error) {
    if (await recoverAccountMutationConflict(error)) return;
    await recoverManagedSetupConflict(accountId, error);
    message.error(t("Key 验证失败: {error}", { error: dashboardErrorDetail(error) }));
  } finally {
    busy.value = false;
  }
}

async function openAccountBrowser(accountId: string, target: BrowserTarget): Promise<void> {
  if (openingBrowserTarget.value) return;
  if (browserCapabilities.value.mode === "unsupported") {
    message.error(browserCapabilities.value.reason || t("当前环境不支持独立浏览器"));
    return;
  }
  let remoteTab: Window | null = null;
  if (browserCapabilities.value.mode === "remote") {
    remoteTab = window.open("", "_blank");
    if (!remoteTab) {
      message.error(t("浏览器阻止了新标签页，请允许此站点打开弹出窗口"));
      return;
    }
    remoteTab.opener = null;
  }
  openingBrowserTarget.value = target;
  try {
    const result = await dashboardApi.openAccountBrowser(accountId, target);
    if (result.mode === "remote") {
      if (!result.session_token) throw new Error(t("服务未返回远程浏览器会话令牌"));
      if (!remoteTab) throw new Error(t("浏览器模式已变化，请重试"));
      remoteTab.location.replace(browserViewUrl(window.location.href, result.session_token));
      message.success(t("远程浏览器已在新标签页打开"));
    } else {
      remoteTab?.close();
      message.success(t("已使用该账号的独立 Profile 打开浏览器"));
    }
  } catch (error) {
    remoteTab?.close();
    message.error(t("打开浏览器失败: {error}", { error: dashboardErrorDetail(error) }));
  } finally {
    openingBrowserTarget.value = null;
  }
}

async function resetBrowserProfile(accountId: string): Promise<void> {
  try {
    const updated = await dashboardApi.resetAccountBrowserProfile(accountId);
    replaceAccount(updated);
    if (!accountIsReady(updated)) {
      delete usageMap.value[accountId];
      delete usageEdits.value[accountId];
    }
    message.success(t("官网登录状态已重置"));
  } catch (error) {
    if (await recoverAccountMutationConflict(error)) return;
    message.error(t("重置官网登录状态失败: {error}", { error: dashboardErrorDetail(error) }));
  }
}

function replaceAccount(account: Account): void {
  accounts.value = accounts.value.map((item) => (item.id === account.id ? account : item));
  if (editingAccount.value?.id === account.id) editingAccount.value = account;
}

function addAccount(account: Account): void {
  accounts.value = [...accounts.value, account];
}

function removeAccountState(id: string): void {
  accounts.value = accounts.value.filter((item) => item.id !== id);
  delete usageMap.value[id];
  delete providerUsageMap.value[id];
  delete usageEdits.value[id];
  delete usageLoading.value[id];
  delete usageLoadErrors.value[id];
  if (testingAccountId.value === id) testingAccountId.value = null;
  delete providerSettingsSaving.value[id];
  delete purchaseDateSaving.value[id];
}

function accountHasUsageDisplay(account: Account): boolean {
  return isCommandCodeGoatAccount(account)
    || isOfficialCnPlanAccount(account)
    || isOllamaCloudAccount(account)
    || account.provider_id === "opencode";
}

async function refreshAccountState(id: string): Promise<Account | null> {
  const loaded = await accountsStore.loadPresented();
  accounts.value = loaded;
  const account = loaded.find((item) => item.id === id);
  if (!account) {
    removeAccountState(id);
    message.warning(t("未找到该账号，已为你刷新列表"));
    return null;
  }
  if (accountIsReady(account) && accountHasUsageDisplay(account)) {
    await loadAccountUsage(id);
  } else {
    delete usageMap.value[id];
    delete providerUsageMap.value[id];
    delete usageEdits.value[id];
  }
  return account;
}

async function recoverManagedSetupConflict(accountId: string, error: unknown): Promise<void> {
  if (!(error instanceof DashboardRequestError) || ![404, 409].includes(error.status)) return;
  try {
    const account = await refreshAccountState(accountId);
    if (!account || accountIsReady(account)) {
      showManagedWizard.value = false;
      managedWizardAccountId.value = null;
    }
  } catch {
    // Preserve the original mutation error; the next explicit refresh can retry reconciliation.
  }
}

async function loadAccounts() {
  accountListLoading.value = true;
  accountListError.value = "";
  try {
    const loaded = await accountsStore.loadPresented();
    accounts.value = loaded;
    applyAccountDeepLink();
    // 限流并发拉取用量，避免账号多时 N 次请求同时打到后端；Zen Free 无 Key 维度用量。
    // GOAT 的本地估算不依赖 OpenCode Go 定价快照是否加载成功。
    if (
      quotaLimits.value
      || loaded.some(isCommandCodeGoatAccount)
      || loaded.some(isOfficialCnPlanAccount)
      || loaded.some(isOllamaCloudAccount)
    ) {
      await mapWithConcurrency(
        loaded.filter((account) => (
          accountIsReady(account)
          && (
            isCommandCodeGoatAccount(account)
            || isOfficialCnPlanAccount(account)
            || isOllamaCloudAccount(account)
            || (
              quotaLimits.value
              && account.provider_id === "opencode"
            )
          )
        )),
        4,
        (account) => loadAccountUsage(account.id),
      );
    }
  } catch (e) {
    accountListError.value = dashboardErrorDetail(e);
    message.error(t("加载账号失败: {error}", { error: accountListError.value }));
  } finally {
    accountListLoading.value = false;
  }
}

async function loadRegistrationOptions(): Promise<void> {
  const [settingsResult, browserResult] = await Promise.allSettled([
    dashboardApi.getSettings(),
    dashboardApi.getBrowserCapabilities(),
  ]);
  if (settingsResult.status === "fulfilled") {
    opencodeInviteUrl.value = settingsResult.value.opencode_invite_url || "";
  } else {
    opencodeInviteUrl.value = "";
  }
  if (browserResult.status === "fulfilled") {
    browserCapabilities.value = browserResult.value;
  } else {
    browserCapabilities.value = {
      mode: "unsupported",
      reason: t("浏览器能力检测失败: {error}", { error: dashboardErrorDetail(browserResult.reason) }),
    };
  }
}

async function loadProviderCatalog(): Promise<void> {
  catalogLoading.value = true;
  catalogError.value = "";
  try {
    providerCatalog.value = await providerApi.getProviderCatalog();
  } catch (e) {
    providerCatalog.value = null;
    catalogError.value = dashboardErrorDetail(e);
    // Fail closed: the add modal will fall back to the legacy OpenCode Go flow
    // so the primary creation path keeps working even when the catalog is down.
  } finally {
    catalogLoading.value = false;
  }
}

async function initializeAccounts() {
  const registrationOptions = loadRegistrationOptions();
  const catalogPromise = loadProviderCatalog();
  await loadQuotaLimits();
  await loadAccounts();
  await Promise.allSettled([registrationOptions, catalogPromise]);
}

async function onFormSave(payload: AccountInput | AccountFormPayload) {
  const editing = editingAccount.value;
  if (editing) {
    if (isCustomApiAccount(editing)) {
      // The edit form always emits the AccountFormPayload shape.
      await saveCustomAccountEdit(editing, payload as AccountFormPayload);
      return;
    }
    const update: AccountUpdate = {
      name: payload.name,
      username: payload.username ?? "",
      purchase_date: payload.purchase_date,
      notes: payload.notes ?? "",
    };
    if (payload.key !== undefined) update.key = payload.key;
    if (payload.ollama_billing_tier !== undefined) {
      update.ollama_billing_tier = payload.ollama_billing_tier;
    }
    busy.value = true;
    try {
      const saved = await dashboardApi.updateAccount(editing.id, update);
      replaceAccount(saved);
      // purchase_date defines the monthly usage window and changing it clears
      // the persisted calibration offset, so the local usage snapshot must be
      // refreshed before the edited account is shown again.
      if (accountHasUsageDisplay(saved)) await loadAccountUsage(saved.id);
      message.success(t("账号已更新"));
      showModal.value = false;
    } catch (e) {
      if (await recoverAccountMutationConflict(e)) return;
      message.error(t("保存失败: {error}", { error: dashboardErrorDetail(e) }));
    } finally {
      busy.value = false;
    }
  } else {
    const input = accountCreateRequestInput(payload as AccountInput);
    busy.value = true;
    try {
      const created = await dashboardApi.createAccount(input);
      addAccount(created);
      message.success(t("账号已添加"));
      // Go uses official usage; GOAT and Ollama project locally priced OCG request logs.
      if (accountHasUsageDisplay(created) && accountIsReady(created)) {
        await loadAccountUsage(created.id);
      }
      showModal.value = false;
      // Create payloads arrive from the Add Account chooser's embedded form;
      // only a successful create closes it, so a failed save keeps the draft.
      showAddModal.value = false;
    } catch (e) {
      if (await recoverAccountMutationConflict(e)) return;
      message.error(t("保存失败: {error}", { error: dashboardErrorDetail(e) }));
    } finally {
      busy.value = false;
    }
  }
}

async function updatePurchaseDate(accountId: string, purchaseDate: string): Promise<void> {
  const account = accounts.value.find((item) => item.id === accountId);
  if (
    !account
    || !accountIsReady(account)
    || isCustomApiAccount(account)
    || isZenFreeAccount(account)
    || busy.value
    || purchaseDateSaving.value[accountId]
  ) return;

  purchaseDateSaving.value[accountId] = true;
  try {
    const saved = await dashboardApi.updateAccount(accountId, {
      purchase_date: purchaseDate,
    });
    replaceAccount(saved);
    if (accountHasUsageDisplay(saved)) await loadAccountUsage(saved.id);
    message.success(t("购买日期已更新"));
  } catch (error) {
    if (!(await recoverAccountMutationConflict(error))) {
      message.error(t("保存失败: {error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    purchaseDateSaving.value[accountId] = false;
  }
}

function openAccountTest(id: string) {
  if (!accounts.value.some((account) => account.id === id)) return;
  testingAccountId.value = id;
}

function setAccountTestVisible(show: boolean) {
  if (!show) testingAccountId.value = null;
}

/**
 * Custom edits validate the whole form before any mutation, then write only
 * the sections that changed. This preserves a verified connection for a
 * metadata-only edit and avoids unnecessary verification invalidation.
 */
async function saveCustomAccountEdit(
  editing: Account,
  payload: AccountFormPayload,
): Promise<void> {
  busy.value = true;
  try {
    await executeCustomAccountEdit(editing, payload, {
      account: async (update) => {
        replaceAccount(await dashboardApi.updateAccount(editing.id, update));
      },
      customConfig: async (config) => {
        replaceAccount(await dashboardApi.updateAccountCustomConfig(editing.id, config));
      },
    });

    message.success(t("账号已更新"));
    showModal.value = false;
  } catch (e) {
    if (await recoverAccountMutationConflict(e)) return;
    message.error(t("保存失败: {error}", { error: dashboardErrorDetail(e) }));
    try {
      await refreshAccountState(editing.id);
    } catch {
      // Keep the original save error; the next explicit refresh reconciles.
    }
  } finally {
    busy.value = false;
  }
}

async function toggleAccount(id: string) {
  const account = accounts.value.find((item) => item.id === id);
  // The Zen Free singleton only accepts the dedicated provider-settings write;
  // never fall back to the generic account PATCH/toggle for it.
  if (account && isZenFreeAccount(account)) {
    await saveZenProviderSettings(account, !account.enabled);
    return;
  }
  try {
    const updated = await dashboardApi.toggleAccount(id);
    replaceAccount(updated);
  } catch (e) {
    if (await recoverAccountMutationConflict(e)) return;
    message.error(t("切换失败: {error}", { error: dashboardErrorDetail(e) }));
  }
}

async function reloadAfterControlPlaneConflict(): Promise<void> {
  const knownIds = new Set(accounts.value.map(({ id }) => id));
  let loaded: Account[];
  try {
    loaded = await accountsStore.loadPresented();
  } catch {
    return;
  }

  const loadedIds = new Set(loaded.map(({ id }) => id));
  for (const id of knownIds) {
    if (!loadedIds.has(id)) removeAccountState(id);
  }
  accounts.value = loaded;
  if (editingAccount.value) {
    const stillListed = reconcileEditingAccount(loaded, editingAccount.value.id);
    editingAccount.value = stillListed;
    // The form derives edit-vs-create from account presence; without closing
    // the modal it would morph into the create form for a deleted account.
    if (!stillListed) showModal.value = false;
  }
  if (managedWizardAccountId.value && !loadedIds.has(managedWizardAccountId.value)) {
    showManagedWizard.value = false;
    managedWizardAccountId.value = null;
  }
}

async function recoverAccountMutationConflict(error: unknown): Promise<boolean> {
  // Only a CAS revision conflict reloads the world. Domain 409s (enable before
  // verify, reorder set mismatch, refresh already running, …) keep the actual
  // backend message and the user's draft instead of a misleading reload.
  if (!isRevisionConflict(error)) return false;
  await reloadAfterControlPlaneConflict();
  message.warning(t("账号设置已被其他操作修改，已重新加载最新状态，请重试"));
  return true;
}

/**
 * The Zen card's enabled switch writes through the dedicated provider-settings
 * endpoint with the latest settings revision attached. A 409 means the settings were changed elsewhere:
 * reload accounts/settings and ask the user to retry, in the same style as the
 * settings-page conflict recovery.
 */
async function saveZenProviderSettings(
  account: Account,
  enabled: boolean,
  successMessage?: string,
): Promise<void> {
  if (providerSettingsSaving.value[account.id]) return;
  providerSettingsSaving.value[account.id] = true;
  try {
    const result = await providerApi.updateProviderSettings(account.id, {
      enabled,
    });
    replaceAccount(result.account);
    if (successMessage) message.success(successMessage);
  } catch (error) {
    if (!(await recoverAccountMutationConflict(error))) {
      message.error(t("保存失败: {error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    providerSettingsSaving.value[account.id] = false;
  }
}

async function deleteAccount(id: string) {
  try {
    await dashboardApi.deleteAccount(id);
    message.success(t("账号已删除"));
    removeAccountState(id);
  } catch (e) {
    if (await recoverAccountMutationConflict(e)) return;
    message.error(t("删除失败: {error}", { error: dashboardErrorDetail(e) }));
  }
}

async function resetCooldown(id: string) {
  try {
    const updated = await dashboardApi.resetAccountCooldown(id);
    replaceAccount(updated);
    message.success(t("已重置冷却"));
  } catch (e) {
    if (await recoverAccountMutationConflict(e)) return;
    message.error(t("重置失败: {error}", { error: dashboardErrorDetail(e) }));
  }
}

let clock: number | undefined;
let activatedOnce = false;

function startClock() {
  if (clock === undefined) {
    clock = window.setInterval(() => {
      now.value = Date.now();
    }, 15_000);
  }
}

function stopClock() {
  if (clock !== undefined) {
    window.clearInterval(clock);
    clock = undefined;
  }
}

onMounted(() => {
  applyAccountAddDeepLink();
  void initializeAccounts();
});
// This view is kept alive by App.vue; coarse states (cooling tags, editor
// enablement) recompute on a 15s clock, while UsageStrip keeps its own 1s
// countdown. Returning to the view refreshes server-side cooldown changes.
onActivated(() => {
  startClock();
  now.value = Date.now();
  applyAccountAddDeepLink();
  applyCachedAccountDeepLink();
  if (activatedOnce) {
    void initializeAccounts();
    platformSectionRef.value?.reload();
  } else activatedOnce = true;
});
onDeactivated(stopClock);
onUnmounted(() => {
  stopClock();
  revertActiveDrag();
});
</script>

<style scoped>
.accounts-view {
  position: relative;
  max-width: 1280px;
  margin: 0 auto;
}

.accounts-content {
  position: relative;
  z-index: 1;
}

.accounts-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  justify-content: space-between;
  gap: 12px 16px;
  min-width: 0;
}

.accounts-actions {
  flex: 0 0 auto;
  margin-left: auto;
}

.account-list {
  display: grid;
  gap: 12px;
}
.account-list-state {
  min-height: 160px;
  display: grid;
  place-items: center;
}

.accounts-filter-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 12px;
  flex: 1 1 auto;
  min-width: 0;
}

.accounts-filter-bar .filter-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.accounts-filter-bar .filter-label {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  line-height: 1.2;
}

.accounts-filter-bar .n-select {
  min-width: 160px;
}

@media (max-width: 640px) {
  .accounts-toolbar {
    gap: 12px;
  }

  .accounts-filter-bar {
    flex-basis: 100%;
    gap: 8px;
  }

  .accounts-actions {
    width: 100%;
    justify-content: flex-end;
  }

  .accounts-filter-bar .filter-field {
    flex: 1 1 calc(50% - 4px);
  }

  .accounts-filter-bar .n-select {
    width: 100%;
  }
}
</style>
