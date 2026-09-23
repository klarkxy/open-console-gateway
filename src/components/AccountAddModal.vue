<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('新增账号')"
    class="account-add-modal"
    style="width: 920px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!interactionLocked"
    @update:show="onOuterUpdateShow"
  >
    <div v-if="showCatalogLoading" class="account-add-loading">
      <n-spin size="large" :description="t('加载中…')" />
    </div>

    <div v-else class="account-add-layout">
      <div class="account-add-mobile">
        <n-radio-group
          :value="mode"
          size="small"
          type="button"
          :disabled="interactionLocked"
          :aria-label="t('账号来源')"
          @update:value="(value: string) => setMode(value as ChooserMode)"
        >
          <n-radio-button value="connections">{{ t("已有连接") }}</n-radio-button>
          <n-radio-button value="services">{{ t("添加新服务") }}</n-radio-button>
        </n-radio-group>
        <n-select
          :value="selectedOptionId || null"
          :options="selectOptions"
          filterable
          :disabled="interactionLocked"
          :aria-label="t('选择要添加的方案')"
          :consistent-menu-width="false"
          @update:value="selectOption"
        />
      </div>

      <aside
        class="account-add-rail"
        :aria-label="t('选择要添加的方案')"
        @keydown="onRailKeydown"
      >
        <div class="account-add-mode">
          <n-radio-group
            :value="mode"
            size="small"
            type="button"
            :disabled="interactionLocked"
            :aria-label="t('账号来源')"
            @update:value="(value: string) => setMode(value as ChooserMode)"
          >
            <n-radio-button value="connections">{{ t("已有连接") }}</n-radio-button>
            <n-radio-button value="services">{{ t("添加新服务") }}</n-radio-button>
          </n-radio-group>
        </div>
        <div class="account-add-search">
          <n-input
            v-model:value="presetQuery"
            size="small"
            clearable
            :placeholder="t('搜索全部选项')"
            :input-props="{ 'aria-label': t('搜索全部选项') }"
          />
        </div>
        <div class="account-add-list">
          <button
            v-for="option in navOptions"
            :id="`account-add-option-${option.optionId}`"
            :key="option.optionId"
            type="button"
            class="account-add-item"
            :class="{
              'account-add-item--active': option.optionId === selectedOptionId,
              'account-add-item--disabled': isChooserOptionDisabled(option),
            }"
            :aria-pressed="option.optionId === selectedOptionId"
            :aria-current="option.optionId === selectedOptionId ? 'true' : undefined"
            @click="selectOption(option.optionId)"
          >
            <ProviderBrandMark
              v-if="brandFamilyFor(optionIconKey(option))"
              :family="brandFamilyFor(optionIconKey(option))!"
              :size="18"
            />
            <n-icon
              v-else
              :component="iconFor(optionIconKey(option))"
              size="16"
              aria-hidden="true"
            />
            <span class="account-add-item__label">{{ option.label }}</span>
            <span
              v-if="isFamilyOption(option) && option.presets.length > 1"
              class="account-add-item__count"
              aria-hidden="true"
            >{{ option.presets.length }}</span>
          </button>
          <p v-if="railEmptyMessage" class="account-add-empty" role="status">{{ railEmptyMessage }}</p>
        </div>
      </aside>

      <div v-if="selected && detail" class="account-add-detail">
        <header class="account-add-detail__header">
          <ProviderBrandMark
            v-if="brandFamilyFor(detailIconKey)"
            :family="brandFamilyFor(detailIconKey)!"
            :size="22"
          />
          <n-icon
            v-else
            :component="iconFor(detailIconKey)"
            size="22"
            aria-hidden="true"
          />
          <div class="account-add-detail__titles">
            <h2>{{ detail.title }}</h2>
            <n-tag
              v-if="detail.tag"
              size="small"
              :bordered="false"
              :type="detail.tag.type"
            >
              {{ t(CHOOSER_TAG_LABEL_KEYS[detail.tag.label]) }}
            </n-tag>
            <span v-if="detail.links" class="account-add-detail__links">
              <a :href="detail.links.docsUrl" target="_blank" rel="noopener noreferrer">{{ t("官方文档") }}</a>
              <a :href="detail.links.websiteUrl" target="_blank" rel="noopener noreferrer">{{ t("控制台") }}</a>
            </span>
          </div>
        </header>

        <p v-if="detail.kind === 'family' || detail.kind === 'preset' || detail.kind === 'manual'" class="account-add-outcome">
          {{ t("可先存草稿，或一次建好供应商和第一个账号。草稿在供应商页继续。") }}
        </p>

        <div
          v-if="selectedFamilyOption && selectedFamilyOption.presets.length > 1"
          class="variant-picker"
        >
          <n-select
            :value="selectedVariantId || null"
            :options="variantOptions"
            size="small"
            :disabled="interactionLocked"
            :consistent-menu-width="false"
            :aria-label="t('选择服务版本')"
            @update:value="(value: string) => selectVariant(value)"
          />
          <p v-if="currentVariantHost" class="variant-picker__summary">
            <span class="mono">{{ currentVariantHost }}</span>
          </p>
        </div>

        <template v-if="selectedPlanOption">
          <n-alert
            v-if="selectedPlanOption.disabled"
            type="warning"
            :title="selectedPlanOption.disabledReason ? t(PLAN_CREATE_DISABLED_REASON_KEYS[selectedPlanOption.disabledReason]) : ''"
          />

          <template v-else>
            <AccountFormModal
              v-if="show"
              embedded
              :show="true"
              :account="null"
              :busy="createBusy"
              :setup-pending="setupPending"
              :plan="selectedPlanOption.plan"
              :catalog="catalog ?? null"
              @save="(payload) => emit('saveAccount', payload)"
            />

            <template v-if="selectedPlanOption.managed">
              <n-alert
                v-if="!managedAvailable"
                type="warning"
                class="account-add-hint"
              >
                <div class="account-add-hint__content">
                  <span>{{ dashboardErrorDetail(managedReason) }}</span>
                  <n-button v-if="inviteMissing" text type="primary" @click="emit('openInviteUrl')">
                    {{ t("前往 OpenCode Go 填写邀请链接") }}
                  </n-button>
                </div>
              </n-alert>
              <div class="account-add-detail__actions">
                <n-tooltip :disabled="managedAvailable">
                  <template #trigger>
                    <n-button
                      secondary
                      :disabled="!managedAvailable || interactionLocked"
                      @click="managedAvailable && !interactionLocked && emit('registerManaged')"
                    >
                      {{ t("注册新账号（Beta）") }}
                    </n-button>
                  </template>
                  {{ dashboardErrorDetail(managedReason) }}
                </n-tooltip>
              </div>
            </template>
          </template>
        </template>

        <DynamicProviderModal
          v-else-if="(currentPreset || selectedManualOption) && show"
          :key="currentPreset ? `dynamic:${currentPreset.id}` : 'dynamic:manual'"
          embedded
          :show="true"
          :provider="null"
          :initial-preset-id="currentPreset?.id ?? null"
          :preset-selection-locked="Boolean(currentPreset)"
          context="account"
          @saved="onPresetSaved"
          @committed="onPresetCommitted"
          @conflict="emit('presetConflict')"
          @busy-change="embeddedFormBusy = $event"
        />

        <PlatformAccountFormModal
          v-else-if="selectedPlatformOption && show"
          embedded
          :show="true"
          :editing="null"
          :preset-kind="selectedPlatformOption.kind"
          :busy="platformBusy"
          @save="(payload) => emit('createPlatform', payload)"
        />
      </div>
    </div>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, toRef, watch } from "vue";
import type { Component } from "vue";
import {
  NAlert,
  NButton,
  NIcon,
  NInput,
  NModal,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NSpin,
  NTag,
  NTooltip,
  useMessage,
} from "naive-ui";
import {
  KeyOutlined,
  CloudOutlined,
  ApiOutlined,
  DatabaseOutlined,
  SwapOutlined,
} from "@vicons/antd";
import { t } from "../i18n/index.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import {
  buildChooserGroups,
  chooserOptionIconKey,
  chooserSelectOptions,
  chooserUniverse,
  defaultChooserOptionId,
  describeChooserSelection,
  isChooserOptionDisabled,
  isValidChooserOption,
  resolveChooserInitialOpen,
  resolveChooserSelection,
  visibleChooserOptions,
  type ChooserMode,
  type ChooserOption,
  type PresetFamilyOption,
  CHOOSER_TAG_LABEL_KEYS,
  MANUAL_CHOOSER_LABEL_KEYS,
  MANUAL_CHOOSER_OPTION_ID,
} from "../domain/account-add-chooser.ts";
import { PROVIDER_FAMILIES, familyOf, type ProviderFamily } from "../domain/provider-families.ts";
import { PROVIDER_PRESETS, type ProviderPreset } from "../domain/provider-presets.ts";
import { isDynamicCatalogEntry } from "../domain/dynamic-provider.ts";
import { PLAN_CREATE_DISABLED_REASON_KEYS } from "../domain/plans.ts";
import { providerApi } from "../api/providers.ts";
import type { Connection } from "../api/connections.ts";
import type { AccountInput } from "../api/dashboard.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import AccountFormModal, { type AccountFormPayload } from "./AccountFormModal.vue";
import DynamicProviderModal from "./DynamicProviderModal.vue";
import PlatformAccountFormModal, {
  type PlatformAccountFormPayload,
} from "./PlatformAccountFormModal.vue";
import ProviderBrandMark from "./ProviderBrandMark.vue";
import { applyAppViewSearchParams } from "../views/app-navigation.ts";
import type { OnboardingIntent } from "../domain/onboarding-draft.ts";

const props = defineProps<{
  show: boolean;
  catalog: readonly ProviderCatalogEntry[] | null | undefined;
  catalogLoading: boolean;
  /** V4 connection projection; unused built-ins stay out of Existing connections. */
  connections?: readonly Connection[] | null;
  setupPending?: boolean;
  managedAvailable: boolean;
  managedReason: string;
  inviteMissing: boolean;
  /** Parent account mutation in flight; embedded account forms bind to it. */
  createBusy: boolean;
  /** Platform section mutation in flight; embedded platform form binds to it. */
  platformBusy: boolean;
  /**
   * One-shot deep-link target (e.g. from the Suppliers Custom API row):
   * preselected only on the closed-to-open transition, never re-applied while
   * the modal is open, so a catalog refresh cannot steal the user's draft.
   */
  initialOptionId?: string | null;
}>();

const emit = defineEmits<{
  (event: "update:show", value: boolean): void;
  (event: "registerManaged"): void;
  (event: "openInviteUrl"): void;
  /** Create path only; the parent owns the account mutation and closes us. */
  (event: "saveAccount", payload: AccountInput | AccountFormPayload): void;
  /** The platform section owns validation, CAS handling, and the write. */
  (event: "createPlatform", payload: PlatformAccountFormPayload): void;
  /** The atomic supplier+first-account create already persisted both lists. */
  (event: "presetSaved", providerId: string): void;
  (event: "presetConflict"): void;
}>();

useLocalizedModalCloseLabel(toRef(props, "show"), "account-add-modal");
const message = useMessage();

const selectedOptionId = ref<string>("");
const selectedVariantId = ref<string>("");
const presetQuery = ref("");
/** Existing connections are the default; preset/platform browsing is explicit. */
const mode = ref<ChooserMode>("connections");
/**
 * provider_id → persisted preset_id for saved user-defined Providers, loaded
 * only to preserve known brand artwork. Offering and ordering always come
 * from the catalog row. Selection is by optionId, so a late completion never
 * unmounts an open form.
 */
const dynamicPresetIds = ref<ReadonlyMap<string, string | null>>(new Map());
let dynamicPresetGeneration = 0;
/** In-flight save/test/discovery inside the embedded dynamic-provider form. */
const embeddedFormBusy = ref(false);

/**
 * The full-screen spinner is only for the first open with no catalog at all.
 * Once the layout has rendered, a background catalog reconciliation (e.g. the
 * reload after a CAS conflict on save) must not swap back to the spinner:
 * doing so unmounts the embedded form and destroys the user's draft, Key,
 * and the conflict notice they need for an explicit retry.
 */
const layoutRendered = ref(false);
const showCatalogLoading = computed(() => (
  props.catalogLoading && !layoutRendered.value && !props.catalog
));

watch(
  () => [props.show, props.catalogLoading] as const,
  ([visible, loading]) => {
    if (visible && !loading) layoutRendered.value = true;
  },
  { immediate: true },
);

watch(
  () => [props.show, props.catalog] as const,
  ([visible, catalog]) => {
    const generation = ++dynamicPresetGeneration;
    if (!visible) return;
    const ids = (catalog ?? [])
      .filter(isDynamicCatalogEntry)
      .map((entry) => entry.provider_id);
    if (ids.length === 0) {
      dynamicPresetIds.value = new Map();
      return;
    }
    void Promise.allSettled(ids.map((id) => providerApi.getProviderDefinition(id))).then((results) => {
      if (generation !== dynamicPresetGeneration) return;
      const next = new Map<string, string | null>();
      results.forEach((result, index) => {
        next.set(ids[index]!, result.status === "fulfilled" ? result.value.preset_id : null);
      });
      dynamicPresetIds.value = next;
    });
  },
  { immediate: true },
);

const manualChooserLabel = computed(() => t(MANUAL_CHOOSER_LABEL_KEYS.manual));
const chooserGroups = computed(() => (
  buildChooserGroups(
    props.catalog,
    dynamicPresetIds.value,
    presetQuery.value,
    mode.value,
    props.connections,
    manualChooserLabel.value,
  )
));
const universe = computed(() => (
  chooserUniverse(
    props.catalog,
    dynamicPresetIds.value,
    mode.value,
    props.connections,
    manualChooserLabel.value,
  )
));
const navOptions = computed(() => visibleChooserOptions(chooserGroups.value));
// The phone selector owns its filter, so it always lists the full option
// universe of the active mode — never the hidden desktop search query.
const selectOptions = computed(() => chooserSelectOptions(
  buildChooserGroups(
    props.catalog,
    dynamicPresetIds.value,
    "",
    mode.value,
    props.connections,
    manualChooserLabel.value,
  ),
  t(CHOOSER_TAG_LABEL_KEYS.user_defined),
));
const railEmptyMessage = computed(() => {
  if (navOptions.value.length > 0) return "";
  if (presetQuery.value.trim()) return t("无匹配选项");
  return mode.value === "connections" ? t("暂无连接") : t("无匹配选项");
});

const selected = computed(() => (
  universe.value.find((option) => option.optionId === selectedOptionId.value) ?? null
));
const selectedPlanOption = computed(() => (
  selected.value && "plan" in selected.value ? selected.value : null
));
const selectedFamilyOption = computed(() => (
  selected.value && "family" in selected.value ? selected.value as PresetFamilyOption : null
));
const selectedPlatformOption = computed(() => (
  selected.value && "kind" in selected.value ? selected.value : null
));
const selectedManualOption = computed(() => (
  selected.value?.optionId === MANUAL_CHOOSER_OPTION_ID ? selected.value : null
));
/**
 * Preset actually fed to the embedded dynamic-provider form. Family options
 * track the variant explicitly (so a search-flattened pick can also resolve);
 * single-variant families and platform options just pass through.
 */
const currentPreset = computed<ProviderPreset | null>(() => {
  if (selectedFamilyOption.value) {
    if (selectedVariantId.value) {
      const match = selectedFamilyOption.value.presets.find((preset) => preset.id === selectedVariantId.value);
      if (match) return match;
    }
    return selectedFamilyOption.value.presets[0] ?? null;
  }
  if (selected.value && "preset" in selected.value) {
    return selected.value.preset;
  }
  return null;
});
const detail = computed(() => {
  if (!selected.value) return null;
  return describeChooserSelection(selected.value, currentPreset.value ?? undefined);
});

/**
 * Brand provenance for a saved user-defined Provider comes from its persisted
 * preset id (loaded with the catalog), never from its display name or URL;
 * without provenance it keeps the generic key glyph.
 */
function optionIconKey(option: ChooserOption): string {
  if ("plan" in option && option.source === "user-defined") {
    const presetId = dynamicPresetIds.value.get(option.optionId);
    const preset = presetId
      ? PROVIDER_PRESETS.find((entry) => entry.id === presetId) ?? null
      : null;
    if (preset) return `family:${familyOf(preset).id}`;
  }
  return chooserOptionIconKey(option);
}

const detailIconKey = computed(() => (
  selected.value ? optionIconKey(selected.value) : ""
));

const variantOptions = computed(() => (
  (selectedFamilyOption.value?.presets ?? []).map((preset) => ({
    value: preset.id,
    label: preset.variant ?? preset.name,
  }))
));

const currentVariantHost = computed(() => {
  const preset = currentPreset.value;
  if (!preset || !selectedFamilyOption.value) return "";
  return variantHost(preset);
});

/**
 * Any in-flight create (parent account save, platform save, or embedded
 * supplier save/test/discovery) blocks closing and switching, so a late
 * success can never land in a different form or duplicate a write.
 */
const interactionLocked = computed(() => (
  props.createBusy || props.platformBusy || embeddedFormBusy.value || props.setupPending
));

function setMode(next: ChooserMode): void {
  if (interactionLocked.value || next === mode.value) return;
  mode.value = next;
  selectedVariantId.value = "";
}

function selectOption(value: string): void {
  if (interactionLocked.value) return;
  // The visible (possibly search-flattened) options resolve first so a
  // flattened preset row maps to its parent family plus the exact variant;
  // the family-shaped universe alone would reject it.
  const resolved = resolveChooserSelection(navOptions.value, universe.value, value);
  if (!resolved) return;
  selectedOptionId.value = resolved.optionId;
  if (resolved.variantId) {
    selectedVariantId.value = resolved.variantId;
    return;
  }
  const target = universe.value.find((option) => option.optionId === resolved.optionId);
  if (target && "family" in target) {
    // Re-picking the family keeps the current variant while it still belongs.
    if (!target.presets.some((preset) => preset.id === selectedVariantId.value)) {
      selectedVariantId.value = target.presets[0]?.id ?? "";
    }
  } else {
    selectedVariantId.value = "";
  }
}

function selectVariant(presetId: string): void {
  if (interactionLocked.value) return;
  if (!selectedFamilyOption.value) return;
  if (!selectedFamilyOption.value.presets.some((preset) => preset.id === presetId)) return;
  selectedVariantId.value = presetId;
}

watch(selectedOptionId, () => {
  // A selection swap unmounts the previous embedded form; its in-flight flags
  // die with it, so the close guard must not outlive the form.
  embeddedFormBusy.value = false;
});
watch(selectedVariantId, () => {
  // Variant switching inside a family remounts the embedded form (the
  // `key="dynamic:..."` on the embed above); its in-flight flags die with it.
  embeddedFormBusy.value = false;
});

let chooserWasVisible = false;
watch(
  () => [props.show, universe.value, props.initialOptionId] as const,
  ([visible, options, initialOptionId]) => {
    const justOpened = visible && !chooserWasVisible;
    chooserWasVisible = visible;
    if (!visible) {
      embeddedFormBusy.value = false;
      return;
    }
    // The search resets only on a fresh open; a background catalog or
    // preset-id reload must not clear what the user is typing.
    if (justOpened) {
      presetQuery.value = "";
      const resolved = resolveChooserInitialOpen(
        initialOptionId,
        props.catalog,
        dynamicPresetIds.value,
        props.connections,
      );
      mode.value = resolved.mode;
      selectedOptionId.value = resolved.optionId;
      selectedVariantId.value = resolved.variantId;
      return;
    }
    // A deep link may open before the provider catalog arrives. Resolve its
    // selection once the options are present; an empty initial result must
    // not leave the detail pane blank for the whole session.
    if (!selectedOptionId.value) {
      const resolved = resolveChooserInitialOpen(
        initialOptionId,
        props.catalog,
        dynamicPresetIds.value,
        props.connections,
      );
      if (resolved.optionId) {
        mode.value = resolved.mode;
        selectedOptionId.value = resolved.optionId;
        selectedVariantId.value = resolved.variantId;
      }
      return;
    }
    if (!isValidChooserOption(options, selectedOptionId.value)) {
      selectedOptionId.value = defaultChooserOptionId(options);
      selectedVariantId.value = "";
    }
  },
  { immediate: true },
);

function onRailKeydown(event: KeyboardEvent): void {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  const ids = navOptions.value.map((option) => option.optionId);
  if (ids.length === 0) return;
  event.preventDefault();
  const current = ids.indexOf(selectedOptionId.value);
  const delta = event.key === "ArrowDown" ? 1 : -1;
  const next = ids[(current + delta + ids.length) % ids.length];
  if (!next) return;
  selectOption(next);
  void nextTick(() => {
    document.getElementById(`account-add-option-${next}`)?.scrollIntoView({ block: "nearest" });
  });
}

let lastCommitWasDraft = false;

function onPresetCommitted(result: {
  connectionId: string;
  mode: OnboardingIntent;
  readbackFailed?: boolean;
}): void {
  lastCommitWasDraft = result.mode === "draft";
  if (result.readbackFailed) {
    message.warning(t("已保存，但列表刷新失败。手动刷新，不要再次提交。"));
  }
  if (!lastCommitWasDraft) return;
  if (!result.readbackFailed) {
    message.success(t("草稿已保存，到供应商页继续设置"));
  }
  const url = applyAppViewSearchParams(new URL(window.location.href), "providers", {
    connection: result.connectionId,
  });
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function onPresetSaved(providerId: string): void {
  if (lastCommitWasDraft) {
    lastCommitWasDraft = false;
    emit("update:show", false);
    return;
  }
  emit("presetSaved", providerId);
}

function onOuterUpdateShow(value: boolean): void {
  if (value) {
    emit("update:show", true);
    return;
  }
  // Failed saves keep the draft; in-flight work keeps the modal open.
  if (props.createBusy || props.platformBusy || embeddedFormBusy.value) return;
  emit("update:show", false);
}

const FAMILY_BY_ID: ReadonlyMap<string, ProviderFamily> = new Map(
  PROVIDER_FAMILIES.map((family) => [family.id, family]),
);

const ICONS: Record<string, Component> = {
  opencode: CloudOutlined,
  "command-code": ApiOutlined,
  minimax: ApiOutlined,
  kimi: ApiOutlined,
  custom: SwapOutlined,
  api: ApiOutlined,
  database: DatabaseOutlined,
};

function iconFor(iconKey: string): Component {
  return ICONS[iconKey] ?? KeyOutlined;
}

function brandFamilyFor(iconKey: string): ProviderFamily | null {
  if (!iconKey.startsWith("family:")) return null;
  return FAMILY_BY_ID.get(iconKey.slice("family:".length)) ?? null;
}

function isFamilyOption(option: ChooserOption): option is PresetFamilyOption {
  return "family" in option;
}

function variantHost(preset: ProviderPreset): string {
  const raw = preset.endpointUrl || preset.endpointPlaceholder || "";
  if (!raw) return "";
  try {
    return new URL(raw).host;
  } catch {
    return raw;
  }
}
</script>

<style scoped>
.account-add-loading {
  display: grid;
  place-items: center;
  min-height: 220px;
}

/* Fixed-height shell: the rail list and the embedded form body are the only
   scroll regions; the detail header and the form footer stay put. */
.account-add-layout {
  display: grid;
  grid-template-columns: 240px minmax(0, 1fr);
  height: min(620px, calc(100vh - 96px));
  overflow: hidden;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
}

.account-add-mobile {
  display: none;
}

.account-add-rail {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  border-right: 1px solid var(--ocg-border);
  background: var(--ocg-canvas);
}

.account-add-search {
  flex: none;
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  background: var(--ocg-canvas);
}

.account-add-mode {
  flex: none;
  display: flex;
  padding: var(--ocg-space-sm) var(--ocg-space-md) 0;
  background: var(--ocg-canvas);
}

.account-add-mode :deep(.n-radio-group) {
  width: 100%;
}

.account-add-mode :deep(.n-radio-button) {
  flex: 1;
  text-align: center;
}

.account-add-outcome {
  flex: none;
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

/* One flat list scrolls in keyboard-navigation order. */
.account-add-list {
  flex: 1;
  min-height: 0;
  padding-bottom: var(--ocg-space-md);
  overflow: auto;
}

.account-add-empty {
  margin: 0;
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.account-add-item {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  width: 100%;
  margin: 0;
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  border: 0;
  border-radius: 0;
  color: var(--ocg-ink);
  font: inherit;
  font-size: var(--ocg-font-sm);
  text-align: left;
  background: transparent;
  cursor: pointer;
}

.account-add-item :deep(.n-icon) {
  flex: none;
  color: var(--ocg-muted);
}

.account-add-item__label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.account-add-item__count {
  margin-left: auto;
  padding: 0 6px;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-variant-numeric: tabular-nums;
}

.account-add-item:hover,
.account-add-item:focus-visible {
  background: var(--ocg-primary-soft);
  outline: none;
}

.account-add-item--active {
  background: var(--ocg-primary-soft);
  font-weight: 600;
}

.account-add-item--active :deep(.n-icon) {
  color: var(--ocg-primary);
}

.account-add-item--disabled {
  color: var(--ocg-muted);
}

.account-add-detail {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-md);
  min-width: 0;
  min-height: 0;
  padding: var(--ocg-space-lg) 20px;
  overflow: auto;
}

.account-add-detail__header {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--ocg-space-md);
}

.account-add-detail__header :deep(.n-icon) {
  color: var(--ocg-primary);
}

.account-add-detail__titles {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
  min-width: 0;
}

.account-add-detail__titles h2 {
  margin: 0;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-lg);
  font-weight: 700;
  line-height: 1.3;
}

.account-add-detail__links {
  display: flex;
  gap: var(--ocg-space-md);
  font-size: var(--ocg-font-xs);
}

.account-add-detail__actions {
  display: flex;
  flex: none;
  gap: var(--ocg-space-sm);
}

.account-add-hint {
  flex: none;
}

.account-add-hint__content {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-sm);
}

.variant-picker {
  display: grid;
  flex: none;
  gap: 6px;
  max-width: 360px;
}

.variant-picker__summary {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  overflow-wrap: anywhere;
}

@media (max-width: 640px) {
  .account-add-layout {
    grid-template-columns: minmax(0, 1fr);
    height: auto;
  }

  .account-add-rail {
    display: none;
  }

  .account-add-mobile {
    display: grid;
    gap: var(--ocg-space-sm);
    padding: var(--ocg-space-md) var(--ocg-space-md) 0;
  }

  .account-add-detail {
    overflow: visible;
  }
}
</style>
