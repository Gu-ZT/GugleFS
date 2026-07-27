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
          <input value="稍后在系统凭据库中配置" disabled />
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
      <div class="dialog-actions">
        <button id="cancel-dialog" class="secondary" type="button">取消</button>
        <button class="primary" type="submit">保存配置</button>
      </div>
    </form>
  </dialog>
`;

const dialog = getElement<HTMLDialogElement>("mapping-dialog");
const form = getElement<HTMLFormElement>("mapping-form");
const list = getElement<HTMLDivElement>("mapping-list");
const emptyState = getElement<HTMLDivElement>("empty-state");
const notice = getElement<HTMLDivElement>("notice");
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

function openForm(runtime?: MappingRuntime): void {
  form.reset();
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
  dialog.showModal();
}

function statusLabel(state: MappingState): string {
  return {
    unmounted: "未挂载",
    mounting: "挂载中",
    mounted: "已挂载",
    error: "异常",
  }[state];
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
    destination.append(mountPoint, status);

    const actions = document.createElement("div");
    actions.className = "item-actions";
    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "secondary compact";
    edit.textContent = "编辑";
    edit.addEventListener("click", () => openForm(runtime));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger compact";
    remove.textContent = "删除";
    remove.addEventListener("click", () => void deleteMapping(config.id));
    actions.append(edit, remove);

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

form.addEventListener("submit", (event) => {
  event.preventDefault();
  void (async () => {
    const protocol = getElement<HTMLSelectElement>("protocol").value as Protocol;
    const config: MappingConfig = {
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

    try {
      await invoke<MappingRuntime>("save_mapping", { config });
      dialog.close();
      await loadMappings();
      showNotice("配置已保存", "success");
    } catch (error) {
      showNotice(String(error));
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
});
getElement<HTMLButtonElement>("new-mapping").addEventListener("click", () => openForm());
getElement<HTMLButtonElement>("empty-add").addEventListener("click", () => openForm());
getElement<HTMLButtonElement>("refresh").addEventListener("click", () => void loadMappings());
getElement<HTMLButtonElement>("close-dialog").addEventListener("click", () => dialog.close());
getElement<HTMLButtonElement>("cancel-dialog").addEventListener("click", () => dialog.close());

void loadMappings();
