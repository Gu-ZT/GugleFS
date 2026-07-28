use std::{collections::HashSet, path::PathBuf};

use guglefs_core::{
    AuthMethod, ConfigDocument, ConnectionSecrets, EntryKind, MappingConfig, MappingManager,
    MappingRuntime, MappingState, MountDriver, Protocol, RemoteFileSystem,
};
use guglefs_remote::{
    inspect_host_key, known_host_fingerprints, FtpFileSystem, SftpFileSystem, WebDavFileSystem,
};
use serde::Serialize;
use tauri::{path::BaseDirectory, AppHandle, Emitter, Manager, State};

use crate::{
    security::{AuthStatus, SecurityManager, TotpSetup},
    AppState,
};

type CommandResult<T> = Result<T, String>;
const MAPPING_RUNTIME_EVENT: &str = "mapping-runtime";

fn emit_mapping_runtime(app: &AppHandle, runtime: &MappingRuntime) {
    let _ = app.emit(MAPPING_RUNTIME_EVENT, runtime);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    os: &'static str,
    default_mount_point: String,
    secure_store: &'static str,
    fuse_t_required: bool,
    fuse_t_installed: bool,
    fuse_t_installer_bundled: bool,
    previous_session_unclean: bool,
}

const FUSE_T_INSTALLER_NAME: &str = "fuse-t-macos-installer-1.2.7.pkg";

#[tauri::command]
pub fn get_platform_info(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<PlatformInfo> {
    let default_mount_point = if cfg!(target_os = "windows") {
        "Z:".to_string()
    } else {
        app.path()
            .home_dir()
            .map_err(|error| format!("读取用户目录失败: {error}"))?
            .join("GugleFS")
            .to_string_lossy()
            .into_owned()
    };
    let fuse_t_installer_bundled = if cfg!(target_os = "macos") {
        bundled_fuse_t_installer(&app).is_ok()
    } else {
        false
    };
    Ok(PlatformInfo {
        os: std::env::consts::OS,
        default_mount_point,
        secure_store: crate::security::secure_store_name(),
        fuse_t_required: cfg!(target_os = "macos"),
        fuse_t_installed: fuse_t_is_installed(),
        fuse_t_installer_bundled,
        previous_session_unclean: state.session_state.previous_session_unclean(),
    })
}

#[tauri::command]
pub fn open_fuse_t_installer(app: AppHandle) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        let installer = bundled_fuse_t_installer(&app)?;
        std::process::Command::new("open")
            .arg(installer)
            .spawn()
            .map_err(|error| format!("打开 FUSE-T 安装器失败: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("FUSE-T 安装器仅适用于 macOS".into())
    }
}

fn bundled_fuse_t_installer(app: &AppHandle) -> CommandResult<std::path::PathBuf> {
    let path = app
        .path()
        .resolve(FUSE_T_INSTALLER_NAME, BaseDirectory::Resource)
        .map_err(|error| format!("定位内置 FUSE-T 安装器失败: {error}"))?;
    if !path.is_file() {
        return Err(format!("内置 FUSE-T 安装器不存在: {}", path.display()));
    }
    Ok(path)
}

fn fuse_t_is_installed() -> bool {
    if !cfg!(target_os = "macos") {
        return true;
    }
    std::path::Path::new("/usr/local/lib/libfuse-t.dylib").is_file()
        && std::path::Path::new("/Library/Application Support/fuse-t").is_dir()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupMountResult {
    mappings: Vec<MappingRuntime>,
    attempted: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportMappingsResult {
    mappings: Vec<MappingRuntime>,
    imported: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectory {
    path: String,
    name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBrowserListing {
    path: String,
    directories: Vec<RemoteDirectory>,
}

pub struct RemoteBrowserSession {
    root: String,
    remote: Box<dyn RemoteFileSystem>,
}

#[tauri::command]
pub fn get_auth_status(state: State<'_, AppState>) -> CommandResult<AuthStatus> {
    state.security.status()
}

#[tauri::command]
pub fn begin_2fa_setup(state: State<'_, AppState>) -> CommandResult<TotpSetup> {
    state.security.begin_setup()
}

#[tauri::command]
pub fn confirm_2fa_setup(state: State<'_, AppState>, code: String) -> CommandResult<AuthStatus> {
    state.security.confirm_setup(&code)
}

#[tauri::command]
pub fn unlock_app(state: State<'_, AppState>, code: String) -> CommandResult<AuthStatus> {
    state.security.unlock(&code)
}

#[tauri::command]
pub async fn lock_app(app: AppHandle, state: State<'_, AppState>) -> CommandResult<AuthStatus> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let mounted: Vec<_> = state
        .manager
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|runtime| runtime.state == MappingState::Mounted)
        .collect();
    let mut failures = Vec::new();
    for runtime in mounted {
        let unmounting = state
            .manager
            .begin_unmount(&runtime.config.id)
            .map_err(|error| error.to_string())?;
        emit_mapping_runtime(&app, &unmounting);
        match state
            .mount_driver
            .unmount(&runtime.config.mount_point)
            .await
        {
            Ok(()) => {
                match state
                    .manager
                    .finish_mount(&runtime.config.id, MappingState::Unmounted, None)
                {
                    Ok(unmounted) => emit_mapping_runtime(&app, &unmounted),
                    Err(error) => failures.push(format!("{}: {error}", runtime.config.name)),
                }
            }
            Err(error) => {
                let message = error.to_string();
                if let Ok(failed) = state.manager.finish_mount(
                    &runtime.config.id,
                    MappingState::Mounted,
                    Some(message.clone()),
                ) {
                    emit_mapping_runtime(&app, &failed);
                }
                failures.push(format!("{}: {message}", runtime.config.name));
            }
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "无法锁定，{} 个映射卸载失败: {}",
            failures.len(),
            failures.join("; ")
        ));
    }
    let browser_sessions: Vec<_> = state
        .remote_browsers
        .lock()
        .await
        .drain()
        .map(|(_, session)| session)
        .collect();
    for session in browser_sessions {
        let _ = session.remote.disconnect().await;
    }
    state.security.lock()
}

#[tauri::command]
pub fn list_mappings(state: State<'_, AppState>) -> CommandResult<Vec<MappingRuntime>> {
    state.security.require_unlocked()?;
    state.manager.list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn export_mappings(state: State<'_, AppState>, path: PathBuf) -> CommandResult<usize> {
    state.security.require_unlocked()?;
    if path == state.config_path {
        return Err("不能覆盖应用正在使用的配置文件".into());
    }
    let mappings: Vec<_> = state
        .manager
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|runtime| runtime.config.portable())
        .collect();
    let exported = mappings.len();
    ConfigDocument::current(mappings)
        .save_to_path(&path)
        .map_err(|error| error.to_string())?;
    state.diagnostics.record("config_export", None, "success");
    Ok(exported)
}

#[tauri::command]
pub fn import_mappings(
    state: State<'_, AppState>,
    path: PathBuf,
) -> CommandResult<ImportMappingsResult> {
    state.security.require_unlocked()?;
    if path == state.config_path {
        return Err("不能从应用正在使用的配置文件导入".into());
    }
    let document = ConfigDocument::load_from_path(&path).map_err(|error| error.to_string())?;
    let existing = state.manager.list().map_err(|error| error.to_string())?;
    let mut used_ids: HashSet<_> = existing
        .iter()
        .map(|runtime| runtime.config.id.clone())
        .collect();
    let mut imported = Vec::with_capacity(document.mappings.len());
    for mut config in document.mappings.into_iter().map(MappingConfig::portable) {
        let base_id = config.id.clone();
        let mut suffix = 1;
        while !used_ids.insert(config.id.clone()) {
            config.id = format!("{base_id}-imported-{suffix}");
            suffix += 1;
        }
        imported.push(config);
    }

    let combined = existing
        .iter()
        .map(|runtime| runtime.config.clone())
        .chain(imported.iter().cloned());
    let candidate = MappingManager::from_configs(combined).map_err(|error| error.to_string())?;
    candidate
        .save_to_path(&state.config_path)
        .map_err(|error| error.to_string())?;
    for config in &imported {
        state
            .manager
            .upsert(config.clone())
            .map_err(|error| error.to_string())?;
    }
    state.diagnostics.record("config_import", None, "success");
    Ok(ImportMappingsResult {
        mappings: state.manager.list().map_err(|error| error.to_string())?,
        imported: imported.len(),
    })
}

#[tauri::command]
pub fn export_diagnostics(state: State<'_, AppState>, path: PathBuf) -> CommandResult<usize> {
    state.security.require_unlocked()?;
    let mappings = state.manager.list().map_err(|error| error.to_string())?;
    let exported = state.diagnostics.export_report(&path, mappings)?;
    state
        .diagnostics
        .record("diagnostics_export", None, "success");
    Ok(exported)
}

/// Windows 下返回已被占用的盘符：本地驱动器，以及 GugleFS 当前已挂载或
/// 正在挂载的映射。未挂载的映射配置不计入。其他平台始终返回空列表。
#[tauri::command]
pub fn occupied_drive_letters(state: State<'_, AppState>) -> CommandResult<Vec<String>> {
    state.security.require_unlocked()?;
    if !cfg!(target_os = "windows") {
        return Ok(Vec::new());
    }
    let mut occupied: Vec<String> = ('A'..='Z')
        .filter(|letter| std::path::Path::new(&format!("{letter}:\\")).exists())
        .map(|letter| letter.to_string())
        .collect();
    if let Ok(runtimes) = state.manager.list() {
        for runtime in runtimes {
            if !matches!(
                runtime.state,
                MappingState::Mounted | MappingState::Mounting | MappingState::Unmounting
            ) {
                continue;
            }
            if let Some(letter) = drive_letter_of(&runtime.config.mount_point) {
                if !occupied.contains(&letter) {
                    occupied.push(letter);
                }
            }
        }
    }
    Ok(occupied)
}

fn drive_letter_of(mount_point: &str) -> Option<String> {
    let mut chars = mount_point.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    if letter.is_ascii_alphabetic() && chars.next() == Some(':') {
        Some(letter.to_string())
    } else {
        None
    }
}

#[tauri::command]
pub fn save_mapping(
    state: State<'_, AppState>,
    config: MappingConfig,
    password: Option<String>,
    private_key: Option<String>,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    let protocol = config.protocol;
    let result = save_mapping_and_credentials(
        &state,
        config,
        normalized_secret(password),
        normalized_secret(private_key),
    );
    state.diagnostics.record(
        "mapping_save",
        Some(protocol),
        if result.is_ok() { "success" } else { "failure" },
    );
    result
}

#[tauri::command]
pub fn delete_mapping(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.security.require_unlocked()?;
    let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
    let credential_id = credential_id(&runtime.config).map(str::to_string);
    let key_id = private_key_id(&runtime.config).map(str::to_string);
    state
        .manager
        .remove(&id)
        .map_err(|error| error.to_string())?;
    state
        .manager
        .save_to_path(&state.config_path)
        .map_err(|error| error.to_string())?;
    state.mount_state.forget(&id)?;
    if let Some(credential_id) = credential_id {
        state.security.delete_mapping_password(&credential_id)?;
    }
    if let Some(key_id) = key_id {
        state.security.delete_mapping_private_key(&key_id)?;
    }
    state
        .diagnostics
        .record("mapping_delete", Some(runtime.config.protocol), "success");
    Ok(())
}

#[tauri::command]
pub async fn inspect_sftp_host_key(
    state: State<'_, AppState>,
    host: String,
    port: u16,
    ignore_system_proxy: bool,
) -> CommandResult<String> {
    state.security.require_unlocked()?;
    validate_host_and_port(&host, port)?;
    inspect_host_key(host.trim(), port, ignore_system_proxy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn import_sftp_known_hosts(
    state: State<'_, AppState>,
    path: PathBuf,
    host: String,
    port: u16,
) -> CommandResult<Vec<String>> {
    state.security.require_unlocked()?;
    validate_host_and_port(&host, port)?;
    if !path.is_file() {
        return Err("选择的 OpenSSH known_hosts 文件不存在".into());
    }
    known_host_fingerprints(&path, host.trim(), port).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_remote_connection(
    state: State<'_, AppState>,
    config: MappingConfig,
    password: Option<String>,
    private_key: Option<String>,
    totp_code: Option<String>,
) -> CommandResult<()> {
    state.security.require_unlocked()?;
    let private_key = normalized_secret(private_key);
    config.validate().map_err(|error| error.to_string())?;
    let secrets = resolve_secrets(
        &state,
        &config,
        normalized_secret(password),
        private_key,
        normalized_secret(totp_code),
    )?;
    let remote = remote_file_system(&config, secrets)?;
    remote.connect().await.map_err(|error| error.to_string())?;
    remote
        .metadata("/")
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn open_remote_browser(
    state: State<'_, AppState>,
    mut config: MappingConfig,
    password: Option<String>,
    private_key: Option<String>,
    totp_code: Option<String>,
    session_id: String,
) -> CommandResult<RemoteBrowserListing> {
    state.security.require_unlocked()?;
    validate_remote_browser_session_id(&session_id)?;
    let root = normalize_remote_browser_path(&config.remote_path)?;
    config.remote_path = root.clone();
    config.validate().map_err(|error| error.to_string())?;
    let secrets = resolve_secrets(
        &state,
        &config,
        normalized_secret(password),
        normalized_secret(private_key),
        normalized_secret(totp_code),
    )?;
    let remote = remote_file_system(&config, secrets)?;
    remote.connect().await.map_err(|error| error.to_string())?;
    let directories = remote_directories(remote.as_ref(), &root, "/").await?;

    let previous = state.remote_browsers.lock().await.remove(&session_id);
    if let Some(previous) = previous {
        let _ = previous.remote.disconnect().await;
    }
    state.remote_browsers.lock().await.insert(
        session_id,
        RemoteBrowserSession {
            root: root.clone(),
            remote,
        },
    );
    Ok(RemoteBrowserListing {
        path: root,
        directories,
    })
}

#[tauri::command]
pub async fn list_remote_directories(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> CommandResult<RemoteBrowserListing> {
    state.security.require_unlocked()?;
    validate_remote_browser_session_id(&session_id)?;
    let path = normalize_remote_browser_path(&path)?;
    let sessions = state.remote_browsers.lock().await;
    let session = sessions
        .get(&session_id)
        .ok_or_else(|| "远程目录浏览会话已过期，请重新打开".to_string())?;
    let relative = browser_path_relative_to_root(&session.root, &path)?;
    let directories = remote_directories(session.remote.as_ref(), &session.root, &relative).await?;
    Ok(RemoteBrowserListing { path, directories })
}

#[tauri::command]
pub async fn close_remote_browser(
    state: State<'_, AppState>,
    session_id: String,
) -> CommandResult<()> {
    validate_remote_browser_session_id(&session_id)?;
    let session = state.remote_browsers.lock().await.remove(&session_id);
    if let Some(session) = session {
        let _ = session.remote.disconnect().await;
    }
    Ok(())
}

async fn remote_directories(
    remote: &dyn RemoteFileSystem,
    root: &str,
    relative_path: &str,
) -> CommandResult<Vec<RemoteDirectory>> {
    let mut directories: Vec<_> = remote
        .read_dir(relative_path)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|entry| entry.metadata.kind == EntryKind::Directory)
        .map(|entry| RemoteDirectory {
            path: absolute_browser_path(root, &entry.path),
            name: entry.name,
        })
        .collect();
    directories.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(directories)
}

fn remote_file_system(
    config: &MappingConfig,
    secrets: ConnectionSecrets,
) -> CommandResult<Box<dyn RemoteFileSystem>> {
    match config.protocol {
        Protocol::Ftp => Ok(Box::new(
            FtpFileSystem::from_config(config, secrets.credential)
                .map_err(|error| error.to_string())?,
        )),
        Protocol::Webdav => Ok(Box::new(
            WebDavFileSystem::from_config(config, secrets.credential)
                .map_err(|error| error.to_string())?,
        )),
        Protocol::Sftp => Ok(Box::new(
            SftpFileSystem::from_config(config, secrets).map_err(|error| error.to_string())?,
        )),
    }
}

fn normalize_remote_browser_path(path: &str) -> CommandResult<String> {
    let path = path.trim();
    if !path.starts_with('/') || path.contains('\\') {
        return Err("远程目录必须是以 / 开头的绝对路径".into());
    }
    let mut segments = Vec::new();
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        if matches!(segment, "." | "..") {
            return Err("远程目录不能包含 . 或 .. 路径段".into());
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn validate_remote_browser_session_id(session_id: &str) -> CommandResult<()> {
    if !(8..=128).contains(&session_id.len())
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("远程目录浏览会话 ID 无效".into());
    }
    Ok(())
}

fn absolute_browser_path(root: &str, relative: &str) -> String {
    if root == "/" {
        relative.to_string()
    } else if relative == "/" {
        root.to_string()
    } else {
        format!("{}{}", root.trim_end_matches('/'), relative)
    }
}

fn browser_path_relative_to_root(root: &str, path: &str) -> CommandResult<String> {
    if path == root {
        return Ok("/".into());
    }
    if root == "/" {
        return Ok(path.to_string());
    }
    let prefix = format!("{}/", root.trim_end_matches('/'));
    path.strip_prefix(&prefix)
        .map(|relative| format!("/{relative}"))
        .ok_or_else(|| "不能浏览初始远程目录之外的路径".into())
}

#[tauri::command]
pub async fn detect_sftp_mfa_requirement(
    state: State<'_, AppState>,
    config: MappingConfig,
    password: Option<String>,
    private_key: Option<String>,
) -> CommandResult<bool> {
    state.security.require_unlocked()?;
    if config.protocol != Protocol::Sftp {
        return Ok(false);
    }
    config.validate().map_err(|error| error.to_string())?;
    let secrets = resolve_secrets(
        &state,
        &config,
        normalized_secret(password),
        normalized_secret(private_key),
        None,
    )?;
    let remote =
        SftpFileSystem::from_config(&config, secrets).map_err(|error| error.to_string())?;
    remote
        .detect_mfa_requirement()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn mount_mapping(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    password: Option<String>,
    totp_code: Option<String>,
    remember: bool,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let password = normalized_secret(password);
    if remember {
        if let Some(password) = password.as_deref() {
            let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
            save_mapping_and_credentials(&state, runtime.config, Some(password.to_string()), None)?;
        }
    }
    let remember_after_mount = password.is_none() || remember;
    mount_by_id(
        &app,
        &state,
        &id,
        password,
        normalized_secret(totp_code),
        remember_after_mount,
    )
    .await
}

#[tauri::command]
pub async fn restore_startup_mappings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<StartupMountResult> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let remembered = state.mount_state.mounted_mapping_ids()?;
    let mapping_ids: Vec<_> = state
        .manager
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|runtime| {
            should_restore_startup_mapping(runtime, remembered.contains(&runtime.config.id))
        })
        .map(|runtime| runtime.config.id)
        .collect();

    let attempted = mapping_ids.len();
    for id in mapping_ids {
        if let Err(message) = mount_by_id(&app, &state, &id, None, None, true).await {
            let error_was_emitted = state
                .manager
                .get(&id)
                .is_ok_and(|runtime| runtime.state == MappingState::Error);
            if !error_was_emitted {
                if let Ok(failed) =
                    state
                        .manager
                        .finish_mount(&id, MappingState::Error, Some(message))
                {
                    emit_mapping_runtime(&app, &failed);
                }
            }
        }
    }
    Ok(StartupMountResult {
        mappings: state.manager.list().map_err(|error| error.to_string())?,
        attempted,
    })
}

#[tauri::command]
pub async fn unmount_mapping(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let runtime = state
        .manager
        .begin_unmount(&id)
        .map_err(|error| error.to_string())?;
    emit_mapping_runtime(&app, &runtime);

    let was_remembered = state.mount_state.contains(&id)?;
    if was_remembered {
        if let Err(error) = state.mount_state.forget(&id) {
            if let Ok(failed) = state.manager.finish_mount(
                &runtime.config.id,
                MappingState::Mounted,
                Some(error.clone()),
            ) {
                emit_mapping_runtime(&app, &failed);
            }
            return Err(error);
        }
    }
    let result = state
        .mount_driver
        .unmount(&runtime.config.mount_point)
        .await;
    let protocol = runtime.config.protocol;
    match result {
        Ok(()) => {
            let result = state
                .manager
                .finish_mount(&runtime.config.id, MappingState::Unmounted, None)
                .map_err(|error| error.to_string());
            if let Ok(unmounted) = &result {
                emit_mapping_runtime(&app, unmounted);
            }
            state.diagnostics.record(
                "mapping_unmount",
                Some(protocol),
                if result.is_ok() { "success" } else { "failure" },
            );
            result
        }
        Err(error) => {
            if was_remembered {
                let _ = state.mount_state.remember(&id);
            }
            let message = error.to_string();
            if let Ok(failed) = state.manager.finish_mount(
                &runtime.config.id,
                MappingState::Mounted,
                Some(message.clone()),
            ) {
                emit_mapping_runtime(&app, &failed);
            }
            state
                .diagnostics
                .record("mapping_unmount", Some(protocol), "failure");
            Err(message)
        }
    }
}

fn save_mapping_and_credentials(
    state: &AppState,
    mut config: MappingConfig,
    credential: Option<String>,
    private_key: Option<String>,
) -> CommandResult<MappingRuntime> {
    let existing = state.manager.get(&config.id).ok();
    let old_credential_id = existing
        .as_ref()
        .and_then(|runtime| credential_id(&runtime.config))
        .map(str::to_string);
    let old_private_key_id = existing
        .as_ref()
        .and_then(|runtime| private_key_id(&runtime.config))
        .map(str::to_string);
    let mut stored_private_key_id = None;

    match &mut config.auth {
        AuthMethod::Password { credential_id } => {
            if private_key.is_some() {
                return Err("密码认证不能保存 SSH 私钥".into());
            }
            if credential.is_some() {
                *credential_id = Some(SecurityManager::mapping_credential_id(&config.id));
            } else if credential_id.is_none() {
                *credential_id = existing
                    .as_ref()
                    .and_then(|runtime| match &runtime.config.auth {
                        AuthMethod::Password { credential_id } => credential_id.clone(),
                        _ => None,
                    });
            }
        }
        AuthMethod::PrivateKey {
            key_path,
            key_id,
            credential_id,
        } => {
            *key_path = key_path
                .take()
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty());
            if let Some(private_key) = private_key.as_deref() {
                let new_key_id = SecurityManager::new_private_key_id(&config.id);
                state
                    .security
                    .store_mapping_private_key(&new_key_id, private_key)?;
                *key_path = None;
                *key_id = Some(new_key_id.clone());
                stored_private_key_id = Some(new_key_id);
            } else if key_path.is_some() {
                *key_id = None;
            } else if key_id.is_none() {
                if let Some(AuthMethod::PrivateKey {
                    key_path: existing_path,
                    key_id: existing_key_id,
                    ..
                }) = existing.as_ref().map(|runtime| &runtime.config.auth)
                {
                    *key_path = existing_path.clone();
                    *key_id = existing_key_id.clone();
                }
            }
            if credential.is_some() {
                *credential_id = Some(SecurityManager::mapping_credential_id(&config.id));
            } else if credential_id.is_none() {
                *credential_id = existing
                    .as_ref()
                    .and_then(|runtime| match &runtime.config.auth {
                        AuthMethod::PrivateKey { credential_id, .. } => credential_id.clone(),
                        _ => None,
                    });
            }
        }
        AuthMethod::SshAgent => {
            if credential.is_some() || private_key.is_some() {
                return Err("SSH Agent 认证不能保存密码或私钥".into());
            }
        }
        AuthMethod::Anonymous => {
            if credential.is_some() || private_key.is_some() {
                return Err("匿名认证不能保存密码或 SSH 私钥".into());
            }
        }
    }

    let runtime = match state.manager.upsert(config) {
        Ok(runtime) => runtime,
        Err(error) => {
            if let Some(key_id) = stored_private_key_id.as_deref() {
                let _ = state.security.delete_mapping_private_key(key_id);
            }
            return Err(error.to_string());
        }
    };
    if let Some(credential) = credential {
        let credential_id =
            credential_id(&runtime.config).ok_or_else(|| "配置缺少凭据引用".to_string())?;
        if let Err(error) = state
            .security
            .store_mapping_password(credential_id, &credential)
        {
            rollback_mapping(state, existing.clone(), &runtime.config.id);
            if let Some(key_id) = stored_private_key_id.as_deref() {
                let _ = state.security.delete_mapping_private_key(key_id);
            }
            return Err(error);
        }
    }
    if let Err(error) = state.manager.save_to_path(&state.config_path) {
        rollback_mapping(state, existing.clone(), &runtime.config.id);
        if let Some(key_id) = stored_private_key_id.as_deref() {
            let _ = state.security.delete_mapping_private_key(key_id);
        }
        return Err(error.to_string());
    }

    let new_credential_id = credential_id(&runtime.config);
    if old_credential_id
        .as_deref()
        .is_some_and(|old| Some(old) != new_credential_id)
    {
        state
            .security
            .delete_mapping_password(old_credential_id.as_deref().expect("checked above"))?;
    }
    let new_private_key_id = private_key_id(&runtime.config);
    if old_private_key_id
        .as_deref()
        .is_some_and(|old| Some(old) != new_private_key_id)
    {
        state
            .security
            .delete_mapping_private_key(old_private_key_id.as_deref().expect("checked above"))?;
    }
    Ok(runtime)
}

async fn mount_by_id(
    app: &AppHandle,
    state: &AppState,
    id: &str,
    supplied_password: Option<String>,
    supplied_totp_code: Option<String>,
    remember_after_mount: bool,
) -> CommandResult<MappingRuntime> {
    let current = state.manager.get(id).map_err(|error| error.to_string())?;
    let secrets = resolve_secrets(
        state,
        &current.config,
        supplied_password,
        None,
        supplied_totp_code,
    )?;
    let runtime = state
        .manager
        .begin_mount(id)
        .map_err(|error| error.to_string())?;
    emit_mapping_runtime(app, &runtime);
    let result = state.mount_driver.mount(&runtime.config, secrets).await;
    match result {
        Ok(()) => {
            let runtime = state
                .manager
                .finish_mount(id, MappingState::Mounted, None)
                .map_err(|error| error.to_string())?;
            emit_mapping_runtime(app, &runtime);
            if remember_after_mount && has_persisted_authentication(&runtime.config) {
                if let Err(error) = state.mount_state.remember(id) {
                    let _ = state
                        .mount_driver
                        .unmount(&runtime.config.mount_point)
                        .await;
                    if let Ok(unmounted) =
                        state
                            .manager
                            .finish_mount(id, MappingState::Unmounted, Some(error.clone()))
                    {
                        emit_mapping_runtime(app, &unmounted);
                    }
                    state.diagnostics.record(
                        "mapping_mount",
                        Some(runtime.config.protocol),
                        "failure",
                    );
                    return Err(format!("保存挂载恢复状态失败，映射已取消: {error}"));
                }
            }
            state
                .diagnostics
                .record("mapping_mount", Some(runtime.config.protocol), "success");
            Ok(runtime)
        }
        Err(error) => {
            let message = error.to_string();
            if let Ok(failed) =
                state
                    .manager
                    .finish_mount(id, MappingState::Error, Some(message.clone()))
            {
                emit_mapping_runtime(app, &failed);
            }
            state
                .diagnostics
                .record("mapping_mount", Some(runtime.config.protocol), "failure");
            Err(message)
        }
    }
}

fn resolve_secrets(
    state: &AppState,
    config: &MappingConfig,
    supplied_credential: Option<String>,
    supplied_private_key: Option<String>,
    supplied_totp_code: Option<String>,
) -> CommandResult<ConnectionSecrets> {
    let credential = if supplied_credential.is_some() {
        supplied_credential
    } else if let Some(credential_id) = credential_id(config) {
        Some(
            state
                .security
                .mapping_password(credential_id)?
                .ok_or_else(|| {
                    "系统凭据库中没有该映射的密码或私钥口令，请重新输入并保存".to_string()
                })?,
        )
    } else {
        None
    };
    let private_key = if supplied_private_key.is_some() {
        supplied_private_key
    } else if let Some(key_id) = private_key_id(config) {
        Some(
            state
                .security
                .mapping_private_key(key_id)?
                .ok_or_else(|| "系统凭据库中没有该映射的 SSH 私钥，请重新粘贴并保存".to_string())?,
        )
    } else {
        None
    };
    Ok(ConnectionSecrets {
        credential,
        private_key,
        totp_code: supplied_totp_code,
    })
}

fn credential_id(config: &MappingConfig) -> Option<&str> {
    match &config.auth {
        AuthMethod::Password { credential_id } | AuthMethod::PrivateKey { credential_id, .. } => {
            credential_id.as_deref()
        }
        AuthMethod::SshAgent | AuthMethod::Anonymous => None,
    }
}

fn private_key_id(config: &MappingConfig) -> Option<&str> {
    match &config.auth {
        AuthMethod::PrivateKey { key_id, .. } => key_id.as_deref(),
        _ => None,
    }
}

fn has_persisted_authentication(config: &MappingConfig) -> bool {
    if config.protocol == Protocol::Webdav {
        match config.webdav_auth {
            guglefs_core::WebDavAuthMethod::ClientCertificate => {
                return config.webdav_client_certificate_path.is_some();
            }
            guglefs_core::WebDavAuthMethod::Anonymous => return true,
            _ => {}
        }
    }
    match &config.auth {
        AuthMethod::Password { credential_id } => credential_id.is_some(),
        AuthMethod::PrivateKey {
            key_path, key_id, ..
        } => key_path.is_some() || key_id.is_some(),
        AuthMethod::SshAgent | AuthMethod::Anonymous => true,
    }
}

fn should_restore_startup_mapping(runtime: &MappingRuntime, was_remembered: bool) -> bool {
    let restore_previous = was_remembered && has_persisted_authentication(&runtime.config);
    !runtime.config.sftp_totp_required
        && (runtime.config.auto_mount || restore_previous)
        && !matches!(
            runtime.state,
            MappingState::Mounted | MappingState::Mounting | MappingState::Unmounting
        )
}

fn normalized_secret(secret: Option<String>) -> Option<String> {
    secret.filter(|value| !value.is_empty())
}

fn validate_host_and_port(host: &str, port: u16) -> CommandResult<()> {
    let host = host.trim();
    if host.is_empty()
        || port == 0
        || host.chars().any(|character| {
            character.is_whitespace() || matches!(character, '/' | '?' | '#' | '@')
        })
    {
        return Err("SSH 服务器地址或端口无效".into());
    }
    Ok(())
}

fn rollback_mapping(state: &AppState, existing: Option<MappingRuntime>, mapping_id: &str) {
    if let Some(existing) = existing {
        let _ = state.manager.upsert(existing.config);
    } else {
        let _ = state.manager.remove(mapping_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_info_serializes_fuse_t_frontend_keys() {
        let value = serde_json::to_value(PlatformInfo {
            os: "macos",
            default_mount_point: "/Users/test/GugleFS".into(),
            secure_store: "macOS Keychain",
            fuse_t_required: true,
            fuse_t_installed: false,
            fuse_t_installer_bundled: true,
            previous_session_unclean: true,
        })
        .expect("platform info should serialize");

        assert_eq!(value["fuseTRequired"], true);
        assert_eq!(value["fuseTInstalled"], false);
        assert_eq!(value["fuseTInstallerBundled"], true);
        assert_eq!(value["previousSessionUnclean"], true);
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(7));
    }

    fn sftp_runtime(mfa_required: bool) -> MappingRuntime {
        MappingRuntime {
            config: MappingConfig {
                id: "sftp".into(),
                name: "SFTP".into(),
                protocol: Protocol::Sftp,
                host: "files.example.com".into(),
                port: 22,
                username: Some("user".into()),
                auth: AuthMethod::Password {
                    credential_id: Some("credential".into()),
                },
                remote_path: "/".into(),
                mount_point: "Z:".into(),
                ftp_tls: false,
                host_key_fingerprint: Some("SHA256:test".into()),
                sftp_totp_required: mfa_required,
                ignore_system_proxy: false,
                webdav_auth: Default::default(),
                webdav_client_certificate_path: None,
                auto_mount: true,
            },
            state: MappingState::Unmounted,
            last_error: None,
        }
    }

    #[test]
    fn startup_restore_skips_mfa_mappings() {
        assert!(!should_restore_startup_mapping(&sftp_runtime(true), true));
        assert!(should_restore_startup_mapping(&sftp_runtime(false), true));
    }

    #[test]
    fn remote_browser_normalizes_absolute_paths() {
        assert_eq!(normalize_remote_browser_path("/").unwrap(), "/");
        assert_eq!(
            normalize_remote_browser_path(" /团队/文档/ ").unwrap(),
            "/团队/文档"
        );
    }

    #[test]
    fn remote_browser_rejects_parent_and_windows_paths() {
        assert!(normalize_remote_browser_path("/data/../secret").is_err());
        assert!(normalize_remote_browser_path("C:\\data").is_err());
    }

    #[test]
    fn remote_browser_keeps_navigation_inside_its_initial_root() {
        assert_eq!(
            absolute_browser_path("/dav/user", "/docs"),
            "/dav/user/docs"
        );
        assert_eq!(
            browser_path_relative_to_root("/dav/user", "/dav/user/docs").unwrap(),
            "/docs"
        );
        assert!(browser_path_relative_to_root("/dav/user", "/dav/other").is_err());
    }
}
