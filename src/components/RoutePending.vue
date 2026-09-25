<template>
  <div class="route-pending" role="status" aria-hidden="true">
    <n-spin size="large" />
  </div>
</template>

<script setup lang="ts">
import { NSpin } from "naive-ui";
</script>

<style scoped>
.route-pending {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 240px;
}
</style>
