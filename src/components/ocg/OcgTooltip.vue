<script setup lang="ts">
import { ref } from "vue";
import { TooltipContent, TooltipPortal, TooltipProvider, TooltipRoot, TooltipTrigger } from "reka-ui";

const props = withDefaults(
  defineProps<{
    delay?: number;
    side?: "top" | "right" | "bottom" | "left";
    sideOffset?: number;
  }>(),
  { delay: 200, side: "top", sideOffset: 4 },
);

const open = ref(false);
</script>

<template>
  <TooltipProvider :delay-duration="props.delay" disable-hoverable-content>
    <TooltipRoot v-model:open="open">
      <TooltipTrigger as-child>
        <slot name="trigger" />
      </TooltipTrigger>
      <TooltipPortal>
        <Transition name="ocg-overlay">
          <TooltipContent
            v-if="open"
            force-mount
            :side="props.side"
            :side-offset="props.sideOffset"
            class="z-[2000] max-w-[280px] rounded-md border border-border bg-surface-raised px-2 py-1 text-[length:var(--ocg-font-xs)] leading-[1.5] text-ink shadow-[var(--ocg-shadow-sm)]"
          >
            <slot />
          </TooltipContent>
        </Transition>
      </TooltipPortal>
    </TooltipRoot>
  </TooltipProvider>
</template>
