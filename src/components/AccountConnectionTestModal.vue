<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('测试连接：{name}', { name: account?.name ?? '' })"
    class="account-test-modal"
    style="width: 880px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    @update:show="setVisible"
  >
    <n-alert type="warning" :show-icon="false" class="test-warning">
      {{ t("测试将锁定当前账号发送最小真实请求，不会切换其他账号；可能产生少量服务商费用。") }}
    </n-alert>

    <div class="test-toolbar">
      <n-input
        v-model:value="query"
        clearable
        :disabled="testingAll"
        :placeholder="t('筛选模型')"
        :input-props="{ 'aria-label': t('筛选模型') }"
      />
      <n-popconfirm
        :positive-text="t('确认测试')"
        :negative-text="t('取消')"
        @positive-click="testFilteredModels"
      >
        <template #trigger>
          <n-button
            type="primary"
            secondary
            :disabled="filteredModels.length === 0 || testingAll"
            :loading="testingAll"
          >
            {{ t("测试全部 {count} 个模型", { count: filteredModels.length }) }}
          </n-button>
        </template>
        {{ t("将通过当前账号依次测试筛选结果中的 {count} 个模型，继续？", { count: filteredModels.length }) }}
      </n-popconfirm>
    </div>

    <n-alert v-if="loadError" type="error" :show-icon="false" class="test-error">
      {{ loadError }}
    </n-alert>

    <n-spin :show="loadingModels">
      <n-empty
        v-if="!loadingModels && filteredModels.length === 0"
        :description="query ? t('没有匹配的模型') : t('当前账号没有可测试模型')"
      >
        <template #extra>
          <n-button size="small" secondary @click="loadModels">
            {{ t("刷新模型列表") }}
          </n-button>
        </template>
      </n-empty>

      <div v-else class="test-table-wrap">
        <table class="test-table">
          <thead>
            <tr>
              <th>{{ t("模型") }}</th>
              <th>{{ t("状态") }}</th>
              <th>{{ t("结果") }}</th>
              <th class="test-table__action">{{ t("操作") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="model in pagedModels" :key="model.modelId">
              <td>
                <div class="model-identity">
                  <strong>{{ model.alias || model.modelId }}</strong>
                  <code v-if="model.alias && model.alias !== model.modelId">{{ model.modelId }}</code>
                </div>
              </td>
              <td>
                <n-tag :type="statusTagType(resultFor(model.modelId).status)" size="small" :bordered="false">
                  {{ statusLabel(resultFor(model.modelId).status) }}
                </n-tag>
              </td>
              <td>
                <span class="test-result" :class="{ 'test-result--error': resultFor(model.modelId).status === 'failed' }">
                  {{ resultSummary(model) }}
                </span>
              </td>
              <td class="test-table__action">
                <n-tooltip>
                  <template #trigger>
                    <n-button
                      circle
                      quaternary
                      size="small"
                      :loading="resultFor(model.modelId).status === 'testing'"
                      :disabled="testingAll || resultFor(model.modelId).status === 'testing'"
                      :aria-label="t('测试模型 {model}', { model: model.alias || model.modelId })"
                      @click="testOne(model)"
                    >
                      <template #icon><n-icon :component="ApiOutlined" /></template>
                    </n-button>
                  </template>
                  {{ t("测试模型 {model}", { model: model.alias || model.modelId }) }}
                </n-tooltip>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </n-spin>

    <div v-if="filteredModels.length > pageSize" class="test-pagination">
      <span>{{ t("共 {count} 个模型", { count: filteredModels.length }) }}</span>
      <n-pagination v-model:page="page" :page-size="pageSize" :item-count="filteredModels.length" />
    </div>

    <template #footer>
      <div class="test-footer">
        <span aria-live="polite">{{ progressText }}</span>
        <n-button @click="setVisible(false)">{{ t("关闭") }}</n-button>
      </div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from "vue";
import {
  NAlert,
  NButton,
  NEmpty,
  NIcon,
  NInput,
  NModal,
  NPagination,
  NPopconfirm,
  NSpin,
  NTag,
  NTooltip,
} from "naive-ui";
import { ApiOutlined } from "@vicons/antd";

import { dashboardApi, type Account, type AccountModelTestResponse } from "../api/dashboard.ts";
import { providerApi, type ProviderCatalogEntry } from "../api/providers.ts";
import {
  accountTestModels,
  filterAccountTestModels,
  type AccountTestModel,
} from "../domain/account-model-test.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

type TestStatus = "untested" | "testing" | "success" | "failed";
type TestState = {
  status: TestStatus;
  response: AccountModelTestResponse | null;
};

const props = defineProps<{
  show: boolean;
  account: Account | null;
  catalog: readonly ProviderCatalogEntry[] | null;
}>();

const emit = defineEmits<{
  "update:show": [value: boolean];
}>();

useLocalizedModalCloseLabel(toRef(props, "show"), "account-test-modal");

const models = ref<AccountTestModel[]>([]);
const results = ref<Record<string, TestState>>({});
const query = ref("");
const page = ref(1);
const pageSize = 30;
const loadingModels = ref(false);
const loadError = ref("");
const testingAll = ref(false);
const testedCount = ref(0);
let runGeneration = 0;

const filteredModels = computed(() => filterAccountTestModels(models.value, query.value));
const pagedModels = computed(() => {
  const start = (page.value - 1) * pageSize;
  return filteredModels.value.slice(start, start + pageSize);
});
const progressText = computed(() => testingAll.value
  ? t("正在测试 {current}/{total}", { current: testedCount.value, total: filteredModels.value.length })
  : "");

watch(query, () => {
  page.value = 1;
});

watch(() => [props.show, props.account?.id] as const, ([show]) => {
  if (!show) {
    runGeneration += 1;
    testingAll.value = false;
    return;
  }
  query.value = "";
  page.value = 1;
  models.value = [];
  results.value = {};
  loadError.value = "";
  void loadModels();
}, { immediate: true });

function setVisible(value: boolean) {
  if (!value) runGeneration += 1;
  emit("update:show", value);
}

async function loadModels() {
  const account = props.account;
  if (!props.show || !account) return;
  loadingModels.value = true;
  loadError.value = "";
  try {
    const contracts = await providerApi.getProviderContracts();
    if (!props.show || props.account?.id !== account.id) return;
    models.value = accountTestModels(account, contracts, props.catalog);
  } catch (error) {
    if (!props.show || props.account?.id !== account.id) return;
    models.value = [];
    loadError.value = t("加载测试模型失败：{error}", { error: dashboardErrorDetail(error) });
  } finally {
    if (props.account?.id === account.id) loadingModels.value = false;
  }
}

function resultFor(modelId: string): TestState {
  return results.value[modelId] ?? { status: "untested", response: null };
}

function setResult(modelId: string, state: TestState) {
  results.value = { ...results.value, [modelId]: state };
}

async function testOne(model: AccountTestModel, generation = runGeneration): Promise<void> {
  const account = props.account;
  if (!account || !props.show || generation !== runGeneration) return;
  setResult(model.modelId, { status: "testing", response: null });
  try {
    const response = await dashboardApi.testAccountModel(account.id, model.modelId);
    if (!props.show || props.account?.id !== account.id || generation !== runGeneration) return;
    setResult(model.modelId, {
      status: response.success ? "success" : "failed",
      response,
    });
  } catch (error) {
    if (!props.show || props.account?.id !== account.id || generation !== runGeneration) return;
    setResult(model.modelId, {
      status: "failed",
      response: {
        accountId: account.id,
        modelId: model.modelId,
        protocol: model.protocol,
        success: false,
        httpStatus: null,
        durationMs: 0,
        error: dashboardErrorDetail(error),
      },
    });
  }
}

async function testFilteredModels() {
  if (testingAll.value) return;
  const queue = [...filteredModels.value];
  const generation = ++runGeneration;
  testingAll.value = true;
  testedCount.value = 0;
  try {
    for (const model of queue) {
      if (!props.show || generation !== runGeneration) break;
      await testOne(model, generation);
      if (!props.show || generation !== runGeneration) break;
      testedCount.value += 1;
    }
  } finally {
    if (generation === runGeneration) testingAll.value = false;
  }
}

function statusLabel(status: TestStatus): string {
  if (status === "testing") return t("测试中");
  if (status === "success") return t("成功");
  if (status === "failed") return t("失败");
  return t("未测试");
}

function statusTagType(status: TestStatus): "default" | "info" | "success" | "error" {
  if (status === "testing") return "info";
  if (status === "success") return "success";
  if (status === "failed") return "error";
  return "default";
}

function resultSummary(model: AccountTestModel): string {
  const response = resultFor(model.modelId).response;
  if (!response) return "—";
  if (!response.success) return response.error || t("测试失败");
  const parts = [protocolDisplayName(response.protocol)];
  if (response.httpStatus !== null) parts.push(`HTTP ${response.httpStatus}`);
  parts.push(`${response.durationMs} ms`);
  return parts.join(" · ");
}
</script>

<style scoped>
.test-warning,
.test-error {
  margin-bottom: var(--ocg-space-lg);
}

.test-toolbar {
  display: grid;
  grid-template-columns: minmax(220px, 1fr) auto;
  gap: var(--ocg-space-md);
  align-items: center;
  margin-bottom: var(--ocg-space-lg);
}

.test-table-wrap {
  max-height: min(56vh, 620px);
  overflow: auto;
  border: 1px solid var(--ocg-divider);
  border-radius: 12px;
}

.test-table {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
}

.test-table th,
.test-table td {
  padding: 10px var(--ocg-space-md);
  border-bottom: 1px solid var(--ocg-divider);
  text-align: left;
  vertical-align: middle;
}

.test-table th {
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--ocg-surface);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
}

.test-table th:nth-child(1) { width: 34%; }
.test-table th:nth-child(2) { width: 14%; }
.test-table th:nth-child(3) { width: 44%; }
.test-table th:nth-child(4) { width: 8%; }
.test-table tbody tr:last-child td { border-bottom: 0; }

.model-identity {
  display: grid;
  min-width: 0;
  gap: 2px;
}

.model-identity strong,
.model-identity code,
.test-result {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.model-identity code {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.test-result {
  display: block;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  font-variant-numeric: tabular-nums;
}

.test-result--error { color: var(--ocg-error); }
.test-table__action { text-align: center !important; }

.test-pagination,
.test-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  font-variant-numeric: tabular-nums;
}

.test-pagination { margin-top: var(--ocg-space-md); }

@media (max-width: 640px) {
  .test-toolbar { grid-template-columns: 1fr; }
  .test-table { min-width: 680px; }
}
</style>
