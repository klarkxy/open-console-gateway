<template>
  <p class="rate-hint">{{ t("每百万 Token，按原币估算；未知模型、复杂分档或额外收费请求保持未知。") }}</p>
  <div v-for="(rate, index) in modelValue" :key="index" class="rate-row">
    <n-form-item :label="t('上游模型')"><n-input :value="rate.model" :disabled="disabled" :aria-label="t('上游模型')" @update:value="value => update(index, { model: value })" /></n-form-item>
    <div class="rate-values">
      <n-form-item v-for="field in fields" :key="field.key" :label="t(field.label)">
        <n-input-number :value="rate[field.key]" :min="0" :show-button="false" :disabled="disabled" :placeholder="t('未知')" :aria-label="t(field.label)" :input-props="{ 'aria-label': t(field.label) }" @update:value="value => update(index, { [field.key]: value })" />
      </n-form-item>
    </div>
    <n-button size="tiny" quaternary :disabled="disabled" @click="emit('update:modelValue', modelValue.filter((_, i) => i !== index))">{{ t("删除") }}</n-button>
  </div>
  <n-button size="tiny" secondary :disabled="disabled" @click="add">{{ t("添加费率") }}</n-button>
</template>
<script setup lang="ts">
import { NButton, NFormItem, NInput, NInputNumber } from "naive-ui";
import type { CreditRate } from "../api/billing.ts";
import { t } from "../i18n/index.ts";
const props = defineProps<{ modelValue: CreditRate[]; disabled?: boolean }>();
const emit = defineEmits<{ "update:modelValue": [rates: CreditRate[]] }>();
const fields = [ { key: "inputPerMillion", label: "输入" }, { key: "outputPerMillion", label: "输出" }, { key: "cacheReadPerMillion", label: "缓存读" }, { key: "cacheWritePerMillion", label: "缓存写" } ] as const;
function update(index: number, patch: Partial<CreditRate>): void { emit("update:modelValue", props.modelValue.map((row, i) => i === index ? { ...row, ...patch } : row)); }
function add(): void { emit("update:modelValue", [...props.modelValue, { model: "", inputPerMillion: 0, outputPerMillion: 0, cacheReadPerMillion: null, cacheWritePerMillion: null }]); }
</script>
<style scoped>
.rate-row { display: grid; gap: var(--ocg-space-xs); }
.rate-values { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--ocg-space-sm); }
.rate-hint { color: var(--ocg-muted); font-size: var(--ocg-font-xs); }
</style>
