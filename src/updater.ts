import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { reactive } from "vue";

const AUTO_CHECK_KEY = "guglefs.autoCheckUpdates";

interface UpdateInfo {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  downloadUrl: string;
}

type UpdateResult =
  | { kind: "available"; info: UpdateInfo }
  | { kind: "current"; info: UpdateInfo }
  | { kind: "error" };

function initialAutoCheck(): boolean {
  return localStorage.getItem(AUTO_CHECK_KEY) !== "false";
}

export const updater = reactive({
  autoCheck: initialAutoCheck(),
  checking: false,
  result: null as UpdateResult | null,

  setAutoCheck(enabled: boolean): void {
    this.autoCheck = enabled;
    localStorage.setItem(AUTO_CHECK_KEY, String(enabled));
  },

  async check(manual = true): Promise<void> {
    if (this.checking) return;
    this.checking = true;
    if (manual) this.result = null;
    try {
      const info = await invoke<UpdateInfo>("check_for_updates");
      if (info.updateAvailable) {
        this.result = { kind: "available", info };
      } else if (manual) {
        this.result = { kind: "current", info };
      }
    } catch {
      if (manual) this.result = { kind: "error" };
    } finally {
      this.checking = false;
    }
  },

  async openDownloadPage(): Promise<void> {
    if (this.result?.kind !== "available") return;
    try {
      await openUrl(this.result.info.downloadUrl);
    } catch {
      this.result = { kind: "error" };
    }
  },
});
