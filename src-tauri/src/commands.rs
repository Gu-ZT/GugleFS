use guglefs_core::{
    AuthMethod, EngineError, MappingConfig, MappingRuntime, MappingState, MountDriver, Protocol,
    RemoteFileSystem,
};
use guglefs_remote::{FtpFileSystem, WebDavFileSystem};
use serde::Serialize;
use tauri::State;

use crate::{
    security::{AuthStatus, SecurityManager, TotpSetup},
    AppState,
};

type CommandResult<T> = Result<T, String>;

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
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    save_mapping_and_credential(&state, config, normalized_password(password))
}

#[tauri::command]
pub fn delete_mapping(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.security.require_unlocked()?;
    let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
    let credential_id = credential_id(&runtime.config).map(str::to_string);
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
    Ok(())
}

#[tauri::command]
pub async fn test_remote_connection(
    state: State<'_, AppState>,
    config: MappingConfig,
    password: Option<String>,
) -> CommandResult<()> {
    state.security.require_unlocked()?;
    config.validate().map_err(|error| error.to_string())?;
    let password = resolve_password(&state, &config, normalized_password(password))?;
    let remote: Box<dyn RemoteFileSystem> = match config.protocol {
        Protocol::Ftp => Box::new(
            FtpFileSystem::from_config(&config, password).map_err(|error| error.to_string())?,
        ),
        Protocol::Webdav => Box::new(
            WebDavFileSystem::from_config(&config, password).map_err(|error| error.to_string())?,
        ),
        Protocol::Sftp => {
            return Err(EngineError::NotImplemented("SFTP adapter".into()).to_string())
        }
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
    remember: bool,
) -> CommandResult<MappingRuntime> {
    state.security.require_unlocked()?;
    let _operation = state.mount_operations.lock().await;
    let password = normalized_password(password);
    if remember {
        if let Some(password) = password.as_deref() {
            let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
            save_mapping_and_credential(&state, runtime.config, Some(password.to_string()))?;
        }
    }
    let remember_after_mount = password.is_none() || remember;
    mount_by_id(&state, &id, password, remember_after_mount).await
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
            let restore_previous =
                remembered.contains(&runtime.config.id) && credential_id(&runtime.config).is_some();
            (runtime.config.auto_mount || restore_previous)
                && !matches!(
                    runtime.state,
                    MappingState::Mounted | MappingState::Mounting
                )
        })
        .map(|runtime| runtime.config.id)
        .collect();

    let attempted = mapping_ids.len();
    for id in mapping_ids {
        if let Err(message) = mount_by_id(&state, &id, None, true).await {
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

fn save_mapping_and_credential(
    state: &AppState,
    mut config: MappingConfig,
    password: Option<String>,
) -> CommandResult<MappingRuntime> {
    let existing = state.manager.get(&config.id).ok();
    if let AuthMethod::Password { credential_id } = &mut config.auth {
        if password.is_some() {
            *credential_id = Some(SecurityManager::mapping_credential_id(&config.id));
        } else if credential_id.is_none() {
            *credential_id = existing.as_ref().and_then(|runtime| {
                if let AuthMethod::Password { credential_id } = &runtime.config.auth {
                    credential_id.clone()
                } else {
                    None
                }
            });
        }
    } else if password.is_some() {
        return Err("当前认证方式不能保存密码".into());
    }

    let runtime = state
        .manager
        .upsert(config)
        .map_err(|error| error.to_string())?;
    if let Some(password) = password {
        let credential_id =
            credential_id(&runtime.config).ok_or_else(|| "密码配置缺少凭据引用".to_string())?;
        if let Err(error) = state
            .security
            .store_mapping_password(credential_id, &password)
        {
            rollback_mapping(state, existing, &runtime.config.id);
            return Err(error);
        }
    }
    if let Err(error) = state.manager.save_to_path(&state.config_path) {
        rollback_mapping(state, existing, &runtime.config.id);
        return Err(error.to_string());
    }
    Ok(runtime)
}

async fn mount_by_id(
    state: &AppState,
    id: &str,
    supplied_password: Option<String>,
    remember_after_mount: bool,
) -> CommandResult<MappingRuntime> {
    let current = state.manager.get(id).map_err(|error| error.to_string())?;
    let password = resolve_password(state, &current.config, supplied_password)?;
    let runtime = state
        .manager
        .begin_mount(id)
        .map_err(|error| error.to_string())?;
    let result = state.mount_driver.mount(&runtime.config, password).await;
    match result {
        Ok(()) => {
            let runtime = state
                .manager
                .finish_mount(id, MappingState::Mounted, None)
                .map_err(|error| error.to_string())?;
            if remember_after_mount && credential_id(&runtime.config).is_some() {
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

fn resolve_password(
    state: &AppState,
    config: &MappingConfig,
    supplied_password: Option<String>,
) -> CommandResult<Option<String>> {
    if supplied_password.is_some() {
        return Ok(supplied_password);
    }
    let direct_credential = credential_id(config).map(str::to_string);
    let credential = direct_credential.or_else(|| {
        state
            .manager
            .get(&config.id)
            .ok()
            .and_then(|runtime| credential_id(&runtime.config).map(str::to_string))
    });
    let Some(credential) = credential else {
        return Ok(None);
    };
    state
        .security
        .mapping_password(&credential)?
        .map(Some)
        .ok_or_else(|| "系统凭据库中没有该映射的密码，请重新输入并保存".into())
}

fn credential_id(config: &MappingConfig) -> Option<&str> {
    match &config.auth {
        AuthMethod::Password { credential_id } | AuthMethod::PrivateKey { credential_id, .. } => {
            credential_id.as_deref()
        }
        AuthMethod::Anonymous => None,
    }
}

fn normalized_password(password: Option<String>) -> Option<String> {
    password.filter(|value| !value.is_empty())
}

fn rollback_mapping(state: &AppState, existing: Option<MappingRuntime>, mapping_id: &str) {
    if let Some(existing) = existing {
        let _ = state.manager.upsert(existing.config);
    } else {
        let _ = state.manager.remove(mapping_id);
    }
}
