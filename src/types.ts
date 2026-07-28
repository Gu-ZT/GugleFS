export type Protocol = "ftp" | "sftp" | "webdav";
export type MappingState = "unmounted" | "mounting" | "mounted" | "error";

export type AuthMethod =
  | { type: "password"; credential_id: string | null }
  | {
      type: "private_key";
      key_path: string | null;
      key_id: string | null;
      credential_id: string | null;
    }
  | { type: "ssh_agent" }
  | { type: "anonymous" };

export interface MappingConfig {
  id: string;
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  username: string | null;
  auth: AuthMethod;
  remotePath: string;
  mountPoint: string;
  ftpTls: boolean;
  hostKeyFingerprint: string | null;
  sftpTotpRequired: boolean;
  ignoreSystemProxy: boolean;
  autoMount: boolean;
}

export interface MappingRuntime {
  config: MappingConfig;
  state: MappingState;
  lastError: string | null;
}

export interface AuthStatus {
  configured: boolean;
  unlocked: boolean;
}

export interface TotpSetup {
  secret: string;
  qrCode: string;
}

export interface StartupMountResult {
  mappings: MappingRuntime[];
  attempted: number;
}

export interface PlatformInfo {
  os: "windows" | "macos" | "linux";
  defaultMountPoint: string;
  secureStore: string;
  fuseTRequired: boolean;
  fuseTInstalled: boolean;
  fuseTInstallerBundled: boolean;
}

export interface ImportMappingsResult {
  mappings: MappingRuntime[];
  imported: number;
}

export interface RemoteDirectory {
  path: string;
  name: string;
}

export interface RemoteBrowserListing {
  path: string;
  directories: RemoteDirectory[];
}

export function hasPersistedAuthentication(config: MappingConfig): boolean {
  switch (config.auth.type) {
    case "password":
      return config.auth.credential_id !== null;
    case "private_key":
      return config.auth.key_path !== null || config.auth.key_id !== null;
    case "ssh_agent":
      return true;
    case "anonymous":
      return true;
  }
}

export function statusLabel(state: MappingState): string {
  return {
    unmounted: "未挂载",
    mounting: "挂载中",
    mounted: "已挂载",
    error: "异常",
  }[state];
}

export function endpointOf(config: MappingConfig): string {
  return `${config.username ? `${config.username}@` : ""}${config.host}:${config.port}${config.remotePath}`;
}
