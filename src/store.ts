import { reactive } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { t } from "./i18n";
import type {
  AuthStatus,
  ImportMappingsResult,
  MappingRuntime,
  PlatformInfo,
  StartupMountResult,
} from "./types";

export type AuthPhase = "loading" | "setup" | "unlock" | "workspace";

export interface Notice {
  message: string;
  kind: "error" | "success";
}

let mappingRuntimeListener: Promise<UnlistenFn> | null = null;

async function initializeMappingRuntimeEvents(): Promise<void> {
  mappingRuntimeListener ??= listen<MappingRuntime>("mapping-runtime", ({ payload }) => {
    store.updateMapping(payload);
  });
  await mappingRuntimeListener;
}

export const store = reactive({
  phase: "loading" as AuthPhase,
  platformInfo: {
    os: "windows",
    defaultMountPoint: "Z:",
    secureStore: "Secure Store",
    fuseTRequired: false,
    fuseTInstalled: true,
    fuseTInstallerBundled: false,
    previousSessionUnclean: false,
  } as PlatformInfo,
  mappings: [] as MappingRuntime[],
  notice: null as Notice | null,
  autoLaunch: false,
  autoLaunchBusy: false,
  occupiedLetters: [] as string[],

  async refreshOccupiedLetters(): Promise<void> {
    if (this.platformInfo.os !== "windows") {
      this.occupiedLetters = [];
      return;
    }
    try {
      this.occupiedLetters = await invoke<string[]>("occupied_drive_letters");
    } catch {
      // 读取失败时保持上次结果，不阻塞表单
    }
  },

  nextFreeMountPoint(): string {
    if (this.platformInfo.os === "windows") {
      for (let code = 90; code >= 65; code -= 1) {
        const letter = String.fromCharCode(code);
        if (!this.occupiedLetters.includes(letter)) {
          return `${letter}:`;
        }
      }
    }
    return this.platformInfo.defaultMountPoint;
  },

  async initAutoLaunch(): Promise<void> {
    try {
      this.autoLaunch = await isEnabled();
    } catch {
      // 读取失败时保持关闭状态，不阻塞界面
    }
  },

  async setAutoLaunch(on: boolean): Promise<void> {
    if (this.autoLaunchBusy) return;
    this.autoLaunchBusy = true;
    try {
      if (on) {
        await enable();
      } else {
        await disable();
      }
      this.autoLaunch = await isEnabled();
    } catch (error) {
      this.setNotice(String(error));
    } finally {
      this.autoLaunchBusy = false;
    }
  },

  setNotice(message: string, kind: Notice["kind"] = "error"): void {
    this.notice = { message, kind };
  },

  clearNotice(): void {
    this.notice = null;
  },

  async loadMappings(): Promise<void> {
    try {
      this.mappings = await invoke<MappingRuntime[]>("list_mappings");
    } catch (error) {
      this.setNotice(String(error));
    }
  },

  updateMapping(runtime: MappingRuntime): void {
    const index = this.mappings.findIndex((item) => item.config.id === runtime.config.id);
    if (index >= 0) this.mappings[index] = runtime;
  },

  async enterWorkspace(): Promise<void> {
    this.phase = "workspace";
    await this.loadMappings();
    this.setNotice(t("notice.restoring"));
    try {
      const result = await invoke<StartupMountResult>("restore_startup_mappings");
      this.mappings = result.mappings;
      if (result.attempted === 0) {
        this.clearNotice();
        return;
      }
      const failed = this.mappings.filter((runtime) => runtime.state === "error");
      if (failed.length > 0) {
        this.setNotice(t("notice.restoreFailed", { count: failed.length }));
      } else {
        this.setNotice(t("notice.restored", { count: result.attempted }), "success");
      }
    } catch (error) {
      this.setNotice(String(error));
    }
  },

  async initializeAuth(): Promise<void> {
    try {
      const status = await invoke<AuthStatus>("get_auth_status");
      this.phase = !status.configured ? "setup" : status.unlocked ? "loading" : "unlock";
      if (status.configured && status.unlocked) {
        await this.enterWorkspace();
      }
    } catch (error) {
      this.phase = "unlock";
      throw error;
    }
  },

  async unlock(code: string): Promise<void> {
    await invoke<AuthStatus>("unlock_app", { code });
    await this.enterWorkspace();
  },

  async lock(): Promise<void> {
    await invoke<AuthStatus>("lock_app");
    this.mappings = [];
    this.clearNotice();
    this.phase = "unlock";
  },

  async deleteMapping(id: string): Promise<void> {
    await invoke("delete_mapping", { id });
    this.mappings = this.mappings.filter((item) => item.config.id !== id);
    this.setNotice(t("notice.deleted"), "success");
  },

  async mountMapping(
    id: string,
    password: string | null,
    totpCode: string | null,
    remember: boolean,
  ): Promise<MappingRuntime> {
    try {
      const runtime = await invoke<MappingRuntime>("mount_mapping", {
        id,
        password,
        totpCode,
        remember,
      });
      this.updateMapping(runtime);
      this.setNotice(t("notice.mounted"), "success");
      return runtime;
    } catch (error) {
      await this.loadMappings();
      throw error;
    }
  },

  async unmountMapping(id: string): Promise<void> {
    try {
      const runtime = await invoke<MappingRuntime>("unmount_mapping", { id });
      this.updateMapping(runtime);
      this.setNotice(t("notice.unmounted"), "success");
    } catch (error) {
      await this.loadMappings();
      this.setNotice(String(error));
    }
  },

  async installFuseT(): Promise<void> {
    await invoke("open_fuse_t_installer");
    this.setNotice(t("notice.fuseInstaller"), "success");
  },

  async exportMappings(path: string): Promise<void> {
    const exported = await invoke<number>("export_mappings", { path });
    this.setNotice(t("notice.exported", { count: exported }), "success");
  },

  async importMappings(path: string): Promise<void> {
    const result = await invoke<ImportMappingsResult>("import_mappings", { path });
    this.mappings = result.mappings;
    await this.refreshOccupiedLetters();
    this.setNotice(t("notice.imported", { count: result.imported }), "success");
  },

  async exportDiagnostics(path: string): Promise<void> {
    const events = await invoke<number>("export_diagnostics", { path });
    this.setNotice(t("notice.diagnostics", { count: events }), "success");
  },
});

export async function initialize(): Promise<void> {
  await initializeMappingRuntimeEvents();
  store.platformInfo = await invoke<PlatformInfo>("get_platform_info");
  await store.initializeAuth();
}
