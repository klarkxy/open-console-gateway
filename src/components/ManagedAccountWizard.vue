<template>
  <n-modal
    :show="show"
    preset="card"
    class="managed-wizard-modal"
    style="width: 820px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    @update:show="$emit('update:show', $event)"
  >
    <template #header>
      <div class="managed-wizard__title">
        <span>{{ t("注册新账号：{name}", { name: account.name }) }}</span>
        <n-tag size="small" type="warning" :bordered="false">Beta</n-tag>
      </div>
    </template>

    <n-steps :current="currentStep" size="small" class="managed-wizard__steps">
      <n-step
        v-for="(step, index) in wizardSteps"
        :key="step.id"
        :status="stepStatus(index)"
      >
        <template #title>
          <button
            type="button"
            class="managed-wizard__step-btn"
            :class="{ 'managed-wizard__step-btn--active': index + 1 === currentStep }"
            :disabled="!canGoToStep(index) || busy"
            @click="goToStep(index)"
          >{{ step.title }}</button>
        </template>
      </n-step>
    </n-steps>

    <n-alert
      v-if="browserCapabilities.mode === 'unsupported'"
      type="error"
      class="managed-wizard__alert"
    >
      {{ browserCapabilities.reason || t("当前环境不支持独立浏览器") }}
    </n-alert>

    <section class="managed-wizard__stage">
      <template v-if="account.setup_step === 'google_account'">
        <p class="managed-wizard__kicker">{{ t("第 1 步，共 4 步") }}</p>
        <h2>{{ t("准备登录身份（可选）") }}</h2>
        <p>{{ t("需要新账号时在此注册 Google 或 GitHub；已有账号可直接跳过，登录在下一步完成。") }}</p>
        <div class="managed-wizard__actions">
          <n-space wrap>
            <n-button
              secondary
              :disabled="!browserAvailable"
              :loading="openingTarget === 'google_signup'"
              @click="$emit('openBrowser', 'google_signup')"
            >{{ t("注册 Google") }}</n-button>
            <n-button
              secondary
              :disabled="!browserAvailable"
              :loading="openingTarget === 'github_signup'"
              @click="$emit('openBrowser', 'github_signup')"
            >{{ t("注册 GitHub") }}</n-button>
          </n-space>
          <n-button type="primary" :loading="busy" @click="$emit('advance', 'opencode_registration')">
            {{ t("跳过此步") }}
          </n-button>
        </div>
      </template>

      <template v-else-if="account.setup_step === 'opencode_registration'">
        <p class="managed-wizard__kicker">{{ t("第 2 步，共 4 步") }}</p>
        <h2>{{ t("打开邀请并完成 OpenCode 登录/注册") }}</h2>
        <p>{{ t("在同一 Profile 打开邀请链接，用 Google 或 GitHub 登录。") }}</p>
        <div class="managed-wizard__actions">
          <n-button
            secondary
            :disabled="!browserAvailable"
            :loading="openingTarget === 'invite'"
            @click="$emit('openBrowser', 'invite')"
          >{{ t("打开邀请链接") }}</n-button>
          <n-button type="primary" :loading="busy" @click="$emit('advance', 'payment')">
            {{ t("我已完成登录/注册") }}
          </n-button>
        </div>
      </template>

      <template v-else-if="account.setup_step === 'payment'">
        <p class="managed-wizard__kicker">{{ t("第 3 步，共 4 步") }}</p>
        <h2>{{ t("完成支付") }}</h2>
        <p>{{ t("在控制台确认套餐与金额；支付仅由你在页面上完成。") }}</p>
        <div class="managed-wizard__actions">
          <n-button
            secondary
            :disabled="!browserAvailable"
            :loading="openingTarget === 'console'"
            @click="$emit('openBrowser', 'console')"
          >{{ t("打开控制台") }}</n-button>
          <n-button type="primary" :loading="busy" @click="$emit('advance', 'key_verification')">
            {{ t("我已完成支付") }}
          </n-button>
        </div>
      </template>

      <template v-else-if="account.setup_step === 'key_verification'">
        <p class="managed-wizard__kicker">{{ t("第 4 步，共 4 步") }}</p>
        <h2>{{ t("粘贴并验证 Key") }}</h2>
        <p>{{ t("从控制台复制 Key；实测成功后账号才启用。") }}</p>
        <n-input
          v-model:value="keyDraft"
          type="password"
          show-password-on="click"
          class="managed-wizard__key"
          placeholder="sk-..."
          :input-props="{ 'aria-label': t('API Key') }"
          @keydown.enter.prevent="verifyKey"
        />
        <div class="managed-wizard__actions">
          <n-button
            secondary
            :disabled="!browserAvailable"
            :loading="openingTarget === 'console'"
            @click="$emit('openBrowser', 'console')"
          >{{ t("打开控制台") }}</n-button>
          <n-button type="primary" :disabled="!keyDraft.trim()" :loading="busy" @click="verifyKey">
            {{ t("保存并实测 Key") }}
          </n-button>
        </div>
      </template>
    </section>

  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from "vue";
import {
  NAlert,
  NButton,
  NInput,
  NModal,
  NSpace,
  NStep,
  NSteps,
  NTag,
} from "naive-ui";
import type {
  Account,
  AccountSetupStep,
  BrowserCapabilities,
  BrowserTarget,
} from "../api/dashboard";
import { t } from "../i18n/index.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import { MANAGED_SETUP_STEPS, setupStepIndex } from "../domain/managed-account.ts";

const props = defineProps<{
  show: boolean;
  account: Account;
  browserCapabilities: BrowserCapabilities;
  openingTarget: BrowserTarget | null;
  busy: boolean;
}>();

useLocalizedModalCloseLabel(toRef(props, "show"), "managed-wizard-modal");

const emit = defineEmits<{
  (event: "update:show", value: boolean): void;
  (event: "openBrowser", target: BrowserTarget): void;
  (event: "advance", setupStep: AccountSetupStep): void;
  (event: "verifyKey", key: string): void;
}>();

const keyDraft = ref("");
const wizardStepIds = MANAGED_SETUP_STEPS.filter((step) => step !== "ready");
const wizardSteps = computed(() => [
  { id: "google_account" as const, title: t("登录身份") },
  { id: "opencode_registration" as const, title: t("邀请注册") },
  { id: "payment" as const, title: t("完成支付") },
  { id: "key_verification" as const, title: t("验证 Key") },
]);
const currentStep = computed(() => Math.min(4, setupStepIndex(props.account.setup_step) + 1));
const browserAvailable = computed(() => props.browserCapabilities.mode !== "unsupported");

watch(() => [props.show, props.account.id, props.account.setup_step] as const, () => {
  keyDraft.value = "";
});

function stepStatus(index: number): "process" | "finish" | "wait" {
  const current = currentStep.value;
  if (index + 1 < current) return "finish";
  if (index + 1 === current) return "process";
  return "wait";
}

function canGoToStep(index: number): boolean {
  // Only rewind to earlier finished steps; forward stays on primary CTAs.
  return index + 1 < currentStep.value;
}

function goToStep(index: number): void {
  if (!canGoToStep(index) || props.busy) return;
  const target = wizardStepIds[index];
  if (target && target !== props.account.setup_step) {
    emit("advance", target);
  }
}

function verifyKey(): void {
  const key = keyDraft.value.trim();
  if (key) emit("verifyKey", key);
}
</script>

<style scoped>
.managed-wizard__title {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
  font-size: var(--ocg-font-lg);
  font-weight: 700;
}

.managed-wizard__steps {
  margin-bottom: var(--ocg-space-lg);
}

.managed-wizard__step-btn {
  margin: 0;
  padding: 0;
  border: 0;
  color: inherit;
  font: inherit;
  font-weight: inherit;
  line-height: inherit;
  text-align: left;
  background: transparent;
  cursor: pointer;
}

.managed-wizard__step-btn:disabled {
  cursor: default;
}

.managed-wizard__step-btn:not(:disabled):hover,
.managed-wizard__step-btn:not(:disabled):focus-visible {
  color: var(--ocg-primary);
  text-decoration: underline;
  outline: none;
}

.managed-wizard__step-btn--active {
  cursor: default;
}

.managed-wizard__alert {
  margin-bottom: 14px;
}

.managed-wizard__stage {
  display: grid;
  gap: 14px;
  min-height: 220px;
  padding: 22px;
  border: 1px solid var(--ocg-divider);
  border-radius: var(--ocg-radius-lg);
}

.managed-wizard__stage h2 {
  margin: 0;
  font-size: var(--ocg-font-lg);
}

.managed-wizard__stage p {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}

.managed-wizard__kicker {
  color: var(--ocg-primary) !important;
  font-weight: 700;
}

.managed-wizard__key {
  max-width: 560px;
}

.managed-wizard__actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-md);
}

@media (max-width: 640px) {
  .managed-wizard__actions {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
