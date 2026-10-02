import { defineComponent, h, type Component } from "vue";

function icon(name: string): Component {
  return defineComponent({
    name,
    setup: () => () => h("span", { "data-icon": name }),
  });
}

export const PlusOutlined = icon("PlusOutlined");
export const ReloadOutlined = icon("ReloadOutlined");
export const SettingOutlined = icon("SettingOutlined");
