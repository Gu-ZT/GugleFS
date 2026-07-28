<div align="center">

<img src="src-tauri/icons/icon.png" width="128" height="128" alt="GugleFS icon">

# GugleFS

**把 FTP、SFTP 和 WebDAV 远程路径挂载为本地磁盘。**

[English](README.md) | 简体中文

</div>

GugleFS 是一个基于 Tauri 的跨平台远程文件系统客户端。配置一次映射并挂载后，远程路径就像本机上的普通磁盘或目录一样，可以被任何应用直接使用，而不仅是文件传输窗口。

- **三种协议，一个界面** —— FTP/FTPS、SFTP（密码、私钥、SSH Agent、MFA）、WebDAV（Basic、Digest、Bearer 或客户端证书）
- **原生挂载** —— Windows 使用 WinFsp 2.1，Linux 使用 FUSE3，macOS 使用 FUSE-T 1.2.7
- **启动即锁定** —— TOTP 双因素认证保护应用启动，凭据保存在系统安全凭据库
- **经得起断网** —— 空闲 keepalive、静默重连、重启后恢复挂载
- **默认就快** —— 有界元数据缓存与 1 MiB 顺序预读，三平台共用

配置界面使用 Tauri + Vue 3 + TypeScript + Vite 构建，文件系统引擎为 Rust，前端包管理器为 pnpm。

## 界面截图

启动解锁由 TOTP 双因素认证保护：

<p align="center">
  <img src="docs/totp.png" width="640" alt="GugleFS 2FA 解锁界面">
</p>

映射列表一眼可见实时状态、端点、挂载点和凭据保存情况：

<p align="center">
  <img src="docs/main.png" width="720" alt="映射列表 —— 已挂载的 SFTP 磁盘">
</p>

添加映射的表单随协议适配：SFTP 提供密码、OpenSSH/PEM 私钥、SSH Agent 和 MFA；WebDAV 提供 Basic、Digest、Bearer Token、客户端证书和匿名认证。每个映射保存前都可以先测试连接：

<table>
  <tr>
    <td width="50%">
        <img src="docs/add-sftp.png" alt="添加映射 —— SFTP">
        <img src="docs/add-sftp2.png" width="640" alt="SFTP 映射选项 —— MFA、自动挂载、代理绕过、测试连接">
    </td>
    <td width="50%"><img src="docs/add-webdav.png" alt="添加映射 —— WebDAV"></td>
  </tr>
  <tr>
    <td align="center"><sub>SFTP：密码、私钥、SSH Agent 或 MFA 认证</sub></td>
    <td align="center"><sub>WebDAV（HTTPS，可选择认证方式）</sub></td>
  </tr>
</table>

## 目录结构

```text
GugleFS/
├─ src/                     # 仅负责配置窗口
├─ src-tauri/               # Tauri 入口与 IPC 命令
├─ crates/
│  ├─ guglefs-core/         # 配置模型、状态和引擎抽象
│  ├─ guglefs-remote/       # FTP / SFTP / WebDAV 适配器
│  └─ guglefs-mount/        # WinFsp / FUSE 平台驱动
├─ docs/                    # README 界面截图
├─ THIRD_PARTY_LICENSES/    # 随包分发的依赖许可证
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
brew install --cask fuse-t
brew install pkgconf
export PKG_CONFIG_PATH="$PWD/scripts/pkgconfig/fuse-t${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
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

Windows 挂载使用大小写不敏感查找，同时保留远端服务器上的原始拼写。精确大小写匹配优先；如果远端同一目录包含多个仅大小写不同的名称，目录只确定性显示其中一个，非精确的歧义访问会失败，不会打开错误文件。新建和重命名会拒绝 Windows 保留设备名、非法字符、尾随空格/点、无效 UTF-16 和超长分量。Windows 无法表示的既有远端条目不会显示在挂载目录中，需要使用协议原生客户端重命名。目录、点号开头的隐藏条目和普通文件归档属性会被投影；远端协议没有原生对应项时，可变 Windows 属性和时间戳暂不持久化。

Linux 使用 FUSE3，挂载点是绝对目录；DEB 包声明 `fuse3` 和 `libsecret-1-0` 依赖。macOS App/DMG 内置官方 FUSE-T 1.2.7 安装器，检测到运行时缺失时可从应用内打开。安装仍需管理员授权，但 FUSE-T 通过本机 NFS、SMB 或 FSKit 后端运行在用户空间，不需要内核或系统扩展；部分应用首次访问挂载点时需要在“隐私与安全性 -> 文件与文件夹”中允许“网络宗卷”。不存在的 Unix 挂载目录会在首次挂载时创建，已有目录必须为空。

FUSE-T 二进制许可允许非商业用途再分发；商业使用或随商业软件捆绑需要从 FUSE-T 作者处取得商业许可。GugleFS 分发未经修改的官方 PKG，固定 SHA-256，并随包附带[FUSE-T 许可条款](THIRD_PARTY_LICENSES/FUSE-T-LICENSE.txt)和[第三方归属声明](THIRD_PARTY_LICENSES/FUSE-T-ATTRIBUTIONS.txt)。

FTP 默认使用被动模式，支持标准 FTP 和显式 FTPS；不支持已弃用的隐式 FTPS。

SFTP 支持密码、OpenSSH/PEM 私钥和 SSH Agent 认证。Unix 通过 `SSH_AUTH_SOCK` 连接 Agent；Windows 会依次尝试配置的 Agent 管道、标准 OpenSSH Agent 管道和 Pageant。私钥文件不限制扩展名，可直接选择 `ssh-keygen` 生成的 `id_ed25519`、`id_rsa` 等文件；也可以粘贴私钥。文件模式只保存路径，粘贴模式会将私钥分块保存到当前平台的安全凭据库。加密私钥的口令可以单独保存。

首次连接会显示服务器 SHA-256 主机密钥指纹，确认后固定到映射配置；后续密钥变化必须重新确认。映射表单可以导入 OpenSSH `known_hosts`，支持哈希主机名和非标准端口条目；只有条目与服务器当前实际提供的密钥一致时才会导入。

SFTP 服务器需要 MFA 时，可在映射中勾选“需要 MFA”，并在测试连接或挂载时手动输入当前 6 位 TOTP 验证码。验证码仅用于本次请求，不会保存到配置或系统凭据库；此类映射不支持自动挂载。空闲 SSH 传输会定时发送协议层 keepalive；只要已认证的 SSH 传输仍然存活，SFTP session 关闭后会静默重建，不需要再次输入验证码。如果 SSH 传输本身已经断开，则必须使用新的验证码手动重新挂载。非 MFA 连接仍会自动重连，并对可安全重试的操作重试一次。

WebDAV 仅允许 HTTPS，支持 Basic、Digest、Bearer Token、客户端证书和匿名认证。密码和 Bearer Token 保存在平台安全凭据库。客户端证书模式读取包含证书链及一个未加密 RSA、EC 或 PKCS#8 私钥的本地组合 PEM 文件；配置只保存本地路径，可移植导出会移除该路径。

WebDAV 重定向限制在原始同源地址。读改写和截断优先使用强 ETag 配合 `If-Match`，弱 ETag 或无 ETag 时在服务器提供 Last-Modified 的情况下回退到 `If-Unmodified-Since`。条件失败会作为文件系统忙/版本冲突返回，不会静默覆盖新版本。服务器同时不提供两种校验器时，GugleFS 会在单个挂载进程内串行写入，但来自其他客户端的并发写入仍可能按最后写入者覆盖；当前不会发送 WebDAV `LOCK`/`UNLOCK`。

保存映射前可以在表单中浏览远程目录。目录浏览复用表单内当前凭据、系统代理设置、SFTP 主机密钥验证和临时 MFA 验证码；选择目录后只将绝对路径写回映射，不会持久化本次临时认证信息。

### 系统代理

每个映射默认读取系统代理，也可以勾选“忽略系统代理”强制直连。Linux 和 macOS 读取协议对应的 `HTTP_PROXY`、`HTTPS_PROXY`、`FTP_PROXY`、`SFTP_PROXY`、`ALL_PROXY` 及小写变量，并遵守 `NO_PROXY`。Windows 读取当前用户注册表 `Internet Settings` 下的 `ProxyEnable`、`ProxyServer` 和 `ProxyOverride`。WebDAV 使用 HTTP(S) 或 SOCKS5 代理；SFTP、FTP 和 FTPS 通过 HTTP CONNECT 或 SOCKS5 建立隧道，FTP 的控制连接和被动数据连接都会使用同一代理。

首次启动会引导使用身份验证器注册 TOTP 2FA，之后每次启动都必须输入 6 位验证码。FTP/FTPS、SFTP 和 WebDAV 的密码、Bearer Token、私钥口令、粘贴私钥，以及用于应用启动 2FA 的 TOTP 密钥分别保存在 Windows Credential Manager、macOS Keychain 或 Linux Secret Service；应用配置和挂载恢复状态只保存凭据引用和映射 ID。SFTP MFA 验证码只在当前测试或挂载请求中使用，不会加入系统凭据库。

点击“锁定”会先安全卸载当前所有映射，再进入 2FA 锁屏。解锁或重启后，GugleFS 会自动恢复上次仍处于挂载状态且已保存凭据的映射；用户主动点击“卸载”后，该映射不会在下次解锁时恢复（启用了 `auto_mount` 的配置除外）。需要 MFA 的 SFTP 映射始终不会自动恢复，必须由用户输入当前验证码后手动挂载。

挂载和卸载命令运行在 Tauri 异步运行时中，驱动状态转换由后端串行管理。每次进入 `mounting`、`unmounting`、`mounted`、`unmounted` 或 `error` 都会向前端发送事件，因此手动操作、启动恢复和锁定卸载共用同一个实时状态源。这些任务仍附属于应用生命周期，托盘“退出”可以在进程结束前停止所有文件系统。

GugleFS 运行期间会创建固定且不含敏感信息的 `session-running` 标记，只有安全退出并成功停止本进程创建的全部挂载后才会删除。崩溃、强制结束或卸载失败留下的标记会在下次启动时被检测；通过 2FA 解锁后，现有恢复状态会重新挂载具备已保存凭据的合格映射，同时工作区提示用户核对近期文件和远端连接。

### 托盘与退出

关闭主窗口会将 GugleFS 隐藏到系统托盘，现有映射继续运行。双击托盘图标或选择“打开 GugleFS”可以恢复并聚焦主窗口；右键选择“退出”会先停止本进程创建的全部 WinFsp/FUSE 文件系统，再退出应用。GugleFS 只允许一个实例运行，重复启动会唤醒现有窗口，避免多个进程同时抢占同一挂载点。强制结束进程或系统崩溃不属于安全退出流程。

工作区和原生对话框支持完整键盘导航。`Ctrl+N`/`Cmd+N` 打开新建映射对话框，`Ctrl+R`/`Cmd+R` 在不重载 WebView 的情况下刷新映射；对话框会聚焦首个相关字段，重复出现的映射操作会向辅助技术读出映射名称，挂载状态变化和错误也会实时播报。

界面支持简体中文和英文。首次启动时 GugleFS 会跟随系统语言，认证页和工作区均可随时切换语言；用户选择会保存在本机并用于后续启动。

### 远程访问性能

共享 VFS 层对 FTP、SFTP 和 WebDAV 启用了有界短期缓存：元数据缓存 3 秒、目录缓存 2 秒、未找到结果缓存 1 秒，最多保留 4096 项。每个打开文件还会进行 1 MiB 顺序预读，以减少文件浏览和连续读取产生的远端往返；创建、写入、截断、重命名和删除会更新或失效相关缓存，跨句柄写入也会使旧预读数据失效。

每个已挂载映射最多同时执行 8 个远程操作；控制请求超时为 30 秒，文件传输超时为 120 秒。读取、写入、截断、时间戳、刷新和连接建立遇到瞬时故障时，会在短暂退避后重试一次。FTP 会先丢弃失败或超时的会话，SFTP 会重建 SFTP session 或 SSH 连接，WebDAV 则使用 HTTP 客户端连接池。创建、删除和重命名在结果不确定时不会自动重放，因为第一次请求可能已经在远端生效。

## 安全边界

`MappingConfig` 只保存凭据 ID、本地 SSH/客户端证书私钥路径、粘贴私钥引用、是否需要 SFTP MFA、代理忽略开关和已确认的 SSH 主机指纹，不保存密码、Bearer Token、私钥口令、代理凭据、粘贴私钥正文或 SFTP TOTP 验证码。FTP/FTPS、SFTP、WebDAV 凭据和应用启动 2FA 的 TOTP 密钥保存在系统安全凭据库；粘贴私钥按唯一 ID 分块保存，以适配平台单条凭据的大小限制。连接测试和挂载中的临时认证材料只通过本次 IPC 请求传递，不会写入配置、日志或 IPC 返回值。

映射配置会保存到 Tauri 应用配置目录下的 `mappings.json`，文件包含 `schemaVersion`。用于解锁后恢复的映射 ID 单独保存在 `mount-state.json`，运行时错误和凭据内容不会写入这两个文件。

在映射工作区使用“导入”和“导出”可以在不同设备之间迁移可移植 JSON 配置。导出内容包含远端端点和已确认的 SSH 指纹，但不包含密码、凭据 ID、本地私钥路径、粘贴私钥引用或自动挂载状态。导入会合并配置，必要时生成新 ID；重新填写凭据后才能挂载。

项目的威胁模型、安全不变量、剩余风险和私下报告渠道记录在 [SECURITY.md](SECURITY.md) 中。

脱敏操作事件以 JSONL 写入应用配置目录的 `logs` 文件夹；单个日志达到 1 MiB 后轮转，最多保留三个旧文件。侧边栏“导出诊断”会生成 JSON 报告，包含应用/平台版本、不具标识性的映射能力与状态摘要，以及这些固定字段事件。报告和日志不会包含主机名、用户名、路径、映射名称/ID、主机指纹、错误正文或认证材料。

## CI 与发布

推送到 `main` 后，GitHub Actions 会在 Windows、Ubuntu 和 macOS 上分别执行格式检查、Clippy、Rust 测试和前端生产构建；Actions 自身使用兼容 Node 24 的运行时，GugleFS 构建环境仍固定为 Node 22。随后 CI 创建 `<version>+build.<run_number>` 预发布，产物包括 Windows x64 NSIS、Linux x64 DEB/AppImage 和 macOS ARM64 App/DMG；正式 GitHub Release 会采用 release tag 作为应用版本。

当前源代码版本线为 `0.10.0`，对应的用户可见变更记录在 [CHANGES.md](CHANGES.md) 和 [CHANGES.zh_CN.md](CHANGES.zh_CN.md) 中。

Windows 安装包内置 WinFsp 运行时，macOS 包内置已校验的官方 FUSE-T 安装器、许可证和第三方归属声明。macOS 工作流支持通过 `APPLE_CERTIFICATE`、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_PASSWORD` 和 `APPLE_TEAM_ID` secrets 完成签名与公证；确认凭据有效后还需设置仓库变量 `APPLE_SIGNING_ENABLED=true` 才会启用签名，否则生成未签名产物。Linux 和 Windows 代码签名仍需配置对应签名密钥。

全部平台 job 结束后，CI 会读取 [CHANGES.md](CHANGES.md) 和 [CHANGES.zh_CN.md](CHANGES.zh_CN.md) 中与版本对应的章节，并将 Release 描述更新为双语更新说明、完整变更链接，以及标明每个附件对应平台、架构和格式的下载表格。

## 许可证

GugleFS 采用 [LGPL-3.0-only](LICENSE) 许可证。随包分发的 FUSE-T 安装器遵循其[自有许可条款](THIRD_PARTY_LICENSES/FUSE-T-LICENSE.txt)。
