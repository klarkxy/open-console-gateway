<template>
  <section v-if="embedded" class="form-surface-embedded">
    <div class="form-surface-embedded__body"><slot /></div>
    <div v-if="$slots.footer" class="form-surface-embedded__footer"><slot name="footer" /></div>
  </section>
  <n-modal
    v-else :show="show" preset="card" :title="title"
    class="form-surface-modal" :class="modalClass"
    :style="[modalStyle, { maxHeight: 'calc(100dvh - 32px)' }]"
    :content-style="{ minHeight: '0', overflowY: 'auto', overscrollBehavior: 'contain' }"
    :mask-closable="false" :close-on-esc="closeOnEsc"
    @update:show="$emit('update:show', $event)"
  >
    <slot />
    <template #footer><slot name="footer" /></template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NModal } from "naive-ui";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

/** Shared chrome only. Embedded and modal forms keep one validation/payload path. */
const props = withDefaults(defineProps<{
  show: boolean;
  title?: string;
  embedded?: boolean;
  modalClass?: string;
  modalStyle?: string;
  closeOnEsc?: boolean;
}>(), {
  title: "", embedded: false, modalClass: "",
  modalStyle: "width: 600px; max-width: calc(100vw - 32px)", closeOnEsc: true,
});
defineEmits<{ (event: "update:show", value: boolean): void }>();
useLocalizedModalCloseLabel(computed(() => props.show && !props.embedded), "form-surface-modal");
</script>

<style scoped>
.form-surface-embedded { display: flex; flex: 1 1 auto; flex-direction: column; min-width: 0; min-height: 0; }
.form-surface-embedded__body { flex: 1 1 auto; min-height: 0; overflow: auto; overscroll-behavior: contain; scrollbar-gutter: stable; }
.form-surface-embedded__footer { flex: none; margin-top: var(--ocg-space-lg); padding-top: var(--ocg-space-lg); border-top: 1px solid var(--ocg-divider); }
</style>
