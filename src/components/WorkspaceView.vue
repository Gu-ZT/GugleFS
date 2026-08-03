<script setup lang="ts">
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import appIconUrl from "../assets/app-icon.png";
import { localeLabel, t, toggleLocale } from "../i18n";
import { store } from "../store";
import { updater } from "../updater";
import type { MappingRuntime } from "../types";
import { hasPersistedAuthentication } from "../types";
import MappingCard from "./MappingCard.vue";
import MappingFormDialog from "./MappingFormDialog.vue";
import MountDialog from "./MountDialog.vue";

const formDialog = ref<InstanceType<typeof MappingFormDialog> | null>(null);
const mountDialog = ref<InstanceType<typeof MountDialog> | null>(null);
const locking = ref(false);
const installingFuseT = ref(false);
const transferringConfig = ref(false);
const exportingDiagnostics = ref(false);
const twoFactorDialog = ref<HTMLDialogElement | null>(null);
const twoFactorCodeInput = ref<HTMLInputElement | null>(null);
const twoFactorCode = ref("");
const twoFactorBusy = ref(false);
const twoFactorError = ref<string | null>(null);

const mappingCount = computed(() => t("workspace.mappingCount", { count: store.mappings.length }));
const showFuseTBanner = computed(
  () => store.platformInfo.fuseTRequired && !store.platformInfo.fuseTInstalled,
);

onMounted(() => {
  void store.initAutoLaunch();
  window.addEventListener("keydown", handleWorkspaceShortcut);
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", handleWorkspaceShortcut);
});

function handleWorkspaceShortcut(event: KeyboardEvent): void {
  if (event.defaultPrevented || event.altKey || (!event.ctrlKey && !event.metaKey)) return;
  const target = event.target;
  if (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  ) {
    return;
  }
  if (event.key.toLowerCase() === "n") {
    event.preventDefault();
    void formDialog.value?.open();
  } else if (event.key.toLowerCase() === "r") {
    event.preventDefault();
    void store.loadMappings();
  }
}

function toggleMount(runtime: MappingRuntime): void {
  const { config } = runtime;
  if (runtime.state === "mounted") {
    void store.unmountMapping(config.id);
  } else if (hasPersistedAuthentication(config) && !config.sftpTotpRequired) {
    void store.mountMapping(config.id, null, null, false).catch(() => {
      // 挂载失败时打开凭据对话框，让用户补充密码或验证码
      const refreshed = store.mappings.find((item) => item.config.id === config.id) ?? runtime;
      void mountDialog.value?.open(refreshed);
    });
  } else {
    void mountDialog.value?.open(runtime);
  }
}

function lock(): void {
  locking.value = true;
  store
    .lock()
    .catch(async (error) => {
      await store.loadMappings();
      store.setNotice(String(error));
    })
    .finally(() => {
      locking.value = false;
    });
}

function normalizeTwoFactorCode(): void {
  twoFactorCode.value = twoFactorCode.value.replace(/\D/g, "").slice(0, 6);
}

async function toggleTwoFactor(): Promise<void> {
  if (twoFactorBusy.value) return;
  if (!store.authStatus.twoFactorEnabled) {
    twoFactorBusy.value = true;
    try {
      await store.setTwoFactorEnabled(true);
      store.setNotice(t("notice.twoFactorEnabled"), "success");
    } catch (error) {
      store.setNotice(String(error));
    } finally {
      twoFactorBusy.value = false;
    }
    return;
  }

  twoFactorCode.value = "";
  twoFactorError.value = null;
  if (!twoFactorDialog.value?.open) twoFactorDialog.value?.showModal();
  await nextTick();
  twoFactorCodeInput.value?.focus();
}

function closeTwoFactorDialog(): void {
  twoFactorDialog.value?.close();
}

async function disableTwoFactor(): Promise<void> {
  twoFactorBusy.value = true;
  twoFactorError.value = null;
  try {
    await store.setTwoFactorEnabled(false, twoFactorCode.value);
    closeTwoFactorDialog();
    store.setNotice(t("notice.twoFactorDisabled"), "success");
  } catch (error) {
    twoFactorError.value = String(error);
  } finally {
    twoFactorBusy.value = false;
  }
}

function onTwoFactorDialogClose(): void {
  twoFactorCode.value = "";
  twoFactorError.value = null;
}

function installFuseT(): void {
  installingFuseT.value = true;
  store
    .installFuseT()
    .catch((error) => store.setNotice(String(error)))
    .finally(() => {
      installingFuseT.value = false;
    });
}

async function importMappings(): Promise<void> {
  transferringConfig.value = true;
  try {
    const selected = await openFileDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "GugleFS JSON", extensions: ["json"] }],
    });
    if (typeof selected === "string") {
      await store.importMappings(selected);
    }
  } catch (error) {
    store.setNotice(String(error));
  } finally {
    transferringConfig.value = false;
  }
}

async function exportMappings(): Promise<void> {
  transferringConfig.value = true;
  try {
    const selected = await saveFileDialog({
      defaultPath: "guglefs-mappings.json",
      filters: [{ name: "GugleFS JSON", extensions: ["json"] }],
    });
    if (typeof selected === "string") {
      await store.exportMappings(selected);
    }
  } catch (error) {
    store.setNotice(String(error));
  } finally {
    transferringConfig.value = false;
  }
}

async function exportDiagnostics(): Promise<void> {
  exportingDiagnostics.value = true;
  try {
    const selected = await saveFileDialog({
      defaultPath: "guglefs-diagnostics.json",
      filters: [{ name: "GugleFS diagnostics", extensions: ["json"] }],
    });
    if (typeof selected === "string") {
      await store.exportDiagnostics(selected);
    }
  } catch (error) {
    store.setNotice(String(error));
  } finally {
    exportingDiagnostics.value = false;
  }
}
</script>

<template>
  <div class="workspace">
    <aside class="sidebar">
      <div class="sidebar-brand">
        <img class="logo-mark" :src="appIconUrl" alt="" />
        <div class="brand-text">
          <h1>GugleFS</h1>
          <p>{{ t("workspace.remoteFs") }}</p>
        </div>
      </div>
      <nav class="sidebar-nav">
        <span class="nav-item active">
          <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M2 5.5A1.5 1.5 0 0 1 3.5 4h3l1.5 2h4.5A1.5 1.5 0 0 1 14 7.5v4a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5v-6Z" />
          </svg>
          {{ t("workspace.mappings") }}
        </span>
      </nav>
      <div class="sidebar-footer">
        <div class="autostart-row">
          <span class="autostart-label">{{ t("workspace.autostart") }}</span>
          <button
            class="switch"
            :class="{ on: store.autoLaunch }"
            type="button"
            role="switch"
            :aria-checked="store.autoLaunch"
            :disabled="store.autoLaunchBusy"
            :aria-label="t('workspace.autostart')"
            @click="store.setAutoLaunch(!store.autoLaunch)"
          >
            <span class="switch-knob" aria-hidden="true"></span>
          </button>
        </div>
        <div class="autostart-row">
          <span class="autostart-label">{{ t("workspace.autoCheckUpdates") }}</span>
          <button
            class="switch"
            :class="{ on: updater.autoCheck }"
            type="button"
            role="switch"
            :aria-checked="updater.autoCheck"
            :aria-label="t('workspace.autoCheckUpdates')"
            @click="updater.setAutoCheck(!updater.autoCheck)"
          >
            <span class="switch-knob" aria-hidden="true"></span>
          </button>
        </div>
        <div class="autostart-row">
          <span class="autostart-label">{{ t("workspace.twoFactor") }}</span>
          <button
            class="switch"
            :class="{ on: store.authStatus.twoFactorEnabled }"
            type="button"
            role="switch"
            :aria-checked="store.authStatus.twoFactorEnabled"
            :aria-label="t('workspace.twoFactor')"
            :disabled="twoFactorBusy"
            @click="toggleTwoFactor"
          >
            <span class="switch-knob" aria-hidden="true"></span>
          </button>
        </div>
        <button
          class="sidebar-lock language-button"
          type="button"
          :aria-label="t('language.label')"
          :title="t('language.label')"
          @click="toggleLocale"
        >
          <span aria-hidden="true">文/A</span>
          {{ localeLabel }}
        </button>
        <button
          class="sidebar-lock"
          type="button"
          :disabled="exportingDiagnostics"
          @click="exportDiagnostics"
        >
          <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M3 2.5h7l3 3v8H3v-11Z" />
            <path d="M10 2.5v3h3M5.5 9h5M5.5 11.5h3" />
          </svg>
          {{ t("workspace.exportDiagnostics") }}
        </button>
        <button class="sidebar-lock" type="button" :disabled="locking" @click="lock">
          <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
            <rect x="3" y="7" width="10" height="6" rx="1.5" />
            <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
          </svg>
          {{ t("workspace.lock") }}
        </button>
      </div>
    </aside>

    <main class="main-panel">
      <section
        v-if="store.platformInfo.previousSessionUnclean"
        class="runtime-banner recovery-banner"
        role="status"
        aria-live="polite"
      >
        <div class="runtime-banner-content">
          <div>
            <strong>{{ t("workspace.uncleanTitle") }}</strong>
            <p>{{ t("workspace.uncleanBody") }}</p>
          </div>
        </div>
      </section>

      <section v-if="showFuseTBanner" class="runtime-banner" aria-live="polite">
        <div class="runtime-banner-content">
          <div>
            <strong>{{ t("workspace.fuseTitle") }}</strong>
            <p>{{ t("workspace.fuseBody") }}</p>
          </div>
          <button
            v-if="store.platformInfo.fuseTInstallerBundled"
            class="secondary compact"
            type="button"
            :disabled="installingFuseT"
            @click="installFuseT"
          >
            {{ t("workspace.openInstaller") }}
          </button>
        </div>
      </section>

      <section
        v-if="updater.result"
        class="runtime-banner update-banner"
        :data-kind="updater.result.kind"
        :role="updater.result.kind === 'error' ? 'alert' : 'status'"
        aria-live="polite"
      >
        <div class="runtime-banner-content">
          <div v-if="updater.result.kind === 'available'">
            <strong>{{ t("update.availableTitle") }}</strong>
            <p>
              {{ t("update.availableBody", {
                latest: updater.result.info.latestVersion,
                current: updater.result.info.currentVersion,
              }) }}
            </p>
          </div>
          <div v-else-if="updater.result.kind === 'current'">
            <strong>{{ t("update.currentTitle") }}</strong>
            <p>{{ t("update.currentBody", { current: updater.result.info.currentVersion }) }}</p>
          </div>
          <div v-else>
            <strong>{{ t("update.failedTitle") }}</strong>
            <p>{{ t("update.failedBody") }}</p>
          </div>
          <button
            v-if="updater.result.kind === 'available'"
            class="secondary compact"
            type="button"
            @click="updater.openDownloadPage"
          >
            {{ t("update.download") }}
          </button>
        </div>
      </section>

      <header class="main-header">
        <div>
          <p class="eyebrow">Links</p>
          <h2>{{ t("workspace.mappings") }}</h2>
          <p class="main-header-count">{{ mappingCount }}</p>
        </div>
        <div class="main-header-actions">
          <button
            class="secondary compact"
            type="button"
            :disabled="transferringConfig"
            @click="importMappings"
          >
            <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M8 2v8M5 7l3 3 3-3M3 13h10" />
            </svg>
            {{ t("workspace.import") }}
          </button>
          <button
            class="secondary compact"
            type="button"
            :disabled="transferringConfig || store.mappings.length === 0"
            @click="exportMappings"
          >
            <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M8 10V2M5 5l3-3 3 3M3 13h10" />
            </svg>
            {{ t("workspace.export") }}
          </button>
          <button
            class="icon-button"
            type="button"
            :title="updater.checking ? t('workspace.checkingUpdates') : t('workspace.checkUpdates')"
            :aria-label="updater.checking ? t('workspace.checkingUpdates') : t('workspace.checkUpdates')"
            :aria-busy="updater.checking"
            :disabled="updater.checking"
            @click="updater.check(true)"
          >
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89M13.5 2.5v3h-3" />
              <path d="M8 5.5v5M5.8 8.4 8 10.6l2.2-2.2" />
            </svg>
          </button>
          <button
            class="icon-button"
            type="button"
            :title="t('workspace.refresh')"
            :aria-label="t('workspace.refresh')"
            @click="store.loadMappings()"
          >
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89M13.5 2.5v3h-3" />
            </svg>
          </button>
          <button class="primary" type="button" @click="formDialog?.open()">
            <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
              <path d="M8 3v10M3 8h10" />
            </svg>
            {{ t("workspace.add") }}
          </button>
        </div>
      </header>

      <div
        v-if="store.notice"
        class="notice"
        :data-kind="store.notice.kind"
        :role="store.notice.kind === 'error' ? 'alert' : 'status'"
        aria-live="polite"
      >
        {{ store.notice.message }}
      </div>

      <div v-if="store.mappings.length > 0" class="mapping-grid">
        <MappingCard
          v-for="runtime in store.mappings"
          :key="runtime.config.id"
          :runtime="runtime"
          @toggle-mount="toggleMount"
          @edit="(target) => formDialog?.open(target)"
          @remove="(id) => store.deleteMapping(id).catch((e) => store.setNotice(String(e)))"
        />
      </div>

      <div v-else class="empty-state">
        <svg viewBox="0 0 48 48" width="56" height="56" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M14 34a8 8 0 1 1 1.4-15.88A10 10 0 0 1 34.6 21 7 7 0 0 1 34 35H14Z" opacity="0.9" />
          <path d="M24 28v10M20 34l4-4 4 4" />
        </svg>
        <p class="empty-title">{{ t("workspace.emptyTitle") }}</p>
        <p class="empty-hint">{{ t("workspace.emptyHint") }}</p>
        <button class="primary" type="button" @click="formDialog?.open()">
          {{ t("workspace.createFirst") }}
        </button>
      </div>
    </main>

    <MappingFormDialog ref="formDialog" />
    <MountDialog ref="mountDialog" />
    <dialog
      ref="twoFactorDialog"
      class="app-dialog security-dialog"
      aria-labelledby="two-factor-dialog-title"
      @close="onTwoFactorDialogClose"
    >
      <form :aria-busy="twoFactorBusy" @submit.prevent="disableTwoFactor">
        <div class="dialog-heading">
          <div>
            <p class="eyebrow">{{ t("workspace.security") }}</p>
            <h2 id="two-factor-dialog-title">{{ t("workspace.twoFactorDisableTitle") }}</h2>
            <p class="dialog-subtitle">{{ t("workspace.twoFactorDisableHint") }}</p>
          </div>
          <button
            class="icon-button"
            type="button"
            :aria-label="t('dialog.close')"
            :title="t('dialog.close')"
            @click="closeTwoFactorDialog"
          >
            ×
          </button>
        </div>

        <div class="form-grid">
          <label class="full-width">
            <span>{{ t("auth.code") }}</span>
            <input
              ref="twoFactorCodeInput"
              v-model="twoFactorCode"
              class="code-input"
              inputmode="numeric"
              autocomplete="one-time-code"
              maxlength="6"
              pattern="[0-9]{6}"
              placeholder="000000"
              required
              @input="normalizeTwoFactorCode"
            />
          </label>
        </div>

        <div v-if="twoFactorError" class="notice dialog-notice" role="alert">
          {{ twoFactorError }}
        </div>

        <div class="dialog-actions">
          <button class="secondary" type="button" @click="closeTwoFactorDialog">
            {{ t("dialog.cancel") }}
          </button>
          <button class="primary" type="submit" :disabled="twoFactorBusy">
            {{ twoFactorBusy ? t("auth.verifying") : t("workspace.twoFactorDisableConfirm") }}
          </button>
        </div>
      </form>
    </dialog>
  </div>
</template>
