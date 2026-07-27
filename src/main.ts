import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type Protocol = "ftp" | "sftp" | "webdav";
type MappingState = "unmounted" | "mounting" | "mounted" | "error";

interface MappingConfig {
  id: string;
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  username: string | null;
  auth: { type: "password"; credential_id: string | null };
  remotePath: string;
  mountPoint: string;
  ftpTls: boolean;
  autoMount: boolean;
}

interface MappingRuntime {
  config: MappingConfig;
  state: MappingState;
  lastError: string | null;
}

interface AuthStatus {
  configured: boolean;
  unlocked: boolean;
}

interface TotpSetup {
  secret: string;
  qrCode: string;
}

interface StartupMountResult {
  mappings: MappingRuntime[];
  attempted: number;
}

const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {
  throw new Error("Missing #app element");
}

app.innerHTML = `
  <div id="auth-screen" class="auth-screen">
    <header class="auth-brand">
      <p class="eyebrow">SECURE ACCESS</p>
      <h1>GugleFS</h1>
    </header>
    <main class="auth-main">
      <section class="auth-panel" aria-labelledby="auth-title">
        <div class="auth-heading">
          <p class="eyebrow">TWO-FACTOR AUTHENTICATION</p>
          <h2 id="auth-title">验证身份</h2>
        </div>
        <div id="auth-notice" class="notice auth-notice" role="status" hidden></div>
        <form id="unlock-form" class="auth-form" hidden>
          <label>
            <span>验证码</span>
            <input id="unlock-code" class="code-input" inputmode="numeric" autocomplete="one-time-code" maxlength="6" pattern="[0-9]{6}" required />
          </label>
          <button id="unlock-submit" class="primary" type="submit">解锁</button>
        </form>
        <div id="setup-view" hidden>
          <div class="setup-layout">
            <img id="totp-qr" class="totp-qr" alt="GugleFS 2FA 二维码" />
            <div class="setup-fields">
              <label>
                <span>密钥</span>
                <div class="input-action">
                  <input id="totp-secret" class="secret-input" readonly />
                  <button id="copy-secret" class="secondary" type="button">复制</button>
                </div>
              </label>
              <form id="setup-form" class="auth-form setup-form">
                <label>
                  <span>验证码</span>
                  <input id="setup-code" class="code-input" inputmode="numeric" autocomplete="one-time-code" maxlength="6" pattern="[0-9]{6}" required />
                </label>
                <button id="setup-submit" class="primary" type="submit">启用 2FA</button>
              </form>
            </div>
          </div>
        </div>
      </section>
    </main>
  </div>

  <div id="workspace" hidden>
    <header class="app-header">
      <div>
        <p class="eyebrow">REMOTE FILESYSTEM</p>
        <h1>GugleFS</h1>
      </div>
      <div class="header-actions">
        <button id="lock-app" class="header-button" type="button">锁定</button>
        <button id="new-mapping" class="primary" type="button">添加映射</button>
      </div>
    </header>
    <main>
      <section class="mapping-section" aria-labelledby="mapping-title">
        <div class="section-heading">
          <div>
            <h2 id="mapping-title">磁盘映射</h2>
            <p id="mapping-count">0 个配置</p>
          </div>
          <button id="refresh" class="icon-button" type="button" title="刷新" aria-label="刷新">↻</button>
        </div>
        <div id="notice" class="notice" role="status" hidden></div>
        <div id="mapping-list" class="mapping-list"></div>
        <div id="empty-state" class="empty-state">
          <p>还没有远程磁盘配置</p>
          <button id="empty-add" class="secondary" type="button">创建第一个映射</button>
        </div>
      </section>
    </main>
    <dialog id="mapping-dialog">
      <form id="mapping-form">
        <div class="dialog-heading">
          <div>
            <p class="eyebrow">MAPPING</p>
            <h2 id="dialog-title">添加映射</h2>
          </div>
          <button id="close-dialog" class="icon-button" type="button" aria-label="关闭" title="关闭">×</button>
        </div>
        <input id="mapping-id" type="hidden" />
        <div class="form-grid">
          <label class="full-width">
            <span>名称</span>
            <input id="name" name="name" required maxlength="64" placeholder="工作文件" />
          </label>
          <label>
            <span>协议</span>
            <select id="protocol" name="protocol">
              <option value="sftp">SFTP (SSH)</option>
              <option value="ftp">FTP</option>
              <option value="webdav">WebDAV (HTTPS)</option>
            </select>
          </label>
          <label>
            <span>端口</span>
            <input id="port" name="port" type="number" min="1" max="65535" required value="22" />
          </label>
          <label class="full-width">
            <span>服务器</span>
            <input id="host" name="host" required placeholder="files.example.com" />
          </label>
          <label>
            <span>用户名</span>
            <input id="username" name="username" autocomplete="username" placeholder="user" />
          </label>
          <label>
            <span>密码</span>
            <input id="password" name="password" type="password" autocomplete="new-password" placeholder="留空则保留已保存密码" />
          </label>
          <label id="ftp-tls-field" class="checkbox-row full-width protocol-field" hidden>
            <input id="ftp-tls" name="ftpTls" type="checkbox" />
            <span>使用显式 TLS (FTPS)</span>
          </label>
          <label>
            <span>远程路径</span>
            <input id="remote-path" name="remotePath" required value="/" />
          </label>
          <label>
            <span>本地挂载点</span>
            <input id="mount-point" name="mountPoint" required value="Z:" placeholder="Z: 或 /mnt/guglefs" />
          </label>
          <label class="checkbox-row full-width">
            <input id="auto-mount" name="autoMount" type="checkbox" />
            <span>解锁后自动挂载</span>
          </label>
        </div>
        <div id="dialog-notice" class="notice dialog-notice" role="status" hidden></div>
        <div class="dialog-actions">
          <button id="cancel-dialog" class="secondary" type="button">取消</button>
          <button id="test-connection" class="secondary" type="button">测试连接</button>
          <button class="primary" type="submit">保存配置</button>
        </div>
      </form>
    </dialog>
    <dialog id="mount-dialog">
      <form id="mount-form">
        <div class="dialog-heading">
          <div>
            <p class="eyebrow">MOUNT</p>
            <h2>挂载映射</h2>
            <p id="mount-target" class="dialog-subtitle"></p>
          </div>
          <button id="close-mount-dialog" class="icon-button" type="button" aria-label="关闭" title="关闭">×</button>
        </div>
        <div class="form-grid">
          <label class="full-width">
            <span>密码</span>
            <input id="mount-password" type="password" autocomplete="current-password" required />
          </label>
          <label class="checkbox-row full-width">
            <input id="remember-password" type="checkbox" checked />
            <span>保存到 Windows 凭据管理器</span>
          </label>
        </div>
        <div id="mount-notice" class="notice dialog-notice" role="status" hidden></div>
        <div class="dialog-actions">
          <button id="cancel-mount-dialog" class="secondary" type="button">取消</button>
          <button id="confirm-mount" class="primary" type="submit">挂载</button>
        </div>
      </form>
    </dialog>
  </div>
`;

const authScreen = getElement<HTMLDivElement>("auth-screen");
const workspace = getElement<HTMLDivElement>("workspace");
const authNotice = getElement<HTMLDivElement>("auth-notice");
const unlockForm = getElement<HTMLFormElement>("unlock-form");
const setupView = getElement<HTMLDivElement>("setup-view");
const dialog = getElement<HTMLDialogElement>("mapping-dialog");
const form = getElement<HTMLFormElement>("mapping-form");
const list = getElement<HTMLDivElement>("mapping-list");
const emptyState = getElement<HTMLDivElement>("empty-state");
const notice = getElement<HTMLDivElement>("notice");
const dialogNotice = getElement<HTMLDivElement>("dialog-notice");
const testConnectionButton = getElement<HTMLButtonElement>("test-connection");
const lockButton = getElement<HTMLButtonElement>("lock-app");
const mountDialog = getElement<HTMLDialogElement>("mount-dialog");
const mountForm = getElement<HTMLFormElement>("mount-form");
const mountNotice = getElement<HTMLDivElement>("mount-notice");
let mountTargetId: string | null = null;
let editingCredentialId: string | null = null;
let mappings: MappingRuntime[] = [];

function getElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing #${id} element`);
  return element as T;
}

function setNotice(element: HTMLElement, message: string, kind: "error" | "success" = "error"): void {
  element.textContent = message;
  element.dataset.kind = kind;
  element.hidden = false;
}

function clearElementNotice(element: HTMLElement): void {
  element.hidden = true;
  element.textContent = "";
}

function showNotice(message: string, kind: "error" | "success" = "error"): void {
  setNotice(notice, message, kind);
}

function clearNotice(): void {
  clearElementNotice(notice);
}

function showDialogNotice(message: string, kind: "error" | "success" = "error"): void {
  setNotice(dialogNotice, message, kind);
}

function showMountNotice(message: string): void {
  setNotice(mountNotice, message);
}

function showAuthNotice(message: string, kind: "error" | "success" = "error"): void {
  setNotice(authNotice, message, kind);
}

function showUnlock(): void {
  workspace.hidden = true;
  authScreen.hidden = false;
  setupView.hidden = true;
  unlockForm.hidden = false;
  clearElementNotice(authNotice);
  const input = getElement<HTMLInputElement>("unlock-code");
  input.value = "";
  input.focus();
}

async function showSetup(): Promise<void> {
  workspace.hidden = true;
  authScreen.hidden = false;
  unlockForm.hidden = true;
  setupView.hidden = false;
  clearElementNotice(authNotice);
  const setup = await invoke<TotpSetup>("begin_2fa_setup");
  getElement<HTMLImageElement>("totp-qr").src = setup.qrCode;
  getElement<HTMLInputElement>("totp-secret").value = setup.secret;
  getElement<HTMLInputElement>("setup-code").value = "";
  getElement<HTMLInputElement>("setup-code").focus();
}

async function enterWorkspace(): Promise<void> {
  authScreen.hidden = true;
  workspace.hidden = false;
  await loadMappings();
  showNotice("正在恢复挂载状态...");
  try {
    const result = await invoke<StartupMountResult>("restore_startup_mappings");
    mappings = result.mappings;
    if (result.attempted === 0) {
      clearNotice();
      return;
    }
    renderMappings();
    const failed = mappings.filter((runtime) => runtime.state === "error");
    if (failed.length > 0) {
      showNotice(`${failed.length} 个映射恢复失败`);
    } else {
      showNotice(`已恢复 ${result.attempted} 个映射`, "success");
    }
  } catch (error) {
    showNotice(String(error));
  }
}

async function initializeAuth(): Promise<void> {
  try {
    const status = await invoke<AuthStatus>("get_auth_status");
    if (!status.configured) {
      await showSetup();
    } else if (!status.unlocked) {
      showUnlock();
    } else {
      await enterWorkspace();
    }
  } catch (error) {
    showAuthNotice(String(error));
  }
}

function clearMountDialog(): void {
  mountTargetId = null;
  getElement<HTMLInputElement>("mount-password").value = "";
  getElement<HTMLInputElement>("remember-password").checked = true;
  clearElementNotice(mountNotice);
}

function updateProtocolControls(): void {
  const protocol = getElement<HTMLSelectElement>("protocol").value as Protocol;
  getElement<HTMLInputElement>("password").disabled = protocol === "sftp";
  getElement<HTMLElement>("ftp-tls-field").hidden = protocol !== "ftp";
  testConnectionButton.textContent = `测试 ${protocol.toUpperCase()} 连接`;
}

function openForm(runtime?: MappingRuntime): void {
  form.reset();
  clearElementNotice(dialogNotice);
  editingCredentialId = runtime?.config.auth.credential_id ?? null;
  getElement<HTMLInputElement>("mapping-id").value = runtime?.config.id ?? "";
  getElement<HTMLHeadingElement>("dialog-title").textContent = runtime ? "编辑映射" : "添加映射";
  getElement<HTMLInputElement>("name").value = runtime?.config.name ?? "";
  getElement<HTMLSelectElement>("protocol").value = runtime?.config.protocol ?? "sftp";
  getElement<HTMLInputElement>("port").value = String(runtime?.config.port ?? 22);
  getElement<HTMLInputElement>("host").value = runtime?.config.host ?? "";
  getElement<HTMLInputElement>("username").value = runtime?.config.username ?? "";
  getElement<HTMLInputElement>("remote-path").value = runtime?.config.remotePath ?? "/";
  getElement<HTMLInputElement>("mount-point").value = runtime?.config.mountPoint ?? "Z:";
  getElement<HTMLInputElement>("ftp-tls").checked = runtime?.config.ftpTls ?? false;
  getElement<HTMLInputElement>("auto-mount").checked = runtime?.config.autoMount ?? false;
  updateProtocolControls();
  dialog.showModal();
}

function openMountDialog(runtime: MappingRuntime): void {
  mountTargetId = runtime.config.id;
  getElement<HTMLParagraphElement>("mount-target").textContent = `${runtime.config.name} → ${runtime.config.mountPoint}`;
  getElement<HTMLInputElement>("mount-password").value = "";
  getElement<HTMLInputElement>("remember-password").checked = true;
  clearElementNotice(mountNotice);
  if (!mountDialog.open) mountDialog.showModal();
  getElement<HTMLInputElement>("mount-password").focus();
}

function statusLabel(state: MappingState): string {
  return {
    unmounted: "未挂载",
    mounting: "挂载中",
    mounted: "已挂载",
    error: "异常",
  }[state];
}

function readMappingConfig(): MappingConfig {
  const protocol = getElement<HTMLSelectElement>("protocol").value as Protocol;
  return {
    id: getElement<HTMLInputElement>("mapping-id").value || crypto.randomUUID(),
    name: getElement<HTMLInputElement>("name").value.trim(),
    protocol,
    host: getElement<HTMLInputElement>("host").value.trim(),
    port: getElement<HTMLInputElement>("port").valueAsNumber,
    username: getElement<HTMLInputElement>("username").value.trim() || null,
    auth: { type: "password", credential_id: editingCredentialId },
    remotePath: getElement<HTMLInputElement>("remote-path").value.trim(),
    mountPoint: getElement<HTMLInputElement>("mount-point").value.trim(),
    ftpTls: protocol === "ftp" && getElement<HTMLInputElement>("ftp-tls").checked,
    autoMount: getElement<HTMLInputElement>("auto-mount").checked,
  };
}

function renderMappings(): void {
  list.replaceChildren();
  emptyState.hidden = mappings.length > 0;
  getElement<HTMLParagraphElement>("mapping-count").textContent = `${mappings.length} 个配置`;

  for (const runtime of mappings) {
    const { config } = runtime;
    const item = document.createElement("article");
    item.className = "mapping-item";

    const identity = document.createElement("div");
    identity.className = "mapping-identity";
    const badge = document.createElement("span");
    badge.className = "protocol-badge";
    badge.textContent = config.protocol.toUpperCase();
    const text = document.createElement("div");
    const title = document.createElement("h3");
    title.textContent = config.name;
    const endpoint = document.createElement("p");
    endpoint.textContent = `${config.username ? `${config.username}@` : ""}${config.host}:${config.port}${config.remotePath}`;
    text.append(title, endpoint);
    identity.append(badge, text);

    const destination = document.createElement("div");
    destination.className = "destination";
    const mountPoint = document.createElement("strong");
    mountPoint.textContent = config.mountPoint;
    const status = document.createElement("span");
    status.className = `status status-${runtime.state}`;
    status.textContent = statusLabel(runtime.state);
    if (runtime.lastError) status.title = runtime.lastError;
    const credential = document.createElement("span");
    credential.className = `credential-state ${config.auth.credential_id ? "credential-stored" : ""}`;
    credential.textContent = config.auth.credential_id ? "密码已保存" : "未保存密码";
    destination.append(mountPoint, status, credential);

    const actions = document.createElement("div");
    actions.className = "item-actions";
    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "secondary compact";
    edit.textContent = "编辑";
    edit.disabled = runtime.state === "mounting" || runtime.state === "mounted";
    edit.addEventListener("click", () => openForm(runtime));
    const mount = document.createElement("button");
    mount.type = "button";
    mount.className = runtime.state === "mounted" ? "danger compact" : "primary compact";
    mount.textContent = runtime.state === "mounted" ? "卸载" : runtime.state === "mounting" ? "挂载中..." : "挂载";
    mount.disabled = runtime.state === "mounting";
    mount.addEventListener("click", () => {
      if (runtime.state === "mounted") {
        void unmountMapping(config.id);
      } else if (config.auth.credential_id) {
        void mountMapping(config.id, null, false);
      } else {
        openMountDialog(runtime);
      }
    });
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger compact";
    remove.textContent = "删除";
    remove.addEventListener("click", () => void deleteMapping(config.id));
    remove.disabled = runtime.state !== "unmounted";
    actions.append(mount, edit, remove);

    item.append(identity, destination, actions);
    list.append(item);
  }
}

async function loadMappings(): Promise<void> {
  clearNotice();
  try {
    mappings = await invoke<MappingRuntime[]>("list_mappings");
    renderMappings();
  } catch (error) {
    showNotice(String(error));
  }
}

async function deleteMapping(id: string): Promise<void> {
  try {
    await invoke("delete_mapping", { id });
    mappings = mappings.filter((item) => item.config.id !== id);
    renderMappings();
    showNotice("配置和凭据已删除", "success");
  } catch (error) {
    showNotice(String(error));
  }
}

function updateMapping(runtime: MappingRuntime): void {
  const index = mappings.findIndex((item) => item.config.id === runtime.config.id);
  if (index >= 0) mappings[index] = runtime;
  renderMappings();
}

async function mountMapping(
  id: string,
  password: string | null,
  remember: boolean,
): Promise<void> {
  const current = mappings.find((item) => item.config.id === id);
  if (!current) return;
  updateMapping({ ...current, state: "mounting", lastError: null });
  try {
    const runtime = await invoke<MappingRuntime>("mount_mapping", { id, password, remember });
    updateMapping(runtime);
    if (mountDialog.open) mountDialog.close();
    showNotice("映射已挂载", "success");
  } catch (error) {
    await loadMappings();
    const refreshed = mappings.find((item) => item.config.id === id) ?? current;
    if (!mountDialog.open) openMountDialog(refreshed);
    showMountNotice(String(error));
  }
}

async function unmountMapping(id: string): Promise<void> {
  try {
    const runtime = await invoke<MappingRuntime>("unmount_mapping", { id });
    updateMapping(runtime);
    showNotice("映射已卸载", "success");
  } catch (error) {
    await loadMappings();
    showNotice(String(error));
  }
}

function normalizeCodeInput(input: HTMLInputElement): void {
  input.value = input.value.replace(/\D/g, "").slice(0, 6);
}

unlockForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const input = getElement<HTMLInputElement>("unlock-code");
  const submit = getElement<HTMLButtonElement>("unlock-submit");
  submit.disabled = true;
  clearElementNotice(authNotice);
  void invoke<AuthStatus>("unlock_app", { code: input.value })
    .then(() => enterWorkspace())
    .catch((error) => showAuthNotice(String(error)))
    .finally(() => {
      submit.disabled = false;
    });
});

getElement<HTMLFormElement>("setup-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const input = getElement<HTMLInputElement>("setup-code");
  const submit = getElement<HTMLButtonElement>("setup-submit");
  submit.disabled = true;
  clearElementNotice(authNotice);
  void invoke<AuthStatus>("confirm_2fa_setup", { code: input.value })
    .then(() => {
      getElement<HTMLImageElement>("totp-qr").removeAttribute("src");
      getElement<HTMLInputElement>("totp-secret").value = "";
      return enterWorkspace();
    })
    .catch((error) => showAuthNotice(String(error)))
    .finally(() => {
      submit.disabled = false;
    });
});

getElement<HTMLButtonElement>("copy-secret").addEventListener("click", () => {
  const input = getElement<HTMLInputElement>("totp-secret");
  void navigator.clipboard
    .writeText(input.value)
    .then(() => showAuthNotice("密钥已复制", "success"))
    .catch(() => {
      input.select();
      document.execCommand("copy");
      showAuthNotice("密钥已复制", "success");
    });
});

for (const id of ["unlock-code", "setup-code"]) {
  getElement<HTMLInputElement>(id).addEventListener("input", (event) => {
    normalizeCodeInput(event.target as HTMLInputElement);
  });
}

lockButton.addEventListener("click", () => {
  lockButton.disabled = true;
  lockButton.textContent = "正在锁定...";
  clearNotice();
  void invoke<AuthStatus>("lock_app")
    .then(() => {
      mappings = [];
      showUnlock();
    })
    .catch(async (error) => {
      await loadMappings();
      showNotice(String(error));
    })
    .finally(() => {
      lockButton.disabled = false;
      lockButton.textContent = "锁定";
    });
});

form.addEventListener("submit", (event) => {
  event.preventDefault();
  void (async () => {
    const config = readMappingConfig();
    const password = getElement<HTMLInputElement>("password").value || null;
    try {
      await invoke<MappingRuntime>("save_mapping", { config, password });
      dialog.close();
      await loadMappings();
      showNotice(password ? "配置和密码已保存" : "配置已保存", "success");
    } catch (error) {
      showDialogNotice(String(error));
    }
  })();
});

testConnectionButton.addEventListener("click", () => {
  void (async () => {
    if (!form.reportValidity()) return;
    testConnectionButton.disabled = true;
    testConnectionButton.textContent = "测试中...";
    clearElementNotice(dialogNotice);
    try {
      const config = readMappingConfig();
      const password = getElement<HTMLInputElement>("password").value || null;
      await invoke("test_remote_connection", { config, password });
      showDialogNotice(`${config.protocol.toUpperCase()} 连接成功`, "success");
    } catch (error) {
      showDialogNotice(String(error));
    } finally {
      updateProtocolControls();
    }
  })();
});

getElement<HTMLSelectElement>("protocol").addEventListener("change", (event) => {
  const protocol = (event.target as HTMLSelectElement).value as Protocol;
  const defaultPorts: Record<Protocol, number> = {
    ftp: 21,
    sftp: 22,
    webdav: 443,
  };
  getElement<HTMLInputElement>("port").value = String(defaultPorts[protocol]);
  updateProtocolControls();
});
getElement<HTMLButtonElement>("new-mapping").addEventListener("click", () => openForm());
getElement<HTMLButtonElement>("empty-add").addEventListener("click", () => openForm());
getElement<HTMLButtonElement>("refresh").addEventListener("click", () => void loadMappings());
getElement<HTMLButtonElement>("close-dialog").addEventListener("click", () => dialog.close());
getElement<HTMLButtonElement>("cancel-dialog").addEventListener("click", () => dialog.close());
dialog.addEventListener("close", () => {
  editingCredentialId = null;
  getElement<HTMLInputElement>("password").value = "";
  clearElementNotice(dialogNotice);
});

mountForm.addEventListener("submit", (event) => {
  event.preventDefault();
  if (!mountTargetId) return;
  const id = mountTargetId;
  const password = getElement<HTMLInputElement>("mount-password").value || null;
  const remember = getElement<HTMLInputElement>("remember-password").checked;
  const submit = getElement<HTMLButtonElement>("confirm-mount");
  submit.disabled = true;
  void mountMapping(id, password, remember).finally(() => {
    submit.disabled = false;
  });
});
getElement<HTMLButtonElement>("close-mount-dialog").addEventListener("click", () => mountDialog.close());
getElement<HTMLButtonElement>("cancel-mount-dialog").addEventListener("click", () => mountDialog.close());
mountDialog.addEventListener("close", clearMountDialog);

void initializeAuth();
