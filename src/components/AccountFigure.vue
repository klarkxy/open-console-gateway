<template>
  <div class="account-figure" :class="{ 'account-figure--compact': compact }">
    <span v-if="label" class="account-figure__label">{{ label }}</span>
    <strong class="account-figure__value mono">{{ value }}</strong>
    <span v-if="caption" class="account-figure__caption">{{ caption }}</span>
  </div>
</template>

<script setup lang="ts">
/**
 * One machine-readable figure on an account card (wallet balance, Key
 * remaining). Same typography as a quota-row heading — muted label, tabular
 * monospace value in ink — so pay-as-you-go cards read like plan cards. The
 * value is data, never a state: no semantic color.
 */
withDefaults(
  defineProps<{
    value: string;
    label?: string;
    caption?: string;
    /** Inline variant for list rows: smaller value, no caption spacing. */
    compact?: boolean;
  }>(),
  { label: "", caption: "", compact: false },
);
</script>

<style scoped>
.account-figure {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--ocg-space-xs) var(--ocg-space-sm);
  min-width: 0;
}

.account-figure__label {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.account-figure__value {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
  font-weight: 600;
}

.account-figure__caption {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.account-figure--compact .account-figure__value {
  font-size: var(--ocg-font-sm);
}
</style>
