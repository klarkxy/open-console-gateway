<template>
  <div class="credit-setup">
    <n-checkbox v-if="presets.length === 0" v-model:checked="draft.enabled" :disabled="disabled">
      {{ t("配置额度") }}
    </n-checkbox>
    <template v-if="draft.enabled">
      <n-form-item :show-feedback="false" v-if="presets.length" :label="t('档位')" required>
        <n-select :value="draft.presetId" :options="options" :disabled="disabled" :aria-label="t('档位')" @update:value="selectPreset" />
      </n-form-item>
      <template v-else>
        <n-form-item :show-feedback="false" :label="t('名称')"><n-input v-model:value="draft.name" :disabled="disabled" :aria-label="t('名称')" /></n-form-item>
        <n-form-item :show-feedback="false" :label="t('币种')"><n-input v-model:value="draft.currency" :disabled="disabled" :aria-label="t('币种')" /></n-form-item>
        <n-checkbox v-model:checked="draft.monthly" :disabled="disabled">{{ t("月度额度") }}</n-checkbox>
        <n-form-item :show-feedback="false" v-if="draft.monthly" :label="t('月度额度')" required><n-input-number v-model:value="draft.monthlyAmount" :min="0" :disabled="disabled" :aria-label="t('月度额度')" :input-props="{ 'aria-label': t('月度额度') }" /></n-form-item>
      </template>
      <n-form-item :show-feedback="false" :label="t('当前剩余')" required>
        <div class="credit-setup__amount">
          <n-input-number v-model:value="draft.remaining" :show-button="false" :min="0" :max="maximum" :precision="4" :disabled="disabled" :aria-label="t('当前剩余')" :input-props="{ 'aria-label': t('当前剩余') }" />
          <span v-if="presets.length" class="mono">M</span>
        </div>
      </n-form-item>
      <n-form-item :show-feedback="false" v-if="presets.length || draft.monthly" :label="t('重置日期')" required>
        <input :value="draft.reset" type="datetime-local" class="credit-setup__date mono" :disabled="disabled" :aria-label="t('重置日期')" @input="setReset">
      </n-form-item>
    </template>
  </div>
</template>
<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NCheckbox, NFormItem, NInput, NInputNumber, NSelect } from "naive-ui";
import type { CreditPreset } from "../api/billing.ts";
import { buildCreditSetup, creditSetupDraft } from "../domain/credit-setup.ts";
import { creditDisplayFactor, creditsToScaled } from "../domain/billing.ts";
import { t } from "../i18n/index.ts";
const props = defineProps<{ presets: readonly CreditPreset[]; disabled?: boolean }>();
const emit = defineEmits<{ change: [result: ReturnType<typeof buildCreditSetup>] }>();
const startedAt = Date.now();
const draft = ref(creditSetupDraft(props.presets, startedAt));
const options = computed(() => props.presets.map(p => ({ label: p.configuration.name, value: p.id })));
const maximum = computed(() => {
  const preset = props.presets.find(p => p.id === draft.value.presetId);
  return preset ? creditsToScaled(preset.initialGrant, creditDisplayFactor(props.presets.length)) ?? undefined : undefined;
});
function selectPreset(id: string): void {
  draft.value.presetId = id;
  draft.value.remaining = maximum.value ?? null;
}
function setReset(event: Event): void {
  draft.value.reset = event.target instanceof HTMLInputElement ? event.target.value : "";
}
watch(() => buildCreditSetup(draft.value, props.presets, startedAt), result => emit("change", result), { deep: true, immediate: true });
</script>
<style scoped>
.credit-setup { display: grid; gap: var(--ocg-space-sm); }
.credit-setup__amount { display: flex; align-items: center; gap: var(--ocg-space-sm); }
.credit-setup__date { width: 100%; padding: var(--ocg-space-sm); color: var(--ocg-ink); background: var(--ocg-surface); border: 1px solid var(--ocg-border); border-radius: var(--ocg-radius-sm); }
</style>
