<template>
  <n-date-picker v-bind="$attrs" />
</template>

<script setup lang="ts">
import { NDatePicker } from "naive-ui/es/date-picker/index.mjs";

defineOptions({ inheritAttrs: false });
</script>
