<template>
  <section class="preset-browser" aria-labelledby="preset-browser-title">
    <header class="preset-browser-head">
      <h2 id="preset-browser-title">{{ t("添加供应商") }}</h2>
      <n-button secondary size="small" :disabled="busy" @click="$emit('cancel')">
        {{ t("返回") }}
      </n-button>
    </header>
    <div class="preset-browser-search">
      <n-input
        v-model:value="query"
        size="small"
        clearable
        :placeholder="t('搜索预设')"
        :input-props="{ 'aria-label': t('搜索预设') }"
      />
    </div>
    <div class="preset-browser-list">
      <button type="button" class="preset-browser-manual" :disabled="busy" @click="$emit('select', null)">
        {{ t("手动配置") }}
      </button>
      <section v-for="pane in panes" :key="pane.id" class="preset-browser-pane">
        <h3 class="preset-browser-pane__label">{{ pane.label }}</h3>
        <n-menu
          :options="pane.options"
          :default-expanded-keys="defaultExpandedKeys"
          :aria-label="`${t('供应商预设')} · ${pane.label}`"
          @update:value="onSelect"
        />
      </section>
      <p v-if="filteredOut" class="preset-browser-empty">{{ t("无匹配预设") }}</p>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, h, ref } from "vue";
import type { VNodeChild } from "vue";
import { NButton, NInput, NMenu } from "naive-ui";
import type { MenuOption } from "naive-ui";
import ProviderBrandMark from "./ProviderBrandMark.vue";
import { t } from "../i18n/index.ts";
import {
  PROVIDER_PRESETS,
  filterProviderPresets,
  groupProviderPresetsByOffering,
  type ProviderPreset,
} from "../domain/provider-presets.ts";
import { groupPresetsByFamily, type ProviderFamily } from "../domain/provider-families.ts";

const props = withDefaults(defineProps<{ busy?: boolean }>(), { busy: false });

const emit = defineEmits<{
  /** null is the manual (preset-less) full form. */
  (event: "select", presetId: string | null): void;
  (event: "cancel"): void;
}>();

const query = ref("");
const BRAND_SIZE = 18;

const groups = computed(() => (
  groupProviderPresetsByOffering(filterProviderPresets(PROVIDER_PRESETS, query.value))
));
const filteredOut = computed(() => (
  Boolean(query.value.trim())
  && groups.value.plan.length === 0
  && groups.value.api.length === 0
));

function brandIcon(family: ProviderFamily): () => VNodeChild {
  return () => h(ProviderBrandMark, { family, size: BRAND_SIZE });
}

function presetVariantHost(preset: ProviderPreset): string {
  const raw = preset.endpointUrl || preset.endpointPlaceholder || "";
  if (!raw) return "";
  try {
    return new URL(raw).host;
  } catch {
    return raw;
  }
}

function presetFamilyMenuOptions(
  presets: readonly ProviderPreset[],
  offering: "plan" | "api",
): MenuOption[] {
  return groupPresetsByFamily(presets).map(({ family, presets: familyPresets }) => {
    if (familyPresets.length === 1) {
      const preset = familyPresets[0]!;
      return {
        key: preset.id,
        label: family.label,
        icon: brandIcon(family),
      };
    }
    return {
      // Family ids carry the offering so the same vendor can appear once per
      // group without a key collision.
      key: `family:${offering}:${family.id}`,
      label: family.label,
      icon: brandIcon(family),
      extra: String(familyPresets.length),
      children: familyPresets.map((preset) => ({
        key: preset.id,
        label: () => h("span", { class: "preset-browser-preset" }, [
          h("span", { class: "preset-browser-preset__variant" }, preset.variant ?? preset.name),
          presetVariantHost(preset)
            ? h("span", { class: "preset-browser-preset__host mono" }, presetVariantHost(preset))
            : null,
        ]),
        icon: brandIcon(family),
      })),
    };
  });
}

const panes = computed<Array<{ id: "plan" | "api"; label: "Plan" | "API"; options: MenuOption[] }>>(() => {
  const result: Array<{ id: "plan" | "api"; label: "Plan" | "API"; options: MenuOption[] }> = [];
  const planOptions = presetFamilyMenuOptions(groups.value.plan, "plan");
  const apiOptions = presetFamilyMenuOptions(groups.value.api, "api");
  if (planOptions.length) result.push({ id: "plan", label: "Plan", options: planOptions });
  result.push({ id: "api", label: "API", options: apiOptions });
  return result;
});

/**
 * Pre-expand every multi-variant family so the variant rows are visible
 * without an extra click. Single-variant families stay flat.
 */
const defaultExpandedKeys = computed<string[]>(() => (
  panes.value.flatMap((pane) => (
    pane.options.flatMap((option) => (
      option.children && option.key ? [String(option.key)] : []
    ))
  ))
));

function onSelect(key: string | number): void {
  if (props.busy) return;
  const value = String(key);
  if (value.startsWith("family:")) return;
  if (!PROVIDER_PRESETS.some((preset) => preset.id === value)) return;
  emit("select", value);
}
</script>

<style scoped>
.preset-browser {
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--ocg-border);
  border-radius: 14px;
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.preset-browser-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
}
.preset-browser-head h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.preset-browser-search {
  margin-bottom: 12px;
}
.preset-browser-list {
  display: grid;
  gap: 12px;
}
.preset-browser-manual {
  padding: 8px 12px;
  border: 1px dashed var(--ocg-border);
  border-radius: 10px;
  background: transparent;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  text-align: left;
  cursor: pointer;
}
.preset-browser-manual:hover:not(:disabled) {
  background: var(--ocg-canvas);
}
.preset-browser-manual:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}
.preset-browser-pane__label {
  margin: 0 0 4px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  line-height: 1.3;
}
.preset-browser-empty {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

/* Vendor family row label (parent) and variant row label (child), matching
   the Accounts dialog variant picker. */
.preset-browser-preset {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  min-width: 0;
  line-height: 1.25;
}
.preset-browser-preset__variant {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 500;
}
.preset-browser-preset__host {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-variant-numeric: tabular-nums;
  word-break: break-all;
}
.preset-browser-list :deep(.n-menu-item-content .preset-browser-preset__host) {
  color: var(--ocg-muted);
}
</style>
