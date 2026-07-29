<script setup lang="ts">
import { onMounted, ref } from "vue";
import { t } from "./i18n";
import { initialize, store } from "./store";
import { updater } from "./updater";
import AuthScreen from "./components/AuthScreen.vue";
import WorkspaceView from "./components/WorkspaceView.vue";

const bootError = ref<string | null>(null);

onMounted(async () => {
  try {
    await initialize();
    if (updater.autoCheck) void updater.check(false);
  } catch (error) {
    bootError.value = String(error);
    if (store.phase === "loading") store.phase = "unlock";
  }
});
</script>

<template>
  <div v-if="store.phase === 'loading'" class="boot-screen" role="status" aria-live="polite">
    <span class="boot-signal" aria-hidden="true"></span>
    <p>{{ t("app.loadingSecureStore") }}</p>
  </div>
  <AuthScreen
    v-else-if="store.phase === 'setup' || store.phase === 'unlock'"
    :key="store.phase"
    :mode="store.phase"
    :boot-error="bootError"
  />
  <WorkspaceView v-else />
</template>
