<template>
  <ProviderPageModelTable v-bind="props" :model-editable="modelEditable" :metadata-editable="metadataEditable"
    :editing-disabled="editorDisabled" :action-locked="props.actionLocked || preparing || editing || metadataEditing"
    :resolve-selection-scope="prepareOperation" :selection-scope="operationScope"
    @edit="openEditor($event)" @metadata="openMetadataEditor($event)"
    @query="emit('query', $event)" @update:overrides="emit('update:overrides', $event)"
    @probe="emit('probe', $event)" @remove="emit('remove', $event)" @error="emit('error', $event)">
    <template #toolbar-actions>
      <ProviderModelEditor v-if="operationScope" ref="editor" :scope="operationScope" :disabled="editorDisabled" :defer-revalidation="true" @update:busy="onEditorBusy" @committed="emit('committed', $event)" />
      <n-button v-else-if="modelEditable" type="primary" size="small" :disabled="editorDisabled" :loading="preparing" @click="openEditor(null)">
        {{ t('添加模型') }}
      </n-button>
    </template>
  </ProviderPageModelTable>
  <template v-if="operationScope">
    <ModelMetadataEditor ref="metadataEditor" :scope="operationScope" :disabled="editorDisabled" @update:busy="onMetadataBusy" @committed="emit('metadataCommitted', $event)" />
  </template>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { NButton } from "naive-ui";
import type { ProviderModelsPage } from "../api/pages.ts";
import type { Destination, DestinationModelMetadataSnapshot } from "../api/destinations.ts";
import type { ContractScopeKind, ModelProtocolOverrideUpdate, ProviderContractsResponse } from "../api/providers.ts";
import type { ProviderScopeView } from "../domain/provider-contracts.ts";
import { t } from "../i18n/index.ts";
import ProviderPageModelTable from "./ProviderPageModelTable.vue";
import ProviderModelEditor from "./ProviderModelEditor.vue";
import ModelMetadataEditor from "./ModelMetadataEditor.vue";

const props = defineProps<{
  scope: ProviderScopeView;
  modelRows: ProviderModelsPage["models"];
  targetModel?: string | null;
  total: number; filteredTotal: number; offset: number; limit: number;
  loading?: boolean; allDisabled?: boolean;
  modelEditable: boolean; metadataEditable: boolean;
  prepareOperation: () => Promise<ProviderScopeView>;
  operationScope?: ProviderScopeView | null;
  optimisticOverrides?: Map<string, boolean>; pendingOverrideKeys?: Set<string>; probingModels?: Set<string>;
  actionLocked?: boolean; removing?: boolean;
}>();
const emit = defineEmits<{
  (event: "query", query: { search: string; enabledOnly: boolean; offset: number }): void;
  (event: "update:overrides", payload: { scopeKind: ContractScopeKind; scopeId: string; overrides: ModelProtocolOverrideUpdate[] }): void;
  (event: "probe", payload: { modelId: string }): void;
  (event: "remove", payload: { modelIds: string[] }): void;
  (event: "error", message: string): void;
  (event: "committed", receipt: { kind: "destination"; destination: Destination } | { kind: "provider"; contracts: ProviderContractsResponse }): void;
  (event: "metadataCommitted", receipt: { destinationId: string; publicModel: string; snapshot: DestinationModelMetadataSnapshot }): void;
}>();
const editor = ref<InstanceType<typeof ProviderModelEditor> | null>(null);
const metadataEditor = ref<InstanceType<typeof ModelMetadataEditor> | null>(null);
const operationReady = ref(false);
const operationScope = computed(() => operationReady.value ? props.operationScope ?? null : null);
const preparing = ref(false);
const editing = ref(false);
const metadataEditing = ref(false);
const editorDisabled = computed(() => Boolean(props.actionLocked || props.removing || preparing.value
  || props.pendingOverrideKeys?.size || props.probingModels?.size));
watch(() => props.scope.key, () => { operationReady.value = false; editing.value = metadataEditing.value = false; });

async function prepareOperation(): Promise<ProviderScopeView> {
  const key = props.scope.key;
  preparing.value = true;
  try {
    const scope = await props.prepareOperation();
    if (key !== props.scope.key) throw new Error(t('状态已变化，请刷新后重试。'));
    operationReady.value = true;
    await nextTick();
    return scope;
  } finally { preparing.value = false; }
}
async function openEditor(model: string | null): Promise<void> {
  try { await prepareOperation(); await nextTick(); await editor.value?.openEditor(model); }
  catch (cause) { emit('error', cause instanceof Error ? cause.message : String(cause)); }
}
async function openMetadataEditor(model: string): Promise<void> {
  try { await prepareOperation(); await nextTick(); await metadataEditor.value?.openEditor(model); }
  catch (cause) { emit('error', cause instanceof Error ? cause.message : String(cause)); }
}
function onEditorBusy(value: boolean): void { editing.value = value; }
function onMetadataBusy(value: boolean): void { metadataEditing.value = value; }
defineExpose({ openMetadataEditor });
</script>
