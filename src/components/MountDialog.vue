<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { t } from "../i18n";
import { store } from "../store";
import type { MappingRuntime } from "../types";
import { hasPersistedAuthentication } from "../types";

const dialog = ref<HTMLDialogElement | null>(null);
const passwordInput = ref<HTMLInputElement | null>(null);
const totpInput = ref<HTMLInputElement | null>(null);

const target = ref<MappingRuntime | null>(null);
const password = ref("");
const totpCode = ref("");
const remember = ref(true);
const busy = ref(false);
const errorMessage = ref<string | null>(null);

const credentialStored = computed(() =>
  target.value ? hasPersistedAuthentication(target.value.config) : false,
);
const totpRequired = computed(() => target.value?.config.sftpTotpRequired ?? false);
const credentialLabel = computed(() =>
  t(target.value?.config.auth.type === "private_key" ? "mount.passphrase" : "mount.password"),
);
const subtitle = computed(() =>
  target.value ? `${target.value.config.name} → ${target.value.config.mountPoint}` : "",
);

async function open(runtime: MappingRuntime): Promise<void> {
  target.value = runtime;
  password.value = "";
  totpCode.value = "";
  remember.value = true;
  errorMessage.value = null;
  if (!dialog.value?.open) dialog.value?.showModal();
  await nextTick();
  const focusTarget = credentialStored.value && totpRequired.value ? totpInput : passwordInput;
  focusTarget.value?.focus();
}

function close(): void {
  dialog.value?.close();
}

async function submit(): Promise<void> {
  if (!target.value) return;
  busy.value = true;
  errorMessage.value = null;
  try {
    await store.mountMapping(
      target.value.config.id,
      password.value || null,
      totpCode.value || null,
      remember.value,
    );
    close();
  } catch (error) {
    errorMessage.value = String(error);
  } finally {
    busy.value = false;
  }
}

function onDialogClose(): void {
  target.value = null;
  password.value = "";
  totpCode.value = "";
  remember.value = true;
  errorMessage.value = null;
}

defineExpose({ open });
</script>

<template>
  <dialog
    ref="dialog"
    class="app-dialog mount-dialog"
    aria-labelledby="mount-dialog-title"
    @close="onDialogClose"
  >
    <form :aria-busy="busy" @submit.prevent="submit">
      <div class="dialog-heading">
        <div>
          <p class="eyebrow">Mount</p>
          <h2 id="mount-dialog-title">{{ t("mount.title") }}</h2>
          <p class="dialog-subtitle">{{ subtitle }}</p>
        </div>
        <button
          class="icon-button"
          type="button"
          :aria-label="t('dialog.close')"
          :title="t('dialog.close')"
          @click="close"
        >
          ×
        </button>
      </div>

      <div class="form-grid">
        <label v-if="!credentialStored" class="full-width">
          <span>{{ credentialLabel }}</span>
          <input
            ref="passwordInput"
            v-model="password"
            type="password"
            autocomplete="current-password"
            required
          />
        </label>
        <label v-if="totpRequired" class="full-width">
          <span>{{ t("mount.totp") }}</span>
          <input
            ref="totpInput"
            v-model="totpCode"
            inputmode="numeric"
            autocomplete="one-time-code"
            maxlength="6"
            required
          />
        </label>
        <label v-if="!credentialStored" class="checkbox-row full-width">
          <input v-model="remember" type="checkbox" />
          <span>{{ t("mount.remember", { store: store.platformInfo.secureStore }) }}</span>
        </label>
      </div>

      <div v-if="errorMessage" class="notice dialog-notice" role="alert">
        {{ errorMessage }}
      </div>

      <div class="dialog-actions">
        <button class="secondary" type="button" @click="close">{{ t("dialog.cancel") }}</button>
        <button class="primary" type="submit" :disabled="busy">
          {{ busy ? t("mapping.mounting") : t("mapping.mount") }}
        </button>
      </div>
    </form>
  </dialog>
</template>
