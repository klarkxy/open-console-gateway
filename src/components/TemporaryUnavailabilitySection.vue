<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import { t } from "../i18n/index.ts";
import { createRevalidateGate } from "../domain/revalidate.ts";
import {
  BUILTIN_GOAT_ID,
  TEMPORARY_POLICY_CLIENT_ERROR_KEYS,
  TEMPORARY_POLICY_EMPTY_KEYS,
  TEMPORARY_POLICY_HINT_KEYS,
  TEMPORARY_POLICY_ISSUE_KEYS,
  TEMPORARY_POLICY_RESTRICTION_SOURCE_KEYS,
  TEMPORARY_POLICY_RESTRICTION_STATE_KEYS,
  TEMPORARY_POLICY_SCOPE_KEYS,
  allocateCustomRuleId,
  customRuleDraftFrom,
  effectiveBuiltinEnabled,
  emptyCustomRuleDraft,
  localOverride,
  maskInheritedCustomRule,
  parseCustomRuleDraft,
  removeRule,
  restrictionSnapshotEmptyCode,
  upsertBuiltinOverride,
  upsertRule,
  validateConfiguredRules,
  visibleCustomRules,
  type PolicyCustomRule,
  type PolicyDraftIssue,
  type PolicyRule,
} from "../domain/temporary-policy.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useSessionStore } from "../stores/session.ts";
import { useTemporaryPolicyStore } from "../stores/temporaryPolicy.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import {
  CLEAR_LOCAL_WAIT_KEY,
  builtinLabel,
  createLocalWaitTicker,
  restrictionConnectionLabel,
  restrictionCredentialLabel,
  restrictionModelLabel,
  restrictionWaitLabel,
  RESTRICTION_MODEL_KEYS,
  RESTRICTION_NAME_KEYS,
  type RestrictionRow,
} from "../views/temporary-policy.ts";

const policyStore = useTemporaryPolicyStore();
const destinationsStore = useDestinationsStore();
const sessionStore = useSessionStore();
const revalidateGate = createRevalidateGate(60_000);

const selectedDestinationId = ref("");
const draft = ref(emptyCustomRuleDraft());
const draftIssues = ref<PolicyDraftIssue[]>([]);
const editingId = ref<string | null>(null);
const draftExpectation = ref<MutationExpectation>();
const nowMs = ref(Date.now());
const ticker = createLocalWaitTicker((now) => {
  nowMs.value = now;
});

const destinationId = computed(() => selectedDestinationId.value === "" ? null : selectedDestinationId.value);
const rules = computed(() => policyStore.configuration?.rules ?? []);
const builtins = computed(() => policyStore.configuration?.builtins ?? []);
const customRows = computed(() => visibleCustomRules(rules.value, destinationId.value));
const builtinEnabled = computed(() =>
  effectiveBuiltinEnabled(rules.value, builtins.value, destinationId.value, BUILTIN_GOAT_ID),
);
const builtinOverridden = computed(() =>
  localOverride(rules.value, destinationId.value, BUILTIN_GOAT_ID) !== null,
);
const restrictionRows = computed(() => policyStore.restrictions?.restrictions ?? []);
const emptyRestrictions = computed(() =>
  restrictionSnapshotEmptyCode(policyStore.restrictionsLoaded, restrictionRows.value.length),
);
const destinations = computed(() => destinationsStore.destinations.map((row) => ({ id: row.id, name: row.name })));
const credentials = computed(() => destinationsStore.credentials.map((row) => ({ id: row.id, name: row.name })));
const goatLabel = computed(() => builtinLabel(BUILTIN_GOAT_ID));
const initialLoading = computed(() => !policyStore.loaded && policyStore.loading);
const restrictionInitialLoading = computed(() => !policyStore.restrictionsLoaded && policyStore.restrictionsLoading);

watch(() => sessionStore.authenticated, (ok) => {
  if (ok) return;
  revalidateGate.reset();
  resetEditor();
  ticker.stop();
});

watch(selectedDestinationId, () => {
  resetEditor();
});

function resetEditor(): void {
  draft.value = emptyCustomRuleDraft();
  draftIssues.value = [];
  editingId.value = null;
  draftExpectation.value = undefined;
}

async function ensureDestinationNames(): Promise<void> {
  if (destinationsStore.loaded || destinationsStore.loading) return;
  try {
    await destinationsStore.load();
  } catch {
    // Friendly names fall back to unknown codes; policy reads do not need this.
  }
}

async function refresh(retain = false): Promise<void> {
  await Promise.all([
    policyStore.load(retain),
    ensureDestinationNames(),
  ]);
}

function issueText(issues: readonly PolicyDraftIssue[]): string {
  return issues.map((issue) => t(TEMPORARY_POLICY_ISSUE_KEYS[issue])).join(" ");
}

async function persist(next: PolicyRule[], captured?: MutationExpectation): Promise<boolean> {
  const listIssues = validateConfiguredRules(next, builtins.value);
  if (listIssues.length > 0) {
    draftIssues.value = listIssues;
    return false;
  }
  try {
    await policyStore.saveRules(next, captured);
    draftIssues.value = [];
    return true;
  } catch {
    if (policyStore.error === "conflict") resetEditor();
    return false;
  }
}

async function toggleBuiltin(enabled: boolean): Promise<void> {
  await persist(upsertBuiltinOverride(rules.value, destinationId.value, BUILTIN_GOAT_ID, { enabled }));
}

async function restoreBuiltinInheritance(): Promise<void> {
  await persist(removeRule(rules.value, destinationId.value, BUILTIN_GOAT_ID));
}

function beginCreate(): void {
  captureDraftExpectation();
  editingId.value = null;
  draft.value = {
    ...emptyCustomRuleDraft(),
    id: allocateCustomRuleId(globalThis.crypto?.randomUUID?.() ?? String(Date.now())),
  };
  draftIssues.value = [];
}

function beginEdit(rule: PolicyCustomRule): void {
  captureDraftExpectation();
  editingId.value = rule.id;
  draft.value = customRuleDraftFrom(rule);
  draftIssues.value = [];
}

async function submitCustomRule(): Promise<void> {
  const parsed = parseCustomRuleDraft(draft.value, destinationId.value, builtins.value);
  if (!parsed.ok) {
    draftIssues.value = parsed.issues;
    return;
  }
  const next = upsertRule(rules.value, parsed.rule);
  if (await persist(next, draftExpectation.value)) resetEditor();
}

function captureDraftExpectation(): void {
  const snapshot = policyStore.configuration?.revision;
  draftExpectation.value = snapshot
    ? { expectedRevision: snapshot.revision, processGeneration: snapshot.processGeneration }
    : undefined;
}

async function deleteCustomRule(id: string): Promise<void> {
  if (await persist(removeRule(rules.value, destinationId.value, id)) && editingId.value === id) {
    resetEditor();
  }
}

async function disableInherited(rule: PolicyCustomRule): Promise<void> {
  if (destinationId.value === null) return;
  await persist(upsertRule(rules.value, maskInheritedCustomRule(rule, destinationId.value)));
}

function connectionLabel(destinationId: string): string {
  const label = restrictionConnectionLabel(destinationId, destinations.value);
  return label.kind === "named" ? label.name : t(RESTRICTION_NAME_KEYS[label.kind]);
}

function credentialLabel(credentialId: string): string {
  const label = restrictionCredentialLabel(credentialId, credentials.value);
  return label.kind === "named" ? label.name : t(RESTRICTION_NAME_KEYS[label.kind]);
}

function modelLabel(row: RestrictionRow): string {
  const label = restrictionModelLabel(row);
  return label.kind === "model" ? label.model : t(RESTRICTION_MODEL_KEYS.credential_scope);
}

function waitLabel(row: RestrictionRow): string {
  const seconds = restrictionWaitLabel(row, policyStore.restrictionsObservedAt ?? Date.now(), nowMs.value);
  if (row.state !== "waiting") return "";
  return t("{seconds} 秒后", { seconds });
}

async function clearWait(id: string): Promise<void> {
  try {
    await policyStore.clearRestriction(id);
  } catch {
    // Error codes stay on the store so the last snapshot remains visible.
  }
}

function activatePage(): void {
  ticker.start();
  if (policyStore.loaded && !revalidateGate.shouldRun()) return;
  revalidateGate.record();
  void refresh(policyStore.loaded);
}

onMounted(() => {
  activatePage();
});
onActivated(() => {
  activatePage();
});
onDeactivated(() => {
  ticker.stop();
});
onUnmounted(() => {
  ticker.stop();
});
</script>

<template>
  <section
    class="rounded-lg border border-border bg-surface p-[22px] text-ink shadow-[var(--ocg-shadow-sm)]"
    data-section="temporary-unavailability"
    :data-loaded="policyStore.loaded ? 'true' : 'false'"
    :data-loading="policyStore.loading ? 'true' : 'false'"
    aria-labelledby="temporary-policy-title"
  >
    <div class="mb-[18px]">
      <h2 id="temporary-policy-title" class="m-0 text-[length:var(--ocg-font-lg)] font-semibold leading-[1.3]">
        {{ t("临时停调") }}
      </h2>
      <p class="mt-1 mb-0 text-[length:var(--ocg-font-xs)] leading-[1.5] text-subtle">
        {{ t("按连接或全局匹配上游错误并在本地等待后重试。读诊断不会发探测。") }}
      </p>
    </div>

    <div v-if="initialLoading" class="text-[length:var(--ocg-font-sm)] text-muted" role="status">
      {{ t("加载中…") }}
    </div>

    <div v-else class="grid gap-5" :data-loaded="policyStore.loaded ? 'true' : 'false'">
      <div
        v-if="policyStore.error"
        class="rounded-md border border-error px-3 py-2 text-[length:var(--ocg-font-sm)] text-error"
        role="alert"
        :data-error-code="policyStore.error"
      >
        <p class="m-0">{{ t(TEMPORARY_POLICY_CLIENT_ERROR_KEYS[policyStore.error]) }}</p>
        <p v-if="policyStore.errorDetail" class="mt-1 mb-0">{{ policyStore.errorDetail }}</p>
        <button
          type="button"
          class="mt-2 rounded-sm border border-border bg-canvas px-2 py-1 text-ink"
          data-action="retry-load"
          :disabled="policyStore.loading"
          @click="refresh(true)"
        >
          {{ t("重试") }}
        </button>
      </div>

      <template v-if="policyStore.loaded || policyStore.configuration">
        <div class="grid gap-2">
          <label class="text-[length:var(--ocg-font-sm)]" for="temporary-policy-scope">{{ t("作用范围") }}</label>
          <select
            id="temporary-policy-scope"
            :value="selectedDestinationId"
            class="max-w-[28rem] rounded-sm border border-border bg-canvas px-2 py-1"
            :disabled="policyStore.mutating"
            @change="selectedDestinationId = ($event.target as HTMLSelectElement).value"
          >
            <option value="">{{ t("全局") }}</option>
            <option v-for="destination in destinations" :key="destination.id" :value="destination.id">
              {{ destination.name }}
            </option>
          </select>
        </div>

        <fieldset class="m-0 grid gap-2 border-0 p-0">
          <legend class="px-0 text-[length:var(--ocg-font-sm)] font-medium">{{ t("内置 GOAT 额度拒绝") }}</legend>
          <p class="m-0 text-[length:var(--ocg-font-xs)] text-subtle">{{ t(TEMPORARY_POLICY_HINT_KEYS.builtin_sealed) }}</p>
          <label class="flex items-center gap-2 text-[length:var(--ocg-font-sm)]">
            <input
              type="checkbox"
              data-action="toggle-builtin"
              :checked="builtinEnabled"
              :disabled="policyStore.mutating"
              @change="toggleBuiltin(($event.target as HTMLInputElement).checked)"
            >
            {{ goatLabel.kind === "key" ? t(goatLabel.key) : goatLabel.id }}
            <span class="text-muted">{{ builtinEnabled ? t("已启用") : t("已禁用") }}</span>
          </label>
          <button
            v-if="builtinOverridden"
            type="button"
            class="w-fit rounded-sm border border-border bg-canvas px-2 py-1 text-[length:var(--ocg-font-sm)]"
            data-action="restore-inheritance"
            :disabled="policyStore.mutating"
            @click="restoreBuiltinInheritance"
          >
            {{ t("恢复继承") }}
          </button>
        </fieldset>

        <section class="grid gap-3" aria-labelledby="temporary-policy-custom-title">
          <div>
            <h3 id="temporary-policy-custom-title" class="m-0 text-[length:var(--ocg-font-md)] font-medium">{{ t("自定义规则") }}</h3>
            <p class="mt-1 mb-0 text-[length:var(--ocg-font-xs)] text-subtle">{{ t(TEMPORARY_POLICY_HINT_KEYS.matcher) }}</p>
          </div>

          <ul v-if="customRows.length > 0" class="m-0 grid list-none gap-2 p-0" :data-custom-count="customRows.length">
            <li
              v-for="row in customRows"
              :key="`${row.origin}-${row.rule.id}`"
              class="grid gap-1 rounded-md border border-border bg-canvas px-3 py-2"
            >
              <div class="flex flex-wrap items-center gap-2 text-[length:var(--ocg-font-sm)]">
                <code>{{ row.rule.id }}</code>
                <span>{{ t(TEMPORARY_POLICY_SCOPE_KEYS[row.rule.scope]) }}</span>
                <span>{{ row.rule.enabled ? t("已启用") : t("已禁用") }}</span>
                <span v-if="row.origin === 'inherited'" class="text-muted">{{ t("继承自全局") }}</span>
              </div>
              <div class="flex flex-wrap gap-2">
                <button
                  v-if="row.origin === 'local'"
                  type="button"
                  class="rounded-sm border border-border bg-surface px-2 py-1 text-[length:var(--ocg-font-xs)]"
                  data-action="edit-custom-rule"
                  :disabled="policyStore.mutating"
                  @click="beginEdit(row.rule)"
                >
                  {{ t("编辑") }}
                </button>
                <button
                  v-if="row.origin === 'local'"
                  type="button"
                  class="rounded-sm border border-border bg-surface px-2 py-1 text-[length:var(--ocg-font-xs)]"
                  data-action="delete-custom-rule"
                  :disabled="policyStore.mutating"
                  @click="deleteCustomRule(row.rule.id)"
                >
                  {{ t("删除此规则") }}
                </button>
                <button
                  v-if="row.origin === 'inherited' && destinationId"
                  type="button"
                  class="rounded-sm border border-border bg-surface px-2 py-1 text-[length:var(--ocg-font-xs)]"
                  data-action="disable-inheritance"
                  :disabled="policyStore.mutating"
                  @click="disableInherited(row.rule)"
                >
                  {{ t("禁用继承") }}
                </button>
              </div>
            </li>
          </ul>

          <form class="grid max-w-[40rem] gap-3" data-action="custom-rule-form" @submit.prevent="submitCustomRule">
            <div class="flex flex-wrap gap-2">
              <button
                type="button"
                class="rounded-sm border border-border bg-canvas px-2 py-1 text-[length:var(--ocg-font-sm)]"
                data-action="add-custom-rule"
                :disabled="policyStore.mutating"
                @click="beginCreate"
              >
                {{ t("添加自定义规则") }}
              </button>
            </div>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("规则标识") }}
              <input
                :value="draft.id"
                class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                :disabled="policyStore.mutating || editingId !== null"
                @input="draft.id = ($event.target as HTMLInputElement).value"
              >
            </label>
            <label class="flex items-center gap-2 text-[length:var(--ocg-font-sm)]">
              <input
                type="checkbox"
                :checked="draft.enabled"
                :disabled="policyStore.mutating"
                @change="draft.enabled = ($event.target as HTMLInputElement).checked"
              >
              {{ draft.enabled ? t("已启用") : t("已禁用") }}
            </label>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("范围") }}
              <select
                :value="draft.scope"
                class="rounded-sm border border-border bg-canvas px-2 py-1"
                :disabled="policyStore.mutating"
                @change="draft.scope = ($event.target as HTMLSelectElement).value as typeof draft.scope"
              >
                <option value="credential">{{ t(TEMPORARY_POLICY_SCOPE_KEYS.credential) }}</option>
                <option value="credential_model">{{ t(TEMPORARY_POLICY_SCOPE_KEYS.credential_model) }}</option>
              </select>
            </label>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("状态码（400–599）") }}
              <input
                :value="draft.statusCodes"
                class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                :disabled="policyStore.mutating"
                @input="draft.statusCodes = ($event.target as HTMLInputElement).value"
              >
            </label>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("error.code") }}
              <textarea
                :value="draft.errorCodes"
                rows="2"
                class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                :disabled="policyStore.mutating"
                @input="draft.errorCodes = ($event.target as HTMLTextAreaElement).value"
              />
            </label>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("error.type") }}
              <textarea
                :value="draft.errorTypes"
                rows="2"
                class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                :disabled="policyStore.mutating"
                @input="draft.errorTypes = ($event.target as HTMLTextAreaElement).value"
              />
            </label>
            <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
              {{ t("消息包含") }}
              <textarea
                :value="draft.messageContains"
                rows="2"
                class="rounded-sm border border-border bg-canvas px-2 py-1"
                :disabled="policyStore.mutating"
                @input="draft.messageContains = ($event.target as HTMLTextAreaElement).value"
              />
            </label>
            <div class="grid grid-cols-2 gap-3">
              <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
                {{ t("初始退避（秒）") }}
                <input
                  :value="draft.initialSeconds"
                  inputmode="numeric"
                  class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                  :disabled="policyStore.mutating"
                  @input="draft.initialSeconds = ($event.target as HTMLInputElement).value"
                >
              </label>
              <label class="grid gap-1 text-[length:var(--ocg-font-sm)]">
                {{ t("最大退避（秒）") }}
                <input
                  :value="draft.maxSeconds"
                  inputmode="numeric"
                  class="rounded-sm border border-border bg-canvas px-2 py-1 font-mono"
                  :disabled="policyStore.mutating"
                  @input="draft.maxSeconds = ($event.target as HTMLInputElement).value"
                >
              </label>
            </div>
            <p v-if="draftIssues.length > 0" class="m-0 text-[length:var(--ocg-font-sm)] text-error" role="alert" :data-issue-count="draftIssues.length">
              {{ issueText(draftIssues) }}
            </p>
            <button
              type="submit"
              class="w-fit rounded-sm border border-primary bg-primary px-3 py-1 text-[length:var(--ocg-font-sm)] text-on-primary disabled:opacity-60"
              data-action="save-custom-rule"
              :disabled="policyStore.mutating"
            >
              {{ t("保存规则") }}
            </button>
          </form>
        </section>
      </template>

      <section
        class="grid gap-3"
        aria-labelledby="temporary-policy-restrictions-title"
        :data-restrictions-loaded="policyStore.restrictionsLoaded ? 'true' : 'false'"
      >
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div>
            <h3 id="temporary-policy-restrictions-title" class="m-0 text-[length:var(--ocg-font-md)] font-medium">{{ t("活动限制") }}</h3>
            <p class="mt-1 mb-0 text-[length:var(--ocg-font-xs)] text-subtle">{{ t(TEMPORARY_POLICY_HINT_KEYS.local_wait) }}</p>
            <p class="mt-1 mb-0 text-[length:var(--ocg-font-xs)] text-subtle">{{ t(TEMPORARY_POLICY_HINT_KEYS.clear_local) }}</p>
          </div>
          <button
            type="button"
            class="rounded-sm border border-border bg-canvas px-2 py-1 text-[length:var(--ocg-font-sm)]"
            data-action="refresh-restrictions"
            :disabled="policyStore.restrictionsLoading"
            @click="policyStore.loadRestrictions(true)"
          >
            {{ t("刷新状态") }}
          </button>
        </div>

        <div
          v-if="policyStore.restrictionsError"
          class="rounded-md border border-error px-3 py-2 text-[length:var(--ocg-font-sm)] text-error"
          role="alert"
          :data-restrictions-error-code="policyStore.restrictionsError"
        >
          <p class="m-0">{{ t(TEMPORARY_POLICY_CLIENT_ERROR_KEYS[policyStore.restrictionsError]) }}</p>
          <p v-if="policyStore.restrictionsErrorDetail" class="mt-1 mb-0">{{ policyStore.restrictionsErrorDetail }}</p>
        </div>

        <div v-if="restrictionInitialLoading" class="text-[length:var(--ocg-font-sm)] text-muted" role="status">
          {{ t("加载中…") }}
        </div>

        <p
          v-else-if="emptyRestrictions"
          class="m-0 text-[length:var(--ocg-font-sm)] text-muted"
          :data-empty-code="emptyRestrictions"
        >
          {{ t(TEMPORARY_POLICY_EMPTY_KEYS[emptyRestrictions]) }}
        </p>

        <div v-else-if="policyStore.restrictionsLoaded" class="overflow-x-auto">
          <table class="w-full border-collapse text-left text-[length:var(--ocg-font-sm)]" :data-restriction-count="restrictionRows.length">
            <caption class="sr-only">{{ t("活动限制") }}</caption>
            <thead>
              <tr class="border-b border-border text-muted">
                <th class="px-2 py-1 font-medium">{{ t("连接") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("凭证") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("上游模型") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("规则来源") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("状态") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("下次本地探测") }}</th>
                <th class="px-2 py-1 font-medium">{{ t("规则标识") }}</th>
                <th class="px-2 py-1 font-medium" />
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in restrictionRows" :key="row.id" class="border-b border-divider">
                <td class="px-2 py-2">{{ connectionLabel(row.destinationId) }}</td>
                <td class="px-2 py-2">{{ credentialLabel(row.credentialId) }}</td>
                <td class="px-2 py-2">{{ modelLabel(row) }}</td>
                <td class="px-2 py-2">{{ t(TEMPORARY_POLICY_RESTRICTION_SOURCE_KEYS[row.source]) }}</td>
                <td class="px-2 py-2">
                  {{ t(TEMPORARY_POLICY_RESTRICTION_STATE_KEYS[row.state]) }}
                  <span v-if="row.probeInFlight" class="text-muted"> · {{ t("探测中") }}</span>
                </td>
                <td class="px-2 py-2 font-mono">{{ waitLabel(row) }}</td>
                <td class="px-2 py-2"><code>{{ row.ruleId }}</code></td>
                <td class="px-2 py-2">
                  <button
                    type="button"
                    class="rounded-sm border border-border bg-canvas px-2 py-1"
                    data-action="clear-local-wait"
                    :disabled="policyStore.clearing"
                    :aria-label="t(CLEAR_LOCAL_WAIT_KEY)"
                    @click="clearWait(row.id)"
                  >
                    {{ t(CLEAR_LOCAL_WAIT_KEY) }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </div>
  </section>
</template>
