<template>
  <section v-if="embedded" class="form-surface-embedded">
    <div class="form-surface-embedded__body">
      <slot />
    </div>
    <div v-if="$slots.footer" class="form-surface-embedded__footer">
      <slot name="footer" />
    </div>
  </section>
  <n-modal
    v-else
    :show="show"
    preset="card"
    :title="title"
    class="form-surface-modal"
    :class="modalClass"
    :style="modalStyle"
    :mask-closable="false"
    :close-on-esc="closeOnEsc"
    @update:show="$emit('update:show', $event)"
  >
    <slot />
    <template #footer>
      <slot name="footer" />
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NModal } from "naive-ui";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

/**
 * Shared chrome for the account/supplier/platform forms: the same default and
 * footer slots render inside an NModal card, or as an ordinary inline section
 * when a host (the Add Account chooser, the Providers preset pane) embeds the
 * form directly. Forms keep one body, one validation path, and one payload.
 */
const props = withDefaults(defineProps<{
  show: boolean;
  title?: string;
  /** Inline mode: no modal, no teleport; the host owns layout and dismissal. */
  embedded?: boolean;
  /** Extra class kept for the localized close-label patch and identification. */
  modalClass?: string;
  modalStyle?: string;
  closeOnEsc?: boolean;
}>(), {
  title: "",
  embedded: false,
  modalClass: "",
  modalStyle: "width: 600px; max-width: calc(100vw - 32px)",
  closeOnEsc: true,
});

defineEmits<{
  (event: "update:show", value: boolean): void;
}>();

// Only the modal rendering has a close control to relabel.
useLocalizedModalCloseLabel(
  computed(() => props.show && !props.embedded),
  "form-surface-modal",
);
</script>

<style scoped>
.form-surface-embedded {
  display: flex;
  flex: 1 1 auto;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.form-surface-embedded__body {
  flex: 1 1 auto;
  min-height: 0;
  overflow: auto;
}

.form-surface-embedded__footer {
  flex: none;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--ocg-border);
}
</style>
