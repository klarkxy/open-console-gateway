<template>
  <div class="api-price-meter">
    <div class="api-price-meter__body">
      <div class="api-price-meter__cells">
        <div
          v-for="cell in cells"
          :key="cell.key"
          class="api-price-meter__cell"
        >
          <n-tooltip v-if="cell.hint" trigger="hover">
            <template #trigger>
              <div class="api-price-meter__stack" tabindex="0">
                <span class="api-price-meter__label">{{ cell.label }}</span>
                <strong class="api-price-meter__value mono">{{ cell.value }}</strong>
                <span v-if="cell.caption" class="api-price-meter__caption">{{ cell.caption }}</span>
              </div>
            </template>
            {{ cell.hint }}
          </n-tooltip>
          <div v-else class="api-price-meter__stack">
            <span class="api-price-meter__label">{{ cell.label }}</span>
            <strong class="api-price-meter__value mono">{{ cell.value }}</strong>
            <span v-if="cell.caption" class="api-price-meter__caption">{{ cell.caption }}</span>
          </div>
        </div>
      </div>
      <p v-if="caption" class="api-price-meter__meter-caption">{{ caption }}</p>
    </div>
    <div v-if="$slots.refresh" class="api-price-meter__action">
      <slot name="refresh" />
    </div>
  </div>
</template>

<script setup lang="ts">
import { NTooltip } from "naive-ui";

export interface ApiPriceMeterCell {
  key: string;
  label: string;
  value: string;
  caption?: string;
  hint?: string;
}

withDefaults(defineProps<{
  cells: readonly ApiPriceMeterCell[];
  caption?: string;
}>(), { caption: "" });
</script>

<style scoped>
.api-price-meter {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  min-width: 0;
}

.api-price-meter__body {
  display: grid;
  gap: var(--ocg-space-xs);
  min-width: 0;
}

.api-price-meter__cells {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-lg) var(--ocg-space-2xl);
  min-width: 0;
}

.api-price-meter__meter-caption {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.api-price-meter__stack {
  display: grid;
  justify-items: start;
  gap: var(--ocg-space-xs);
  min-width: 0;
  border-radius: var(--ocg-radius-sm);
}

.api-price-meter__stack:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: 2px;
}

.api-price-meter__label {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.api-price-meter__value {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-xl);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  line-height: 1.15;
}

.api-price-meter__caption {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.api-price-meter__action {
  flex: 0 0 auto;
  padding-top: var(--ocg-space-xs);
}

@media (max-width: 640px) {
  .api-price-meter {
    flex-wrap: wrap;
  }
}
</style>
