<template>
  <n-card :data-account-id="routeId" size="small" class="account-card" :class="{
    'account-card--cooling': tone === 'cooling', 'account-card--pending': tone === 'pending',
    'account-card--draft': tone === 'draft', 'account-card--unavailable': tone === 'unavailable',
    'account-card--dragging': dragging,
  }">
    <template #header>
      <div class="account-title">
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button circle quaternary size="small" class="account-order-handle" :class="{ 'account-order-handle--dragging': dragging }"
              :disabled="orderHandleDisabled" :aria-label="t('拖动调整账号 {name} 的优先级', { name })" aria-describedby="account-order-instructions"
              @click.prevent @keydown="emit('order-keydown', $event)" @pointerdown="emit('order-drag-start', $event)">
              <template #icon><n-icon :component="HolderOutlined" /></template>
            </n-button>
          </template>
          {{ (orderHandleDisabled && orderHandleHint) || t("拖动调整账号 {name} 的优先级", { name }) }}
        </n-tooltip>
        <ProviderBrandMark :family="family" :size="BRAND_SIZE" class="account-brand" />
        <div class="account-heading">
          <div class="account-name-row"><span class="account-name">{{ name }}</span><n-tag v-if="showTypeTag" size="small" :bordered="false">{{ typeLabel }}</n-tag><slot name="tags" /></div>
          <span v-if="subtitle" class="account-subtitle mono">{{ subtitle }}</span>
        </div>
      </div>
    </template>
    <template #header-extra><div class="account-actions"><slot name="actions" /></div></template>
    <slot />
  </n-card>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NButton, NCard, NIcon, NTag, NTooltip } from "naive-ui";
import { HolderOutlined } from "@vicons/antd";
import type { ProviderFamily } from "../domain/provider-families.ts";
import { t } from "../i18n/index.ts";
import ProviderBrandMark from "./ProviderBrandMark.vue";

/** One shared header/action grammar for singleton accounts and credential groups. */
export type AccountCardTone = "cooling" | "pending" | "draft" | "unavailable" | null;
const BRAND_SIZE = 20;
const props = withDefaults(defineProps<{
  routeId: string; name: string; family: ProviderFamily; typeLabel: string;
  subtitle?: string; tone?: AccountCardTone; orderHandleDisabled: boolean;
  orderHandleHint?: string; dragging: boolean;
}>(), { subtitle: "", tone: null, orderHandleHint: "" });
const emit = defineEmits<{
  "order-keydown": [event: KeyboardEvent];
  "order-drag-start": [event: PointerEvent];
}>();
/** The type tag is redundant when the card name already carries the same text. */
const showTypeTag = computed(() => {
  const label = props.typeLabel.trim().toLowerCase();
  if (!label) return false;
  const name = props.name.trim().toLowerCase();
  return label !== name && !name.includes(label);
});
</script>

<style scoped>
.account-card { border-radius: var(--ocg-radius-lg); box-shadow: none; transition: border-color var(--ocg-motion-fast) var(--ocg-ease), opacity var(--ocg-motion-fast) var(--ocg-ease); }
.account-card--cooling { border-color: color-mix(in srgb, var(--ocg-warning) 45%, var(--ocg-border)); }
.account-card--unavailable { background: var(--ocg-surface-sunken); border-color: var(--ocg-border); }
.account-card--pending, .account-card--draft { border-color: color-mix(in srgb, var(--ocg-primary) 32%, var(--ocg-divider)); }
.account-card--dragging { border-color: var(--ocg-primary); box-shadow: var(--ocg-shadow-lg); opacity: 0.72; }
.account-title { display: flex; align-items: center; gap: var(--ocg-space-sm); min-width: 0; width: 100%; }
.account-order-handle { flex: 0 0 auto; cursor: grab; touch-action: none; user-select: none; }
.account-order-handle--dragging { cursor: grabbing; }
.account-brand { flex: 0 0 auto; }
.account-heading { display: grid; gap: var(--ocg-space-xs); flex: 1 1 auto; min-width: 0; }
.account-name-row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--ocg-space-xs) var(--ocg-space-sm); min-width: 0; }
.account-name { overflow: hidden; color: var(--ocg-ink); font-size: var(--ocg-font-md); font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.account-name-row :slotted(.n-tag), .account-name-row .n-tag { flex: 0 0 auto; }
.account-subtitle { overflow: hidden; color: var(--ocg-subtle); font-size: var(--ocg-font-xs); text-overflow: ellipsis; white-space: nowrap; max-width: 64ch; }
.account-actions { display: flex; align-items: center; justify-content: flex-end; column-gap: var(--ocg-space-xs); }
.account-actions :slotted(.account-action) { display: flex; align-items: center; justify-content: center; min-width: 0; }
.account-card--collapsed .account-subtitle { display: none; }
.account-card--collapsed :deep(.n-card__content) { display: none; }
@media (max-width: 900px) {
  .account-card :deep(.n-card-header) { align-items: flex-start; }
  .account-card :deep(.n-card-header__extra) { margin-left: var(--ocg-space-sm); }
}
@media (max-width: 640px) {
  .account-card :deep(.n-card-header) { flex-wrap: wrap; gap: var(--ocg-space-sm); }
  .account-card :deep(.n-card-header__main), .account-card :deep(.n-card-header__extra) { width: 100%; }
  .account-card :deep(.n-card-header__extra) { display: flex; justify-content: flex-end; margin-left: 0; }
}
</style>
