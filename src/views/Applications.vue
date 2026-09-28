<template>
  <div class="applications-page">
    <h1 class="sr-only">{{ t("应用") }}</h1>
    <TabsRoot v-model="activeTab" class="applications-tabs" activation-mode="manual">
      <TabsList ref="tabList" class="applications-tab-list" :aria-label="t('应用')">
        <TabsTrigger value="dsh" class="applications-tab">DSH</TabsTrigger>
        <TabsTrigger
          v-for="client in BYOK_CLIENTS"
          :key="client"
          :value="client"
          class="applications-tab"
        >
          {{ BYOK_CLIENT_LABELS[client] }}
        </TabsTrigger>
      </TabsList>
      <TabsContent value="dsh" class="applications-tab-content">
        <DshApplicationPanel />
      </TabsContent>
      <TabsContent
        v-for="client in BYOK_CLIENTS"
        :key="client"
        :value="client"
        class="applications-tab-content"
      >
        <ByokApplicationPanel :client="client" />
      </TabsContent>
    </TabsRoot>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from "reka-ui";
import { t } from "../i18n/index.ts";
import {
  BYOK_CLIENTS,
  BYOK_CLIENT_LABELS,
  readApplicationsTab,
  type ApplicationsTab,
} from "../domain/byok-applications.ts";
import { routeQuerySearch } from "./app-navigation.ts";
import DshApplicationPanel from "../components/applications/DshApplicationPanel.vue";
import ByokApplicationPanel from "../components/applications/ByokApplicationPanel.vue";

const route = useRoute();
const router = useRouter();

// Roving tabindex plus Arrow/Home/End still move focus; manual activation
// keeps keyboard focus from remounting every harness inspector.
const activeTab = ref<ApplicationsTab>(readApplicationsTab(routeQuerySearch("applications", route.query)));
const tabList = ref<{ $el?: HTMLElement } | null>(null);

// Keep the active trigger reachable when the strip overflows on narrow screens.
function revealActiveTab(): void {
  void nextTick(() => {
    const list = tabList.value?.$el as Partial<HTMLElement> | undefined;
    if (typeof list?.querySelector !== "function") return;
    list.querySelector('[data-state="active"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });
}

watch(activeTab, (tab) => {
  if (route.query.app !== tab) {
    void router.replace({ query: { ...route.query, app: tab } });
  }
  revealActiveTab();
});

// A deep-linked reload (e.g. ?app=zcode) starts on the clipped trigger.
onMounted(revealActiveTab);

// Same-view navigation (back/forward, deep links) drives the tab back.
watch(() => route.query.app, () => {
  const tab = readApplicationsTab(routeQuerySearch("applications", route.query));
  if (tab !== activeTab.value) activeTab.value = tab;
});
</script>

<style scoped>
.applications-page {
  min-width: 0;
  max-width: 1060px;
  margin: 0 auto;
  overflow-x: hidden;
}
.applications-tab-list {
  display: flex;
  gap: var(--ocg-space-xl);
  margin-bottom: var(--ocg-space-md);
  border-bottom: 1px solid var(--ocg-divider);
  overflow-x: auto;
  scrollbar-width: thin;
}
.applications-tab {
  appearance: none;
  margin: 0;
  padding: var(--ocg-space-sm) 2px;
  border: none;
  border-bottom: 2px solid transparent;
  background: none;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-md);
  font-weight: 500;
  white-space: nowrap;
  cursor: pointer;
}
.applications-tab:hover {
  color: var(--ocg-primary-hover);
}
.applications-tab[data-state="active"] {
  color: var(--ocg-primary);
  border-bottom-color: var(--ocg-primary);
}
.applications-tab:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: -2px;
  border-radius: var(--ocg-radius-sm);
}
.applications-tab-content {
  min-width: 0;
  outline: none;
}
</style>
