<script setup lang="ts">
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { computed, onMounted, ref } from "vue";
import appIconUrl from "../assets/app-icon.png";
import { store } from "../store";
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

const mappingCount = computed(() => `${store.mappings.length} 个配置`);
const showFuseTBanner = computed(
  () => store.platformInfo.fuseTRequired && !store.platformInfo.fuseTInstalled,
);

onMounted(() => {
  void store.initAutoLaunch();
});

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
          <p>远程文件系统</p>
        </div>
      </div>
      <nav class="sidebar-nav">
        <span class="nav-item active">
          <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M2 5.5A1.5 1.5 0 0 1 3.5 4h3l1.5 2h4.5A1.5 1.5 0 0 1 14 7.5v4a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5v-6Z" />
          </svg>
          磁盘映射
        </span>
      </nav>
      <div class="sidebar-footer">
        <div class="autostart-row">
          <span class="autostart-label">开机自启动</span>
          <button
            class="switch"
            :class="{ on: store.autoLaunch }"
            type="button"
            role="switch"
            :aria-checked="store.autoLaunch"
            :disabled="store.autoLaunchBusy"
            aria-label="开机自启动"
            @click="store.setAutoLaunch(!store.autoLaunch)"
          >
            <span class="switch-knob" aria-hidden="true"></span>
          </button>
        </div>
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
          导出诊断
        </button>
        <button class="sidebar-lock" type="button" :disabled="locking" @click="lock">
          <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
            <rect x="3" y="7" width="10" height="6" rx="1.5" />
            <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
          </svg>
          锁定
        </button>
      </div>
    </aside>

    <div class="main-panel">
      <section v-if="showFuseTBanner" class="runtime-banner" aria-live="polite">
        <div class="runtime-banner-content">
          <div>
            <strong>需要安装 FUSE-T</strong>
            <p>安装后请重新启动 GugleFS；部分应用首次访问时还需允许“网络宗卷”权限。</p>
          </div>
          <button
            v-if="store.platformInfo.fuseTInstallerBundled"
            class="secondary compact"
            type="button"
            :disabled="installingFuseT"
            @click="installFuseT"
          >
            打开安装器
          </button>
        </div>
      </section>

      <header class="main-header">
        <div>
          <p class="eyebrow">Links</p>
          <h2>磁盘映射</h2>
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
            导入
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
            导出
          </button>
          <button class="icon-button" type="button" title="刷新" aria-label="刷新" @click="store.loadMappings()">
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89M13.5 2.5v3h-3" />
            </svg>
          </button>
          <button class="primary" type="button" @click="formDialog?.open()">
            <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
              <path d="M8 3v10M3 8h10" />
            </svg>
            添加映射
          </button>
        </div>
      </header>

      <div v-if="store.notice" class="notice" :data-kind="store.notice.kind" role="status">
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
        <p class="empty-title">还没有远程磁盘配置</p>
        <p class="empty-hint">支持 SFTP、FTP、WebDAV，挂载后像本地磁盘一样使用</p>
        <button class="primary" type="button" @click="formDialog?.open()">创建第一个映射</button>
      </div>
    </div>

    <MappingFormDialog ref="formDialog" />
    <MountDialog ref="mountDialog" />
  </div>
</template>
