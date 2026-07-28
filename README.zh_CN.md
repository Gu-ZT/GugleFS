<div align="center">

<img src="src-tauri/icons/icon.png" width="256" height="256" alt="GugleFS icon">

# GugleFS

</div>

[English](README.md) | 简体中文

GugleFS 是一个基于 Tauri 的跨平台远程文件系统客户端，目标是将 FTP、SFTP（SSH）和 WebDAV 远程路径映射为本地磁盘或挂载目录。

- Windows：WinFsp
- Linux / macOS：FUSE
- 配置界面：Tauri + TypeScript + Vite
- 文件系统引擎：Rust
- 前端包管理器：pnpm

> Windows、Linux 和 macOS 共用 FTP/显式 FTPS、SFTP、WebDAV、系统代理、系统凭据库、启动 2FA、托盘和挂载恢复逻辑。Windows 使用 WinFsp，Linux 使用 FUSE3，macOS 使用 macFUSE 5。

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

需要 Node.js 20+、pnpm 10+、Rust 1.85.1+，并安装 Tauri 对应平台的系统依赖。

各平台还需要以下系统依赖：

Windows：

- Visual Studio Build Tools 2022
- `Desktop development with C++` 工作负载及 Windows 10/11 SDK
- WebView2（现代 Windows 通常已经包含）

- WinFsp 2.1 SDK

Linux（Debian/Ubuntu）：

```bash
sudo apt install fuse3 libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libdbus-1-dev pkg-config
```

macOS：

```bash
brew install --cask macfuse
brew install pkgconf
```

```bash
pnpm install
pnpm dev
```

仅检查前端和 Rust workspace：

```bash
pnpm check
```

### 平台挂载运行时

Windows NSIS 安装包内置并静默安装 [WinFsp 2.1](https://github.com/winfsp/winfsp/releases) 运行时，挂载点可以是 `Z:` 盘符或绝对目录。WinFsp 会自行创建目录挂载点；GugleFS 会暂时移除已有空目录并在正常卸载后恢复，非空目录会被拒绝。

Linux 使用 FUSE3，挂载点是绝对目录；DEB 包声明 `fuse3` 和 `libsecret-1-0` 依赖。macOS App/DMG 内置官方 macFUSE 5.3.3 安装器，检测到运行时缺失时可从应用内打开。macFUSE 包含系统扩展，因此仍必须由用户以管理员权限安装，并在“隐私与安全性”中批准，GugleFS 不会自动或静默安装。不存在的 Unix 挂载目录会在首次挂载时创建，已有目录必须为空。

macFUSE 的二进制许可允许非商业软件再分发，但商业软件捆绑、自动下载或安装需要版权方书面许可。GugleFS 分发未经修改的官方 DMG，固定 SHA-256，并随包附带[完整许可条款](THIRD_PARTY_LICENSES/macFUSE-LICENSE.txt)。

FTP 默认使用被动模式，支持标准 FTP 和显式 FTPS；不支持已弃用的隐式 FTPS。

SFTP 支持密码认证，以及 OpenSSH/PEM 私钥认证。私钥文件不限制扩展名，可直接选择 `ssh-keygen` 生成的 `id_ed25519`、`id_rsa` 等文件；也可以粘贴私钥。文件模式只保存路径，粘贴模式会将私钥分块保存到当前平台的安全凭据库。加密私钥的口令可以单独保存。首次连接会显示服务器 SHA-256 主机密钥指纹，确认后固定到映射配置；后续密钥变化必须重新确认。

### 系统代理

每个映射默认读取系统代理，也可以勾选“忽略系统代理”强制直连。Linux 和 macOS 读取协议对应的 `HTTP_PROXY`、`HTTPS_PROXY`、`FTP_PROXY`、`SFTP_PROXY`、`ALL_PROXY` 及小写变量，并遵守 `NO_PROXY`。Windows 读取当前用户注册表 `Internet Settings` 下的 `ProxyEnable`、`ProxyServer` 和 `ProxyOverride`。WebDAV 使用 HTTP(S) 或 SOCKS5 代理；SFTP、FTP 和 FTPS 通过 HTTP CONNECT 或 SOCKS5 建立隧道，FTP 的控制连接和被动数据连接都会使用同一代理。

首次启动会引导使用身份验证器注册 TOTP 2FA，之后每次启动都必须输入 6 位验证码。FTP/FTPS、SFTP 和 WebDAV 的密码、私钥口令、粘贴私钥和 TOTP 密钥分别保存在 Windows Credential Manager、macOS Keychain 或 Linux Secret Service；应用配置和挂载恢复状态只保存凭据引用和映射 ID。

点击“锁定”会先安全卸载当前所有映射，再进入 2FA 锁屏。解锁或重启后，GugleFS 会自动恢复上次仍处于挂载状态且已保存凭据的映射；用户主动点击“卸载”后，该映射不会在下次解锁时恢复（启用了 `auto_mount` 的配置除外）。

### 托盘与退出

关闭主窗口会将 GugleFS 隐藏到系统托盘，现有映射继续运行。双击托盘图标或选择“打开 GugleFS”可以恢复并聚焦主窗口；右键选择“退出”会先停止本进程创建的全部 WinFsp/FUSE 文件系统，再退出应用。GugleFS 只允许一个实例运行，重复启动会唤醒现有窗口，避免多个进程同时抢占同一挂载点。强制结束进程或系统崩溃不属于安全退出流程。

### 远程访问性能

共享 VFS 层对 FTP、SFTP 和 WebDAV 启用了有界短期缓存：元数据缓存 3 秒、目录缓存 2 秒、未找到结果缓存 1 秒，最多保留 4096 项。每个打开文件还会进行 1 MiB 顺序预读，以减少文件浏览和连续读取产生的远端往返；创建、写入、截断、重命名和删除会更新或失效相关缓存，跨句柄写入也会使旧预读数据失效。

## 安全边界

`MappingConfig` 只保存凭据 ID、本地私钥路径、粘贴私钥引用、代理忽略开关和已确认的 SSH 主机指纹，不保存密码、私钥口令、代理凭据或粘贴私钥正文。FTP/FTPS、SFTP、WebDAV 凭据和 TOTP 密钥保存在系统安全凭据库；粘贴私钥按唯一 ID 分块保存，以适配平台单条凭据的大小限制。连接测试和挂载中的认证材料只通过本次 IPC 请求传递，不会写入配置、日志或 IPC 返回值。

映射配置会保存到 Tauri 应用配置目录下的 `mappings.json`，文件包含 `schemaVersion`。用于解锁后恢复的映射 ID 单独保存在 `mount-state.json`，运行时错误和凭据内容不会写入这两个文件。

## CI 与发布

推送到 `main` 后，GitHub Actions 会在 Windows、Ubuntu 和 macOS 上分别执行格式检查、Clippy、Rust 测试和前端生产构建，再创建 `<version>+build.<run_number>` 预发布。发布产物包括 Windows x64 NSIS、Linux x64 DEB/AppImage 和 macOS ARM64 App/DMG；正式 GitHub Release 会采用 release tag 作为应用版本。

Windows 安装包内置 WinFsp 运行时，macOS 包内置已校验的官方 macFUSE 安装器和许可证。macOS 工作流支持通过 `APPLE_CERTIFICATE`、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_PASSWORD` 和 `APPLE_TEAM_ID` secrets 完成签名与公证；确认凭据有效后还需设置仓库变量 `APPLE_SIGNING_ENABLED=true` 才会启用签名，否则生成未签名产物。Linux 和 Windows 代码签名仍需配置对应签名密钥。
