use guglefs_core::{
    EngineError, MappingConfig, MappingRuntime, MappingState, MountDriver, Protocol,
    RemoteFileSystem,
};
use guglefs_remote::WebDavFileSystem;
use tauri::State;

use crate::AppState;

type CommandResult<T> = Result<T, String>;

#[tauri::command]
pub fn list_mappings(state: State<'_, AppState>) -> CommandResult<Vec<MappingRuntime>> {
    state.manager.list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_mapping(
    state: State<'_, AppState>,
    config: MappingConfig,
) -> CommandResult<MappingRuntime> {
    let runtime = state
        .manager
        .upsert(config)
        .map_err(|error| error.to_string())?;
    state
        .manager
        .save_to_path(&state.config_path)
        .map_err(|error| error.to_string())?;
    Ok(runtime)
}

#[tauri::command]
pub fn delete_mapping(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state
        .manager
        .remove(&id)
        .map_err(|error| error.to_string())?;
    state
        .manager
        .save_to_path(&state.config_path)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_webdav_connection(
    config: MappingConfig,
    password: Option<String>,
) -> CommandResult<()> {
    if config.protocol != Protocol::Webdav {
        return Err(EngineError::InvalidConfig(
            "connection testing is currently available for WebDAV only".into(),
        )
        .to_string());
    }
    config.validate().map_err(|error| error.to_string())?;
    let remote = WebDavFileSystem::from_config(&config, password.filter(|value| !value.is_empty()))
        .map_err(|error| error.to_string())?;
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
) -> CommandResult<MappingRuntime> {
    let runtime = state
        .manager
        .begin_mount(&id)
        .map_err(|error| error.to_string())?;
    let result = state
        .mount_driver
        .mount(&runtime.config, password.filter(|value| !value.is_empty()))
        .await;
    match result {
        Ok(()) => state
            .manager
            .finish_mount(&id, MappingState::Mounted, None)
            .map_err(|error| error.to_string()),
        Err(error) => {
            let message = error.to_string();
            let _ = state
                .manager
                .finish_mount(&id, MappingState::Error, Some(message.clone()));
            Err(message)
        }
    }
}

#[tauri::command]
pub async fn unmount_mapping(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<MappingRuntime> {
    let runtime = state.manager.get(&id).map_err(|error| error.to_string())?;
    if runtime.state != MappingState::Mounted {
        return Err(EngineError::NotMounted(id).to_string());
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
