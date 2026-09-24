<template>
  <tr class="alias-explain-row">
    <td :colspan="3">
      <section class="alias-explain" :aria-label="t('路由解释')">
        <header class="alias-explain-head">
          <span class="alias-explain-title">{{ t("当前路由资格") }}</span>
          <n-tag v-if="state" size="small" :type="stateTagType" :bordered="false">
            {{ t(ROUTING_EXPLANATION_STATE_KEYS[state]) }}
          </n-tag>
          <div class="alias-explain-protocol">
            <span class="alias-explain-label">{{ t("客户端协议") }}</span>
            <n-select
              size="small"
              class="alias-explain-select"
              :value="protocol"
              :options="protocolOptions"
              :consistent-menu-width="false"
              :aria-label="t('客户端协议')"
              @update:value="onProtocolUpdate"
            />
          </div>
        </header>

        <div v-if="loading" class="alias-explain-status" role="status" :aria-label="t('加载中…')">
          <n-spin size="small" />
          <span>{{ t("加载中…") }}</span>
        </div>

        <n-alert
          v-else-if="error"
          type="error"
          :title="t('加载路由解释失败：{error}', { error })"
        >
          <n-button size="small" secondary :loading="loading" @click="request()">
            {{ t("重试") }}
          </n-button>
        </n-alert>

        <template v-else-if="explanation">
          <div class="alias-explain-block">
            <span class="alias-explain-label">{{ t("解析结果") }}</span>
            <div class="alias-explain-resolved">
              <n-tag size="small" :bordered="false">
                {{ t(ROUTING_RESOLVED_KIND_KEYS[explanation.resolved.kind]) }}
              </n-tag>
              <code v-if="explanation.resolved.alias">{{ explanation.resolved.alias }}</code>
              <span
                v-for="mapping in explanation.resolved.mappings"
                :key="`${mapping.provider_id}:${mapping.upstream_model}`"
                class="alias-explain-mapping"
              >
                <span class="alias-explain-provider">{{ mapping.provider_id }}</span>
                <span aria-hidden="true">→</span>
                <code>{{ mapping.upstream_model }}</code>
              </span>
              <span v-if="explanation.resolved.mappings.length === 0" class="alias-explain-empty">
                {{ t(ROUTING_EXPLANATION_STATE_KEYS.unresolved) }}
              </span>
            </div>
          </div>

          <div class="alias-explain-block">
            <h3 class="alias-explain-subtitle">{{ t("合格候选") }}</h3>
            <p v-if="orderedEligible.length === 0" class="alias-explain-empty">{{ t("暂无候选") }}</p>
            <table v-else class="alias-explain-table">
              <thead>
                <tr>
                  <th>{{ t("名称") }}</th>
                  <th>{{ t("连接") }}</th>
                  <th>{{ t("上游模型 ID") }}</th>
                  <th>{{ t("上游协议") }}</th>
                  <th>{{ t("全局路由顺位") }}</th>
                  <th>{{ t("通道") }}</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="candidate in orderedEligible"
                  :key="candidateKey(candidate)"
                  :class="{ 'alias-explain-first-pick': isFirstPick(candidate) }"
                >
                  <td>
                    <span class="alias-explain-candidate-name">{{ candidate.account_name }}</span>
                    <n-tag v-if="isFirstPick(candidate)" size="tiny" type="success" :bordered="false">
                      {{ t("预期首选") }}
                    </n-tag>
                  </td>
                  <td>{{ candidate.destination_name ?? "—" }}</td>
                  <td><code>{{ candidate.resolved_model }}</code></td>
                  <td>{{ t(ROUTING_CLIENT_PROTOCOL_KEYS[candidate.upstream_protocol]) }}</td>
                  <td>{{ candidate.routing_rank }}</td>
                  <td>{{ t(ROUTING_CHANNEL_KEYS[candidate.channel]) }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div class="alias-explain-block">
            <h3 class="alias-explain-subtitle">{{ t("被排除的候选") }}</h3>
            <p v-if="explanation.exclusions.length === 0" class="alias-explain-empty">{{ t("无排除项") }}</p>
            <ul v-else class="alias-explain-exclusions">
              <li
                v-for="(exclusion, index) in explanation.exclusions"
                :key="`${exclusion.account_id ?? exclusion.provider_id ?? 'none'}:${exclusion.upstream_model ?? ''}:${index}`"
              >
                <span class="alias-explain-exclusion-head">
                  <code v-if="exclusion.upstream_model">{{ exclusion.upstream_model }}</code>
                  <span v-if="exclusion.provider_id" class="alias-explain-provider">{{ exclusion.provider_id }}</span>
                </span>
                <span>{{ t(ROUTING_EXCLUSION_KEYS[exclusion.code]) }}</span>
                <span v-if="exclusion.detail" class="alias-explain-detail">{{ exclusion.detail }}</span>
              </li>
            </ul>
          </div>

          <p class="alias-explain-note">
            {{ t("基于当前配置快照的预测；对话绑定与后续运行时状态（冷却、重试、粘性推进）未模拟。") }}
          </p>
          <p class="alias-explain-meta">{{ t("快照时间") }} · {{ formatDateTime(explanation.observed_at) }}</p>
        </template>
      </section>
    </td>
  </tr>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NAlert, NButton, NSelect, NSpin, NTag } from "naive-ui";
import type { RoutingClientProtocol, RoutingEligibleCandidateView } from "../api/destinations.ts";
import {
  ROUTING_CHANNEL_KEYS,
  ROUTING_CLIENT_PROTOCOL_KEYS,
  ROUTING_EXCLUSION_KEYS,
  ROUTING_EXPLANATION_STATE_KEYS,
  ROUTING_RESOLVED_KIND_KEYS,
  routingEligibleOrdered,
  routingExplanationState,
  type RoutingExplanationState,
} from "../domain/routing-explain.ts";
import { t } from "../i18n/index.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { formatDateTime } from "../utils/format.ts";

const props = defineProps<{ model: string; protocol: RoutingClientProtocol }>();
const emit = defineEmits<{ (event: "update:protocol", value: RoutingClientProtocol): void }>();

const destinationsStore = useDestinationsStore();

const protocolOptions = computed(() => (
  (Object.keys(ROUTING_CLIENT_PROTOCOL_KEYS) as RoutingClientProtocol[]).map((value) => ({
    value,
    label: t(ROUTING_CLIENT_PROTOCOL_KEYS[value]),
  }))
));

const explainKey = computed(() => destinationsStore.explainKey(props.model, props.protocol));
const explanation = computed(() => destinationsStore.explanations[explainKey.value]);
const loading = computed(() => Boolean(destinationsStore.explainLoading[explainKey.value]));
const error = computed(() => destinationsStore.explainErrors[explainKey.value] ?? "");
const state = computed<RoutingExplanationState | null>(() => (
  explanation.value ? routingExplanationState(explanation.value) : null
));
const stateTagType = computed(() => {
  if (state.value === "routeable") return "success";
  if (state.value === "excluded") return "warning";
  return "default";
});
const orderedEligible = computed(() => (explanation.value ? routingEligibleOrdered(explanation.value) : []));

function candidateKey(candidate: RoutingEligibleCandidateView): string {
  return `${candidate.account_id}:${candidate.destination_id ?? ""}:${candidate.resolved_model}`;
}

function isFirstPick(candidate: RoutingEligibleCandidateView): boolean {
  const pick = explanation.value?.expected_base_policy_first_pick;
  return Boolean(pick
    && pick.account_id === candidate.account_id
    && pick.destination_id === candidate.destination_id
    && pick.resolved_model === candidate.resolved_model);
}

function request(): void {
  void destinationsStore.explainRouting(props.model, props.protocol).catch(() => {});
}

function onProtocolUpdate(value: string): void {
  if (Object.prototype.hasOwnProperty.call(ROUTING_CLIENT_PROTOCOL_KEYS, value)) {
    emit("update:protocol", value as RoutingClientProtocol);
  }
}
</script>

<style scoped>
.alias-explain-row > td {
  padding: 0 var(--ocg-space-md) var(--ocg-space-md);
  border-bottom: 1px solid var(--ocg-border);
  background: var(--ocg-primary-soft);
}
.alias-explain {
  padding: var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: var(--ocg-surface);
  font-size: var(--ocg-font-sm);
}
.alias-explain-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
  margin-bottom: var(--ocg-space-md);
}
.alias-explain-title {
  font-weight: 600;
}
.alias-explain-protocol {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  margin-left: auto;
}
.alias-explain-select {
  min-width: 150px;
}
.alias-explain-label {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.alias-explain-status {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  color: var(--ocg-muted);
}
.alias-explain-block {
  margin-top: var(--ocg-space-md);
}
.alias-explain-subtitle {
  margin: 0 0 var(--ocg-space-xs);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}
.alias-explain-resolved {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
}
.alias-explain-mapping {
  display: inline-flex;
  align-items: center;
  gap: var(--ocg-space-xs);
}
.alias-explain-provider {
  color: var(--ocg-muted);
}
.alias-explain-table {
  width: 100%;
  border-collapse: collapse;
}
.alias-explain-table th,
.alias-explain-table td {
  padding: 6px var(--ocg-space-sm);
  border-bottom: 1px solid var(--ocg-border);
  text-align: left;
  vertical-align: middle;
}
.alias-explain-table th {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
}
.alias-explain-first-pick > td {
  background: var(--ocg-success-soft);
}
.alias-explain-candidate-name {
  margin-right: var(--ocg-space-xs);
}
.alias-explain-exclusions {
  margin: 0;
  padding-left: var(--ocg-space-lg);
  display: grid;
  gap: var(--ocg-space-xs);
}
.alias-explain-exclusion-head {
  display: inline-flex;
  align-items: center;
  gap: var(--ocg-space-xs);
  margin-right: var(--ocg-space-sm);
}
.alias-explain-detail {
  color: var(--ocg-muted);
}
.alias-explain-empty {
  margin: 0;
  color: var(--ocg-muted);
}
.alias-explain-note {
  margin: var(--ocg-space-md) 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.alias-explain-meta {
  margin: var(--ocg-space-xs) 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
</style>
