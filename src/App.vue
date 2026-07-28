<script setup lang="ts">
import { onMounted, ref } from "vue";
import { initialize, store } from "./store";
import AuthScreen from "./components/AuthScreen.vue";
import WorkspaceView from "./components/WorkspaceView.vue";

const bootError = ref<string | null>(null);

onMounted(async () => {
  try {
    await initialize();
  } catch (error) {
    bootError.value = String(error);
    if (store.phase === "loading") store.phase = "unlock";
  }
});
</script>

<template>
  <div v-if="store.phase === 'loading'" class="boot-screen">
    <span class="boot-signal" aria-hidden="true"></span>
    <p>正在连接本地安全存储…</p>
  </div>
  <AuthScreen
    v-else-if="store.phase === 'setup' || store.phase === 'unlock'"
    :key="store.phase"
    :mode="store.phase"
    :boot-error="bootError"
  />
  <WorkspaceView v-else />
</template>
