<template>
  <span
    v-if="logo"
    class="provider-brand provider-brand--logo"
    :class="{ 'provider-brand--plate': needsPlate }"
    :style="{ width: `${size}px`, height: `${size}px` }"
    aria-hidden="true"
  >
    <img :src="logo" alt="" :width="size" :height="size" />
  </span>
  <span
    v-else
    class="provider-brand provider-brand--monogram"
    :style="{
      width: `${size}px`,
      height: `${size}px`,
      background: family.tint,
      fontSize: `${Math.round(size * 0.5)}px`,
    }"
    aria-hidden="true"
  >{{ monogram }}</span>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { ProviderFamily } from "../domain/provider-families.ts";
import { providerBrandLogo, providerBrandLogoNeedsPlate } from "./provider-brand-logos.ts";

const props = withDefaults(
  defineProps<{ family: ProviderFamily; size?: number }>(),
  { size: 18 },
);

const logo = computed(() => providerBrandLogo(props.family.id));
const needsPlate = computed(() => providerBrandLogoNeedsPlate(props.family.id));

const monogram = computed(() => (props.family.label.trim().charAt(0) || "").toLocaleUpperCase());
</script>

<style scoped>
.provider-brand {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  border-radius: 6px;
  vertical-align: middle;
}

.provider-brand--logo img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: contain;
}

/* Near-black artwork sits on a fixed neutral plate so it reads on dark
   surfaces; the plate is per-icon, never a global filter. */
.provider-brand--plate {
  background: #f5f5f4;
  box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.08);
}

.provider-brand--plate img {
  padding: 1px;
  box-sizing: border-box;
}

.provider-brand--monogram {
  color: #ffffff;
  font-weight: 600;
  line-height: 1;
  letter-spacing: 0;
  box-shadow: inset 0 0 0 1px var(--ocg-border);
}
</style>
