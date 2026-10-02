import { defineComponent, h, type Component, type PropType } from "vue";

/**
 * Host-renderer stand-in for the Naive controls these panel bundles render.
 * Values, clicks, and v-model stay on the vnode props the behavior tests call.
 */

function numberPayload(payload: unknown): number | null {
  if (typeof payload === "number") return Number.isFinite(payload) ? payload : null;
  if (typeof payload !== "string" || payload.trim() === "") return null;
  const value = Number(payload);
  return Number.isFinite(value) ? value : null;
}

function callHandler(handler: unknown): unknown {
  return typeof handler === "function" ? (handler as () => unknown)() : undefined;
}

export const NButton = defineComponent({
  inheritAttrs: false,
  props: {
    circle: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
    loading: { type: Boolean, default: false },
    type: { type: String, default: "" },
  },
  setup(props, { attrs, slots }) {
    return () => {
      const blocked = props.disabled || props.loading;
      return h("button", {
        ...attrs,
        type: "button",
        class: attrs.class,
        disabled: blocked ? true : undefined,
        "data-circle": props.circle ? "1" : undefined,
        "data-loading": props.loading ? "true" : "false",
        "data-variant": props.type,
        onClick: () => (blocked ? undefined : callHandler(attrs.onClick)),
      }, [slots.icon?.(), slots.default?.()]);
    };
  },
});

export const NCheckbox = defineComponent({
  inheritAttrs: false,
  props: {
    checked: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
  },
  emits: ["update:checked"],
  setup(props, { attrs, emit, slots }) {
    return () => h("button", {
      ...attrs,
      type: "button",
      role: "checkbox",
      "aria-checked": props.checked ? "true" : "false",
      disabled: props.disabled ? true : undefined,
      onClick: () => {
        if (!props.disabled) emit("update:checked", !props.checked);
      },
    }, slots.default?.());
  },
});

export const NForm = defineComponent({
  inheritAttrs: false,
  setup(_props, { attrs, slots }) {
    return () => h("form", { ...attrs, onSubmit: (event: Event) => {
      event.preventDefault();
      callHandler(attrs.onSubmit);
    } }, slots.default?.());
  },
});

export const NFormItem = defineComponent({
  inheritAttrs: false,
  props: { label: { type: String, default: "" } },
  setup(props, { attrs, slots }) {
    return () => h("label", { ...attrs }, [props.label, slots.default?.()]);
  },
});

export const NIcon = defineComponent({
  inheritAttrs: false,
  props: {
    component: { type: Object as PropType<Component | null>, default: null },
  },
  setup(props, { attrs, slots }) {
    return () => h("span", { ...attrs, "data-icon": "1" }, [
      props.component ? h(props.component) : null,
      slots.default?.(),
    ]);
  },
});

export const NInput = defineComponent({
  inheritAttrs: false,
  props: {
    disabled: { type: Boolean, default: false },
    value: { type: String, default: "" },
  },
  emits: ["update:value"],
  setup(props, { attrs, emit }) {
    return () => h("input", {
      ...attrs,
      value: props.value,
      disabled: props.disabled ? true : undefined,
      onInput: (payload: unknown) => {
        const value = typeof payload === "string"
          ? payload
          : (payload as { target?: { value?: string } } | null)?.target?.value ?? "";
        emit("update:value", value);
      },
    });
  },
});

export const NInputNumber = defineComponent({
  inheritAttrs: false,
  props: {
    disabled: { type: Boolean, default: false },
    value: { default: null as number | null },
  },
  emits: ["update:value"],
  setup(props, { attrs, emit, slots }) {
    return () => {
      const value = props.value;
      const shown = value === null || value === undefined ? "" : String(value);
      return h("input", {
        ...attrs,
        "data-number": "1",
        "data-value": shown,
        disabled: props.disabled ? true : undefined,
        onInput: (payload: unknown) => {
          emit("update:value", numberPayload(payload));
        },
      }, slots.suffix?.());
    };
  },
});

export const NModal = defineComponent({
  inheritAttrs: false,
  props: { show: { type: Boolean, default: false } },
  emits: ["update:show"],
  setup(props, { attrs, slots }) {
    return () => props.show
      ? h("div", { role: "dialog", class: attrs.class }, [
        slots.header?.(),
        slots.default?.(),
        slots.footer?.(),
      ])
      : null;
  },
});

export const NProgress = defineComponent({
  inheritAttrs: false,
  props: { percentage: { type: Number, default: 0 } },
  setup(props) {
    return () => h("div", {
      "data-percentage": props.percentage,
      "data-progress": "1",
    });
  },
});

export const NSlider = defineComponent({
  inheritAttrs: false,
  props: {
    disabled: { type: Boolean, default: false },
    value: { type: Number, default: 0 },
  },
  emits: ["update:value"],
  setup(props, { attrs }) {
    return () => h("div", {
      ...attrs,
      role: "slider",
      "aria-valuenow": props.value,
      "data-slider": "1",
    });
  },
});

export const NSpace = defineComponent({
  inheritAttrs: false,
  setup(_props, { attrs, slots }) {
    return () => h("div", attrs, slots.default?.());
  },
});

export const NTooltip = defineComponent({
  inheritAttrs: false,
  setup(_props, { attrs, slots }) {
    return () => h("span", attrs, [slots.trigger?.(), slots.default?.()]);
  },
});
