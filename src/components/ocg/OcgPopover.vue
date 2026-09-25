<script setup lang="ts">
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from "reka-ui";

const open = defineModel<boolean>("open", { default: false });

const props = withDefaults(
  defineProps<{
    side?: "top" | "right" | "bottom" | "left";
    align?: "start" | "center" | "end";
    sideOffset?: number;
  }>(),
  { side: "bottom", align: "start", sideOffset: 4 },
);
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <slot name="trigger" />
    </PopoverTrigger>
    <PopoverPortal>
      <Transition name="ocg-overlay">
        <PopoverContent
          v-if="open"
          force-mount
          :side="props.side"
          :align="props.align"
          :side-offset="props.sideOffset"
          class="z-[2000] rounded-md border border-border bg-surface-raised p-2 text-ink shadow-[var(--ocg-shadow-lg)]"
        >
          <slot />
        </PopoverContent>
      </Transition>
    </PopoverPortal>
  </PopoverRoot>
</template>
