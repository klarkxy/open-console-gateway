<template>
  <n-modal
    :show="show"
    preset="card"
    :title="isUpdate ? t('更新 {client} 配置', { client: label }) : t('配置 {client}', { client: label })"
    class="byok-configure-modal"
    style="width: 560px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <div v-if="view" class="byok-confirm">
      <n-alert
        v-if="actionError"
        type="error"
        :title="actionError"
        closable
        @close="actionError = ''"
      />

      <div>
        <p class="byok-paths-title">{{ t("目标配置文件") }}</p>
        <code class="byok-selected-path">{{ displayTarget || t("未知") }}</code>
      </div>

      <p class="byok-hint">
        {{ t("将使用 Key {name} 及其全部已发布模型。", { name: keyName }) }}
      </p>
      <p class="byok-hint">
        {{ t("保存会把 Key 明文写入此文件；请确认本机访问权限。首次写入前会创建备份，中断后可在此页恢复。") }}
      </p>
      <p v-if="client === 'minimax'" class="byok-hint">
        {{ t("MiniMax 配置会重新排版；原有注释保留在首次备份中。") }}
      </p>

      <n-checkbox v-if="view.requiresClosedClient" v-model:checked="closedAck" :disabled="busy">
        {{ t("我已完全关闭 {client}（CLI 与桌面应用），确认没有正在运行的实例。", { client: label }) }}
      </n-checkbox>
    </div>
    <template #footer>
      <div class="byok-confirm-footer">
        <n-button quaternary :disabled="busy" @click="setVisible(false)">
          {{ t("取消") }}
        </n-button>
        <n-button
          type="primary"
          :loading="busy"
          :disabled="!confirmable"
          @click="confirm"
        >
          {{ busy ? t("保存中…") : t("确认保存") }}
        </n-button>
      </div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from "vue";
import { NAlert, NButton, NCheckbox, NModal } from "naive-ui";
import { DashboardRequestError, isRevisionConflict } from "../../api/dashboard.ts";
import type { ByokApplicationView } from "../../api/byok-applications.ts";
import { t } from "../../i18n/index.ts";
import { useConnectionStore } from "../../stores/connection.ts";
import { useControlPlaneStore } from "../../stores/controlPlane.ts";
import { useByokApplicationsStore } from "../../stores/byokApplications.ts";
import { dashboardErrorDetail } from "../../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../../utils/modal-close-label.ts";
import {
  BYOK_CLIENT_LABELS,
  HARNESS_DEFAULT_KEY_NAMES,
  byokDisplayTarget,
  byokMutationExpectation,
  refreshConnectionAfterHarnessMutation,
  type ByokClientId,
} from "../../domain/byok-applications.ts";

const props = defineProps<{
  show: boolean;
  client: ByokClientId;
  view: ByokApplicationView | null;
  targetPath?: string;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  saved: [result: ByokApplicationView];
  conflict: [];
}>();

const connectionStore = useConnectionStore();
const controlPlane = useControlPlaneStore();
const store = useByokApplicationsStore();

const actionError = ref("");
const closedAck = ref(false);
const mutating = ref(false);

const label = computed(() => BYOK_CLIENT_LABELS[props.client]);
const keyName = computed(() => HARNESS_DEFAULT_KEY_NAMES[props.client]);
const view = computed(() => props.view);
const busy = computed(() => mutating.value);
const isUpdate = computed(() => (view.value?.configuredModelIds.length ?? 0) > 0);
const displayTarget = computed(() => byokDisplayTarget(view.value));
const confirmable = computed(() =>
  !busy.value
  && Boolean(view.value?.fingerprint)
  && !(view.value?.requiresClosedClient && !closedAck.value),
);

watch(() => props.show, (visible) => {
  if (!visible) return;
  actionError.value = "";
  closedAck.value = false;
  mutating.value = false;
});

function setVisible(show: boolean): void {
  if (busy.value) return;
  emit("update:show", show);
}

async function confirm(): Promise<void> {
  const current = view.value;
  const fingerprint = current?.fingerprint;
  if (!current || !fingerprint || !confirmable.value) return;
  const session = {
    captured: connectionStore.currentSession(),
    current: () => connectionStore.currentSession(),
  };
  actionError.value = "";
  mutating.value = true;
  let failed: unknown;
  let result: ByokApplicationView | undefined;
  try {
    result = await controlPlane.runMutation(
      (expectation) => store.configure(
        props.client,
        {
          targetPath: props.targetPath ?? null,
          expectedFingerprint: fingerprint,
          clientClosed: current.requiresClosedClient ? closedAck.value : false,
        },
        expectation,
      ),
      byokMutationExpectation(current),
    );
  } catch (error) {
    failed = error;
  }
  mutating.value = false;
  void refreshConnectionAfterHarnessMutation(
    () => connectionStore.reloadAfterMutation(session.captured),
    session,
  );
  if (failed) {
    if (isRevisionConflict(failed) || (failed instanceof DashboardRequestError && failed.status === 409)) {
      emit("conflict");
    } else {
      actionError.value = t("保存失败：{error}", { error: dashboardErrorDetail(failed) });
    }
    return;
  }
  if (!result) return;
  emit("saved", result);
}

useLocalizedModalCloseLabel(toRef(props, "show"), "byok-configure-modal");
</script>

<style scoped>
.byok-confirm {
  display: grid;
  gap: var(--ocg-space-md);
}
.byok-paths-title {
  margin: 0 0 var(--ocg-space-xs);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}
.byok-selected-path {
  overflow-wrap: anywhere;
}
.byok-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}
.byok-confirm-footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
}
</style>
