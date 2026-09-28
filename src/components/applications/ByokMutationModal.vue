<template>
  <n-modal
    :show="show"
    preset="card"
    :title="title"
    :class="`byok-${mode}-modal`"
    style="width: 520px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!mutating"
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
      <template v-if="mode === 'remove'">
        <p class="byok-hint">
          {{ t("将从 {client} 配置中移除 OCG 写入的供应商与模型条目；OCG Key 与账号数据不受影响，也不需要当前 Key 或模型仍然存在。", { client: label }) }}
        </p>
        <p class="byok-hint">{{ t("客户端中其他未由 OCG 管理的配置保持不变。") }}</p>
      </template>
      <template v-else>
        <p class="byok-hint">
          {{ t("回滚上一次未完成的写入；仅当文件仍保持中断时的状态才会执行，若已被手动修改则保留当前内容并报告冲突。") }}
        </p>
      </template>
      <p v-if="view.backupPath" class="byok-hint">
        {{ t("首次写入前的备份：{path}", { path: view.backupPath }) }}
      </p>
      <p class="byok-hint">
        {{ t("操作只修改本地配置文件，不会发送模型请求。") }}
      </p>
      <n-checkbox v-if="view.requiresClosedClient" v-model:checked="closedAck" :disabled="mutating">
        {{ t("我已完全关闭 {client}（CLI 与桌面应用），确认没有正在运行的实例。", { client: label }) }}
      </n-checkbox>
    </div>
    <template #footer>
      <div class="byok-confirm-footer">
        <n-button quaternary :disabled="mutating" @click="setVisible(false)">
          {{ t("取消") }}
        </n-button>
        <n-button
          type="primary"
          :loading="mutating"
          :disabled="!confirmable"
          @click="confirm"
        >
          {{ confirmLabel }}
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
import { useControlPlaneStore } from "../../stores/controlPlane.ts";
import { useByokApplicationsStore } from "../../stores/byokApplications.ts";
import { dashboardErrorDetail } from "../../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../../utils/modal-close-label.ts";
import {
  BYOK_CLIENT_LABELS,
  byokDisplayTarget,
  byokMutationExpectation,
  type ByokClientId,
} from "../../domain/byok-applications.ts";

const props = defineProps<{
  show: boolean;
  mode: "remove" | "recover";
  client: ByokClientId;
  view: ByokApplicationView | null;
  targetPath?: string;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  saved: [result: ByokApplicationView];
  conflict: [];
}>();

const controlPlane = useControlPlaneStore();
const store = useByokApplicationsStore();

const actionError = ref("");
const closedAck = ref(false);
const mutating = ref(false);

const label = computed(() => BYOK_CLIENT_LABELS[props.client]);
const view = computed(() => props.view);
const displayTarget = computed(() => byokDisplayTarget(view.value));
const title = computed(() => props.mode === "remove"
  ? t("移除 {client} 的 OCG 配置", { client: label.value })
  : t("恢复 {client} 中断的写入", { client: label.value }));
const confirmLabel = computed(() => {
  if (mutating.value) return props.mode === "remove" ? t("移除中…") : t("恢复中…");
  return props.mode === "remove" ? t("确认移除") : t("确认恢复");
});
const confirmable = computed(() =>
  !mutating.value
  && Boolean(view.value?.fingerprint)
  && !(view.value?.requiresClosedClient && !closedAck.value),
);

watch(() => props.show, (visible) => {
  if (visible) {
    actionError.value = "";
    closedAck.value = false;
    mutating.value = false;
  }
});

function setVisible(show: boolean): void {
  if (mutating.value) return;
  emit("update:show", show);
}

async function confirm(): Promise<void> {
  const current = view.value;
  const fingerprint = current?.fingerprint;
  if (!current || !fingerprint || !confirmable.value) return;
  actionError.value = "";
  mutating.value = true;
  const input = {
    targetPath: props.targetPath ?? null,
    expectedFingerprint: fingerprint,
    clientClosed: current.requiresClosedClient ? closedAck.value : false,
  };
  try {
    const result = await controlPlane.runMutation(
      (expectation) => props.mode === "remove"
        ? store.remove(props.client, input, expectation)
        : store.recover(props.client, input, expectation),
      byokMutationExpectation(current),
    );
    mutating.value = false;
    emit("saved", result);
  } catch (error) {
    mutating.value = false;
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      emit("conflict");
    } else {
      actionError.value = t(props.mode === "remove" ? "移除失败：{error}" : "恢复失败：{error}", {
        error: dashboardErrorDetail(error),
      });
    }
  }
}

useLocalizedModalCloseLabel(toRef(props, "show"), `byok-${props.mode}-modal`);
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
