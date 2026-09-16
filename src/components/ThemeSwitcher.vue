<template>
  <n-tooltip trigger="hover" :disabled="themeMenuShown">
    <template #trigger>
      <n-dropdown
        trigger="click"
        :keyboard="false"
        :show="themeMenuShown"
        :options="themeMenuOptions"
        :menu-props="themeMenuProps"
        @select="selectTheme"
        @update:show="updateThemeMenuShown"
      >
        <n-button
          circle
          quaternary
          aria-controls="theme-menu"
          aria-haspopup="menu"
          :aria-expanded="themeMenuShown"
          :aria-label="t('主题：{theme}', { theme: themeLabel })"
          @keydown.esc.prevent.stop="closeThemeMenu"
        >
          <template #icon><n-icon :component="BgColorsOutlined" /></template>
        </n-button>
      </n-dropdown>
    </template>
    {{ t("主题：{theme}", { theme: themeLabel }) }}
  </n-tooltip>
</template>

<script setup lang="ts">
import { computed, h, nextTick, onMounted, onUnmounted, ref } from "vue";
import { NButton, NDropdown, NIcon, NTooltip } from "naive-ui";
import type { DropdownMenuProps, DropdownOption } from "naive-ui";
import { BgColorsOutlined, CheckOutlined } from "@vicons/antd";
import { t } from "../i18n/index.ts";
import type { MessageKey } from "../i18n/index.ts";
import { THEME_OPTIONS } from "../theme";
import type { ResolvedTheme, ThemeName } from "../theme";

const props = defineProps<{
  themeName: ThemeName;
  resolvedTheme: ResolvedTheme;
}>();
const emit = defineEmits<{ "update:themeName": [value: ThemeName] }>();

const themeMenuShown = ref(false);
const themeNames = new Set<ThemeName>(THEME_OPTIONS.map(({ value }) => value));

const themeLabel = computed(() => {
  const selected = t((THEME_OPTIONS.find(({ value }) => value === props.themeName)?.label ?? "默认") as MessageKey);
  if (props.themeName !== "default") return selected;
  const resolved = t((THEME_OPTIONS.find(({ value }) => value === props.resolvedTheme)?.label ?? "皓白") as MessageKey);
  return t("默认 · {theme}", { theme: resolved });
});
const themeMenuOptions = computed<DropdownOption[]>(() => THEME_OPTIONS.map((option) => ({
  key: option.value,
  label: t(option.label as MessageKey),
  icon: () => h("span", {
    "aria-hidden": "true",
    style: {
      display: "inline-block",
      width: "16px",
      height: "16px",
      borderRadius: "50%",
      background: option.swatch,
      boxShadow: "inset 0 0 0 1px rgba(128, 128, 140, 0.45)",
    },
  }),
  extra: props.themeName === option.value
    ? () => h(NIcon, { component: CheckOutlined, size: 14, "aria-hidden": true })
    : undefined,
  props: {
    id: `theme-menu-option-${option.value}`,
    role: "menuitemradio",
    tabindex: -1,
    "aria-checked": props.themeName === option.value ? "true" : "false",
    onKeydown: (event: KeyboardEvent) => handleThemeMenuKeydown(event, option.value),
  },
})));
const themeMenuProps: DropdownMenuProps = () => ({
  id: "theme-menu",
  role: "menu",
  "aria-label": t("选择主题"),
});

function selectTheme(key: string | number) {
  if (typeof key === "string" && themeNames.has(key as ThemeName)) {
    emit("update:themeName", key as ThemeName);
    if (themeMenuShown.value) {
      themeMenuShown.value = false;
      void nextTick(focusThemeTrigger);
    }
  }
}

async function updateThemeMenuShown(show: boolean) {
  themeMenuShown.value = show;
  if (!show) return;
  await nextTick();
  focusThemeMenuOption(props.themeName);
}

function focusThemeMenuOption(theme: ThemeName) {
  document.querySelector<HTMLElement>(`#theme-menu-option-${theme}`)?.focus();
}

function focusThemeTrigger() {
  document.querySelector<HTMLElement>('[aria-controls="theme-menu"]')?.focus();
}

function closeThemeMenu() {
  if (!themeMenuShown.value) return;
  themeMenuShown.value = false;
  void nextTick(focusThemeTrigger);
}

function closeOpenThemeMenuOnEscape(event: KeyboardEvent) {
  if (!themeMenuShown.value || event.key !== "Escape") return;
  event.preventDefault();
  closeThemeMenu();
}

function handleThemeMenuKeydown(event: KeyboardEvent, current: ThemeName) {
  const index = THEME_OPTIONS.findIndex(({ value }) => value === current);
  let nextIndex: number | undefined;
  if (event.key === "ArrowDown" || event.key === "ArrowRight") {
    nextIndex = (index + 1) % THEME_OPTIONS.length;
  } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
    nextIndex = (index - 1 + THEME_OPTIONS.length) % THEME_OPTIONS.length;
  } else if (event.key === "Home") {
    nextIndex = 0;
  } else if (event.key === "End") {
    nextIndex = THEME_OPTIONS.length - 1;
  } else if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    event.stopPropagation();
    selectTheme(current);
    return;
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeThemeMenu();
    return;
  } else {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  focusThemeMenuOption(THEME_OPTIONS[nextIndex].value);
}

onMounted(() => {
  document.addEventListener("keydown", closeOpenThemeMenuOnEscape);
});

onUnmounted(() => {
  document.removeEventListener("keydown", closeOpenThemeMenuOnEscape);
});
</script>
