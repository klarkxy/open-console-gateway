<template>
  <div>
    <ProviderModelEditor
      :scope="props.scope"
      :disabled="editorDisabled"
      @update:busy="editing = $event"
    />
    <ProviderModelTable
      v-bind="props"
      :action-locked="props.actionLocked || editing"
      @update:overrides="emit('update:overrides', $event)"
      @probe="emit('probe', $event)"
      @remove="emit('remove', $event)"
      @error="emit('error', $event)"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import type { ContractScopeKind, ModelProtocolOverrideUpdate } from "../api/providers.ts";
import type { ProviderScopeView } from "../domain/provider-contracts.ts";
import ProviderModelEditor from "./ProviderModelEditor.vue";
import ProviderModelTable from "./ProviderModelTable.vue";

// Keep the existing matrix's public contract. The table still owns selection,
// deletion, protocol switches, probes and their optimistic presentation.
const props = defineProps<{
  scope: ProviderScopeView;
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
const editing = ref(false);
const editorDisabled = computed(() => Boolean(
  props.actionLocked || props.removing
  || props.pendingOverrideKeys?.size || props.probingModels?.size,
));
</script>
