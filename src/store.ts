import { reactive } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type {
  AuthStatus,
  MappingRuntime,
  PlatformInfo,
  StartupMountResult,
} from "./types";

export type AuthPhase = "loading" | "setup" | "unlock" | "workspace";

export interface Notice {
  message: string;
  kind: "error" | "success";
}

export const store = reactive({
  phase: "loading" as AuthPhase,
  platformInfo: {
    os: "windows",
    defaultMountPoint: "Z:",
    secureStore: "系统凭据库",
    macfuseRequired: false,
    macfuseInstalled: true,
    macfuseInstallerBundled: false,
  } as PlatformInfo,
  mappings: [] as MappingRuntime[],
  notice: null as Notice | null,

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
    this.setNotice("正在恢复挂载状态...");
    try {
      const result = await invoke<StartupMountResult>("restore_startup_mappings");
      this.mappings = result.mappings;
      if (result.attempted === 0) {
        this.clearNotice();
        return;
      }
      const failed = this.mappings.filter((runtime) => runtime.state === "error");
      if (failed.length > 0) {
        this.setNotice(`${failed.length} 个映射恢复失败`);
      } else {
        this.setNotice(`已恢复 ${result.attempted} 个映射`, "success");
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
    this.setNotice("配置和凭据已删除", "success");
  },

  async mountMapping(
    id: string,
    password: string | null,
    totpCode: string | null,
    remember: boolean,
  ): Promise<MappingRuntime> {
    const current = this.mappings.find((item) => item.config.id === id);
    if (current) {
      this.updateMapping({ ...current, state: "mounting", lastError: null });
    }
    try {
      const runtime = await invoke<MappingRuntime>("mount_mapping", {
        id,
        password,
        totpCode,
        remember,
      });
      this.updateMapping(runtime);
      this.setNotice("映射已挂载", "success");
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
      this.setNotice("映射已卸载", "success");
    } catch (error) {
      await this.loadMappings();
      this.setNotice(String(error));
    }
  },

  async installMacfuse(): Promise<void> {
    await invoke("open_macfuse_installer");
    this.setNotice("已打开 macFUSE 安装器", "success");
  },
});

export async function initialize(): Promise<void> {
  store.platformInfo = await invoke<PlatformInfo>("get_platform_info");
  await store.initializeAuth();
}
