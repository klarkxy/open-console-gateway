<template>
  <div class="account-routing">
    <div class="routing-control">
      <n-tooltip placement="bottom-start" :show="routingHelpFocused || undefined">
        <template #trigger>
          <n-button
            text size="tiny" icon-placement="right" :aria-label="t('账号路由')"
            @focus="routingHelpFocused = true" @blur="routingHelpFocused = false"
            @keydown.esc="routingHelpFocused = false"
          >
            {{ t("账号路由") }}
            <template #icon><n-icon :component="QuestionCircleOutlined" /></template>
          </n-button>
        </template>
        <div class="routing-help">
          <p v-for="option in routingOptions" :key="option.value">
            <strong>{{ option.label }}</strong><br />
            {{ t(ROUTING_MODE_DESCRIPTION_KEYS[option.value]) }}
          </p>
        </div>
      </n-tooltip>
      <n-select
        class="routing-select"
        :value="settingsStore.settings?.routing_mode ?? null"
        :options="routingOptions"
        :aria-label="t('账号路由')"
        :placeholder="t('加载中…')"
        :disabled="disabled"
        :loading="saving || settingsStore.loading"
        :consistent-menu-width="false"
        size="small"
        @update:value="save({ routing_mode: $event })"
      />
    </div>
    <div class="routing-control">
      <n-tooltip placement="bottom-start" :show="stickyHelpFocused || undefined">
        <template #trigger>
          <n-button
            text size="tiny" icon-placement="right" :aria-label="t('对话粘性')"
            @focus="stickyHelpFocused = true" @blur="stickyHelpFocused = false"
            @keydown.esc="stickyHelpFocused = false"
          >
            {{ t("对话粘性") }}
            <template #icon><n-icon :component="QuestionCircleOutlined" /></template>
          </n-button>
        </template>
        <div class="routing-help">
          <p>{{ t("同一时刻只能选择一个基础路由方案；对话粘性是可叠加开关，不会替换基础方案。") }}</p>
          <p>{{ t("同一对话尽量走同一账号。可带 X-OCG-Conversation-Id，否则用 Prompt 指纹。") }}</p>
        </div>
      </n-tooltip>
      <n-switch
        :value="settingsStore.settings?.conversation_sticky ?? false"
        :aria-label="t('对话粘性')"
        :disabled="disabled"
        size="small"
        @update:value="save({ conversation_sticky: $event })"
      >
        <template #checked>{{ t("开启") }}</template>
        <template #unchecked>{{ t("关闭") }}</template>
      </n-switch>
    </div>
    <n-alert v-if="settingsStore.error" class="routing-error" type="error">
      {{ t("加载设置失败：{error}", { error: settingsStore.error }) }}
      <n-button size="small" :loading="settingsStore.loading" @click="reload">
        {{ t("重试") }}
      </n-button>
    </n-alert>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { NAlert, NButton, NIcon, NSelect, NSwitch, NTooltip, useMessage } from "naive-ui";
import { QuestionCircleOutlined } from "@vicons/antd";
import { isRevisionConflict, type AppConfig, type RoutingMode } from "../api/dashboard.ts";
import { ROUTING_MODE_KEYS, ROUTING_MODE_DESCRIPTION_KEYS } from "../domain/routing-explain.ts";
import { t } from "../i18n/index.ts";
import { useSettingsStore } from "../stores/settings.ts";

const settingsStore = useSettingsStore();
const message = useMessage();
const saving = ref(false);
const routingHelpFocused = ref(false);
const stickyHelpFocused = ref(false);
const disabled = computed(() => saving.value || settingsStore.loading || !settingsStore.settings || !!settingsStore.error);
const routingOptions = computed(() => (Object.keys(ROUTING_MODE_KEYS) as RoutingMode[]).map((value) => ({
  value, label: t(ROUTING_MODE_KEYS[value]),
})));

async function reload() {
  await settingsStore.loadPresented().catch(() => undefined);
}

async function save(update: Partial<Pick<AppConfig, "routing_mode" | "conversation_sticky">>) {
  const current = settingsStore.settings;
  if (disabled.value || !current) return;
  if (Object.entries(update).every(([key, value]) => current[key as keyof AppConfig] === value)) return;
  saving.value = true;
  try {
    await settingsStore.putPresented({ ...current, ...update });
    message.success(t("设置已保存；运行时路由状态已重置"));
  } catch (error) {
    if (isRevisionConflict(error)) {
      message.warning(t("状态已变化，请刷新后重试。"));
    } else {
      message.error(t("保存失败：{error}", { error: String(error) }));
    }
  } finally {
    saving.value = false;
  }
}
</script>

<style scoped>
.account-routing {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm) var(--ocg-space-xl);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.routing-control {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  min-width: 0;
}
.routing-select { width: 160px; }
.routing-help { max-width: min(320px, 80vw); }
.routing-help p { margin: 0; }
.routing-help p + p { margin-top: var(--ocg-space-sm); }
.routing-error { flex-basis: 100%; }
</style>
