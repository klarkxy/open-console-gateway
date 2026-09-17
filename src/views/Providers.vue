<template>
  <div class="providers-page">
    <div
      v-if="initialLoading"
      class="providers-state"
      role="status"
      aria-live="polite"
      :aria-label="t('加载中…')"
    >
      <n-spin size="small" />
    </div>

    <n-alert
      v-else-if="loadError && !contracts && connections.length === 0"
      type="error"
      :title="t('加载供应商失败: {error}', { error: loadError })"
    >
      <n-button size="small" secondary :loading="loading" @click="loadAll()">
        {{ t("重试") }}
      </n-button>
    </n-alert>

    <div v-else class="providers-layout">
      <aside class="providers-rail">
        <div class="providers-rail-search">
          <n-input
            v-model:value="railQuery"
            size="small"
            clearable
            :placeholder="t('搜索供应商')"
            :input-props="{ 'aria-label': t('搜索供应商') }"
          />
        </div>
        <div class="providers-rail-list">
          <section v-for="pane in railPanes" :key="pane.id" class="providers-rail-pane">
            <h3 class="providers-rail-pane__label">{{ pane.label }}</h3>
            <n-menu
              :value="selectedConnectionId"
              :options="pane.options"
              :aria-label="`${t('选择供应商范围')} · ${pane.label}`"
              @update:value="selectConnection"
            />
          </section>
          <p v-if="railFilteredOut" class="providers-rail-empty">
            {{ t("无匹配供应商") }}
          </p>
          <p v-else-if="railPanes.length === 0" class="providers-rail-empty">
            {{ t("暂无已接入的供应商") }}
          </p>
        </div>
        <div class="providers-rail-footer">
          <n-button
            secondary
            size="small"
            block
            :disabled="inlineFormBusy || addKeyBusy"
            @click="openAddFlow"
          >
            {{ t("添加供应商") }}
          </n-button>
        </div>
      </aside>

      <div class="providers-main">
        <div class="providers-mobile-nav">
          <n-select
            :value="addStage ? ADD_SELECT_VALUE : selectedConnectionId"
            :options="mobileSelectOptions"
            filterable
            :aria-label="t('选择供应商范围')"
            :disabled="actionLocked || inlineFormBusy || addKeyBusy"
            :consistent-menu-width="false"
            @update:value="onMobileSelect"
          />
        </div>

        <n-alert
          v-if="loadError && (contracts || connections.length > 0)"
          type="warning"
          :title="t('加载供应商失败: {error}', { error: loadError })"
        >
          <n-button size="small" secondary :loading="loading" @click="loadAll({ retain: true })">
            {{ t("重试") }}
          </n-button>
        </n-alert>

        <ProviderPresetBrowser
          v-if="addStage?.stage === 'browse'"
          :busy="inlineFormBusy"
          @select="onPresetBrowserSelect"
          @cancel="exitAddFlow"
        />

        <section v-else-if="addStage?.stage === 'form'" class="providers-section" aria-labelledby="add-provider-title">
          <div class="providers-catalog-head">
            <div class="providers-catalog-heading">
              <h2 id="add-provider-title">{{ addPreset ? addPreset.name : t("手动配置") }}</h2>
              <div v-if="addPreset" class="providers-catalog-meta">
                <n-tag size="small" :bordered="false">
                  {{ providerPresetOffering(addPreset) === "plan" ? "Plan" : "API" }}
                </n-tag>
                <n-tag size="small" :bordered="false">{{ t("供应商预设") }}</n-tag>
                <a :href="addPreset.docsUrl" target="_blank" rel="noopener noreferrer">{{ t("官方文档") }}</a>
                <a :href="addPreset.websiteUrl" target="_blank" rel="noopener noreferrer">{{ t("控制台") }}</a>
              </div>
            </div>
            <n-button secondary size="small" :disabled="inlineFormBusy || addKeyBusy" @click="exitAddFlow">
              {{ t("返回") }}
            </n-button>
          </div>
          <DynamicProviderModal
            :key="addFormKey"
            embedded
            :show="true"
            :provider="null"
            :initial-preset-id="addStage.presetId"
            :preset-selection-locked="Boolean(addStage.presetId)"
            @saved="onDynamicSaved"
            @committed="onDynamicCommitted"
            @conflict="onDynamicConflict"
            @busy-change="inlineFormBusy = $event"
          />
        </section>

        <section
          v-else-if="selectedConnection && isCustomAccountConnection"
          class="providers-section"
          aria-labelledby="provider-detail-title"
        >
          <div class="providers-catalog-head">
            <div class="providers-catalog-heading providers-detail-heading">
              <ProviderBrandMark :family="selectedConnectionFamily" :size="22" />
              <h2 id="provider-detail-title">{{ selectedConnection.name }}</h2>
              <div class="providers-catalog-meta">
                <n-tag size="small" :bordered="false">{{ t("Custom API 账号") }}</n-tag>
                <n-tag
                  v-if="selectedStatus.label"
                  size="small"
                  :type="selectedStatus.kind === 'missing_credential' || selectedStatus.kind === 'draft' ? 'warning' : 'default'"
                  :bordered="false"
                >{{ statusLabelText(selectedStatus.label) }}</n-tag>
              </div>
            </div>
          </div>
          <dl class="providers-connection-facts" :aria-label="t('连接信息')">
            <div class="providers-connection-facts__row">
              <dt>{{ t("API 地址") }}</dt>
              <dd><code>{{ customAccountEndpoint || t("未设置") }}</code></dd>
            </div>
            <div class="providers-connection-facts__row">
              <dt>{{ t("上游协议") }}</dt>
              <dd>{{ customAccountProtocol }}</dd>
            </div>
            <div class="providers-connection-facts__row">
              <dt>{{ t("凭据数量") }}</dt>
              <dd>{{ selectedConnection.credential_count }}</dd>
            </div>
            <div class="providers-connection-facts__row">
              <dt>{{ t("模型数量") }}</dt>
              <dd>{{ selectedConnection.target_count }}</dd>
            </div>
          </dl>
          <div class="providers-connection-targets">
            <table class="providers-connection-table">
              <thead>
                <tr>
                  <th>{{ t("对外模型名") }}</th>
                  <th>{{ t("上游模型 ID") }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="target in selectedConnection.targets" :key="target.id">
                  <td><code>{{ target.public_name }}</code></td>
                  <td><code>{{ target.upstream_model_id }}</code></td>
                </tr>
              </tbody>
            </table>
          </div>
          <n-button type="primary" size="small" @click="openAccountEditor(selectedConnection.legacy.id)">
            {{ t("在账号页编辑") }}
          </n-button>
        </section>

        <section v-else-if="selectedEntry" class="providers-section" aria-labelledby="provider-detail-title">
          <div class="providers-catalog-head">
            <div class="providers-catalog-heading providers-detail-heading">
              <ProviderBrandMark :family="selectedConnectionFamily" :size="22" />
              <h2 id="provider-detail-title">{{ selectedEntry.display_name }}</h2>
              <div class="providers-catalog-meta">
                <n-tag size="small" :bordered="false">{{ originLabel(selectedEntry.origin) }}</n-tag>
                <n-tag
                  v-if="selectedStatus.label"
                  size="small"
                  :type="selectedStatus.kind === 'missing_credential' || selectedStatus.kind === 'draft' ? 'warning' : 'default'"
                  :bordered="false"
                >{{ statusLabelText(selectedStatus.label) }}</n-tag>
              </div>
            </div>
            <n-space>
              <n-button
                v-if="isDraftConnection"
                type="primary"
                :disabled="actionLocked || definitionLoading || !selectedDefinition"
                @click="openContinueSetup"
              >
                {{ t("继续设置") }}
              </n-button>
              <n-button
                v-else-if="selectedEntry.editable"
                secondary
                :disabled="actionLocked || definitionLoading || !selectedDefinition"
                @click="openEdit"
              >
                {{ t("编辑供应商") }}
              </n-button>
              <n-popconfirm
                v-if="selectedEntry.deletable"
                :positive-text="t('删除')"
                :negative-text="t('取消')"
                @positive-click="deleteSelected"
              >
                <template #trigger>
                  <n-button type="error" secondary :disabled="actionLocked">{{ t("删除供应商") }}</n-button>
                </template>
                {{ t("请先删除引用该供应商的账号，再删除供应商。不会级联删除账号。") }}
              </n-popconfirm>
            </n-space>
          </div>

          <n-alert
            v-if="isDraftConnection"
            type="warning"
            class="providers-definition-error"
            :title="t('草稿')"
          >
            <p class="providers-note">{{ t("此连接仍是草稿，不参与路由。继续设置可补全模型与 Key；不会自动测试。") }}</p>
            <n-space>
              <n-button
                size="small"
                type="primary"
                :disabled="actionLocked || definitionLoading || !selectedDefinition"
                @click="openContinueSetup"
              >
                {{ t("继续设置") }}
              </n-button>
            </n-space>
          </n-alert>

          <n-alert
            v-else-if="selectedStatus.kind === 'missing_credential'"
            type="warning"
            class="providers-definition-error"
            :title="t('待补充凭据')"
          >
            <p class="providers-note">{{ t("此连接已保存，但还没有 Key，暂不参与路由。添加 Key 后即可使用；不会自动测试。") }}</p>
            <n-space>
              <n-button
                v-if="canAddKey"
                size="small"
                type="primary"
                secondary
                :disabled="actionLocked || inlineFormBusy || addKeyBusy"
                @click="openAddKey"
              >
                {{ t("添加 Key") }}
              </n-button>
              <n-button size="small" secondary :disabled="addKeyBusy" @click="openAccounts">
                {{ t("打开账号页") }}
              </n-button>
            </n-space>
          </n-alert>

          <n-alert
            v-else-if="selectedStatus.kind === 'disabled' || selectedStatus.kind === 'invalid' || selectedStatus.kind === 'cooling'"
            type="info"
            class="providers-definition-error"
            :title="statusLabelText(selectedStatus.label)"
          >
            <n-button size="small" secondary @click="openAccounts">
              {{ t("打开账号页") }}
            </n-button>
          </n-alert>

          <n-alert
            v-if="definitionError"
            type="error"
            class="providers-definition-error"
            :title="t('加载供应商失败: {error}', { error: definitionError })"
          >
            <n-button size="small" secondary :loading="definitionLoading" @click="retryDefinition">
              {{ t("重试") }}
            </n-button>
          </n-alert>

          <n-tabs v-model:value="activeTab" class="providers-tabs" display-directive="if">
            <n-tab-pane name="models" :tab="t('模型')">
              <template v-if="activeScope">
                <div class="providers-models-head">
                  <div class="providers-catalog-meta">
                    <span>{{ catalogSourceLabel(activeScope.catalog.source) }}</span>
                    <a
                      v-if="safeSourceUrl"
                      :href="safeSourceUrl"
                      target="_blank"
                      rel="noopener noreferrer"
                    >{{ t("官方来源") }}</a>
                    <span v-if="activeScope.catalog.refreshed_at">
                      {{ t("刷新时间") }} · {{ formatDateTime(activeScope.catalog.refreshed_at) }}
                    </span>
                  </div>
                  <div class="providers-catalog-actions">
                    <n-button
                      v-if="catalogRefreshVisible"
                      type="primary"
                      size="small"
                      :loading="catalogRefreshing"
                      :disabled="actionLocked"
                      @click="refreshCatalog"
                    >
                      {{ catalogRefreshing ? t("正在刷新模型目录…") : t("刷新模型目录") }}
                    </n-button>
                  </div>
                </div>
                <n-alert
                  v-if="catalogRefreshError"
                  type="error"
                  :title="t('刷新模型目录失败: {error}', { error: catalogRefreshError })"
                />
                <n-alert
                  v-if="probeSummary"
                  :type="probeSummary.hasFailures ? 'warning' : 'success'"
                  :title="probeSummary.hasFailures ? t('连接测试失败') : t('连接测试成功')"
                  class="providers-probe-summary"
                >
                  <div v-for="result in probeSummary.results" :key="result.protocol" class="providers-probe-result">
                    <strong>{{ protocolDisplayName(result.protocol) }}</strong>
                    <span>{{ probeResultStatus(result) }}</span>
                    <span v-if="probeResultHttpStatus(result.error)">HTTP {{ probeResultHttpStatus(result.error) }}</span>
                    <span v-if="probeResultMessage(result.error)">{{ probeResultMessage(result.error) }}</span>
                    <a
                      v-if="probeResultUrl(result.error)"
                      :href="probeResultUrl(result.error)"
                      target="_blank"
                      rel="noopener noreferrer"
                    >{{ t("帮助链接") }}</a>
                  </div>
                </n-alert>
                <n-alert
                  v-if="matrixError"
                  type="error"
                  :title="t('保存协议覆盖失败: {error}', { error: matrixError })"
                />
                <n-alert
                  v-if="probeError"
                  type="error"
                  :title="t('连接测试失败: {error}', { error: probeError })"
                />
                <ProviderModelMatrix
                  :key="activeScope.key"
                  :scope="activeScope"
                  :optimistic-overrides="optimisticOverrides"
                  :pending-override-keys="pendingOverrideKeys"
                  :probing-models="probingModels"
                  :action-locked="matrixActionLocked"
                  :removing="catalogRemoving"
                  @update:overrides="updateOverrides"
                  @probe="runModelProbe"
                  @remove="removeCatalogModels"
                  @error="matrixError = $event"
                />
              </template>

              <template v-else-if="selectedEntry.origin === 'builtin' && selectedEntry.provider_id === 'custom'">
                <p class="providers-note">
                  {{ t("模型与端点按账号配置；每个 Custom API 账号独立管理自己的连接与映射。") }}
                </p>
                <n-button secondary size="small" @click="openAccounts">
                  {{ t("打开账号页") }}
                </n-button>
              </template>

              <div v-else-if="selectedEntry.origin === 'builtin'" class="providers-state" role="status">
                <n-spin size="small" />
              </div>

              <template v-else>
                <div v-if="definitionLoading && !selectedDefinition" class="providers-state" role="status">
                  <n-spin size="small" />
                </div>
                <ProviderModelMappings
                  v-else-if="selectedDefinition"
                  :models="selectedDefinition.models"
                  :editable="selectedEntry.editable"
                  @edit="openDefinitionEditor"
                />
              </template>
            </n-tab-pane>

            <n-tab-pane name="pricing" :tab="t('模型价格')">
              <OfficialApiPanel v-if="selectedEntry.model_source === 'official_api_preset'" :provider-id="selectedEntry.provider_id" />
              <PricingCatalog v-else :provider-id="selectedEntry.provider_id" />
            </n-tab-pane>

            <n-tab-pane name="settings" :tab="t('设置')">
              <ProviderSettingsPanel
                :entry="selectedEntry"
                :definition="selectedDefinition"
                :definition-loading="selectedEntry.origin !== 'builtin' && definitionLoading"
                :action-locked="actionLocked"
                @edit="openDefinitionEditor"
                @delete="deleteSelected"
                @open-accounts="openAccounts"
              />
            </n-tab-pane>
          </n-tabs>
        </section>

        <section
          v-else-if="selectedConnection && isDraftConnection"
          class="providers-section"
          aria-labelledby="provider-detail-title"
        >
          <div class="providers-catalog-head">
            <div class="providers-catalog-heading providers-detail-heading">
              <ProviderBrandMark :family="selectedConnectionFamily" :size="22" />
              <h2 id="provider-detail-title">{{ selectedConnection.name }}</h2>
              <n-tag size="small" type="warning" :bordered="false">{{ t("草稿") }}</n-tag>
            </div>
            <n-space>
              <n-button type="primary" :disabled="actionLocked || definitionLoading" @click="openContinueSetup">
                {{ t("继续设置") }}
              </n-button>
              <n-popconfirm v-if="selectedDefinition?.deletable" :positive-text="t('删除')" :negative-text="t('取消')" @positive-click="deleteSelected">
                <template #trigger>
                  <n-button type="error" secondary :disabled="actionLocked">{{ t("删除供应商") }}</n-button>
                </template>
                {{ t("请先删除引用该供应商的账号，再删除供应商。不会级联删除账号。") }}
              </n-popconfirm>
            </n-space>
          </div>
          <n-alert type="warning" class="providers-definition-error" :title="t('草稿')">
            <p class="providers-note">{{ t("此连接仍是草稿，不参与路由。继续设置可补全模型与 Key；不会自动测试。") }}</p>
          </n-alert>
        </section>

        <section v-else class="providers-section" :aria-label="t('暂无已接入的供应商')">
          <n-empty :description="t('暂无已接入的供应商')">
            <template #extra>
              <p class="providers-note">{{ t("添加供应商后会出现在这里。") }}</p>
              <n-space>
                <n-button type="primary" size="small" @click="openAccounts">
                  {{ t("打开账号页") }}
                </n-button>
                <n-button secondary size="small" :disabled="inlineFormBusy || addKeyBusy" @click="openAddFlow">
                  {{ t("添加供应商") }}
                </n-button>
              </n-space>
            </template>
          </n-empty>
        </section>
      </div>
    </div>

    <DynamicProviderModal
      :show="showEditModal"
      :provider="editingDefinition"
      :resume-connection-id="resumeConnectionId"
      :has-saved-key="resumeHasSavedKey"
      @update:show="onEditModalShow"
      @saved="onDynamicSaved"
      @committed="onDynamicCommitted"
      @conflict="onDynamicConflict"
    />
    <AccountFormModal
      :show="showAddKeyModal"
      :account="null"
      :busy="addKeyBusy"
      :catalog="catalog"
      :plan="addKeyPlan"
      @update:show="onAddKeyShow"
      @save="onAddKeySave"
    />
    <span class="sr-only" aria-live="polite" aria-atomic="true">{{ actionLive }}</span>
  </div>
</template>

<script setup lang="ts">
import { computed, h, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NEmpty,
  NInput,
  NMenu,
  NPopconfirm,
  NSelect,
  NSpace,
  NSpin,
  NTabPane,
  NTabs,
  NTag,
  useMessage,
} from "naive-ui";
import type { MenuOption, SelectOption } from "naive-ui";
import type { Connection } from "../api/connections.ts";
import { DashboardRequestError, dashboardApi, type AccountInput } from "../api/dashboard";
import { isRevisionConflict, providerApi } from "../api/providers.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import type {
  ProviderDefinitionView,
  ModelProtocolOverrideUpdate,
  ProviderCatalogEntry,
  ProviderContractsResponse,
  ProtocolProbeResponse,
  ProtocolProbeResult,
} from "../api/providers.ts";
import ProviderModelMatrix from "../components/ProviderModelMatrix.vue";
import ProviderModelMappings from "../components/ProviderModelMappings.vue";
import ProviderPresetBrowser from "../components/ProviderPresetBrowser.vue";
import ProviderSettingsPanel from "../components/ProviderSettingsPanel.vue";
import PricingCatalog from "../components/PricingCatalog.vue";
import OfficialApiPanel from "../components/OfficialApiPanel.vue";
import DynamicProviderModal from "../components/DynamicProviderModal.vue";
import AccountFormModal, { type AccountFormPayload } from "../components/AccountFormModal.vue";
import ProviderBrandMark from "../components/ProviderBrandMark.vue";
import { t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { formatDateTime } from "../utils/format.ts";
import {
  applyAppViewSearchParams,
  readProviderPageQuery,
  resolveAppViewKey,
  type ProviderDetailTab,
} from "./app-navigation.ts";
import {
  applyModelContractToResponse,
  catalogRefreshSupported,
  effectiveModelTestProtocol,
  flattenProviderScopes,
  isSafeSourceUrl,
  modelProtocolOverrideKey,
  normalizeProviderContractsResponse,
  protocolDisplayName,
} from "../domain/provider-contracts.ts";
import {
  catalogEntryForConnection,
  connectionBrandFamily,
  connectionForLegacyProvider,
  connectionStatus,
  filterConnections,
  groupConnectionsByOffering,
  isOnboardingDraftConnection,
  selectedConnectionIdFromQuery,
} from "../domain/connections.ts";

import {
  catalogEntryFamily,
  providerAddStageFromQuery,
  providerAddStageToQuery,
  type ProviderAddStage,
} from "../domain/provider-catalog.ts";
import { accountCreateRequestInput } from "../domain/account-create-payload.ts";
import { providerSurfaceFromCatalog } from "../domain/plans.ts";
import {
  PROVIDER_PRESETS,
  providerPresetOffering,
} from "../domain/provider-presets.ts";
import {
  CATALOG_SOURCE_CUSTOM_DISCOVERY,
  CATALOG_SOURCE_DECLARED,
  CATALOG_SOURCE_OPENCODE_MODELS,
  CATALOG_SOURCE_COMMAND_CODE_MODELS,
  CATALOG_SOURCE_OFFICIAL_ZEN,
  CATALOG_SOURCE_STATIC,
} from "../domain/provider-contracts.ts";

const message = useMessage();
const accountsStore = useAccountsStore();
const providersStore = useProvidersStore();
const contracts = ref<ProviderContractsResponse | null>(null);
const catalog = ref<ProviderCatalogEntry[] | null>(null);
const connections = ref<Connection[]>([]);
const showEditModal = ref(false);
const editingDefinition = ref<ProviderDefinitionView | null>(null);
const resumeConnectionId = ref<string | null>(null);
const resumeHasSavedKey = ref(false);
const showAddKeyModal = ref(false);
const addKeyBusy = ref(false);
/** In-flight save/test/discovery inside the embedded create form. */
const inlineFormBusy = ref(false);
/** Add flow shown in the main pane; the rail selection is kept underneath. */
const addStage = ref<ProviderAddStage | null>(null);
const railQuery = ref("");
const loading = ref(false);
const loadError = ref("");
const selectedConnectionId = ref<string | null>(null);
const lastCommittedConnectionId = ref<string | null>(null);
const activeTab = ref<ProviderDetailTab>("models");
const definitions = ref<Map<string, ProviderDefinitionView>>(new Map());
const definitionLoading = ref(false);
const definitionError = ref("");
const catalogRefreshing = ref(false);
const catalogRemoving = ref(false);
const catalogRefreshError = ref("");
const matrixError = ref("");
const probeError = ref("");
const probeSummary = ref<{ results: ProtocolProbeResult[]; hasFailures: boolean } | null>(null);
const probingModels = ref<Set<string>>(new Set());
const optimisticOverrides = ref<Map<string, boolean>>(new Map());
const pendingOverrideKeys = ref<Set<string>>(new Set());
const actionLive = ref("");
let activatedOnce = false;
let overrideSequence = 0;
let overrideQueue: Promise<void> = Promise.resolve();
let definitionGeneration = 0;
const latestOverrideSequence = new Map<string, number>();

const RAIL_BRAND_SIZE = 18;
const ADD_SELECT_VALUE = "__add__";

const allCatalogEntries = computed(() => catalog.value ?? []);
const scopes = computed(() => (
  contracts.value
    ? flattenProviderScopes(contracts.value, catalog.value)
      .filter((scope) => scope.scope_kind === "provider")
    : []
));
const selectedConnection = computed(() => (
  connections.value.find((item) => item.id === selectedConnectionId.value) ?? null
));
const selectedEntry = computed(() => {
  const connection = selectedConnection.value;
  if (!connection) return null;
  return catalogEntryForConnection(connection, allCatalogEntries.value);
});
const isCustomAccountConnection = computed(() => (
  selectedConnection.value?.legacy.kind === "custom_account"
));
const selectedStatus = computed(() => (
  selectedConnection.value
    ? connectionStatus(selectedConnection.value)
    : { kind: "ok" as const, label: null }
));
const selectedConnectionFamily = computed(() => (
  selectedConnection.value
    ? connectionBrandFamily(selectedConnection.value, allCatalogEntries.value)
    : catalogEntryFamily({ provider_id: "", display_family: "", display_name: "" })
));
const customAccountEndpoint = computed(() => (
  selectedConnection.value?.endpoints.find((endpoint) => endpoint.url)?.url ?? ""
));
const customAccountProtocol = computed(() => {
  const protocol = selectedConnection.value?.endpoints[0]?.wire_protocol;
  return protocol ? protocolDisplayName(protocol) : t("未设置");
});
const addKeyPlan = computed(() => {
  const entry = selectedEntry.value;
  if (!entry) return null;
  return providerSurfaceFromCatalog(entry);
});
const isDraftConnection = computed(() => (
  selectedConnection.value ? isOnboardingDraftConnection(selectedConnection.value) : false
));
const canAddKey = computed(() => {
  const entry = selectedEntry.value;
  return Boolean(
    entry
    && !isDraftConnection.value
    && entry.credential_kind !== "none"
    && entry.creation_availability === "available"
    && addKeyPlan.value
  );
});
const selectedDefinition = computed(() => {
  const providerId = selectedEntry.value?.provider_id
    ?? (selectedConnection.value?.legacy.kind === "dynamic_provider" ? selectedConnection.value.legacy.id : null);
  return providerId ? definitions.value.get(providerId) ?? null : null;
});
const activeScope = computed(() => {
  const entry = selectedEntry.value;
  if (!entry || entry.origin !== "builtin" || entry.provider_id === "custom") return null;
  return scopes.value.find((scope) => scope.provider_id === entry.provider_id) ?? null;
});
const addPreset = computed(() => {
  const stage = addStage.value;
  if (!stage || stage.stage !== "form" || !stage.presetId) return null;
  return PROVIDER_PRESETS.find((preset) => preset.id === stage.presetId) ?? null;
});
const addFormKey = computed(() => {
  const stage = addStage.value;
  return stage?.stage === "form" ? `add-form:${stage.presetId ?? "manual"}` : "add-form:none";
});
const initialLoading = computed(() => (
  loading.value && !contracts.value && !catalog.value && connections.value.length === 0 && !loadError.value
));
const actionLocked = computed(() => (
  catalogRefreshing.value
  || catalogRemoving.value
  || probingModels.value.size > 0
  || pendingOverrideKeys.value.size > 0
));
const matrixActionLocked = computed(() => (
  catalogRefreshing.value
  || catalogRemoving.value
  || probingModels.value.size > 0
));

function originLabel(origin: ProviderCatalogEntry["origin"]): string {
  if (origin === "custom") return t("自定义");
  return t("供应商预设");
}

function statusLabelText(label: string | null): string {
  return label ? t(label as MessageKey) : "";
}

function railStatusExtra(connection: Connection) {
  const label = connectionStatus(connection).label;
  if (!label) return undefined;
  return () => h("span", {
    style: {
      fontSize: "var(--ocg-font-xs)",
      color: "var(--ocg-muted)",
      fontWeight: "400",
    },
  }, t(label as MessageKey));
}

const railPanes = computed<Array<{ id: "plan" | "api"; label: "Plan" | "API"; options: MenuOption[] }>>(() => {
  const filtered = filterConnections(connections.value, railQuery.value);
  const groups = groupConnectionsByOffering(filtered);
  const toOptions = (list: readonly Connection[]): MenuOption[] => (
    list.map((item) => ({
      key: item.id,
      label: item.name,
      icon: () => h(ProviderBrandMark, {
        family: connectionBrandFamily(item, allCatalogEntries.value),
        size: RAIL_BRAND_SIZE,
      }),
      extra: railStatusExtra(item),
    }))
  );
  const panes: Array<{ id: "plan" | "api"; label: "Plan" | "API"; options: MenuOption[] }> = [];
  const planOptions = toOptions(groups.plan);
  const apiOptions = toOptions(groups.api);
  if (planOptions.length) panes.push({ id: "plan", label: "Plan", options: planOptions });
  if (apiOptions.length) panes.push({ id: "api", label: "API", options: apiOptions });
  return panes;
});
const railFilteredOut = computed(() => (
  Boolean(railQuery.value.trim())
  && railPanes.value.every((pane) => pane.options.length === 0)
));
const mobileSelectOptions = computed<SelectOption[]>(() => {
  // The mobile selector has its own built-in filter; the rail search query
  // must not shrink these options when the rail itself is hidden.
  const groups = groupConnectionsByOffering(connections.value);
  const labelFor = (item: Connection, offering: "Plan" | "API"): string => {
    const status = connectionStatus(item);
    return status.label
      ? `${item.name} · ${offering} · ${t(status.label as MessageKey)}`
      : `${item.name} · ${offering}`;
  };
  return [
    ...groups.plan.map((item) => ({
      value: item.id,
      label: labelFor(item, "Plan"),
    })),
    ...groups.api.map((item) => ({
      value: item.id,
      label: labelFor(item, "API"),
    })),
    { value: ADD_SELECT_VALUE, label: t("添加供应商") },
  ];
});
const catalogRefreshVisible = computed(() => {
  const scope = activeScope.value;
  return Boolean(scope && catalogRefreshSupported(scope));
});
const safeSourceUrl = computed(() => {
  const url = activeScope.value?.catalog.source_url ?? "";
  return isSafeSourceUrl(url) ? url : "";
});

function catalogSourceLabel(source: string): string {
  if (source === CATALOG_SOURCE_STATIC) return t("静态目录");
  if (source === CATALOG_SOURCE_OFFICIAL_ZEN) return t("官方 Zen 目录");
  if (source === CATALOG_SOURCE_CUSTOM_DISCOVERY) return t("自定义发现");
  if (source === CATALOG_SOURCE_DECLARED) return t("账号声明");
  if (source === CATALOG_SOURCE_OPENCODE_MODELS) return `OpenCode · ${t("官方来源")}`;
  if (source === CATALOG_SOURCE_COMMAND_CODE_MODELS) return `Command Code · ${t("官方来源")}`;
  return source;
}

/**
 * This view stays mounted under KeepAlive after the user leaves it; only
 * touch selection state or the URL when the current URL actually targets it.
 * Legacy "pricing" resolves to providers, so bookmarks keep working.
 */
function currentUrlIsProvidersView(): boolean {
  const view = new URL(window.location.href).searchParams.get("view");
  return resolveAppViewKey(view) === "providers";
}

function writeUrl() {
  // An in-flight load finishing after navigation must not rewrite the URL
  // (e.g. strip the one-shot Accounts `add` deep link) for another view.
  if (!currentUrlIsProvidersView()) return;
  const stage = addStage.value;
  const url = applyAppViewSearchParams(new URL(window.location.href), "providers", {
    ...(selectedConnectionId.value ? { connection: selectedConnectionId.value } : {}),
    ...(stage
      ? providerAddStageToQuery(stage)
      : activeTab.value !== "models" ? { tab: activeTab.value } : {}),
  });
  window.history.replaceState(null, "", url);
}

function applyFromQuery(
  fellBackNotice = false,
  prefer?: { connectionId?: string; providerId?: string },
) {
  const query = readProviderPageQuery(window.location.search);
  addStage.value = providerAddStageFromQuery(query.add, query.preset);
  const wanted = selectedConnectionIdFromQuery({
    connection: prefer?.connectionId ?? query.connection,
    provider: prefer?.providerId ?? query.provider,
  }, connections.value)
    ?? selectedConnectionId.value;
  const rows = connections.value;
  if (rows.length === 0) {
    selectedConnectionId.value = null;
    writeUrl();
    return;
  }
  const row = rows.find((item) => item.id === wanted) ?? rows[0]!;
  if (fellBackNotice && wanted && row.id !== wanted) {
    actionLive.value = t("已选择过期范围，已回到第一个供应商");
  }
  selectedConnectionId.value = row.id;
  const candidate = query.tab ?? activeTab.value;
  activeTab.value = candidate;
  writeUrl();
}

function selectConnection(key: string | number) {
  // An embedded form with in-flight save/test/discovery must not be swapped
  // out; its stale-generation guards only cover responses, not dismissal.
  if (inlineFormBusy.value || addKeyBusy.value) return;
  const connectionId = String(key);
  if (!connections.value.some((item) => item.id === connectionId)) return;
  addStage.value = null;
  selectedConnectionId.value = connectionId;
  writeUrl();
}

function onMobileSelect(key: string | number) {
  const value = String(key);
  if (value === ADD_SELECT_VALUE) {
    openAddFlow();
    return;
  }
  selectConnection(value);
}

function openAddFlow() {
  if (inlineFormBusy.value || addKeyBusy.value) return;
  showEditModal.value = false;
  editingDefinition.value = null;
  addStage.value = { stage: "browse" };
  writeUrl();
}

function onPresetBrowserSelect(presetId: string | null) {
  if (inlineFormBusy.value || addKeyBusy.value) return;
  addStage.value = { stage: "form", presetId };
  writeUrl();
}

function exitAddFlow() {
  if (inlineFormBusy.value || addKeyBusy.value) return;
  addStage.value = null;
  writeUrl();
}

function openAccounts() {
  const url = applyAppViewSearchParams(new URL(window.location.href), "accounts");
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function openAccountEditor(accountId: string) {
  const url = applyAppViewSearchParams(new URL(window.location.href), "accounts");
  url.searchParams.set("account_id", accountId);
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function resetScopeActions() {
  catalogRefreshError.value = "";
  matrixError.value = "";
  probeError.value = "";
  probeSummary.value = null;
}

async function loadDefinition(providerId: string): Promise<ProviderDefinitionView | null> {
  const generation = ++definitionGeneration;
  definitionLoading.value = true;
  definitionError.value = "";
  try {
    const definition = await providerApi.getProviderDefinition(providerId);
    if (generation !== definitionGeneration) return null;
    const next = new Map(definitions.value);
    next.set(providerId, definition);
    definitions.value = next;
    return definition;
  } catch (error) {
    if (generation !== definitionGeneration) return null;
    definitionError.value = dashboardErrorDetail(error);
    return null;
  } finally {
    if (generation === definitionGeneration) definitionLoading.value = false;
  }
}

async function ensureDefinition(providerId: string) {
  if (definitions.value.has(providerId)) return;
  await loadDefinition(providerId);
}

function retryDefinition() {
  const entry = selectedEntry.value;
  if (!entry || entry.origin === "builtin") return;
  void ensureDefinition(entry.provider_id);
}

async function loadAll(options: {
  retain?: boolean;
  preferConnectionId?: string;
  preferProviderId?: string;
} = {}): Promise<{ ok: boolean; error: string }> {
  if (loading.value) {
    return { ok: false, error: loadError.value };
  }
  loading.value = true;
  if (!options.retain) loadError.value = "";
  try {
    const [contractsResult, catalogResult, connectionsResult] = await Promise.allSettled([
      providersStore.loadContracts(),
      providersStore.loadCatalog(),
      providersStore.loadConnections(),
      accountsStore.loadPresented(),
    ]);
    if (catalogResult.status === "fulfilled") {
      catalog.value = catalogResult.value;
    }
    if (connectionsResult.status === "fulfilled") {
      connections.value = connectionsResult.value;
    }
    if (contractsResult.status === "fulfilled") {
      contracts.value = normalizeProviderContractsResponse(contractsResult.value);
    }
    applyFromQuery(true, {
      connectionId: options.preferConnectionId,
      providerId: options.preferProviderId,
    });
    if (connectionsResult.status === "rejected") {
      const error = dashboardErrorDetail(connectionsResult.reason);
      loadError.value = error;
      return { ok: false, error };
    }
    if (contractsResult.status === "rejected") {
      const error = dashboardErrorDetail(contractsResult.reason);
      loadError.value = error;
      return { ok: false, error };
    }
    loadError.value = "";
    return { ok: true, error: "" };
  } finally {
    loading.value = false;
  }
}

function openDefinitionEditor(): void {
  if (isDraftConnection.value) {
    void openContinueSetup();
    return;
  }
  openEdit();
}

async function openEdit(): Promise<void> {
  const entry = selectedEntry.value;
  if (!entry?.editable || isDraftConnection.value) return;
  const definition = await loadDefinition(entry.provider_id);
  if (!definition) return;
  resumeConnectionId.value = null;
  resumeHasSavedKey.value = false;
  editingDefinition.value = definition;
  showEditModal.value = true;
}

async function openContinueSetup(): Promise<void> {
  const connection = selectedConnection.value;
  if (!connection || !isOnboardingDraftConnection(connection)) return;
  const definition = await loadDefinition(connection.legacy.id);
  if (!definition) return;
  resumeConnectionId.value = connection.id;
  resumeHasSavedKey.value = false;
  editingDefinition.value = definition;
  showEditModal.value = true;
}

function onEditModalShow(visible: boolean): void {
  showEditModal.value = visible;
  if (!visible) {
    editingDefinition.value = null;
    resumeConnectionId.value = null;
    resumeHasSavedKey.value = false;
  }
}

function onDynamicCommitted(result: { connectionId: string }): void {
  lastCommittedConnectionId.value = result.connectionId;
}

async function onDynamicSaved(providerId: string): Promise<void> {
  // Create emits `committed` then `saved`; edit emits only `saved`.
  const preferConnectionId = lastCommittedConnectionId.value ?? undefined;
  const created = preferConnectionId !== undefined && resumeConnectionId.value === null;
  lastCommittedConnectionId.value = null;
  addStage.value = null;
  resumeConnectionId.value = null;
  resumeHasSavedKey.value = false;
  const next = new Map(definitions.value);
  next.delete(providerId);
  definitions.value = next;
  const loaded = await loadAll({ retain: true, preferConnectionId, preferProviderId: providerId });
  if (!loaded.ok) {
    message.warning(t("已保存，但未能刷新列表。请手动刷新，不要再次提交。"));
    return;
  }
  const createdConnection = preferConnectionId
    ? connections.value.find((item) => item.id === preferConnectionId)
    : connectionForLegacyProvider(connections.value, providerId);
  if (createdConnection && isOnboardingDraftConnection(createdConnection)) {
    message.success(t("草稿已保存"));
  } else if (created && createdConnection && connectionStatus(createdConnection).kind === "missing_credential") {
    message.success(t("供应商已保存，待补充凭据"));
  } else {
    message.success(created ? t("供应商已创建") : t("供应商已更新"));
  }
}

function onAddKeyShow(visible: boolean): void {
  if (!visible && addKeyBusy.value) return;
  showAddKeyModal.value = visible;
}

function openAddKey(): void {
  if (inlineFormBusy.value || actionLocked.value || addKeyBusy.value || !canAddKey.value) return;
  showAddKeyModal.value = true;
}

async function onAddKeySave(payload: AccountInput | AccountFormPayload): Promise<void> {
  if (inlineFormBusy.value || actionLocked.value || addKeyBusy.value) return;
  const input = accountCreateRequestInput(payload as AccountInput);
  addKeyBusy.value = true;
  try {
    await dashboardApi.createAccount(input);
    message.success(t("账号已添加"));
    showAddKeyModal.value = false;
    await accountsStore.loadPresented();
    await loadAll({ retain: true, preferConnectionId: selectedConnectionId.value ?? undefined });
  } catch (error) {
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      await loadAll({ retain: true });
      message.warning(t("数据已更新，请检查后重新保存。不会自动重试。"));
      return;
    }
    message.error(t("保存失败: {error}", { error: dashboardErrorDetail(error) }));
  } finally {
    addKeyBusy.value = false;
  }
}

async function onDynamicConflict(): Promise<void> {
  await loadAll({ retain: true });
}

async function deleteSelected(): Promise<void> {
  const entry = selectedEntry.value;
  const definition = selectedDefinition.value;
  const providerId = entry?.provider_id ?? definition?.id;
  if (!providerId || !(entry?.deletable ?? definition?.deletable)) return;
  try {
    await providerApi.deleteProviderDefinition(providerId);
    message.success(t("供应商已删除"));
    const next = new Map(definitions.value);
    next.delete(providerId);
    definitions.value = next;
    selectedConnectionId.value = connections.value.find((item) => (
      !(item.legacy.kind === "dynamic_provider" && item.legacy.id === providerId)
    ))?.id ?? null;
    await loadAll({ retain: true });
  } catch (error) {
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      await loadAll({ retain: true });
      message.warning(t("数据已更新，请检查后重新保存。不会自动重试。"));
      return;
    }
    message.error(t("删除供应商失败: {error}", { error: dashboardErrorDetail(error) }));
  }
}

async function removeCatalogModels(payload: { modelIds: string[] }) {
  const scope = activeScope.value;
  if (!scope || catalogRemoving.value || payload.modelIds.length === 0) return;
  catalogRemoving.value = true;
  matrixError.value = "";
  try {
    const refreshed = await providersStore.removeContractCatalogModels(
      scope.scope_kind,
      scope.scope_id,
      payload.modelIds,
    );
    contracts.value = normalizeProviderContractsResponse(refreshed);
    actionLive.value = t("已从目录删除模型");
    message.success(t("已从目录删除模型"));
  } catch (error) {
    if (error instanceof DashboardRequestError && error.status === 409) {
      await loadAll({ retain: true });
      actionLive.value = t("供应商设置已在其他位置更新，已重新加载，请重试");
      message.warning(t("供应商设置已在其他位置更新，已重新加载，请重试"));
    } else {
      matrixError.value = dashboardErrorDetail(error);
      message.error(t("删除模型失败: {error}", { error: matrixError.value }));
    }
  } finally {
    catalogRemoving.value = false;
  }
}

async function refreshCatalog() {
  const scope = activeScope.value;
  if (!scope || !catalogRefreshVisible.value || catalogRefreshing.value) return;
  catalogRefreshing.value = true;
  catalogRefreshError.value = "";
  try {
    const refreshed = await providersStore.refreshContractCatalog(scope.scope_kind, scope.scope_id);
    contracts.value = normalizeProviderContractsResponse(refreshed);
    applyFromQuery();
    actionLive.value = t("已刷新模型目录");
    message.success(t("已刷新模型目录"));
  } catch (error) {
    catalogRefreshError.value = dashboardErrorDetail(error);
    message.error(t("刷新模型目录失败: {error}", { error: catalogRefreshError.value }));
  } finally {
    catalogRefreshing.value = false;
  }
}

type OverridePayload = {
  scopeKind: "provider" | "custom_endpoint";
  scopeId: string;
  overrides: ModelProtocolOverrideUpdate[];
};

function overrideKey(payload: OverridePayload, item: ModelProtocolOverrideUpdate): string {
  return modelProtocolOverrideKey(
    payload.scopeKind,
    payload.scopeId,
    item.model_id,
    item.protocol,
  );
}

function showOptimisticOverrides(payload: OverridePayload, sequence: number) {
  const nextOptimistic = new Map(optimisticOverrides.value);
  const nextPending = new Set(pendingOverrideKeys.value);
  for (const item of payload.overrides) {
    const key = overrideKey(payload, item);
    latestOverrideSequence.set(key, sequence);
    // Map the override state to the cell the operator will see before the
    // response lands: `force_on` flips the cell on, `force_off` flips it off.
    // The override builders only emit these two states.
    const optimisticValue = item.state === "force_on";
    nextOptimistic.set(key, optimisticValue);
    nextPending.add(key);
  }
  optimisticOverrides.value = nextOptimistic;
  pendingOverrideKeys.value = nextPending;
}

function settleOptimisticOverrides(payload: OverridePayload, sequence: number) {
  const nextOptimistic = new Map(optimisticOverrides.value);
  const nextPending = new Set(pendingOverrideKeys.value);
  for (const item of payload.overrides) {
    const key = overrideKey(payload, item);
    if (latestOverrideSequence.get(key) !== sequence) continue;
    latestOverrideSequence.delete(key);
    nextOptimistic.delete(key);
    nextPending.delete(key);
  }
  optimisticOverrides.value = nextOptimistic;
  pendingOverrideKeys.value = nextPending;
}

function updateOverrides(payload: OverridePayload) {
  const sequence = ++overrideSequence;
  showOptimisticOverrides(payload, sequence);
  matrixError.value = "";
  overrideQueue = overrideQueue.then(() => persistOverrides(payload, sequence));
}

async function persistOverrides(payload: OverridePayload, sequence: number) {
  try {
    const response = await providersStore.putModelProtocolOverrides(
      payload.scopeKind,
      payload.scopeId,
      payload.overrides,
    );
    contracts.value = normalizeProviderContractsResponse(response);
    actionLive.value = t("协议覆盖已保存");
  } catch (error) {
    if (error instanceof DashboardRequestError && error.status === 409) {
      await loadAll({ retain: true });
      actionLive.value = t("供应商设置已在其他位置更新，已重新加载，请重试");
      message.warning(t("供应商设置已在其他位置更新，已重新加载，请重试"));
    } else {
      matrixError.value = dashboardErrorDetail(error);
      message.error(t("保存协议覆盖失败: {error}", { error: matrixError.value }));
    }
  } finally {
    settleOptimisticOverrides(payload, sequence);
  }
}

async function runModelProbe(payload: { modelId: string }) {
  const scope = activeScope.value;
  if (!scope || actionLocked.value || probingModels.value.has(payload.modelId)) return;
  // Configured-route test only: the effective preferred protocol, or the first
  // enabled fallback when the preferred one is disabled. Never a blind scan.
  const model = scope.models.find((item) => item.model_id === payload.modelId);
  const protocol = effectiveModelTestProtocol(model);
  if (!protocol) {
    probeError.value = t("该模型没有已开启的协议；请先在矩阵中开启后再测试");
    message.warning(probeError.value);
    return;
  }
  probingModels.value = new Set(probingModels.value).add(payload.modelId);
  probeError.value = "";
  try {
    const response = await providerApi.runProtocolProbes(scope.provider_id, {
      model_id: payload.modelId,
      protocols: [protocol],
    });
    probeSummary.value = probeSummaryFromResponse(response);
    if (response.contract && contracts.value) {
      contracts.value = applyModelContractToResponse(contracts.value, {
        scope_kind: scope.scope_kind,
        scope_id: scope.scope_id,
      }, response.contract);
    }
    const loaded = await loadAll({ retain: true });
    if (!loaded.ok) {
      probeError.value = loaded.error;
      message.error(t("连接测试失败: {error}", { error: probeError.value }));
      return;
    }
    const failures = response.results.filter((result) => !result.success);
    if (failures.length > 0) {
      actionLive.value = t("连接测试失败");
      message.warning(actionLive.value);
      return;
    }
    actionLive.value = t("连接测试成功");
    message.success(t("连接测试成功"));
  } catch (error) {
    probeError.value = dashboardErrorDetail(error);
    message.error(t("连接测试失败: {error}", { error: probeError.value }));
  } finally {
    const next = new Set(probingModels.value);
    next.delete(payload.modelId);
    probingModels.value = next;
  }
}

function probeSummaryFromResponse(response: ProtocolProbeResponse) {
  return {
    results: response.results,
    hasFailures: response.results.some((result) => !result.success),
  };
}

function probeResultStatus(result: ProtocolProbeResult): string {
  if (result.success) return t("成功");
  if (result.skipped) return t("已跳过");
  return t("失败");
}

function probeErrorValue(error: string | null): { raw: string; parsed: unknown } | null {
  if (!error?.trim()) return null;
  const raw = error.trim();
  const objectStart = raw.indexOf("{");
  try {
    return { raw, parsed: JSON.parse(objectStart >= 0 ? raw.slice(objectStart) : raw) as unknown };
  } catch {
    return { raw, parsed: null };
  }
}

function nestedErrorMessage(value: unknown): string | null {
  if (typeof value === "string") return value.trim() || null;
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  for (const candidate of [record.message, record.error]) {
    const message = nestedErrorMessage(candidate);
    if (message) return message;
  }
  return null;
}

function probeResultMessage(error: string | null): string {
  const value = probeErrorValue(error);
  return nestedErrorMessage(value?.parsed) ?? value?.raw ?? "";
}

function probeResultHttpStatus(error: string | null): string {
  const match = error?.match(/\b(?:HTTP\s+|returned\s+)(\d{3})\b/i);
  return match?.[1] ?? "";
}

function findSafeHttpUrl(value: unknown): string | null {
  if (typeof value === "string") {
    const match = value.match(/https?:\/\/[^\s"'<>]+/i);
    return match && isSafeSourceUrl(match[0]) ? match[0] : null;
  }
  if (!value || typeof value !== "object") return null;
  for (const item of Object.values(value as Record<string, unknown>)) {
    const url = findSafeHttpUrl(item);
    if (url) return url;
  }
  return null;
}

function probeResultUrl(error: string | null): string {
  const value = probeErrorValue(error);
  return findSafeHttpUrl(value?.parsed) ?? findSafeHttpUrl(value?.raw) ?? "";
}

function onPopState() {
  // KeepAlive keeps this view mounted; a popstate for another view (e.g. the
  // Accounts add deep link) is not ours to apply.
  if (!currentUrlIsProvidersView()) return;
  applyFromQuery();
}

watch(selectedConnectionId, () => {
  // The embedded form unmounts on selection change; its busy flags die with
  // it, so the navigation lock must not outlive the form.
  inlineFormBusy.value = false;
  if (!addKeyBusy.value) showAddKeyModal.value = false;
});

watch(selectedConnection, (connection) => {
  if (connection?.legacy.kind === "dynamic_provider" && connection.lifecycle === "draft") {
    void ensureDefinition(connection.legacy.id);
  }
});

watch(selectedEntry, (entry, previous) => {
  if (entry?.provider_id === previous?.provider_id) return;
  resetScopeActions();
  definitionError.value = "";
  if (entry && entry.origin !== "builtin") void ensureDefinition(entry.provider_id);
});

watch([selectedConnectionId, activeTab, addStage], () => {
  writeUrl();
});

onMounted(() => {
  window.addEventListener("popstate", onPopState);
  void loadAll();
});
onActivated(() => {
  if (activatedOnce) void loadAll({ retain: true });
  else activatedOnce = true;
});
onUnmounted(() => {
  window.removeEventListener("popstate", onPopState);
});
</script>

<style scoped>
.providers-page {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  height: 100%;
  max-width: 1440px;
  margin: 0 auto;
  overflow: hidden;
}
.providers-note {
  margin: 0 0 12px;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.providers-state {
  flex: 1 1 auto;
  min-height: 160px;
  display: grid;
  place-items: center;
}
.providers-layout {
  display: grid;
  flex: 1 1 auto;
  grid-template-columns: 208px minmax(0, 1fr);
  gap: 16px;
  min-width: 0;
  min-height: 0;
}
.providers-probe-summary {
  margin: 12px 0;
}
.providers-probe-result {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: baseline;
  margin-top: 4px;
}
.providers-catalog-actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 8px;
}
.providers-rail {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  height: 100%;
  padding: 8px 0;
  overflow: hidden;
  border: 1px solid var(--ocg-border);
  border-radius: 10px;
  background: var(--ocg-surface);
}
.providers-rail-search {
  flex: none;
  padding: 0 8px 8px;
}
/* One list scrolls; the Plan / API group labels stick to its top edge instead
   of splitting the rail into two independently scrolling half-height panes. */
.providers-rail-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.providers-rail-pane + .providers-rail-pane {
  border-top: 1px solid var(--ocg-border);
}
.providers-rail-pane__label {
  position: sticky;
  top: 0;
  z-index: 1;
  margin: 0;
  padding: 4px 12px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  line-height: 1.3;
  background: var(--ocg-surface);
}
.providers-rail-footer {
  flex: none;
  padding: 8px;
  border-top: 1px solid var(--ocg-border);
}
.providers-rail-empty {
  margin: 0;
  padding: 8px 12px;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.providers-mobile-nav {
  display: none;
  min-width: 0;
  margin-bottom: 12px;
}
.providers-main {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 16px;
  min-width: 0;
  min-height: 0;
  overflow: auto;
  align-content: start;
}
.providers-tabs {
  min-width: 0;
  max-width: 100%;
}
.providers-tabs :deep(.n-tabs-nav) {
  margin-bottom: 12px;
}
.providers-section {
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--ocg-border);
  border-radius: 14px;
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.providers-section h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.providers-catalog-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 16px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--ocg-border);
}
.providers-catalog-heading {
  min-width: 0;
}
.providers-detail-heading {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px 10px;
}
.providers-catalog-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 12px;
  margin-top: 4px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}
.providers-detail-heading .providers-catalog-meta {
  margin-top: 0;
}
.providers-models-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 12px;
}
.providers-models-head .providers-catalog-meta {
  margin-top: 0;
}
.providers-definition-error {
  margin-bottom: 12px;
}
.providers-connection-facts {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 8px 16px;
  margin: 0 0 16px;
  padding: 10px 12px;
  border: 1px solid var(--ocg-border);
  border-radius: 10px;
  background: var(--ocg-canvas);
}
.providers-connection-facts__row {
  display: grid;
  gap: 2px;
  min-width: 0;
}
.providers-connection-facts dt {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.providers-connection-facts dd {
  margin: 0;
}
.providers-connection-facts code {
  overflow-wrap: anywhere;
}
.providers-connection-targets {
  overflow-x: auto;
  margin-bottom: 16px;
}
.providers-connection-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}
.providers-connection-table th,
.providers-connection-table td {
  padding: 8px 12px;
  border-bottom: 1px solid var(--ocg-border);
  text-align: left;
}
.providers-connection-table th {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
}
.providers-connection-table td code {
  overflow-wrap: anywhere;
}

@media (max-width: 720px) {
  .providers-page {
    height: auto;
    overflow: visible;
  }
  .providers-layout {
    grid-template-columns: minmax(0, 1fr);
    flex: none;
  }
  .providers-rail {
    display: none;
  }
  .providers-main {
    overflow: visible;
  }
  .providers-mobile-nav {
    display: block;
  }
  .providers-catalog-head {
    align-items: stretch;
    flex-direction: column;
  }
  .providers-models-head {
    align-items: stretch;
    flex-direction: column;
  }
}

@media (max-width: 390px) {
  .providers-page,
  .providers-layout,
  .providers-main,
  .providers-section {
    min-width: 0;
  }
}
</style>
