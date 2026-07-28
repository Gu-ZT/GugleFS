<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, reactive, ref, watch } from "vue";
import { t } from "../i18n";
import { store } from "../store";
import type {
  AuthMethod,
  MappingConfig,
  MappingRuntime,
  Protocol,
  RemoteBrowserListing,
  RemoteDirectory,
  WebDavAuthMethod,
} from "../types";

const DEFAULT_PORTS: Record<Protocol, number> = { ftp: 21, sftp: 22, webdav: 443 };

const dialog = ref<HTMLDialogElement | null>(null);
const dialogEl = dialog; // template ref alias for clarity
const formEl = ref<HTMLFormElement | null>(null);
const nameInput = ref<HTMLInputElement | null>(null);

const draft = reactive({
  id: "",
  name: "",
  protocol: "sftp" as Protocol,
  host: "",
  port: 22,
  username: "",
  password: "",
  sftpAuth: "password" as "password" | "private_key" | "ssh_agent",
  webdavAuth: "basic" as WebDavAuthMethod,
  webdavClientCertificatePath: "",
  keySource: "local" as "local" | "pasted",
  keyPath: "",
  privateKey: "",
  sftpTotpEnabled: false,
  sftpTotpCode: "",
  ftpTls: false,
  remotePath: "/",
  mountPoint: "Z:",
  autoMount: false,
  ignoreSystemProxy: false,
});

const editing = ref<MappingRuntime | null>(null);
const editingCredentialId = ref<string | null>(null);
const editingAuthType = ref<AuthMethod["type"] | null>(null);
const editingKeyId = ref<string | null>(null);
const editingWebDavAuth = ref<WebDavAuthMethod | null>(null);
const trustedHostKeyFingerprint = ref<string | null>(null);

const notice = ref<{ message: string; kind: "error" | "success" } | null>(null);
const testing = ref(false);
const saving = ref(false);
const remoteBrowserOpen = ref(false);
const remoteBrowserBusy = ref(false);
const remoteBrowserRoot = ref("/");
const remoteBrowserPath = ref("/");
const remoteDirectories = ref<RemoteDirectory[]>([]);
let remoteBrowserSessionId = crypto.randomUUID();
const mountPointInput = ref<HTMLInputElement | null>(null);

const mountPointError = computed(() => {
  if (store.platformInfo.os !== "windows") {
    return null;
  }
  const match = /^([A-Za-z]):[\\/]?$/.exec(draft.mountPoint.trim());
  if (match && store.occupiedLetters.includes(match[1].toUpperCase())) {
    return t("form.driveOccupied", { drive: match[1].toUpperCase() });
  }
  return null;
});

watch(mountPointError, (error) => {
  mountPointInput.value?.setCustomValidity(error ?? "");
});

const privateKeyAuth = computed(
  () => draft.protocol === "sftp" && draft.sftpAuth === "private_key",
);
const sshAgentAuth = computed(
  () => draft.protocol === "sftp" && draft.sftpAuth === "ssh_agent",
);
const totpActive = computed(() => draft.protocol === "sftp" && draft.sftpTotpEnabled);
const webdavClientCertificateAuth = computed(
  () => draft.protocol === "webdav" && draft.webdavAuth === "client_certificate",
);
const webdavAnonymousAuth = computed(
  () => draft.protocol === "webdav" && draft.webdavAuth === "anonymous",
);
const usernameVisible = computed(
  () =>
    draft.protocol !== "webdav" ||
    draft.webdavAuth === "basic" ||
    draft.webdavAuth === "digest",
);
const credentialInputVisible = computed(
  () =>
    !sshAgentAuth.value &&
    !webdavClientCertificateAuth.value &&
    !webdavAnonymousAuth.value,
);
const credentialLabel = computed(() => {
  if (privateKeyAuth.value) return t("form.passphrase");
  if (draft.protocol === "webdav" && draft.webdavAuth === "bearer") return "Bearer Token";
  return t("form.password");
});
const passwordPlaceholder = computed(() =>
  privateKeyAuth.value
    ? t("form.keepPassphrase")
    : draft.protocol === "webdav" && draft.webdavAuth === "bearer"
      ? t("form.keepToken")
      : t("form.keepPassword"),
);

function onProtocolChange(): void {
  draft.port = DEFAULT_PORTS[draft.protocol];
}

function readMappingConfig(): MappingConfig {
  const preserveCredential =
    editing.value !== null &&
    draft.protocol === editing.value.config.protocol &&
    (draft.protocol !== "sftp" || draft.sftpAuth === editingAuthType.value) &&
    (draft.protocol !== "webdav" || draft.webdavAuth === editingWebDavAuth.value);
  let auth: AuthMethod = {
    type: "password",
    credential_id: preserveCredential ? editingCredentialId.value : null,
  };
  if (sshAgentAuth.value) {
    auth = { type: "ssh_agent" };
  } else if (webdavClientCertificateAuth.value || webdavAnonymousAuth.value) {
    auth = { type: "anonymous" };
  } else if (privateKeyAuth.value) {
    auth = {
      type: "private_key",
      key_path: draft.keySource === "local" ? draft.keyPath.trim() || null : null,
      key_id:
        draft.keySource === "pasted" && editingAuthType.value === "private_key"
          ? editingKeyId.value
          : null,
      credential_id: preserveCredential ? editingCredentialId.value : null,
    };
  }
  return {
    id: draft.id || crypto.randomUUID(),
    name: draft.name.trim(),
    protocol: draft.protocol,
    host: draft.host.trim(),
    port: draft.port,
    username: usernameVisible.value ? draft.username.trim() || null : null,
    auth,
    remotePath: draft.remotePath.trim(),
    mountPoint: draft.mountPoint.trim(),
    ftpTls: draft.protocol === "ftp" && draft.ftpTls,
    hostKeyFingerprint:
      draft.protocol === "sftp" ? trustedHostKeyFingerprint.value : null,
    sftpTotpRequired: totpActive.value,
    ignoreSystemProxy: draft.ignoreSystemProxy,
    webdavAuth: draft.protocol === "webdav" ? draft.webdavAuth : "basic",
    webdavClientCertificatePath: webdavClientCertificateAuth.value
      ? draft.webdavClientCertificatePath.trim() || null
      : null,
    autoMount: totpActive.value ? false : draft.autoMount,
  };
}

async function verifySftpHostKey(config: MappingConfig): Promise<void> {
  if (config.protocol !== "sftp") return;
  const fingerprint = await invoke<string>("inspect_sftp_host_key", {
    host: config.host,
    port: config.port,
    ignoreSystemProxy: config.ignoreSystemProxy,
  });
  if (fingerprint !== trustedHostKeyFingerprint.value) {
    const changed = trustedHostKeyFingerprint.value !== null;
    const accepted = window.confirm(
      changed
        ? t("form.hostKeyChanged", {
            oldFingerprint: trustedHostKeyFingerprint.value ?? "",
            fingerprint,
          })
        : t("form.confirmHostKey", { fingerprint }),
    );
    if (!accepted) throw new Error(t("form.hostKeyRejected"));
  }
  trustedHostKeyFingerprint.value = fingerprint;
  config.hostKeyFingerprint = fingerprint;
}

async function chooseKey(): Promise<void> {
  try {
    const selected = await openFileDialog({ multiple: false, directory: false });
    if (typeof selected === "string") draft.keyPath = selected;
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  }
}

async function chooseMountPoint(): Promise<void> {
  try {
    const selected = await openFileDialog({ multiple: false, directory: true });
    if (typeof selected === "string") draft.mountPoint = selected;
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  }
}

const mfaDetectedMessage = (): string => t("form.mfaDetected");

async function probeMfaRequirement(config: MappingConfig): Promise<boolean> {
  if (config.protocol !== "sftp" || config.sftpTotpRequired) {
    return false;
  }
  const required = await invoke<boolean>("detect_sftp_mfa_requirement", {
    config,
    password: draft.password || null,
    privateKey: draft.privateKey.trim() || null,
  });
  if (required) {
    draft.sftpTotpEnabled = true;
  }
  return required;
}

async function save(): Promise<void> {
  saving.value = true;
  notice.value = null;
  try {
    const config = readMappingConfig();
    await verifySftpHostKey(config);
    if (config.autoMount && (await probeMfaRequirement(config))) {
      throw new Error(`${mfaDetectedMessage()}${t("form.saveAgain")}`);
    }
    await invoke("save_mapping", {
      config,
      password: draft.password || null,
      privateKey: draft.privateKey.trim() || null,
    });
    close();
    await store.loadMappings();
    store.setNotice(
      t(draft.password || draft.privateKey.trim() ? "form.savedWithAuth" : "form.saved"),
      "success",
    );
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  } finally {
    saving.value = false;
  }
}

async function testConnection(): Promise<void> {
  if (!formEl.value?.reportValidity()) {
    return;
  }
  testing.value = true;
  notice.value = null;
  try {
    const config = readMappingConfig();
    const totpCode = totpActive.value ? draft.sftpTotpCode.trim() || null : null;
    if (config.sftpTotpRequired && !totpCode) {
      throw new Error(t("form.mfaTestRequired"));
    }
    await verifySftpHostKey(config);
    if (await probeMfaRequirement(config)) {
      notice.value = {
        message: `${mfaDetectedMessage()}${t("form.testAgain")}`,
        kind: "success",
      };
      return;
    }
    await invoke("test_remote_connection", {
      config,
      password: draft.password || null,
      privateKey: draft.privateKey.trim() || null,
      totpCode,
    });
    notice.value = {
      message: t("form.connectionSucceeded", { protocol: config.protocol.toUpperCase() }),
      kind: "success",
    };
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  } finally {
    testing.value = false;
  }
}

async function chooseWebDavClientCertificate(): Promise<void> {
  try {
    const selected = await openFileDialog({
      multiple: false,
      directory: false,
      title: t("form.chooseCertificate"),
    });
    if (typeof selected === "string") draft.webdavClientCertificatePath = selected;
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  }
}

async function importKnownHosts(): Promise<void> {
  notice.value = null;
  try {
    const selected = await openFileDialog({
      multiple: false,
      directory: false,
      title: t("form.chooseKnownHosts"),
    });
    if (typeof selected !== "string") return;
    const fingerprints = await invoke<string[]>("import_sftp_known_hosts", {
      path: selected,
      host: draft.host.trim(),
      port: draft.port,
    });
    const liveFingerprint = await invoke<string>("inspect_sftp_host_key", {
      host: draft.host.trim(),
      port: draft.port,
      ignoreSystemProxy: draft.ignoreSystemProxy,
    });
    if (!fingerprints.includes(liveFingerprint)) {
      throw new Error(t("form.knownHostsMismatch"));
    }
    trustedHostKeyFingerprint.value = liveFingerprint;
    notice.value = { message: t("form.knownHostsImported"), kind: "success" };
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  }
}

function normalizedRemotePath(path: string): string {
  const segments = path.trim().split("/").filter(Boolean);
  return segments.length === 0 ? "/" : `/${segments.join("/")}`;
}

function parentRemotePath(path: string): string {
  const segments = normalizedRemotePath(path).split("/").filter(Boolean);
  segments.pop();
  return segments.length === 0 ? "/" : `/${segments.join("/")}`;
}

function applyRemoteBrowserListing(listing: RemoteBrowserListing): void {
  remoteBrowserPath.value = listing.path;
  remoteDirectories.value = listing.directories;
  remoteBrowserOpen.value = true;
}

async function openRemoteBrowser(): Promise<void> {
  if (!formEl.value?.reportValidity()) return;
  remoteBrowserBusy.value = true;
  notice.value = null;
  try {
    const config = readMappingConfig();
    const totpCode = totpActive.value ? draft.sftpTotpCode.trim() || null : null;
    await verifySftpHostKey(config);
    if (await probeMfaRequirement(config)) {
      throw new Error(`${mfaDetectedMessage()}${t("form.browseAgain")}`);
    }
    if (config.sftpTotpRequired && !totpCode) {
      throw new Error(t("form.mfaBrowseRequired"));
    }
    const listing = await invoke<RemoteBrowserListing>("open_remote_browser", {
      config,
      password: draft.password || null,
      privateKey: draft.privateKey.trim() || null,
      totpCode,
      sessionId: remoteBrowserSessionId,
    });
    remoteBrowserRoot.value = listing.path;
    applyRemoteBrowserListing(listing);
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  } finally {
    remoteBrowserBusy.value = false;
  }
}

async function loadRemoteDirectories(path: string): Promise<void> {
  remoteBrowserBusy.value = true;
  notice.value = null;
  try {
    const listing = await invoke<RemoteBrowserListing>("list_remote_directories", {
      sessionId: remoteBrowserSessionId,
      path: normalizedRemotePath(path),
    });
    applyRemoteBrowserListing(listing);
  } catch (error) {
    notice.value = { message: String(error), kind: "error" };
  } finally {
    remoteBrowserBusy.value = false;
  }
}

async function closeRemoteBrowser(): Promise<void> {
  remoteBrowserOpen.value = false;
  remoteDirectories.value = [];
  try {
    await invoke("close_remote_browser", { sessionId: remoteBrowserSessionId });
  } catch {
    // 会话也会在应用锁定或退出时清理，不阻塞关闭表单。
  }
}

function selectRemotePath(): void {
  draft.remotePath = remoteBrowserPath.value;
  void closeRemoteBrowser();
}

async function open(runtime?: MappingRuntime): Promise<void> {
  remoteBrowserSessionId = crypto.randomUUID();
  editing.value = runtime ?? null;
  if (!runtime) {
    await store.refreshOccupiedLetters();
  }
  const config = runtime?.config;
  const auth = config?.auth;
  const privateKeyConfig = auth?.type === "private_key" ? auth : null;
  editingCredentialId.value = auth && "credential_id" in auth ? auth.credential_id : null;
  editingAuthType.value = auth?.type ?? null;
  editingKeyId.value = privateKeyConfig?.key_id ?? null;
  editingWebDavAuth.value = config?.webdavAuth ?? null;
  trustedHostKeyFingerprint.value = config?.hostKeyFingerprint ?? null;

  draft.id = config?.id ?? "";
  draft.name = config?.name ?? "";
  draft.protocol = config?.protocol ?? "sftp";
  draft.host = config?.host ?? "";
  draft.port = config?.port ?? 22;
  draft.username = config?.username ?? "";
  draft.password = "";
  draft.sftpAuth =
    auth?.type === "ssh_agent" ? "ssh_agent" : privateKeyConfig ? "private_key" : "password";
  draft.webdavAuth = config?.webdavAuth ?? "basic";
  draft.webdavClientCertificatePath = config?.webdavClientCertificatePath ?? "";
  draft.keySource = privateKeyConfig?.key_id ? "pasted" : "local";
  draft.keyPath = privateKeyConfig?.key_path ?? "";
  draft.privateKey = "";
  draft.sftpTotpEnabled = config?.sftpTotpRequired ?? false;
  draft.sftpTotpCode = "";
  draft.ftpTls = config?.ftpTls ?? false;
  draft.remotePath = config?.remotePath ?? "/";
  draft.mountPoint = config?.mountPoint ?? store.nextFreeMountPoint();
  draft.autoMount = config?.autoMount ?? false;
  draft.ignoreSystemProxy = config?.ignoreSystemProxy ?? false;
  notice.value = null;
  dialogEl.value?.showModal();
  await nextTick();
  nameInput.value?.focus();
}

function close(): void {
  dialogEl.value?.close();
}

function onDialogClose(): void {
  void closeRemoteBrowser();
  editing.value = null;
  editingCredentialId.value = null;
  editingAuthType.value = null;
  editingKeyId.value = null;
  editingWebDavAuth.value = null;
  trustedHostKeyFingerprint.value = null;
  draft.password = "";
  draft.privateKey = "";
  draft.sftpTotpCode = "";
  remoteBrowserOpen.value = false;
  remoteBrowserBusy.value = false;
  remoteDirectories.value = [];
  notice.value = null;
}

defineExpose({ open });
</script>

<template>
  <dialog
    ref="dialog"
    class="app-dialog"
    aria-labelledby="mapping-dialog-title"
    @close="onDialogClose"
  >
    <form ref="formEl" :aria-busy="saving || testing" @submit.prevent="save">
      <div class="dialog-heading">
        <div>
          <p class="eyebrow">Mapping</p>
          <h2 id="mapping-dialog-title">
            {{ t(editing ? "form.editTitle" : "form.addTitle") }}
          </h2>
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
        <label class="full-width">
          <span>{{ t("form.name") }}</span>
          <input
            ref="nameInput"
            v-model="draft.name"
            required
            maxlength="64"
            :placeholder="t('form.namePlaceholder')"
          />
        </label>
        <label>
          <span>{{ t("form.protocol") }}</span>
          <select v-model="draft.protocol" @change="onProtocolChange">
            <option value="sftp">SFTP (SSH)</option>
            <option value="ftp">FTP</option>
            <option value="webdav">WebDAV (HTTPS)</option>
          </select>
        </label>
        <label>
          <span>{{ t("form.port") }}</span>
          <input v-model.number="draft.port" type="number" min="1" max="65535" required />
        </label>
        <label class="full-width">
          <span>{{ t("form.server") }}</span>
          <input v-model="draft.host" required placeholder="files.example.com" />
        </label>
        <label v-if="usernameVisible">
          <span>{{ t("form.username") }}</span>
          <input
            v-model="draft.username"
            autocomplete="username"
            placeholder="user"
            :required="draft.protocol === 'webdav'"
          />
        </label>
        <label v-if="draft.protocol === 'sftp'">
          <span>{{ t("form.authMethod") }}</span>
          <select v-model="draft.sftpAuth">
            <option value="password">{{ t("form.password") }}</option>
            <option value="private_key">{{ t("form.privateKey") }}</option>
            <option value="ssh_agent">SSH Agent</option>
          </select>
        </label>
        <label v-if="draft.protocol === 'webdav'">
          <span>{{ t("form.authMethod") }}</span>
          <select v-model="draft.webdavAuth">
            <option value="basic">Basic</option>
            <option value="digest">Digest</option>
            <option value="bearer">Bearer Token</option>
            <option value="client_certificate">{{ t("form.clientCertificate") }}</option>
            <option value="anonymous">{{ t("form.anonymous") }}</option>
          </select>
        </label>
        <label v-if="draft.protocol === 'sftp'" class="full-width">
          <span>{{ t("form.hostKey") }}</span>
          <div class="input-action">
            <input
              :value="trustedHostKeyFingerprint ?? ''"
              readonly
              :placeholder="t('form.hostKeyPlaceholder')"
            />
            <button class="secondary" type="button" @click="importKnownHosts">
              {{ t("form.importKnownHosts") }}
            </button>
          </div>
        </label>
        <label v-if="credentialInputVisible">
          <span>{{ credentialLabel }}</span>
          <input
            v-model="draft.password"
            type="password"
            autocomplete="new-password"
            :placeholder="passwordPlaceholder"
          />
        </label>
        <label v-if="privateKeyAuth">
          <span>{{ t("form.keySource") }}</span>
          <select v-model="draft.keySource">
            <option value="local">{{ t("form.localFile") }}</option>
            <option value="pasted">{{ t("form.pasteSecurely") }}</option>
          </select>
        </label>
        <label v-if="privateKeyAuth && draft.keySource === 'local'" class="full-width">
          <span>{{ t("form.keyFile") }}</span>
          <div class="input-action">
            <input v-model="draft.keyPath" readonly :placeholder="t('form.keyFilePlaceholder')" />
            <button class="secondary" type="button" @click="chooseKey">
              {{ t("form.choose") }}
            </button>
          </div>
        </label>
        <label v-if="privateKeyAuth && draft.keySource === 'pasted'" class="full-width">
          <span>{{ t("form.privateKey") }}</span>
          <textarea
            v-model="draft.privateKey"
            rows="6"
            spellcheck="false"
            placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
          ></textarea>
        </label>
        <label v-if="draft.protocol === 'sftp'" class="checkbox-row full-width">
          <input v-model="draft.sftpTotpEnabled" type="checkbox" />
          <span>{{ t("mapping.mfaRequired") }}</span>
        </label>
        <label v-if="totpActive" class="full-width">
          <span>{{ t("form.totp") }}</span>
          <input
            v-model="draft.sftpTotpCode"
            inputmode="numeric"
            autocomplete="one-time-code"
            maxlength="6"
            :placeholder="t('form.totpPlaceholder')"
          />
        </label>
        <label v-if="draft.protocol === 'ftp'" class="checkbox-row full-width">
          <input v-model="draft.ftpTls" type="checkbox" />
          <span>{{ t("form.explicitTls") }}</span>
        </label>
        <label>
          <span>{{ t("form.remotePath") }}</span>
          <div class="input-action">
            <input v-model="draft.remotePath" required />
            <button
              class="secondary"
              type="button"
              :disabled="remoteBrowserBusy"
              @click="openRemoteBrowser"
            >
              {{ t("form.browse") }}
            </button>
          </div>
        </label>
        <label v-if="webdavClientCertificateAuth" class="full-width">
          <span>{{ t("form.clientCertificate") }}</span>
          <div class="input-action">
            <input
              v-model="draft.webdavClientCertificatePath"
              readonly
              required
              :placeholder="t('form.certificatePlaceholder')"
            />
            <button class="secondary" type="button" @click="chooseWebDavClientCertificate">
              {{ t("form.choose") }}
            </button>
          </div>
        </label>
        <label>
          <span>{{ t("form.mountPoint") }}</span>
          <div class="input-action">
            <input
              ref="mountPointInput"
              v-model="draft.mountPoint"
              required
              :class="{ 'input-error': mountPointError }"
              :aria-invalid="mountPointError !== null"
              :aria-describedby="mountPointError ? 'mount-point-error' : undefined"
              :placeholder="store.platformInfo.os === 'windows' ? t('form.windowsMountPlaceholder') : store.platformInfo.defaultMountPoint"
            />
            <button class="secondary" type="button" @click="chooseMountPoint">
              {{ t("form.choose") }}
            </button>
          </div>
          <span v-if="mountPointError" id="mount-point-error" class="field-error" role="alert">
            {{ mountPointError }}
          </span>
        </label>
        <label class="checkbox-row full-width">
          <input v-model="draft.autoMount" type="checkbox" :disabled="totpActive" />
          <span>{{ t("mapping.autoMount") }}</span>
        </label>
        <label class="checkbox-row full-width">
          <input v-model="draft.ignoreSystemProxy" type="checkbox" />
          <span>{{ t("form.ignoreProxy") }}</span>
        </label>
        <section
          v-if="remoteBrowserOpen"
          class="remote-browser full-width"
          :aria-label="t('form.remoteBrowser')"
        >
          <header class="remote-browser-header">
            <button
              class="icon-button"
              type="button"
              :title="t('form.parentDirectory')"
              :aria-label="t('form.parentDirectory')"
              :disabled="remoteBrowserPath === remoteBrowserRoot || remoteBrowserBusy"
              @click="loadRemoteDirectories(parentRemotePath(remoteBrowserPath))"
            >
              ↑
            </button>
            <code>{{ remoteBrowserPath }}</code>
            <button
              class="icon-button"
              type="button"
              :title="t('form.closeBrowser')"
              :aria-label="t('form.closeBrowser')"
              @click="closeRemoteBrowser"
            >
              ×
            </button>
          </header>
          <div class="remote-browser-list" :aria-busy="remoteBrowserBusy">
            <button
              v-for="directory in remoteDirectories"
              :key="directory.path"
              class="remote-directory"
              type="button"
              :disabled="remoteBrowserBusy"
              @click="loadRemoteDirectories(directory.path)"
            >
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M2 5.5A1.5 1.5 0 0 1 3.5 4h3l1.5 2h4.5A1.5 1.5 0 0 1 14 7.5v4a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5v-6Z" />
              </svg>
              <span>{{ directory.name }}</span>
              <span aria-hidden="true">›</span>
            </button>
            <p v-if="remoteDirectories.length === 0" class="remote-browser-empty">
              {{ t("form.noSubdirectories") }}
            </p>
          </div>
          <footer class="remote-browser-footer">
            <button class="primary compact" type="button" @click="selectRemotePath">
              {{ t("form.selectCurrent") }}
            </button>
          </footer>
        </section>
      </div>

      <div
        v-if="notice"
        class="notice dialog-notice"
        :data-kind="notice.kind"
        :role="notice.kind === 'error' ? 'alert' : 'status'"
        aria-live="polite"
      >
        {{ notice.message }}
      </div>

      <div class="dialog-actions">
        <button class="secondary" type="button" @click="close">{{ t("dialog.cancel") }}</button>
        <button class="secondary" type="button" :disabled="testing" @click="testConnection">
          {{
            testing
              ? t("form.testing")
              : t("form.testConnection", { protocol: draft.protocol.toUpperCase() })
          }}
        </button>
        <button class="primary" type="submit" :disabled="saving">
          {{ saving ? t("form.saving") : t("form.save") }}
        </button>
      </div>
    </form>
  </dialog>
</template>
