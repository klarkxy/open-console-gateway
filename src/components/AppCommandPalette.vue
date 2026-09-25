<template>
  <n-tooltip trigger="hover">
    <template #trigger>
      <n-button quaternary class="command-trigger" :aria-label="t('搜索全部选项')" aria-keyshortcuts="Control+k Meta+k" @click="open">
        <template #icon><n-icon :component="SearchOutlined" /></template>
        <span class="command-trigger-label">{{ t("搜索全部选项") }}</span>
        <kbd class="command-shortcut" aria-hidden="true">{{ shortcut }}</kbd>
      </n-button>
    </template>
    {{ t("搜索全部选项") }} · {{ shortcut }}
  </n-tooltip>
  <n-modal
    v-model:show="shown" preset="card" :title="t('搜索全部选项')"
    class="ocg-command-palette" style="width: min(560px, calc(100vw - 32px))"
    :auto-focus="false" @after-enter="focusSearch"
  >
    <n-input
      ref="searchInput" v-model:value="query" clearable :placeholder="t('搜索全部选项')"
      :input-props="{
        'aria-label': t('搜索全部选项'), role: 'combobox', 'aria-autocomplete': 'list',
        'aria-controls': 'ocg-navigation-results', 'aria-expanded': shown,
        'aria-activedescendant': results[activeIndex] ? `ocg-nav-result-${results[activeIndex].key}` : undefined,
      }"
      @keydown="onSearchKeydown"
    >
      <template #prefix><n-icon :component="SearchOutlined" aria-hidden="true" /></template>
    </n-input>
    <div id="ocg-navigation-results" ref="resultsElement" class="command-results" role="listbox" :aria-label="t('搜索全部选项')">
      <div
        v-for="(item, index) in results" :id="`ocg-nav-result-${item.key}`" :key="item.key"
        class="command-result" :class="{ 'command-result--active': index === activeIndex }"
        role="option" :aria-selected="index === activeIndex" @mousemove="activeIndex = index" @click="choose(item.key)"
      >
        <n-icon :component="item.icon" aria-hidden="true" />
        <span>{{ item.label }}</span>
        <n-icon v-if="item.key === activeKey" :component="CheckOutlined" aria-hidden="true" />
      </div>
      <div v-if="!results.length" class="command-empty" role="status">{{ t("无匹配选项") }}</div>
    </div>
    <template #footer>
      <div class="command-footer"><span aria-hidden="true"><kbd>↑</kbd> <kbd>↓</kbd> <kbd>Enter</kbd></span><span><kbd>Esc</kbd> {{ t("取消") }}</span></div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import type { Component } from "vue";
import { NButton, NIcon, NInput, NModal, NTooltip } from "naive-ui";
import type { InputInst } from "naive-ui";
import { CheckOutlined, SearchOutlined } from "@vicons/antd";
import { t } from "../i18n/index.ts";
import { filterNavigationItems, isNavigationShortcut, stepSelection } from "../domain/navigation-search.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

const props = defineProps<{ items: Array<{ key: string; label: string; icon: Component }>; activeKey: string }>();
const emit = defineEmits<{ select: [key: string] }>();
const shown = ref(false);
const query = ref("");
const activeIndex = ref(0);
const searchInput = ref<InputInst | null>(null);
const resultsElement = ref<HTMLElement | null>(null);
const results = computed(() => filterNavigationItems(props.items, query.value));
const shortcut = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘ K" : "Ctrl K";

useLocalizedModalCloseLabel(shown, "ocg-command-palette");
watch(results, () => { activeIndex.value = 0; });
watch(activeIndex, () => {
  void nextTick(() => resultsElement.value?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" }));
});
function open() { query.value = ""; activeIndex.value = 0; shown.value = true; }
function focusSearch() { searchInput.value?.focus(); }
function choose(key: string) { shown.value = false; emit("select", key); }
function onSearchKeydown(event: KeyboardEvent) {
  if (event.isComposing) return;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    activeIndex.value = stepSelection(activeIndex.value, event.key === "ArrowDown" ? 1 : -1, results.value.length);
  } else if (event.key === "Enter" && results.value[activeIndex.value]) {
    event.preventDefault();
    choose(results.value[activeIndex.value].key);
  }
}
function onShortcut(event: KeyboardEvent) {
  if (!isNavigationShortcut(event)) return;
  // Never open a second modal over a credential form or a destructive confirmation.
  if (!shown.value && document.querySelector('.n-modal, .n-dialog, [role="dialog"], [aria-modal="true"]')) return;
  event.preventDefault();
  if (shown.value) shown.value = false;
  else open();
}
onMounted(() => window.addEventListener("keydown", onShortcut));
onUnmounted(() => window.removeEventListener("keydown", onShortcut));
</script>

<style scoped>
.command-trigger { color: var(--ocg-muted); }
.command-trigger-label { font-size: var(--ocg-font-sm); }
.command-shortcut { margin-left: var(--ocg-space-sm); }
.command-results { display: grid; gap: var(--ocg-space-xs); margin-top: var(--ocg-space-md); max-height: min(420px, 52vh); overflow-y: auto; overscroll-behavior: contain; }
.command-result { display: grid; grid-template-columns: 20px minmax(0, 1fr) 16px; align-items: center; gap: var(--ocg-space-md); padding: var(--ocg-space-md); color: var(--ocg-muted); border-radius: var(--ocg-radius-md); cursor: pointer; transition: background-color var(--ocg-motion-fast) var(--ocg-ease), color var(--ocg-motion-fast) var(--ocg-ease); }
.command-result--active { background: var(--ocg-primary-soft); color: var(--ocg-primary); }
.command-empty { padding: var(--ocg-space-2xl); text-align: center; color: var(--ocg-muted); }
.command-footer { display: flex; justify-content: space-between; align-items: center; gap: var(--ocg-space-md); color: var(--ocg-muted); font-size: var(--ocg-font-xs); }
@media (max-width: 1100px) { .command-trigger-label, .command-shortcut { display: none; } }
</style>
