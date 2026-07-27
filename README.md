<div align="center">

<img src=".idea/icon.png" style="width: 256px; height: 256px;" alt="Icon">

# GugleFS

</div>

GugleFS 是一个基于 Tauri 的跨平台远程文件系统客户端，目标是将 FTP、SFTP（SSH）和 WebDAV 远程路径映射为本地磁盘或挂载目录。

- Windows：WinFsp
- Linux / macOS：FUSE
- 配置界面：Tauri + TypeScript + Vite
- 文件系统引擎：Rust
- 前端包管理器：pnpm

> 当前仓库已具备配置窗口、版本化配置持久化、核心 VFS 语义和 WebDAV 基础操作。FTP/SFTP、凭据库和系统挂载仍在实现中，进度见 `TODO.md`。

## 目录结构

```text
GugleFS/
├─ src/                     # 仅负责配置窗口
├─ src-tauri/               # Tauri 入口与 IPC 命令
├─ crates/
│  ├─ guglefs-core/         # 配置模型、状态和引擎抽象
│  ├─ guglefs-remote/       # FTP / SFTP / WebDAV 适配器
│  └─ guglefs-mount/        # WinFsp / FUSE 平台驱动
├─ Cargo.toml               # Rust workspace
├─ package.json             # pnpm 命令
└─ TODO.md
```

依赖方向固定为：`配置窗口 -> Tauri IPC -> core <- remote / mount`。前端不直接处理网络请求或文件系统操作。

## 开发

需要 Node.js 20+、pnpm 10+、Rust 1.77+，并安装 Tauri 对应平台的系统依赖。

Windows 开发环境还需要：

- Visual Studio Build Tools 2022
- `Desktop development with C++` 工作负载及 Windows 10/11 SDK
- WebView2（现代 Windows 通常已经包含）

安装后请重新打开终端，确认 `cargo` 和 MSVC 的 `link.exe` 可用。开始实现挂载驱动时还需安装 WinFsp SDK。

```bash
pnpm install
pnpm dev
```

仅检查前端和 Rust workspace：

```bash
pnpm check
```

### Windows mount runtime

Install [WinFsp 2.1](https://github.com/winfsp/winfsp/releases) before using a WebDAV mapping. GugleFS loads WinFsp at mount time, accepts either a drive letter such as `Z:` or an existing absolute directory, and keeps the WebDAV password in memory only for that mount session.

开发真实挂载功能前，Linux 需要 FUSE3 开发包；macOS 需要 macFUSE。

## 安全边界

`MappingConfig` 只保存 `credential_id`，不保存密码或私钥口令。WebDAV 连接测试中的密码只通过本次 IPC 请求传递，不会写入配置或返回值；正式挂载仍需接入 Windows Credential Manager、macOS Keychain 或 Linux Secret Service。

映射配置会保存到 Tauri 的应用配置目录下的 `mappings.json`，文件包含 `schemaVersion`，运行时挂载状态不会写入配置文件。
