<template>
  <section class="invite-section" aria-labelledby="opencode-invite-title">
    <h2 id="opencode-invite-title">{{ t("OpenCode 邀请链接（注册新账号）") }}</h2>
    <n-alert
      v-if="loadError"
      type="error"
      :title="t('加载设置失败：{error}', { error: loadError })"
    >
      <n-button size="small" secondary :loading="loading" @click="loadInvite">
        {{ t("重试") }}
      </n-button>
    </n-alert>
    <n-form v-if="loaded" :show-feedback="false">
      <n-form-item
        :show-feedback="true"
        :validation-status="inviteUrlPreview.status"
        :feedback="inviteUrlPreview.feedback"
      >
        <n-input
          v-model:value="inviteUrl"
          clearable
          class="mono"
          :disabled="!loaded || saving"
          :placeholder="DEFAULT_OPENCODE_INVITE_URL"
          :input-props="{ 'aria-label': t('OpenCode 邀请链接（注册新账号）') }"
          @blur="saveInviteUrl"
        />
      </n-form-item>
    </n-form>
  </section>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NForm, NFormItem, NInput, useMessage } from "naive-ui";
import { useSettingsStore } from "../stores/settings.ts";
import { useSessionStore } from "../stores/session.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import {
  DEFAULT_OPENCODE_INVITE_URL,
  normalizeOpenCodeInviteUrl,
} from "../domain/managed-account.ts";

const message = useMessage();
const settingsStore = useSettingsStore();
const sessionStore = useSessionStore();
const inviteUrl = ref("");
const savedInviteUrl = ref("");
const loaded = ref(false);
const loading = ref(false);
const saving = ref(false);
const loadError = ref("");
let activatedOnce = false;
let inviteLoadGeneration = 0;

watch(() => sessionStore.authenticated, (ok) => {
  if (ok) return;
  inviteLoadGeneration += 1;
  inviteUrl.value = "";
  savedInviteUrl.value = "";
  loaded.value = false;
  loading.value = false;
  saving.value = false;
  loadError.value = "";
});

const inviteUrlPreview = computed<{ status?: "error"; feedback: string }>(() => {
  try {
    const normalized = normalizeOpenCodeInviteUrl(inviteUrl.value);
    if (!normalized) {
      return {
        feedback: t("留空时“注册新账号”入口不可用。仅接受 opencode.ai 官方 HTTPS 链接。"),
      };
    }
    return {
      feedback: t("仅用于注册向导打开邀请页面。注册前请改为你自己的邀请链接；默认链接仅作演示，注册收益归链接所有者。"),
    };
  } catch (error) {
    return {
      status: "error",
      feedback: error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"),
    };
  }
});

async function loadInvite(): Promise<void> {
  const generation = ++inviteLoadGeneration;
  const epoch = sessionStore.sessionEpoch;
  loading.value = true;
  loadError.value = "";
  try {
    const settings = await settingsStore.loadPresented();
    if (generation !== inviteLoadGeneration || sessionStore.sessionEpoch !== epoch) return;
    const keepDraft = loaded.value && inviteUrl.value !== savedInviteUrl.value;
    if (!keepDraft) inviteUrl.value = settings.opencode_invite_url;
    savedInviteUrl.value = settings.opencode_invite_url;
    loaded.value = true;
  } catch (error) {
    if (generation !== inviteLoadGeneration || sessionStore.sessionEpoch !== epoch) return;
    loadError.value = error instanceof Error ? dashboardErrorDetail(error) : "";
    message.error(t("加载设置失败：{error}", { error: loadError.value }));
  } finally {
    if (generation === inviteLoadGeneration) loading.value = false;
  }
}

async function saveInviteUrl(): Promise<void> {
  if (!loaded.value || saving.value) return;
  const epoch = sessionStore.sessionEpoch;
  let normalized: string;
  try {
    normalized = normalizeOpenCodeInviteUrl(inviteUrl.value);
  } catch (error) {
    message.error(error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"));
    return;
  }
  inviteUrl.value = normalized;
  if (normalized === savedInviteUrl.value) return;
  saving.value = true;
  try {
    await settingsStore.patchPresented({ opencode_invite_url: normalized });
    if (sessionStore.sessionEpoch !== epoch) return;
    savedInviteUrl.value = normalized;
    message.success(t("邀请链接已保存"));
  } catch (error) {
    if (sessionStore.sessionEpoch !== epoch) return;
    const detail = error instanceof Error ? dashboardErrorDetail(error) : "";
    message.error(t("保存失败：{error}", { error: detail }));
  } finally {
    saving.value = false;
  }
}

onMounted(() => {
  void loadInvite();
});
onActivated(() => {
  if (activatedOnce) {
    if (!saving.value) void loadInvite();
  } else {
    activatedOnce = true;
  }
});
</script>

<style scoped>
.invite-section {
  min-width: 0;
  padding: var(--ocg-space-lg);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.invite-section h2 {
  margin: 0 0 var(--ocg-space-md);
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
</style>
