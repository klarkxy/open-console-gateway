<template>
  <div v-if="editable" class="provider-model-editor">
    <n-button type="primary" size="small" :disabled="toolbarLocked" @click="openEditor(null)">
      {{ t("添加模型") }}
    </n-button>
    <n-select
      v-if="modelOptions.length > 0"
      v-model:value="selectedModelId"
      class="provider-model-editor__select"
      size="small"
      filterable
      clearable
      :options="modelOptions"
      :disabled="toolbarLocked"
      :placeholder="t('搜索模型名或别名')"
      :aria-label="t('模型映射')"
    />
    <n-button
      v-if="modelOptions.length > 0"
      size="small"
      secondary
      :disabled="toolbarLocked || !selectedModelId"
      @click="selectedModelId && openEditor(selectedModelId)"
    >
      {{ t("编辑") }}
    </n-button>
  </div>

  <n-modal
    :show="show"
    preset="card"
    :title="editingModelId === null ? t('添加模型') : t('模型映射')"
    class="provider-model-edit-modal"
    style="width: 600px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!saving"
    @update:show="onUpdateShow"
  >
    <div v-if="draft" class="provider-model-edit-body">
      <n-form label-placement="top" @submit.prevent="save">
        <n-form-item :label="t('上游模型 ID')">
          <n-input
            v-model:value="draft.upstream_model"
            :disabled="saving || stale"
            :input-props="{ 'aria-label': t('上游模型 ID') }"
          />
        </n-form-item>
        <n-form-item :label="t('对外模型名')">
          <n-input
            v-model:value="draft.public_model"
            :placeholder="draft.upstream_model.trim() || t('对外模型名')"
            :disabled="saving || stale"
            :input-props="{ 'aria-label': t('对外模型名') }"
          />
        </n-form-item>
        <n-form-item :label="t('上游协议')">
          <n-checkbox-group
            :value="draft.protocols"
            :disabled="saving || stale"
            :aria-label="t('上游协议')"
            @update:value="setProtocols"
          >
            <n-space>
              <n-checkbox v-for="protocol in availableProtocols" :key="protocol" :value="protocol">
                {{ protocolDisplayName(protocol) }}
              </n-checkbox>
            </n-space>
          </n-checkbox-group>
        </n-form-item>
        <n-form-item :label="t('首选协议')">
          <n-select
            v-model:value="draft.preferred"
            :options="preferredOptions"
            :disabled="saving || stale || draft.protocols.length === 0"
            :aria-label="t('首选协议')"
          />
        </n-form-item>
        <n-form-item :label="t('允许路由')">
          <n-switch v-model:value="draft.enabled" :disabled="saving || stale" :aria-label="t('允许路由')" />
        </n-form-item>
        <n-form-item v-if="upstreamOverride" :label="t('覆盖的上游地址')">
          <code class="provider-model-edit-endpoint">{{ upstreamOverride.endpoint_url }}</code>
        </n-form-item>
      </n-form>
      <n-alert v-if="stale && !saving" type="warning" :title="t('状态已变化，请刷新后重试。')">
        <n-button size="small" secondary :loading="reloading" @click="reloadEditor">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert v-else-if="errorText" type="error" :title="errorText" />
    </div>
    <template #footer>
      <n-space justify="end">
        <n-button secondary :disabled="saving || reloading" @click="closeEditor">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="saving" :disabled="!draft || stale || reloading || props.disabled" @click="save">
          {{ t("保存") }}
        </n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onDeactivated, ref, shallowRef, watch } from "vue";
import { NAlert, NButton, NCheckbox, NCheckboxGroup, NForm, NFormItem, NInput, NModal, NSelect, NSpace, NSwitch, useMessage } from "naive-ui";
import type { Destination, ProtocolDto } from "../api/destinations.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import { protocolDisplayName, type ProviderScopeView } from "../domain/provider-contracts.ts";
import {
  PROVIDER_MODEL_EDIT_ISSUE_KEYS,
  canEditProviderModels,
  planProviderModelEdit,
  providerModelDraft,
  providerModelEditFingerprint,
  providerModelProtocols,
  type ProviderModelDraft,
} from "../domain/provider-model-edit.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

const props = defineProps<{ scope: ProviderScopeView; disabled?: boolean }>();
const emit = defineEmits<{ (event: "update:busy", value: boolean): void }>();
const destinationsStore = useDestinationsStore();
const providersStore = useProvidersStore();
const sessionStore = useSessionStore();
const message = useMessage();
const destination = computed(() => props.scope.scope_kind === "custom_endpoint"
  ? destinationsStore.byId.get(props.scope.scope_id) ?? null : null);
const editable = computed(() => canEditProviderModels(destination.value));
const selectedModelId = ref<string | null>(null);
const modelOptions = computed(() => (destination.value?.catalog ?? []).map((model) => ({
  value: model.public_model,
  label: model.public_model === model.upstream_model
    ? model.public_model : `${model.public_model} → ${model.upstream_model}`,
})));
const show = ref(false);
const saving = ref(false);
const reloading = ref(false);
const draft = ref<ProviderModelDraft | null>(null);
// This immutable form baseline and its CAS token belong together. It is not
// a second live copy of server state; the store remains the sole live owner.
const captured = shallowRef<Destination | null>(null);
const capturedExpectation = ref<MutationExpectation | null>(null);
const editingModelId = ref<string | null>(null);
const conflict = ref(false);
const errorKey = ref<MessageKey | null>(null);
const requestError = ref("");
let generation = 0;
let mounted = true;
useLocalizedModalCloseLabel(show, "provider-model-edit-modal");

const errorText = computed(() => errorKey.value ? t(errorKey.value) : requestError.value);
const toolbarLocked = computed(() => Boolean(props.disabled || show.value || saving.value));
const stale = computed(() => Boolean(show.value && (
  conflict.value || !captured.value || !destination.value
  || providerModelEditFingerprint(captured.value) !== providerModelEditFingerprint(destination.value)
)));
const availableProtocols = computed(() => captured.value
  ? providerModelProtocols(captured.value, editingModelId.value) : []);
const preferredOptions = computed(() => (draft.value?.protocols ?? []).map((value) => ({
  value, label: protocolDisplayName(value),
})));
const upstreamOverride = computed(() => captured.value?.catalog.find((model) => (
  model.public_model === editingModelId.value
))?.upstream_override ?? null);

watch([show, saving, reloading], () => emit("update:busy", show.value || saving.value || reloading.value));
watch(modelOptions, (options) => {
  if (selectedModelId.value && !options.some((option) => option.value === selectedModelId.value)) {
    selectedModelId.value = null;
  }
});
watch(() => props.scope.key, () => { selectedModelId.value = null; resetEditor(); });
watch(() => sessionStore.authenticated, (authenticated) => { if (!authenticated) resetEditor(); });
onBeforeUnmount(() => { mounted = false; generation += 1; });
// Providers is kept alive by the shell: leaving the page must close teleported
// dialogs and invalidate their pending UI receipts, not leave them on another view.
onDeactivated(resetEditor);

function resetEditor(): void {
  generation += 1;
  show.value = false;
  saving.value = false;
  reloading.value = false;
  draft.value = null;
  captured.value = null;
  capturedExpectation.value = null;
  editingModelId.value = null;
  errorKey.value = null;
  requestError.value = "";
  conflict.value = false;
}

function openEditor(modelId: string | null): void {
  const source = destination.value;
  if (toolbarLocked.value || !canEditProviderModels(source) || !sessionStore.authenticated) return;
  const expectation = destinationsStore.expectation;
  const nextDraft = providerModelDraft(source, modelId);
  if (!expectation || !nextDraft) {
    message.warning(t("状态已变化，请刷新后重试。"));
    return;
  }
  resetEditor();
  captured.value = source;
  capturedExpectation.value = { ...expectation };
  editingModelId.value = modelId;
  draft.value = nextDraft;
  show.value = true;
}

function closeEditor(): void {
  if (!saving.value && !reloading.value) resetEditor();
}

function onUpdateShow(value: boolean): void {
  if (!value) closeEditor();
}

function setProtocols(values: (string | number)[]): void {
  if (!draft.value || saving.value || stale.value) return;
  const selected = values.filter((value): value is ProtocolDto => availableProtocols.value.includes(value as ProtocolDto));
  draft.value.protocols = selected;
  if (!draft.value.preferred || !selected.includes(draft.value.preferred)) {
    draft.value.preferred = selected[0] ?? null;
  }
}

function isCurrent(attempt: number): boolean {
  return mounted && generation === attempt && sessionStore.authenticated;
}

async function reloadEditor(): Promise<void> {
  if (reloading.value || saving.value) return;
  const attempt = generation;
  const modelId = editingModelId.value;
  reloading.value = true;
  try {
    await destinationsStore.load();
    if (!isCurrent(attempt)) return;
    resetEditor();
    openEditor(modelId);
  } catch (error) {
    if (isCurrent(attempt)) message.error(t("加载供应商失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (isCurrent(attempt)) reloading.value = false;
  }
}

async function save(): Promise<void> {
  const source = captured.value;
  const expectation = capturedExpectation.value;
  if (!source || !expectation || !draft.value || !show.value || saving.value || reloading.value || stale.value || props.disabled) return;
  const plan = planProviderModelEdit(source, draft.value, editingModelId.value);
  errorKey.value = null;
  requestError.value = "";
  if (plan.kind === "invalid") {
    errorKey.value = PROVIDER_MODEL_EDIT_ISSUE_KEYS[plan.issue];
    return;
  }
  const attempt = generation;
  saving.value = true;
  try {
    await destinationsStore.patchDestination(source.id, plan.input, expectation);
    if (!isCurrent(attempt)) return;
    message.success(t("连接已保存"));
    // Refresh only local projections used by Aliases and supplier details.
    // Never discover models, probe an upstream, or authorize another Key.
    const reads: Promise<unknown>[] = [
      providersStore.loadContracts(), providersStore.loadConnections(), providersStore.loadCatalog(),
    ];
    if (source.legacy.kind === "dynamic") {
      providersStore.invalidateDefinition(source.legacy.id);
      reads.push(providersStore.loadDefinition(source.legacy.id, true));
    }
    const results = await Promise.allSettled(reads);
    if (!isCurrent(attempt)) return;
    const failed = results.find((result) => result.status === "rejected");
    if (failed?.status === "rejected") {
      message.warning(t("加载供应商失败：{error}", { error: dashboardErrorDetail(failed.reason) }));
    }
    resetEditor();
  } catch (error) {
    if (!isCurrent(attempt)) return;
    if (isRevisionConflict(error)) {
      // The store reloaded on conflict. Preserve the draft until the user
      // explicitly chooses to reload this form; never replay the old PATCH.
      conflict.value = true;
    } else {
      requestError.value = t("保存失败：{error}", { error: dashboardErrorDetail(error) });
    }
  } finally {
    if (isCurrent(attempt)) saving.value = false;
  }
}
</script>

<style scoped>
.provider-model-editor {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
  margin-bottom: var(--ocg-space-md);
}
.provider-model-editor__select {
  flex: 1 1 220px;
  min-width: 160px;
  max-width: 420px;
}
.provider-model-edit-body {
  max-height: min(560px, calc(100dvh - 220px));
  overflow: auto;
}
.provider-model-edit-endpoint {
  overflow-wrap: anywhere;
}
</style>
