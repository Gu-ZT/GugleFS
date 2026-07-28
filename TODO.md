# GugleFS TODO

## P0：项目骨架

- [x] 初始化 pnpm + Vite + TypeScript 前端
- [x] 初始化 Tauri v2 应用和 Rust workspace
- [x] 拆分 `core`、`remote`、`mount` 三层
- [x] 建立 FTP / SFTP / WebDAV 与 WinFsp / FUSE 条件编译入口
- [x] 建立配置增删改查 IPC
- [x] 建立最小配置窗口
- [x] 初始化 Git 仓库
- [x] 使用版本化 schema 持久化映射配置
- [x] 添加应用图标和安装包元数据
- [ ] 配置 Windows/macOS/Linux 签名与公证
- [x] 配置 push 预发布、GitHub Release 附件上传、双语更新日志和多平台下载表格 CI

## P1：核心文件系统语义

- [x] 定义完整 VFS 接口：`lookup`、`getattr`、`readdir`、`open`、`read`、`write`、`flush`、`release`
- [x] 补充目录创建、重命名、删除、截断和时间戳操作
- [x] 建立统一错误码，将远端错误映射为 POSIX / Windows 文件系统错误
- [x] 定义文件句柄、目录句柄与并发访问生命周期
- [x] 实现有界 TTL 元数据缓存、目录缓存和负缓存
- [x] 实现按打开文件隔离的顺序预读，并在写入后使旧预读数据失效
- [ ] 设计分块写入、写回与失败恢复策略
- [ ] 增加限流、超时、重试和断线重连
- [x] 定义统一远程文件操作抽象（元数据、目录、范围读写、创建、删除、重命名）
- [x] 为核心语义编写与平台无关的测试套件

## P1：远程协议

- [x] 选型并接入 Rust FTP 客户端库
- [x] 选型并接入 Rust SSH/SFTP 客户端库（优先验证 Windows 构建链）
- [x] FTP：支持显式 FTPS；明确不支持已弃用的隐式 FTPS
- [x] FTP：使用被动模式和 MLST/MLSD，并为旧服务器回退解析 `LIST`
- [x] SFTP：支持密码、本地私钥和粘贴私钥认证
- [x] SFTP：允许选择 `ssh-keygen` 生成的无扩展名私钥文件
- [x] SFTP：空闲连接发送 SSH keepalive，SSH 仍存活时静默重建 SFTP session，非 MFA 传输断开后自动重连并安全重试
- [x] SFTP：支持需要 MFA 的手动连接测试和挂载，TOTP 验证码仅用于当前请求且不持久化
- [ ] SFTP：支持 SSH Agent
- [x] SFTP：实现 SHA-256 主机指纹固定和首次连接确认流程
- [ ] SFTP：支持导入 OpenSSH `known_hosts`
- [x] WebDAV：接入 Rust HTTPS/WebDAV 客户端基础依赖，首个版本仅允许 HTTPS
- [x] WebDAV：实现 `PROPFIND`、`GET`、`PUT`、`MKCOL`、`MOVE` 和 `DELETE` 基础请求
- [x] WebDAV：将运行时凭据接入连接测试
- [x] WebDAV：将系统凭据接入挂载编排
- [x] WebDAV：支持 Basic 认证
- [ ] WebDAV：支持 Digest，评估 Bearer Token 与客户端证书
- [ ] WebDAV：正确处理 ETag 和条件请求
- [x] WebDAV：正确处理 Range 请求和同源重定向
- [ ] WebDAV：实现 `LOCK` / `UNLOCK` 或明确无锁服务器的并发写入策略
- [ ] WebDAV：验证 Nextcloud、ownCloud、Apache mod_dav 和常见云存储兼容性
- [x] WebDAV：限制跨域重定向时的认证头转发，防止凭据泄漏
- [x] FTP/FTPS/SFTP/WebDAV：读取 Windows 注册表或 Unix 环境变量中的系统代理，并支持按映射忽略
- [ ] 抽象连接池，避免每个文件操作重复建立连接
- [ ] 建立可注入的协议测试服务和集成测试

## P1：系统挂载

- [x] Windows：选型并接入 WinFsp Rust bindings
- [x] Windows：实现盘符/目录挂载、占用检测和安全卸载
- [x] Windows：恢复空目录挂载点并清理失效的 WinFsp 目录 reparse point
- [ ] Windows：处理大小写、Windows 文件名限制和文件属性
- [x] Linux：接入 FUSE3，完成挂载、卸载和基础权限映射
- [x] macOS：接入 FUSE-T 1.2.7，完成无内核扩展挂载、安全卸载和通过 pkg-config shim 直接链接 `libfuse-t`
- [ ] macOS：配置签名、公证证书并验证发布产物
- [x] 应用安全退出时卸载所有由 GugleFS 创建的挂载点
- [ ] 处理休眠、网络切换和系统关机
- [ ] 挂载操作移入独立后台任务，向前端推送状态事件

## P1：配置与凭据

- [x] 使用 Windows Credential Manager 保存 FTP/FTPS/SFTP/WebDAV 密码和私钥口令，禁止写入配置文件或日志
- [x] 使用 Windows Credential Manager 分块保存粘贴的 SSH 私钥，本地私钥仅保存路径
- [x] 使用 macOS Keychain 和 Linux Secret Service 保存凭据、应用启动 2FA 的 TOTP 密钥和粘贴私钥
- [x] 使用 TOTP 2FA 保护应用启动和凭据操作
- [x] 实现配置迁移、导入与导出（导出不包含凭据）
- [x] 解锁时恢复 `auto_mount` 及上次仍挂载且已保存凭据的映射
- [x] 增加 FTP/FTPS/SFTP/WebDAV 连接测试
- [ ] 增加远程路径选择
- [x] 校验重复挂载点、平台路径和非空 Unix 挂载目录

## P2：桌面体验

- [x] 增加挂载 / 卸载操作和实时状态
- [x] 锁定时自动卸载，解锁后恢复上次挂载状态
- [x] 增加系统托盘、关闭窗口后后台运行和托盘退出
- [x] 增加开机启动选项
- [ ] 增加结构化日志、日志轮转和诊断包导出
- [ ] 增加更新机制和崩溃恢复
- [ ] 完善键盘操作、屏幕阅读器标签和多语言

## P2：质量与发布

- [x] CI 覆盖 Linux、macOS 的格式检查、Clippy、测试和前端构建
- [x] 增加 Windows `rustfmt`、Clippy、测试和前端静态检查门禁
- [ ] 使用本地 FTP / SFTP 容器执行协议集成测试
- [ ] 使用内存后端执行 WinFsp / FUSE 一致性测试
- [ ] 压测大文件、海量小文件、并发读写和高延迟网络
- [x] 建立未签名 Windows x64 NSIS 自动打包流程
- [x] 建立 Linux x64 DEB/AppImage 和 macOS ARM64 App/DMG 打包流程
- [x] macOS 包内置已校验的官方 FUSE-T 安装器、许可和第三方归属声明
- [ ] 配置 Windows/Linux 签名及 macOS 签名公证凭据
- [x] 编写威胁模型，审查凭据、路径穿越、符号链接与日志泄漏风险

## 首个可用版本验收标准

- [ ] Windows 可将一个 SFTP 路径稳定映射为指定盘符
- [ ] Linux / macOS 可将同一配置挂载到指定目录
- [x] 支持基础文件浏览、创建、读取、修改、重命名和删除
- [ ] 异常断网后不会损坏已确认写入的数据，并能恢复连接
- [x] 密码和私钥口令不会以明文出现在磁盘、日志或 IPC 返回值中
- [x] 应用退出前能安全卸载所有由 GugleFS 创建的挂载点
