<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('模型能力')"
    class="model-metadata-modal"
    style="width: 640px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!saving"
    @update:show="onUpdateShow"
  >
    <div v-if="loading" class="model-metadata-state" role="status">
      <n-spin size="small" />
    </div>
    <div v-else-if="draft" class="model-metadata-body">
      <div class="model-metadata-head">
        <code class="model-metadata-model">{{ editingPublicModel }}</code>
        <n-tag v-if="capturedSource" size="small" :bordered="false">{{ sourceLabel(capturedSource) }}</n-tag>
      </div>
      <p class="model-metadata-hint">{{ t("留空表示未知，只声明网关路径实际可用的能力。") }}</p>
      <n-form label-placement="top" @submit.prevent="save">
        <n-form-item :label="t('显示名称')">
          <n-input
            v-model:value="draft.name"
            :disabled="saving || stale"
            :input-props="{ 'aria-label': t('显示名称') }"
          />
        </n-form-item>
        <n-form-item :label="t('上下文窗口')">
          <n-input-number
            v-model:value="draft.contextWindow"
            :min="1"
            :precision="0"
            clearable
            class="model-metadata-number"
            :placeholder="t('未知')"
            :disabled="saving || stale"
            :input-props="{ 'aria-label': t('上下文窗口') }"
          />
        </n-form-item>
        <n-form-item :label="t('最大输出')">
          <n-input-number
            v-model:value="draft.maxOutputTokens"
            :min="1"
            :precision="0"
            clearable
            class="model-metadata-number"
            :placeholder="t('未知')"
            :disabled="saving || stale"
            :input-props="{ 'aria-label': t('最大输出') }"
          />
        </n-form-item>
        <n-form-item :label="t('输入模态')">
          <n-checkbox-group
            v-model:value="draft.inputModalities"
            :disabled="saving || stale"
            :aria-label="t('输入模态')"
          >
            <n-space>
              <n-checkbox v-for="modality in METADATA_MODALITIES" :key="modality" :value="modality">
                {{ modalityLabel(modality) }}
              </n-checkbox>
            </n-space>
          </n-checkbox-group>
        </n-form-item>
        <n-form-item :label="t('输出模态')">
          <n-checkbox-group
            v-model:value="draft.outputModalities"
            :disabled="saving || stale"
            :aria-label="t('输出模态')"
          >
            <n-space>
              <n-checkbox v-for="modality in METADATA_MODALITIES" :key="modality" :value="modality">
                {{ modalityLabel(modality) }}
              </n-checkbox>
            </n-space>
          </n-checkbox-group>
        </n-form-item>
        <n-form-item :label="t('推理')">
          <n-select
            v-model:value="draft.reasoning"
            :options="triStateOptions"
            :disabled="saving || stale"
            :aria-label="t('推理')"
          />
        </n-form-item>
        <n-form-item :label="t('推理档位')">
          <div class="model-metadata-efforts">
            <div v-for="level in REASONING_EFFORT_LEVELS" :key="level" class="model-metadata-effort">
              <n-checkbox
                :checked="draft.reasoningEfforts[level] !== undefined"
                :disabled="saving || stale || draft.reasoning === 'no'"
                :aria-label="level"
                @update:checked="(on: boolean) => toggleEffort(level, on)"
              >
                <code>{{ level }}</code>
              </n-checkbox>
              <n-input
                v-if="draft.reasoningEfforts[level] !== undefined"
                :value="draft.reasoningEfforts[level]"
                size="small"
                class="model-metadata-effort-wire"
                :disabled="saving || stale"
                :input-props="{ 'aria-label': t('{level} 档位参数', { level }) }"
                @update:value="(value: string) => setEffortWire(level, value)"
              />
            </div>
          </div>
        </n-form-item>
        <n-form-item :label="t('工具调用')">
          <n-select
            v-model:value="draft.toolCalling"
            :options="triStateOptions"
            :disabled="saving || stale"
            :aria-label="t('工具调用')"
          />
        </n-form-item>
        <n-form-item :label="t('并行工具调用')">
          <n-select
            v-model:value="draft.parallelToolCalls"
            :options="triStateOptions"
            :disabled="saving || stale || draft.toolCalling === 'no'"
            :aria-label="t('并行工具调用')"
          />
        </n-form-item>
      </n-form>
      <n-alert v-if="stale && !saving" type="warning" :title="t('状态已变化，请刷新后重试。')">
        <n-button size="small" secondary :loading="reloading" @click="reloadEditor">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert v-else-if="errorText" type="error" :title="errorText" />
    </div>
    <n-alert v-else type="error" :title="loadError || t('状态已变化，请刷新后重试。')" />
    <template #footer>
      <n-space justify="space-between">
        <n-popconfirm
          :positive-text="t('清除声明')"
          :disabled="saving || reloading || stale || capturedSource !== 'operator'"
          @positive-click="clearDeclaration"
        >
          <template #trigger>
            <n-button
              secondary
              type="error"
              :disabled="saving || reloading || stale || capturedSource !== 'operator'"
            >
              {{ t("清除声明") }}
            </n-button>
          </template>
          {{ t("清除人工声明并恢复使用目录发现的事实？") }}
        </n-popconfirm>
        <n-space>
          <n-button secondary :disabled="saving || reloading" @click="closeEditor">{{ t("取消") }}</n-button>
          <n-button type="primary" :loading="saving" :disabled="!draft || stale || reloading || props.disabled" @click="save">
            {{ t("保存声明") }}
          </n-button>
        </n-space>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onDeactivated, ref, shallowRef, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NCheckboxGroup,
  NForm,
  NFormItem,
  NInput,
  NInputNumber,
  NModal,
  NPopconfirm,
  NSelect,
  NSpace,
  NSpin,
  NTag,
  useMessage,
} from "naive-ui";
import type { DestinationModelMetadataEntryView } from "../api/destinations.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import type { ProviderScopeView } from "../domain/provider-contracts.ts";
import {
  METADATA_MODALITIES,
  MODEL_METADATA_ISSUE_KEYS,
  REASONING_EFFORT_LEVELS,
  buildModelMetadata,
  modelMetadataDraft,
  modelMetadataFingerprint,
  type MetadataModality,
  type MetadataTriState,
  type ModelMetadataDraft,
  type ReasoningEffortLevel,
} from "../domain/model-metadata.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useSessionStore } from "../stores/session.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

const props = defineProps<{ scope: ProviderScopeView; disabled?: boolean }>();
const emit = defineEmits<{ (event: "update:busy", value: boolean): void }>();
const destinationsStore = useDestinationsStore();
const sessionStore = useSessionStore();
const message = useMessage();

// Metadata attaches to the projected destination of either scope kind; the
// lookup mirrors ProviderModelEditor so builtin and HTTP rows behave alike.
const destination = computed(() => props.scope.scope_kind === "custom_endpoint"
  ? destinationsStore.byId.get(props.scope.scope_id) ?? null
  : [...destinationsStore.byId.values()].find((d) => d.legacy.kind === "builtin" && d.legacy.id === props.scope.scope_id) ?? null);
const editable = computed(() => destination.value !== null);

const show = ref(false);
const loading = ref(false);
const saving = ref(false);
const reloading = ref(false);
const draft = ref<ModelMetadataDraft | null>(null);
// The immutable form baseline and its CAS token belong together; the store
// remains the sole live owner of server state.
const capturedEntry = shallowRef<DestinationModelMetadataEntryView | null>(null);
const capturedExpectation = ref<MutationExpectation | null>(null);
const editingPublicModel = ref("");
const conflict = ref(false);
const errorKey = ref<MessageKey | null>(null);
const requestError = ref("");
const loadError = ref("");
let generation = 0;
let mounted = true;
useLocalizedModalCloseLabel(show, "model-metadata-modal");

const capturedSource = computed(() => capturedEntry.value?.source ?? null);
const errorText = computed(() => errorKey.value ? t(errorKey.value) : requestError.value);
const storeEntry = computed(() => {
  const id = destination.value?.id;
  if (!id || !editingPublicModel.value) return null;
  return destinationsStore.modelMetadata[id]?.models
    .find((entry) => entry.public_model === editingPublicModel.value) ?? null;
});
const stale = computed(() => Boolean(show.value && (
  conflict.value || !capturedEntry.value
    || modelMetadataFingerprint(storeEntry.value) !== modelMetadataFingerprint(capturedEntry.value)
)));

const triStateOptions = computed(() => [
  { value: "unknown" satisfies MetadataTriState, label: t("未知") },
  { value: "yes" satisfies MetadataTriState, label: t("支持") },
  { value: "no" satisfies MetadataTriState, label: t("不支持") },
]);

function sourceLabel(source: string): string {
  if (source === "operator") return t("人工声明");
  if (source === "upstream") return t("上游发现");
  if (source === "modelsdev") return t("models.dev 目录");
  return t("未知");
}

function modalityLabel(modality: MetadataModality): string {
  if (modality === "text") return t("文本");
  if (modality === "image") return t("图片");
  if (modality === "audio") return t("音频");
  return t("视频");
}

watch([show, saving, reloading], () => emit("update:busy", show.value || saving.value || reloading.value));
watch(() => props.scope.key, resetEditor);
watch(() => sessionStore.authenticated, (authenticated) => { if (!authenticated) resetEditor(); });
onBeforeUnmount(() => { mounted = false; generation += 1; });
// Providers is kept alive by the shell: leaving the page must close teleported
// dialogs and invalidate their pending UI receipts, not leave them on another view.
onDeactivated(resetEditor);

function resetEditor(): void {
  generation += 1;
  show.value = false;
  loading.value = false;
  saving.value = false;
  reloading.value = false;
  draft.value = null;
  capturedEntry.value = null;
  capturedExpectation.value = null;
  editingPublicModel.value = "";
  errorKey.value = null;
  requestError.value = "";
  loadError.value = "";
  conflict.value = false;
}

function isCurrent(attempt: number): boolean {
  return mounted && generation === attempt && sessionStore.authenticated;
}

async function openEditor(modelId: string): Promise<void> {
  const source = destination.value;
  if (!source || show.value || !sessionStore.authenticated) return;
  // Table identity is the public model on HTTP scopes and the upstream ID on
  // builtin scopes; the API always wants the exact saved public model.
  const row = source.catalog.find((model) => model.public_model === modelId)
    ?? source.catalog.find((model) => model.upstream_model === modelId);
  const expectation = destinationsStore.expectation;
  if (!row || !expectation) {
    message.warning(t("状态已变化，请刷新后重试。"));
    return;
  }
  resetEditor();
  editingPublicModel.value = row.public_model;
  capturedExpectation.value = { ...expectation };
  show.value = true;
  loading.value = true;
  const attempt = generation;
  try {
    const snapshot = await destinationsStore.loadModelMetadata(source.id);
    if (!isCurrent(attempt)) return;
    const entry = snapshot.models.find((item) => item.public_model === row.public_model);
    if (!entry) {
      loadError.value = t("状态已变化，请刷新后重试。");
      return;
    }
    capturedEntry.value = entry;
    draft.value = modelMetadataDraft(entry.metadata);
  } catch (error) {
    if (isCurrent(attempt)) {
      loadError.value = t("加载模型能力失败：{error}", { error: dashboardErrorDetail(error) });
    }
  } finally {
    if (isCurrent(attempt)) loading.value = false;
  }
}

function closeEditor(): void {
  if (!saving.value && !reloading.value) resetEditor();
}

function onUpdateShow(value: boolean): void {
  if (!value) closeEditor();
}

function toggleEffort(level: ReasoningEffortLevel, on: boolean): void {
  if (!draft.value || saving.value || stale.value) return;
  const next = { ...draft.value.reasoningEfforts };
  if (on) next[level] = next[level] ?? level;
  else delete next[level];
  draft.value.reasoningEfforts = next;
}

function setEffortWire(level: ReasoningEffortLevel, value: string): void {
  if (!draft.value || saving.value || stale.value) return;
  if (draft.value.reasoningEfforts[level] === undefined) return;
  draft.value.reasoningEfforts = { ...draft.value.reasoningEfforts, [level]: value };
}

async function reloadEditor(): Promise<void> {
  const source = destination.value;
  const publicModel = editingPublicModel.value;
  if (!source || !publicModel || reloading.value || saving.value) return;
  const attempt = generation;
  reloading.value = true;
  try {
    const snapshot = await destinationsStore.loadModelMetadata(source.id);
    if (!isCurrent(attempt)) return;
    const entry = snapshot.models.find((item) => item.public_model === publicModel);
    if (!entry) {
      resetEditor();
      message.warning(t("状态已变化，请刷新后重试。"));
      return;
    }
    conflict.value = false;
    capturedEntry.value = entry;
    capturedExpectation.value = { ...snapshot.expectation };
    draft.value = modelMetadataDraft(entry.metadata);
  } catch (error) {
    if (isCurrent(attempt)) {
      message.error(t("加载模型能力失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    if (isCurrent(attempt)) reloading.value = false;
  }
}

async function submit(metadata: Parameters<typeof destinationsStore.declareModelMetadata>[2], successKey: MessageKey): Promise<void> {
  const source = destination.value;
  const expectation = capturedExpectation.value;
  const publicModel = editingPublicModel.value;
  if (!source || !expectation || !publicModel || saving.value || reloading.value || stale.value || props.disabled) return;
  errorKey.value = null;
  requestError.value = "";
  const attempt = generation;
  saving.value = true;
  try {
    await destinationsStore.declareModelMetadata(source.id, publicModel, metadata, expectation);
    if (!isCurrent(attempt)) return;
    message.success(t(successKey));
    resetEditor();
  } catch (error) {
    if (!isCurrent(attempt)) return;
    if (isRevisionConflict(error)) {
      // The store reloaded on conflict. Preserve the draft until the user
      // explicitly chooses to reload this form; never replay the old PUT.
      conflict.value = true;
    } else {
      requestError.value = t("保存失败：{error}", { error: dashboardErrorDetail(error) });
    }
  } finally {
    if (isCurrent(attempt)) saving.value = false;
  }
}

async function save(): Promise<void> {
  if (!draft.value) return;
  const build = buildModelMetadata(draft.value);
  if (build.kind === "invalid") {
    errorKey.value = MODEL_METADATA_ISSUE_KEYS[build.issue];
    requestError.value = "";
    return;
  }
  await submit(build.metadata, "模型能力已声明");
}

async function clearDeclaration(): Promise<void> {
  await submit(null, "已清除模型能力声明");
}

defineExpose({ editable, openEditor });
</script>

<style scoped>
.model-metadata-state {
  display: flex;
  justify-content: center;
  padding: var(--ocg-space-lg) 0;
}
.model-metadata-body {
  max-height: min(560px, calc(100dvh - 220px));
  overflow: auto;
}
.model-metadata-head {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  margin-bottom: var(--ocg-space-xs);
}
.model-metadata-model {
  overflow-wrap: anywhere;
  font-size: var(--ocg-font-sm);
}
.model-metadata-hint {
  margin: 0 0 var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.model-metadata-number {
  width: 220px;
}
.model-metadata-efforts {
  display: flex;
  flex-direction: column;
  gap: 6px;
  width: 100%;
}
.model-metadata-effort {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.model-metadata-effort-wire {
  width: 200px;
}
</style>
