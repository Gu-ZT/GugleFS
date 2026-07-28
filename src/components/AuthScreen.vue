<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { onMounted, ref } from "vue";
import appIconUrl from "../assets/app-icon.png";
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
    <div class="auth-content">
      <div class="auth-logo">
        <img class="logo-mark" :src="appIconUrl" alt="" />
        <h1>GugleFS</h1>
      </div>
      <p class="auth-tagline">把远程服务器挂载为本地磁盘</p>

      <section class="auth-panel" aria-labelledby="auth-title">
        <div class="auth-heading">
          <p class="eyebrow">Two-Factor Authentication</p>
          <h2 id="auth-title">{{ mode === "setup" ? "绑定双因素认证" : "验证身份" }}</h2>
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
            <span>验证码</span>
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
            {{ busy ? "验证中…" : "解锁" }}
          </button>
        </form>

        <div v-else class="setup-layout">
          <img v-if="qrCode" class="totp-qr" :src="qrCode" alt="GugleFS 2FA 二维码" />
          <div v-else class="totp-qr totp-qr-loading" aria-hidden="true"></div>
          <div class="setup-fields">
            <p class="setup-hint">
              使用 authenticator 应用扫描二维码，然后输入生成的 6 位验证码完成绑定。
            </p>
            <label>
              <span>密钥</span>
              <div class="input-action">
                <input class="secret-input" :value="secret" readonly />
                <button class="secondary" type="button" @click="copySecret">复制</button>
              </div>
            </label>
            <form class="auth-form setup-form" :aria-busy="busy" @submit.prevent="submitSetup">
              <label>
                <span>验证码</span>
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
                {{ busy ? "验证中…" : "启用 2FA" }}
              </button>
            </form>
          </div>
        </div>
      </section>

      <p class="auth-footnote">凭据经系统安全存储加密 · 双因素认证保护</p>
    </div>
  </div>
</template>
