<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, reactive, ref, watch } from "vue";
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
    return `盘符 ${match[1].toUpperCase()}: 已被占用，请选择其他盘符`;
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
  if (privateKeyAuth.value) return "私钥口令";
  if (draft.protocol === "webdav" && draft.webdavAuth === "bearer") return "Bearer Token";
  return "密码";
});
const passwordPlaceholder = computed(() =>
  privateKeyAuth.value
    ? "可选；留空则保留已保存口令"
    : draft.protocol === "webdav" && draft.webdavAuth === "bearer"
      ? "留空则保留已保存 Token"
      : "留空则保留已保存密码",
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
        ? `SSH 服务器主机密钥已变化。\n\n原指纹：${trustedHostKeyFingerprint.value}\n新指纹：${fingerprint}\n\n只有确认服务器已更换密钥时才继续。`
        : `确认 SSH 服务器主机密钥指纹：\n\n${fingerprint}`,
    );
    if (!accepted) throw new Error("未信任 SSH 服务器主机密钥");
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

const MFA_DETECTED_MESSAGE =
  "检测到服务器要求二次验证（MFA），已自动勾选“需要 MFA”并禁用自动挂载，请输入当前 6 位 TOTP 验证码";

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
      throw new Error(`${MFA_DETECTED_MESSAGE}后重新保存`);
    }
    await invoke("save_mapping", {
      config,
      password: draft.password || null,
      privateKey: draft.privateKey.trim() || null,
    });
    close();
    await store.loadMappings();
    store.setNotice(
      draft.password || draft.privateKey.trim() ? "配置和认证信息已保存" : "配置已保存",
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
      throw new Error("测试 MFA 连接时请输入当前 6 位 TOTP 验证码");
    }
    await verifySftpHostKey(config);
    if (await probeMfaRequirement(config)) {
      notice.value = { message: `${MFA_DETECTED_MESSAGE}后重新测试`, kind: "success" };
      return;
    }
    await invoke("test_remote_connection", {
      config,
      password: draft.password || null,
      privateKey: draft.privateKey.trim() || null,
      totpCode,
    });
    notice.value = { message: `${config.protocol.toUpperCase()} 连接成功`, kind: "success" };
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
      title: "选择 PEM 客户端证书和私钥",
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
      title: "选择 OpenSSH known_hosts 文件",
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
      throw new Error("known_hosts 中没有当前服务器提供的主机密钥");
    }
    trustedHostKeyFingerprint.value = liveFingerprint;
    notice.value = { message: "已从 known_hosts 验证并导入主机密钥", kind: "success" };
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
      throw new Error(`${MFA_DETECTED_MESSAGE}后重新浏览`);
    }
    if (config.sftpTotpRequired && !totpCode) {
      throw new Error("浏览 MFA SFTP 目录时请输入当前 6 位 TOTP 验证码");
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
          <h2 id="mapping-dialog-title">{{ editing ? "编辑映射" : "添加映射" }}</h2>
        </div>
        <button class="icon-button" type="button" aria-label="关闭" title="关闭" @click="close">
          ×
        </button>
      </div>

      <div class="form-grid">
        <label class="full-width">
          <span>名称</span>
          <input
            ref="nameInput"
            v-model="draft.name"
            required
            maxlength="64"
            placeholder="工作文件"
          />
        </label>
        <label>
          <span>协议</span>
          <select v-model="draft.protocol" @change="onProtocolChange">
            <option value="sftp">SFTP (SSH)</option>
            <option value="ftp">FTP</option>
            <option value="webdav">WebDAV (HTTPS)</option>
          </select>
        </label>
        <label>
          <span>端口</span>
          <input v-model.number="draft.port" type="number" min="1" max="65535" required />
        </label>
        <label class="full-width">
          <span>服务器</span>
          <input v-model="draft.host" required placeholder="files.example.com" />
        </label>
        <label v-if="usernameVisible">
          <span>用户名</span>
          <input
            v-model="draft.username"
            autocomplete="username"
            placeholder="user"
            :required="draft.protocol === 'webdav'"
          />
        </label>
        <label v-if="draft.protocol === 'sftp'">
          <span>认证方式</span>
          <select v-model="draft.sftpAuth">
            <option value="password">密码</option>
            <option value="private_key">SSH 私钥</option>
            <option value="ssh_agent">SSH Agent</option>
          </select>
        </label>
        <label v-if="draft.protocol === 'webdav'">
          <span>认证方式</span>
          <select v-model="draft.webdavAuth">
            <option value="basic">Basic</option>
            <option value="digest">Digest</option>
            <option value="bearer">Bearer Token</option>
            <option value="client_certificate">客户端证书</option>
            <option value="anonymous">匿名</option>
          </select>
        </label>
        <label v-if="draft.protocol === 'sftp'" class="full-width">
          <span>SSH 主机密钥</span>
          <div class="input-action">
            <input
              :value="trustedHostKeyFingerprint ?? ''"
              readonly
              placeholder="首次连接时确认，或从 known_hosts 导入"
            />
            <button class="secondary" type="button" @click="importKnownHosts">
              导入 known_hosts
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
          <span>私钥来源</span>
          <select v-model="draft.keySource">
            <option value="local">本地文件</option>
            <option value="pasted">粘贴并安全保存</option>
          </select>
        </label>
        <label v-if="privateKeyAuth && draft.keySource === 'local'" class="full-width">
          <span>私钥文件</span>
          <div class="input-action">
            <input v-model="draft.keyPath" readonly placeholder="选择 OpenSSH/PEM 私钥" />
            <button class="secondary" type="button" @click="chooseKey">选择</button>
          </div>
        </label>
        <label v-if="privateKeyAuth && draft.keySource === 'pasted'" class="full-width">
          <span>SSH 私钥</span>
          <textarea
            v-model="draft.privateKey"
            rows="6"
            spellcheck="false"
            placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
          ></textarea>
        </label>
        <label v-if="draft.protocol === 'sftp'" class="checkbox-row full-width">
          <input v-model="draft.sftpTotpEnabled" type="checkbox" />
          <span>需要 MFA</span>
        </label>
        <label v-if="totpActive" class="full-width">
          <span>当前 TOTP 验证码</span>
          <input
            v-model="draft.sftpTotpCode"
            inputmode="numeric"
            autocomplete="one-time-code"
            maxlength="6"
            placeholder="测试连接时输入 6 位验证码"
          />
        </label>
        <label v-if="draft.protocol === 'ftp'" class="checkbox-row full-width">
          <input v-model="draft.ftpTls" type="checkbox" />
          <span>使用显式 TLS (FTPS)</span>
        </label>
        <label>
          <span>远程路径</span>
          <div class="input-action">
            <input v-model="draft.remotePath" required />
            <button
              class="secondary"
              type="button"
              :disabled="remoteBrowserBusy"
              @click="openRemoteBrowser"
            >
              浏览
            </button>
          </div>
        </label>
        <label v-if="webdavClientCertificateAuth" class="full-width">
          <span>客户端证书</span>
          <div class="input-action">
            <input
              v-model="draft.webdavClientCertificatePath"
              readonly
              required
              placeholder="包含证书链和未加密私钥的 PEM 文件"
            />
            <button class="secondary" type="button" @click="chooseWebDavClientCertificate">
              选择
            </button>
          </div>
        </label>
        <label>
          <span>本地挂载点</span>
          <div class="input-action">
            <input
              ref="mountPointInput"
              v-model="draft.mountPoint"
              required
              :class="{ 'input-error': mountPointError }"
              :aria-invalid="mountPointError !== null"
              :aria-describedby="mountPointError ? 'mount-point-error' : undefined"
              :placeholder="store.platformInfo.os === 'windows' ? 'Z: 或 C:\\Mounts\\GugleFS' : store.platformInfo.defaultMountPoint"
            />
            <button class="secondary" type="button" @click="chooseMountPoint">选择</button>
          </div>
          <span v-if="mountPointError" id="mount-point-error" class="field-error" role="alert">
            {{ mountPointError }}
          </span>
        </label>
        <label class="checkbox-row full-width">
          <input v-model="draft.autoMount" type="checkbox" :disabled="totpActive" />
          <span>解锁后自动挂载</span>
        </label>
        <label class="checkbox-row full-width">
          <input v-model="draft.ignoreSystemProxy" type="checkbox" />
          <span>忽略系统代理</span>
        </label>
        <section
          v-if="remoteBrowserOpen"
          class="remote-browser full-width"
          aria-label="远程目录选择器"
        >
          <header class="remote-browser-header">
            <button
              class="icon-button"
              type="button"
              title="上一级"
              aria-label="上一级"
              :disabled="remoteBrowserPath === remoteBrowserRoot || remoteBrowserBusy"
              @click="loadRemoteDirectories(parentRemotePath(remoteBrowserPath))"
            >
              ↑
            </button>
            <code>{{ remoteBrowserPath }}</code>
            <button
              class="icon-button"
              type="button"
              title="关闭目录选择器"
              aria-label="关闭目录选择器"
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
              此目录没有子目录
            </p>
          </div>
          <footer class="remote-browser-footer">
            <button class="primary compact" type="button" @click="selectRemotePath">
              选择当前目录
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
        <button class="secondary" type="button" @click="close">取消</button>
        <button class="secondary" type="button" :disabled="testing" @click="testConnection">
          {{ testing ? "测试中…" : `测试 ${draft.protocol.toUpperCase()} 连接` }}
        </button>
        <button class="primary" type="submit" :disabled="saving">
          {{ saving ? "保存中…" : "保存配置" }}
        </button>
      </div>
    </form>
  </dialog>
</template>
