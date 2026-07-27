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
- [ ] 配置 CI

## P1：核心文件系统语义

- [x] 定义完整 VFS 接口：`lookup`、`getattr`、`readdir`、`open`、`read`、`write`、`flush`、`release`
- [x] 补充目录创建、重命名、删除、截断和时间戳操作
- [x] 建立统一错误码，将远端错误映射为 POSIX / Windows 文件系统错误
- [x] 定义文件句柄、目录句柄与并发访问生命周期
- [ ] 设计元数据缓存、目录缓存和负缓存策略
- [ ] 设计分块读写、预读、写回与失败恢复策略
- [ ] 增加限流、超时、重试和断线重连
- [x] 定义统一远程文件操作抽象（元数据、目录、范围读写、创建、删除、重命名）
- [x] 为核心语义编写与平台无关的测试套件

## P1：远程协议

- [ ] 选型并接入 Rust FTP 客户端库
- [ ] 选型并接入 Rust SSH/SFTP 客户端库（优先验证 Windows 构建链）
- [ ] FTP：支持显式 FTPS，并明确是否支持隐式 FTPS
- [ ] FTP：处理被动模式、UTF-8、时区和不可靠 `LIST` 格式
- [ ] SFTP：支持密码、私钥和 SSH Agent
- [ ] SFTP：实现 known_hosts 校验和首次连接确认流程
- [x] WebDAV：接入 Rust HTTPS/WebDAV 客户端基础依赖，首个版本仅允许 HTTPS
- [x] WebDAV：实现 `PROPFIND`、`GET`、`PUT`、`MKCOL`、`MOVE` 和 `DELETE` 基础请求
- [x] WebDAV：将运行时凭据接入连接测试
- [ ] WebDAV：将运行时凭据接入挂载编排
- [x] WebDAV：支持 Basic 认证
- [ ] WebDAV：支持 Digest，评估 Bearer Token 与客户端证书
- [ ] WebDAV：正确处理 ETag 和条件请求
- [x] WebDAV：正确处理 Range 请求和同源重定向
- [ ] WebDAV：实现 `LOCK` / `UNLOCK` 或明确无锁服务器的并发写入策略
- [ ] WebDAV：验证 Nextcloud、ownCloud、Apache mod_dav 和常见云存储兼容性
- [x] WebDAV：限制跨域重定向时的认证头转发，防止凭据泄漏
- [ ] 抽象连接池，避免每个文件操作重复建立连接
- [ ] 建立可注入的协议测试服务和集成测试

## P1：系统挂载

- [ ] Windows：选型并接入 WinFsp Rust bindings
- [ ] Windows：实现盘符检查、占用检测、挂载和安全卸载
- [ ] Windows：处理大小写、Windows 文件名限制和文件属性
- [ ] Linux：接入 FUSE3，完成挂载、卸载和权限映射
- [ ] macOS：验证 macFUSE API、签名、公证和卸载流程
- [ ] 处理休眠、网络切换、应用退出和系统关机
- [ ] 挂载操作移入独立后台任务，向前端推送状态事件

## P1：配置与凭据

- [ ] 使用系统凭据库保存密码和私钥口令，禁止写入配置文件或日志
- [ ] 实现配置迁移、导入与导出（导出不包含凭据）
- [ ] 启动时恢复 `auto_mount` 配置
- [ ] 增加连接测试及远程路径选择
- [ ] 校验重复盘符、重复挂载目录和非法平台路径

## P2：桌面体验

- [ ] 增加挂载 / 卸载操作和实时状态
- [ ] 增加系统托盘、开机启动和后台运行选项
- [ ] 增加结构化日志、日志轮转和诊断包导出
- [ ] 增加更新机制和崩溃恢复
- [ ] 完善键盘操作、屏幕阅读器标签和多语言

## P2：质量与发布

- [ ] CI 覆盖 Windows、Linux、macOS 的格式检查、Clippy、测试和前端构建
- [ ] 增加 `rustfmt`、Clippy 和前端静态检查门禁
- [ ] 使用本地 FTP / SFTP 容器执行协议集成测试
- [ ] 使用内存后端执行 WinFsp / FUSE 一致性测试
- [ ] 压测大文件、海量小文件、并发读写和高延迟网络
- [ ] 建立 Windows 签名、macOS 签名公证和 Linux 打包流程
- [ ] 编写威胁模型，审查凭据、路径穿越、符号链接与日志泄漏风险

## 首个可用版本验收标准

- [ ] Windows 可将一个 SFTP 路径稳定映射为指定盘符
- [ ] Linux / macOS 可将同一配置挂载到指定目录
- [ ] 支持基础文件浏览、创建、读取、修改、重命名和删除
- [ ] 异常断网后不会损坏已确认写入的数据，并能恢复连接
- [ ] 密码和私钥口令不会以明文出现在磁盘、日志或 IPC 返回值中
- [ ] 应用退出前能安全卸载所有由 GugleFS 创建的挂载点
