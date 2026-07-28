<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { onMounted, ref } from "vue";
import appIconUrl from "../assets/app-icon.png";
import { localeLabel, t, toggleLocale } from "../i18n";
import { store } from "../store";
import type { TotpSetup } from "../types";

const props = defineProps<{
  mode: "setup" | "unlock";
  bootError: string | null;
}>();

const code = ref("");
const secret = ref("");
const qrCode = ref("");
const busy = ref(false);
const errorMessage = ref<string | null>(props.bootError);
const codeInput = ref<HTMLInputElement | null>(null);

function normalizeCode(): void {
  code.value = code.value.replace(/\D/g, "").slice(0, 6);
}

async function submitUnlock(): Promise<void> {
  busy.value = true;
  errorMessage.value = null;
  try {
    await store.unlock(code.value);
  } catch (error) {
    errorMessage.value = String(error);
  } finally {
    busy.value = false;
  }
}

async function submitSetup(): Promise<void> {
  busy.value = true;
  errorMessage.value = null;
  try {
    await invoke("confirm_2fa_setup", { code: code.value });
    qrCode.value = "";
    secret.value = "";
    await store.enterWorkspace();
  } catch (error) {
    errorMessage.value = String(error);
  } finally {
    busy.value = false;
  }
}

async function copySecret(): Promise<void> {
  try {
    await navigator.clipboard.writeText(secret.value);
  } catch {
    // clipboard API 不可用时静默失败，密钥本身可见可手抄
  }
}

onMounted(async () => {
  if (props.mode === "setup") {
    try {
      const setup = await invoke<TotpSetup>("begin_2fa_setup");
      qrCode.value = setup.qrCode;
      secret.value = setup.secret;
    } catch (error) {
      errorMessage.value = String(error);
    }
  }
  codeInput.value?.focus();
});
</script>

<template>
  <div class="auth-screen">
    <button
      class="language-button auth-language"
      type="button"
      :aria-label="t('language.label')"
      :title="t('language.label')"
      @click="toggleLocale"
    >
      {{ localeLabel }}
    </button>
    <div class="auth-content">
      <div class="auth-logo">
        <img class="logo-mark" :src="appIconUrl" alt="" />
        <h1>GugleFS</h1>
      </div>
      <p class="auth-tagline">{{ t("app.tagline") }}</p>

      <section class="auth-panel" aria-labelledby="auth-title">
        <div class="auth-heading">
          <p class="eyebrow">Two-Factor Authentication</p>
          <h2 id="auth-title">{{ t(mode === "setup" ? "auth.setupTitle" : "auth.unlockTitle") }}</h2>
        </div>

        <div v-if="errorMessage" class="notice auth-notice" role="alert">
          {{ errorMessage }}
        </div>

        <form
          v-if="mode === 'unlock'"
          class="auth-form"
          :aria-busy="busy"
          @submit.prevent="submitUnlock"
        >
          <label>
            <span>{{ t("auth.code") }}</span>
            <input
              ref="codeInput"
              v-model="code"
              class="code-input"
              inputmode="numeric"
              autocomplete="one-time-code"
              maxlength="6"
              pattern="[0-9]{6}"
              placeholder="000000"
              required
              @input="normalizeCode"
            />
          </label>
          <button class="primary btn-block" type="submit" :disabled="busy">
            {{ busy ? t("auth.verifying") : t("auth.unlock") }}
          </button>
        </form>

        <div v-else class="setup-layout">
          <img v-if="qrCode" class="totp-qr" :src="qrCode" :alt="t('auth.qrAlt')" />
          <div v-else class="totp-qr totp-qr-loading" aria-hidden="true"></div>
          <div class="setup-fields">
            <p class="setup-hint">
              {{ t("auth.setupHint") }}
            </p>
            <label>
              <span>{{ t("auth.secret") }}</span>
              <div class="input-action">
                <input class="secret-input" :value="secret" readonly />
                <button class="secondary" type="button" @click="copySecret">{{ t("auth.copy") }}</button>
              </div>
            </label>
            <form class="auth-form setup-form" :aria-busy="busy" @submit.prevent="submitSetup">
              <label>
                <span>{{ t("auth.code") }}</span>
                <input
                  ref="codeInput"
                  v-model="code"
                  class="code-input"
                  inputmode="numeric"
                  autocomplete="one-time-code"
                  maxlength="6"
                  pattern="[0-9]{6}"
                  placeholder="000000"
                  required
                  @input="normalizeCode"
                />
              </label>
              <button class="primary btn-block" type="submit" :disabled="busy">
                {{ busy ? t("auth.verifying") : t("auth.enable") }}
              </button>
            </form>
          </div>
        </div>
      </section>

      <p class="auth-footnote">{{ t("auth.footnote") }}</p>
    </div>
  </div>
</template>
