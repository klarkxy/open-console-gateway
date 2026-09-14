<template>
  <n-modal
    v-model:show="visible"
    preset="card"
    :title="mode === 'export' ? t('导出节点迁移包') : t('导入节点迁移包')"
    class="account-transfer-modal"
    style="width: 680px; max-width: calc(100vw - 32px)"
    :mask-closable="!operationLocked"
    :close-on-esc="!operationLocked"
  >
    <template v-if="mode === 'export'">
      <n-alert type="info" :show-icon="false" class="transfer-note">
        {{ t('节点迁移包由服务端加密，浏览器不会读取或显示明文 Key。设置一个独立密码后，即可迁移账号、Access Keys、路由设置、Zen Free 与 Provider 模型配置。') }}
      </n-alert>
      <n-form label-placement="top" @submit.prevent="exportBundle">
        <n-form-item :label="t('迁移包密码')" :feedback="t('至少 12 个字符；此密码只用于加密迁移文件，密码丢失后无法找回。')" required>
          <n-input v-model:value="bundlePassword" :input-props="{ 'aria-label': t('迁移包密码') }" type="password" show-password-on="click" autocomplete="new-password" :disabled="operationLocked" />
        </n-form-item>
        <n-form-item :label="t('确认迁移包密码')" required>
          <n-input v-model:value="bundlePasswordConfirmation" :input-props="{ 'aria-label': t('确认迁移包密码') }" type="password" show-password-on="click" autocomplete="new-password" :disabled="operationLocked" />
        </n-form-item>
      </n-form>
      <p class="transfer-lifecycle">{{ t('同 ID 记录会在目标端原位置归并；目标端现有顺序保持不变，迁移包中新增的账号按包内顺序接在后面。浏览器 Profile/Cookie、登录密码、邀请码、日志、用量、冷却状态及系统专属设置不会迁移；未完成的托管注册草稿会跳过。') }}</p>
      <n-alert v-if="errorText" type="error" :title="errorText" class="transfer-note" />
      <n-alert v-if="resultText" type="success" :title="resultText" class="transfer-note" />
      <div class="transfer-actions">
        <n-button :disabled="operationLocked" @click="visible = false">{{ t('取消') }}</n-button>
        <n-button type="primary" :loading="busy" :disabled="!canExport || operationLocked" @click="exportBundle">
          {{ t('下载加密迁移包') }}
        </n-button>
      </div>
    </template>

    <template v-else>
      <n-alert type="info" :show-icon="false" class="transfer-note">
        {{ t('选择由 Open Console Gateway 导出的加密 .ocgbackup 文件。文件和密码仅在此窗口内存中使用。') }}
      </n-alert>
      <input
        ref="fileInput"
        class="sr-only"
        type="file"
        accept=".ocgbackup,application/json"
        :disabled="operationLocked"
        aria-describedby="account-transfer-file-help"
        :aria-label="t('迁移包文件')"
        @change="readBundleFile"
      />
      <n-form label-placement="top" @submit.prevent="previewBundle">
        <n-form-item :label="t('迁移包文件')" required>
          <n-space align="center" wrap>
            <n-button :disabled="operationLocked" @click="fileInput?.click()">{{ t('选择文件') }}</n-button>
            <span id="account-transfer-file-help" class="transfer-file-name">{{ fileName || t('尚未选择文件') }}</span>
          </n-space>
        </n-form-item>
        <n-form-item :label="t('迁移包密码')" required>
          <n-input v-model:value="bundlePassword" :input-props="{ 'aria-label': t('迁移包密码') }" type="password" show-password-on="click" autocomplete="current-password" :disabled="operationLocked" @update:value="clearPreview" />
        </n-form-item>
      </n-form>
      <p class="transfer-lifecycle">{{ t('同 ID 记录会采用迁移包内容但保留目标端位置；目标端现有顺序不变，新增账号按迁移包顺序接在后面。导入后原有主/子 Key、可用账号、Custom API、Zen Free 与模型路由设置可直接继续使用。') }}</p>

      <n-alert v-if="errorText" type="error" :title="errorText" class="transfer-note" />
      <n-alert v-if="resultText" type="success" :title="resultText" class="transfer-note" />

      <template v-if="preview">
        <n-alert type="info" :show-icon="false" class="transfer-note">
          {{ t('预览：新增 {created} 项，归并 {merged} 项，旧版重复跳过 {duplicate} 项。', { created: previewCreated, merged: previewMerged, duplicate: preview.duplicateAccounts }) }}
        </n-alert>
        <div class="transfer-preview" role="region" :aria-label="t('迁移预览')">
          <div v-for="item in preview.items" :key="item.index" class="transfer-preview-row">
            <span class="transfer-disposition">{{ dispositionLabel(item.disposition) }}</span>
            <span>{{ item.name }}</span>
            <span class="mono">{{ item.providerId }}</span>
            <span v-if="item.reason" class="transfer-reason">{{ dispositionReason(item.disposition) }}</span>
          </div>
        </div>
        <n-checkbox v-model:checked="importConfirmed" :disabled="operationLocked" class="transfer-confirmation">
          {{ t('我确认同 ID 的账号与 Key 将采用迁移包内容；目标端账号顺序保持不变，新增账号按迁移包顺序接在后面。') }}
        </n-checkbox>
      </template>

      <span class="sr-only" aria-live="polite" aria-atomic="true">{{ liveSummary }}</span>
      <div class="transfer-actions">
        <n-button :disabled="operationLocked" @click="visible = false">{{ t('取消') }}</n-button>
        <n-button :loading="previewing" :disabled="!canPreview || operationLocked" @click="previewBundle">
          {{ t('预览导入') }}
        </n-button>
        <n-button type="primary" :loading="busy" :disabled="!canImport" @click="importBundle">
          {{ t('确认导入') }}
        </n-button>
      </div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NButton, NCheckbox, NForm, NFormItem, NInput, NModal, NSpace } from "naive-ui";
import { dashboardApi } from "../api/dashboard.ts";
import type { AccountImportDisposition, AccountImportPreview } from "../api/generated/dashboard-v3.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

const MAX_BUNDLE_BYTES = 4 * 1024 * 1024;

const props = defineProps<{
  show: boolean;
  mode: "import" | "export";
}>();
const emit = defineEmits<{
  "update:show": [value: boolean];
  imported: [count: number];
}>();

const fileInput = ref<HTMLInputElement | null>(null);
const bundlePassword = ref("");
const bundlePasswordConfirmation = ref("");
const bundle = ref("");
const fileName = ref("");
const preview = ref<AccountImportPreview | null>(null);
const previewBundleSnapshot = ref("");
const previewPasswordSnapshot = ref("");
const importConfirmed = ref(false);
const busy = ref(false);
const previewing = ref(false);
let previewEpoch = 0;
const errorText = ref("");
const resultText = ref("");

const visible = computed({
  get: () => props.show,
  set: (value: boolean) => emit("update:show", value),
});
const operationLocked = computed(() => busy.value || previewing.value);
useLocalizedModalCloseLabel(visible, "account-transfer-modal");
const canExport = computed(() => (
  bundlePassword.value.length >= 12
  && bundlePassword.value === bundlePasswordConfirmation.value
));
const canPreview = computed(() => Boolean(bundle.value) && bundlePassword.value.length >= 12);
const canImport = computed(() => (
  Boolean(preview.value)
  && ((preview.value?.importableAccounts ?? 0) > 0 || preview.value?.items.length === 0)
  && Boolean(bundle.value)
  && bundlePassword.value.length >= 12
  && bundle.value === previewBundleSnapshot.value
  && bundlePassword.value === previewPasswordSnapshot.value
  && importConfirmed.value
  && !busy.value
));
const liveSummary = computed(() => resultText.value || errorText.value || (preview.value
  ? t('预览：新增 {created} 项，归并 {merged} 项，旧版重复跳过 {duplicate} 项。', {
    created: previewCreated.value,
    merged: previewMerged.value,
    duplicate: preview.value.duplicateAccounts,
  })
  : ""));
const previewMerged = computed(() => preview.value?.items.filter((item) => item.disposition === "merge").length ?? 0);
const previewCreated = computed(() => Math.max(0, (preview.value?.importableAccounts ?? 0) - previewMerged.value));

watch(() => props.show, (show) => {
  if (!show) clearTransient();
});
watch(() => props.mode, clearTransient);

function clearPreview(): void {
  previewEpoch += 1;
  preview.value = null;
  previewBundleSnapshot.value = "";
  previewPasswordSnapshot.value = "";
  importConfirmed.value = false;
  resultText.value = "";
  errorText.value = "";
}

function clearTransient(): void {
  clearPreview();
  bundlePassword.value = "";
  bundlePasswordConfirmation.value = "";
  bundle.value = "";
  fileName.value = "";
  busy.value = false;
  previewing.value = false;
  errorText.value = "";
  resultText.value = "";
  if (fileInput.value) fileInput.value.value = "";
}

function dispositionLabel(disposition: AccountImportDisposition): string {
  if (disposition === "merge" || disposition === "merged") return t('按 ID 归并');
  return disposition === "import" || disposition === "imported" ? t('将新增') : t('重复，跳过');
}

function dispositionReason(disposition: AccountImportDisposition): string {
  return disposition === "duplicate" ? t('同一 Plan 和名称的账号已存在') : "";
}

async function readBundleFile(event: Event): Promise<void> {
  if (operationLocked.value) return;
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  clearPreview();
  const epoch = previewEpoch;
  bundle.value = "";
  fileName.value = "";
  if (!file) return;
  if (file.size > MAX_BUNDLE_BYTES) {
    errorText.value = t('迁移包文件不能超过 4 MiB');
    input.value = "";
    return;
  }
  if (!/\.(ocgbackup|json)$/iu.test(file.name)) {
    errorText.value = t('请选择 .ocgbackup 或 JSON 迁移包文件');
    input.value = "";
    return;
  }
  try {
    const nextBundle = await file.text();
    if (epoch !== previewEpoch) return;
    bundle.value = nextBundle;
    fileName.value = file.name;
  } catch (error) {
    if (epoch === previewEpoch) errorText.value = dashboardErrorDetail(error);
    input.value = "";
  }
}

function downloadBundle(bundleText: string, filename: string): void {
  const url = URL.createObjectURL(new Blob([bundleText], { type: "application/octet-stream" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}

async function exportBundle(): Promise<void> {
  if (!canExport.value || busy.value) return;
  busy.value = true;
  errorText.value = "";
  try {
    const exported = await dashboardApi.exportAccountTransfer({
      bundlePassword: bundlePassword.value,
    });
    downloadBundle(exported.bundle, exported.filename);
    resultText.value = t('已下载加密迁移包：导出 {exported} 项，跳过 {skipped} 项。', {
      exported: exported.exportedAccounts,
      skipped: exported.skippedAccounts,
    });
    bundlePassword.value = "";
    bundlePasswordConfirmation.value = "";
  } catch (error) {
    errorText.value = dashboardErrorDetail(error);
  } finally {
    busy.value = false;
  }
}

async function previewBundle(): Promise<void> {
  if (!canPreview.value || previewing.value || busy.value) return;
  previewing.value = true;
  clearPreview();
  const epoch = previewEpoch;
  const requestBundle = bundle.value;
  const requestPassword = bundlePassword.value;
  try {
    const nextPreview = await dashboardApi.previewAccountTransfer({
      bundle: requestBundle,
      password: requestPassword,
    });
    if (
      epoch !== previewEpoch
      || bundle.value !== requestBundle
      || bundlePassword.value !== requestPassword
    ) return;
    preview.value = nextPreview;
    previewBundleSnapshot.value = requestBundle;
    previewPasswordSnapshot.value = requestPassword;
  } catch (error) {
    if (epoch === previewEpoch) errorText.value = dashboardErrorDetail(error);
  } finally {
    if (epoch === previewEpoch) previewing.value = false;
  }
}

async function importBundle(): Promise<void> {
  if (!canImport.value || busy.value) return;
  busy.value = true;
  errorText.value = "";
  try {
    const result = await dashboardApi.importAccountTransfer({
      bundle: bundle.value,
      password: bundlePassword.value,
    });
    resultText.value = t('节点配置迁移完成：处理 {count} 项账号。', { count: result.importedAccounts });
    bundlePassword.value = "";
    previewEpoch += 1;
    preview.value = null;
    previewBundleSnapshot.value = "";
    previewPasswordSnapshot.value = "";
    importConfirmed.value = false;
    bundle.value = "";
    fileName.value = "";
    if (fileInput.value) fileInput.value.value = "";
    emit("imported", result.importedAccounts);
  } catch (error) {
    errorText.value = dashboardErrorDetail(error);
  } finally {
    busy.value = false;
  }
}
</script>

<style scoped>
.transfer-note { margin-bottom: 12px; }
.transfer-lifecycle { color: var(--ocg-subtle); font-size: var(--ocg-font-sm); line-height: 1.55; }
.transfer-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; margin-top: 16px; }
.transfer-file-name { min-width: 0; overflow-wrap: anywhere; color: var(--ocg-subtle); }
.transfer-preview { max-height: 260px; overflow: auto; border: 1px solid var(--ocg-border); border-radius: 6px; margin: 12px 0; }
.transfer-preview-row { display: grid; grid-template-columns: minmax(84px, auto) minmax(0, 1fr) minmax(0, 1.2fr); gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--ocg-border); font-size: var(--ocg-font-sm); }
.transfer-preview-row:last-child { border-bottom: 0; }
.transfer-disposition { font-weight: 600; }
.transfer-reason { grid-column: 2 / -1; color: var(--ocg-subtle); }
.transfer-confirmation { display: flex; align-items: flex-start; margin-top: 12px; }
@media (max-width: 560px) { .transfer-preview-row { grid-template-columns: minmax(0, 1fr); gap: 4px; } .transfer-reason { grid-column: auto; } .transfer-actions > * { flex: 1 1 auto; } }
</style>
