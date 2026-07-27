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
  autoMount: boolean;
}

interface MappingRuntime {
  config: MappingConfig;
  state: MappingState;
  lastError: string | null;
}

const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {
  throw new Error("Missing #app element");
}

app.innerHTML = `
  <header class="app-header">
    <div>
      <p class="eyebrow">REMOTE FILESYSTEM</p>
      <h1>GugleFS</h1>
    </div>
    <button id="new-mapping" class="primary" type="button">添加映射</button>
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
          <span>凭据</span>
          <input id="password" name="password" type="password" autocomplete="current-password" placeholder="仅用于本次连接测试" />
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
          <span>应用启动时自动挂载</span>
        </label>
      </div>
      <div id="dialog-notice" class="notice dialog-notice" role="status" hidden></div>
      <div class="dialog-actions">
        <button id="cancel-dialog" class="secondary" type="button">取消</button>
        <button id="test-connection" class="secondary" type="button">测试 WebDAV 连接</button>
        <button class="primary" type="submit">保存配置</button>
      </div>
    </form>
  </dialog>
  <dialog id="mount-dialog">
    <form id="mount-form">
      <div class="dialog-heading">
        <div>
          <p class="eyebrow">MOUNT</p>
          <h2>Mount mapping</h2>
          <p id="mount-target" class="dialog-subtitle"></p>
        </div>
        <button id="close-mount-dialog" class="icon-button" type="button" aria-label="Close" title="Close">X</button>
      </div>
      <div class="form-grid">
        <label class="full-width">
          <span>Runtime password</span>
          <input id="mount-password" type="password" autocomplete="current-password" />
        </label>
      </div>
      <div id="mount-notice" class="notice dialog-notice" role="status" hidden></div>
      <div class="dialog-actions">
        <button id="cancel-mount-dialog" class="secondary" type="button">Cancel</button>
        <button id="confirm-mount" class="primary" type="submit">Mount</button>
      </div>
    </form>
  </dialog>
`;

const dialog = getElement<HTMLDialogElement>("mapping-dialog");
const form = getElement<HTMLFormElement>("mapping-form");
const list = getElement<HTMLDivElement>("mapping-list");
const emptyState = getElement<HTMLDivElement>("empty-state");
const notice = getElement<HTMLDivElement>("notice");
const dialogNotice = getElement<HTMLDivElement>("dialog-notice");
const testConnectionButton = getElement<HTMLButtonElement>("test-connection");
const mountDialog = getElement<HTMLDialogElement>("mount-dialog");
const mountForm = getElement<HTMLFormElement>("mount-form");
const mountNotice = getElement<HTMLDivElement>("mount-notice");
let mountTargetId: string | null = null;
let mappings: MappingRuntime[] = [];

function getElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing #${id} element`);
  return element as T;
}

function showNotice(message: string, kind: "error" | "success" = "error"): void {
  notice.textContent = message;
  notice.dataset.kind = kind;
  notice.hidden = false;
}

function clearNotice(): void {
  notice.hidden = true;
  notice.textContent = "";
}

function showDialogNotice(message: string, kind: "error" | "success" = "error"): void {
  dialogNotice.textContent = message;
  dialogNotice.dataset.kind = kind;
  dialogNotice.hidden = false;
}

function clearDialogNotice(): void {
  dialogNotice.hidden = true;
  dialogNotice.textContent = "";
}

function showMountNotice(message: string, kind: "error" | "success" = "error"): void {
  mountNotice.textContent = message;
  mountNotice.dataset.kind = kind;
  mountNotice.hidden = false;
}

function clearMountDialog(): void {
  mountTargetId = null;
  getElement<HTMLInputElement>("mount-password").value = "";
  mountNotice.hidden = true;
  mountNotice.textContent = "";
}

function updateProtocolControls(): void {
  const isWebDav = getElement<HTMLSelectElement>("protocol").value === "webdav";
  getElement<HTMLInputElement>("password").disabled = !isWebDav;
  testConnectionButton.disabled = !isWebDav;
}

function openForm(runtime?: MappingRuntime): void {
  form.reset();
  clearDialogNotice();
  getElement<HTMLInputElement>("mapping-id").value = runtime?.config.id ?? "";
  getElement<HTMLHeadingElement>("dialog-title").textContent = runtime ? "编辑映射" : "添加映射";
  getElement<HTMLInputElement>("name").value = runtime?.config.name ?? "";
  getElement<HTMLSelectElement>("protocol").value = runtime?.config.protocol ?? "sftp";
  getElement<HTMLInputElement>("port").value = String(runtime?.config.port ?? 22);
  getElement<HTMLInputElement>("host").value = runtime?.config.host ?? "";
  getElement<HTMLInputElement>("username").value = runtime?.config.username ?? "";
  getElement<HTMLInputElement>("remote-path").value = runtime?.config.remotePath ?? "/";
  getElement<HTMLInputElement>("mount-point").value = runtime?.config.mountPoint ?? "Z:";
  getElement<HTMLInputElement>("auto-mount").checked = runtime?.config.autoMount ?? false;
  updateProtocolControls();
  dialog.showModal();
}

function openMountDialog(runtime: MappingRuntime): void {
  mountTargetId = runtime.config.id;
  getElement<HTMLParagraphElement>("mount-target").textContent = `${runtime.config.name} -> ${runtime.config.mountPoint}`;
  getElement<HTMLInputElement>("mount-password").value = "";
  mountNotice.hidden = true;
  mountNotice.textContent = "";
  mountDialog.showModal();
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
    auth: { type: "password", credential_id: null },
    remotePath: getElement<HTMLInputElement>("remote-path").value.trim(),
    mountPoint: getElement<HTMLInputElement>("mount-point").value.trim(),
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
    destination.append(mountPoint, status);

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
    mount.textContent = runtime.state === "mounted" ? "Unmount" : runtime.state === "mounting" ? "Mounting..." : "Mount";
    mount.disabled = runtime.state === "mounting";
    mount.addEventListener("click", () => {
      if (runtime.state === "mounted") {
        void unmountMapping(config.id);
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
    showNotice("配置已删除", "success");
  } catch (error) {
    showNotice(String(error));
  }
}

function updateMapping(runtime: MappingRuntime): void {
  const index = mappings.findIndex((item) => item.config.id === runtime.config.id);
  if (index >= 0) mappings[index] = runtime;
  renderMappings();
}

async function mountMapping(id: string, password: string | null): Promise<void> {
  const current = mappings.find((item) => item.config.id === id);
  if (!current) return;
  updateMapping({ ...current, state: "mounting", lastError: null });
  try {
    const runtime = await invoke<MappingRuntime>("mount_mapping", { id, password });
    updateMapping(runtime);
    mountDialog.close();
    showNotice("Mapping mounted", "success");
  } catch (error) {
    await loadMappings();
    showMountNotice(String(error));
  }
}

async function unmountMapping(id: string): Promise<void> {
  try {
    const runtime = await invoke<MappingRuntime>("unmount_mapping", { id });
    updateMapping(runtime);
    showNotice("Mapping unmounted", "success");
  } catch (error) {
    await loadMappings();
    showNotice(String(error));
  }
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  void (async () => {
    const config = readMappingConfig();

    try {
      await invoke<MappingRuntime>("save_mapping", { config });
      dialog.close();
      await loadMappings();
      showNotice("配置已保存", "success");
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
    clearDialogNotice();
    try {
      const config = readMappingConfig();
      const password = getElement<HTMLInputElement>("password").value || null;
      await invoke("test_webdav_connection", { config, password });
      showDialogNotice("WebDAV 连接成功", "success");
    } catch (error) {
      showDialogNotice(String(error));
    } finally {
      testConnectionButton.textContent = "测试 WebDAV 连接";
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
  getElement<HTMLInputElement>("password").value = "";
  clearDialogNotice();
});

mountForm.addEventListener("submit", (event) => {
  event.preventDefault();
  if (!mountTargetId) return;
  const id = mountTargetId;
  const password = getElement<HTMLInputElement>("mount-password").value || null;
  const submit = getElement<HTMLButtonElement>("confirm-mount");
  submit.disabled = true;
  void mountMapping(id, password).finally(() => {
    submit.disabled = false;
  });
});
getElement<HTMLButtonElement>("close-mount-dialog").addEventListener("click", () => mountDialog.close());
getElement<HTMLButtonElement>("cancel-mount-dialog").addEventListener("click", () => mountDialog.close());
mountDialog.addEventListener("close", clearMountDialog);

void loadMappings();
