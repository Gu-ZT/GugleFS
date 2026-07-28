use guglefs_core::{
    AuthMethod, ConnectionSecrets, EngineError, MappingConfig, MappingRuntime, MappingState,
    MountDriver, Protocol, RemoteFileSystem,
};
use guglefs_remote::{inspect_host_key, FtpFileSystem, SftpFileSystem, WebDavFileSystem};
use serde::Serialize;
use tauri::{path::BaseDirectory, AppHandle, Manager, State};

use crate::{
    security::{AuthStatus, SecurityManager, TotpSetup},
    AppState,
};

type CommandResult<T> = Result<T, String>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    os: &'static str,
    default_mount_point: String,
    secure_store: &'static str,
    macfuse_required: bool,
    macfuse_installed: bool,
    macfuse_installer_bundled: bool,
}

const MACFUSE_INSTALLER_NAME: &str = "macfuse-5.3.3.dmg";

#[tauri::command]
pub fn get_platform_info(app: AppHandle) -> CommandResult<PlatformInfo> {
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
    let macfuse_installer_bundled = if cfg!(target_os = "macos") {
        bundled_macfuse_installer(&app).is_ok()
    } else {
        false
    };
    Ok(PlatformInfo {
        os: std::env::consts::OS,
        default_mount_point,
        secure_store: crate::security::secure_store_name(),
        macfuse_required: cfg!(target_os = "macos"),
        macfuse_installed: macfuse_is_installed(),
        macfuse_installer_bundled,
    })
}

#[tauri::command]
pub fn open_macfuse_installer(app: AppHandle) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        let installer = bundled_macfuse_installer(&app)?;
        std::process::Command::new("open")
            .arg(installer)
            .spawn()
            .map_err(|error| format!("打开 macFUSE 安装器失败: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("macFUSE 安装器仅适用于 macOS".into())
    }
}

fn bundled_macfuse_installer(app: &AppHandle) -> CommandResult<std::path::PathBuf> {
    let path = app
        .path()
        .resolve(MACFUSE_INSTALLER_NAME, BaseDirectory::Resource)
        .map_err(|error| format!("定位内置 macFUSE 安装器失败: {error}"))?;
    if !path.is_file() {
        return Err(format!("内置 macFUSE 安装器不存在: {}", path.display()));
    }
    Ok(path)
}

fn macfuse_is_installed() -> bool {
    if !cfg!(target_os = "macos") {
        return true;
    }
    [
        "/Library/Filesystems/macfuse.fs",
        "/Library/Frameworks/macFUSE.framework",
    ]
    .iter()
    .any(|path| std::path::Path::new(path).exists())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupMountResult {
    mappings: Vec<MappingRuntime>,
    attempted: usize,
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
pub async fn lock_app(state: State<'_, AppState>) -> CommandResult<AuthStatus> {
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
        match state
            .mount_driver
            .unmount(&runtime.config.mount_point)
            .await
        {
            Ok(()) => {
                if let Err(error) =
                    state
                        .manager
                        .finish_mount(&runtime.config.id, MappingState::Unmounted, None)
                {
                    failures.push(format!("{}: {error}", runtime.config.name));
                }
            }
            Err(error) => {
                let message = error.to_string();
                let _ = state.manager.finish_mount(
                    &runtime.config.id,
                    MappingState::Mounted,
                    Some(message.clone()),
                );
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
    state.security.lock()
}

#[tauri::command]
pub fn list_mappings(state: State<'_, AppState>) -> CommandResult<Vec<MappingRuntime>> {
    state.security.require_unlocked()?;
    state.manager.list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_mapping(
    state: State<'_, AppState>,
    config: MappingConfig,
    password: Option<String>,
    private_key: Option<String>,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    save_mapping_and_credentials(
        &state,
        config,
        normalized_secret(password),
        normalized_secret(private_key),
    )
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
    let remote: Box<dyn RemoteFileSystem> = match config.protocol {
        Protocol::Ftp => Box::new(
            FtpFileSystem::from_config(&config, secrets.credential)
                .map_err(|error| error.to_string())?,
        ),
        Protocol::Webdav => Box::new(
            WebDavFileSystem::from_config(&config, secrets.credential)
                .map_err(|error| error.to_string())?,
        ),
        Protocol::Sftp => Box::new(
            SftpFileSystem::from_config(&config, secrets).map_err(|error| error.to_string())?,
        ),
    };
    remote.connect().await.map_err(|error| error.to_string())?;
    remote
        .metadata("/")
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn mount_mapping(
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
        if let Err(message) = mount_by_id(&state, &id, None, None, true).await {
            let _ = state
                .manager
                .finish_mount(&id, MappingState::Error, Some(message));
        }
    }
    Ok(StartupMountResult {
        mappings: state.manager.list().map_err(|error| error.to_string())?,
        attempted,
    })
}

#[tauri::command]
pub async fn unmount_mapping(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
    if runtime.state != MappingState::Mounted {
        return Err(EngineError::NotMounted(id).to_string());
    }

    let was_remembered = state.mount_state.contains(&id)?;
    if was_remembered {
        state.mount_state.forget(&id)?;
    }
    let result = state
        .mount_driver
        .unmount(&runtime.config.mount_point)
        .await;
    match result {
        Ok(()) => state
            .manager
            .finish_mount(&runtime.config.id, MappingState::Unmounted, None)
            .map_err(|error| error.to_string()),
        Err(error) => {
            if was_remembered {
                let _ = state.mount_state.remember(&id);
            }
            let message = error.to_string();
            let _ = state.manager.finish_mount(
                &runtime.config.id,
                MappingState::Mounted,
                Some(message.clone()),
            );
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
    let result = state.mount_driver.mount(&runtime.config, secrets).await;
    match result {
        Ok(()) => {
            let runtime = state
                .manager
                .finish_mount(id, MappingState::Mounted, None)
                .map_err(|error| error.to_string())?;
            if remember_after_mount && has_persisted_authentication(&runtime.config) {
                if let Err(error) = state.mount_state.remember(id) {
                    let _ = state
                        .mount_driver
                        .unmount(&runtime.config.mount_point)
                        .await;
                    let _ = state.manager.finish_mount(
                        id,
                        MappingState::Unmounted,
                        Some(error.clone()),
                    );
                    return Err(format!("保存挂载恢复状态失败，映射已取消: {error}"));
                }
            }
            Ok(runtime)
        }
        Err(error) => {
            let message = error.to_string();
            let _ = state
                .manager
                .finish_mount(id, MappingState::Error, Some(message.clone()));
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
        AuthMethod::Anonymous => None,
    }
}

fn private_key_id(config: &MappingConfig) -> Option<&str> {
    match &config.auth {
        AuthMethod::PrivateKey { key_id, .. } => key_id.as_deref(),
        _ => None,
    }
}

fn has_persisted_authentication(config: &MappingConfig) -> bool {
    match &config.auth {
        AuthMethod::Password { credential_id } => credential_id.is_some(),
        AuthMethod::PrivateKey {
            key_path, key_id, ..
        } => key_path.is_some() || key_id.is_some(),
        AuthMethod::Anonymous => true,
    }
}

fn should_restore_startup_mapping(runtime: &MappingRuntime, was_remembered: bool) -> bool {
    let restore_previous = was_remembered && has_persisted_authentication(&runtime.config);
    !runtime.config.sftp_totp_required
        && (runtime.config.auto_mount || restore_previous)
        && !matches!(
            runtime.state,
            MappingState::Mounted | MappingState::Mounting
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
}
