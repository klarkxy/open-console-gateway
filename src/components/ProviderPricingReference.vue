<template>
  <div class="provider-pricing-reference">
    <GoatQuotaReference
      :snapshot="snapshot"
      :saving-model-id="savingModelId"
      :disabled="disabled"
      :editable="editable"
      @save-multiplier="(modelId, multiplier) => emit('save-multiplier', modelId, multiplier)"
    />
  </div>
</template>

<script setup lang="ts">
import GoatQuotaReference from "./GoatQuotaReference.vue";
import type { ProviderNeutralPricingSnapshot } from "../api/providers.ts";

withDefaults(defineProps<{
  snapshot?: ProviderNeutralPricingSnapshot | null;
  savingModelId?: string | null;
  disabled?: boolean;
  editable?: boolean;
}>(), {
  editable: false,
});

const emit = defineEmits<{
  "save-multiplier": [modelId: string, multiplier: number];
}>();
</script>

<style scoped>
.provider-pricing-reference {
  min-width: 0;
}
</style>
