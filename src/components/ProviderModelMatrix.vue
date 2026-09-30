<template>
  <div>
    <ProviderModelTable
      v-bind="props"
      :model-editable="editor?.editable ?? false"
      :metadata-editable="metadataEditor?.editable ?? false"
      :editing-disabled="editorDisabled"
      @edit="editor?.openEditor($event)"
      @metadata="metadataEditor?.openEditor($event)"
      :action-locked="props.actionLocked || editing || metadataEditing"
      @update:overrides="emit('update:overrides', $event)"
      @probe="emit('probe', $event)"
      @remove="emit('remove', $event)"
      @error="emit('error', $event)"
    >
      <template #toolbar-actions>
        <ProviderModelEditor
          ref="editor"
          :scope="props.scope"
          :disabled="editorDisabled"
          @update:busy="editing = $event"
        />
        <ModelMetadataEditor
          ref="metadataEditor"
          :scope="props.scope"
          :disabled="editorDisabled"
          @update:busy="metadataEditing = $event"
        />
      </template>
    </ProviderModelTable>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import type { ContractScopeKind, ModelProtocolOverrideUpdate } from "../api/providers.ts";
import type { ProviderScopeView } from "../domain/provider-contracts.ts";
import ModelMetadataEditor from "./ModelMetadataEditor.vue";
import ProviderModelEditor from "./ProviderModelEditor.vue";
import ProviderModelTable from "./ProviderModelTable.vue";

// Keep the existing matrix's public contract. The table still owns selection,
// deletion, protocol switches, probes and their optimistic presentation.
const props = defineProps<{
  scope: ProviderScopeView;
  targetModel?: string | null;
  optimisticOverrides?: Map<string, boolean>;
  pendingOverrideKeys?: Set<string>;
  probingModels?: Set<string>;
  actionLocked?: boolean;
  removing?: boolean;
}>();
const emit = defineEmits<{
  (event: "update:overrides", payload: {
    scopeKind: ContractScopeKind;
    scopeId: string;
    overrides: ModelProtocolOverrideUpdate[];
  }): void;
  (event: "probe", payload: { modelId: string }): void;
  (event: "remove", payload: { modelIds: string[] }): void;
  (event: "error", message: string): void;
}>();
const editor = ref<InstanceType<typeof ProviderModelEditor> | null>(null);
const metadataEditor = ref<InstanceType<typeof ModelMetadataEditor> | null>(null);
const editing = ref(false);
const metadataEditing = ref(false);
const editorDisabled = computed(() => Boolean(
  props.actionLocked || props.removing
  || props.pendingOverrideKeys?.size || props.probingModels?.size,
));

/** Deep-link entry: open the capabilities editor for one model row. */
function openMetadataEditor(modelId: string): void {
  void metadataEditor.value?.openEditor(modelId);
}
defineExpose({ openMetadataEditor });
</script>
