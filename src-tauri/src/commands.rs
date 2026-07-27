use guglefs_core::{EngineError, MappingConfig, MappingRuntime, Protocol, RemoteFileSystem};
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
pub fn mount_mapping(_id: String) -> CommandResult<()> {
    Err(EngineError::NotImplemented("mount orchestration is tracked in TODO.md".into()).to_string())
}

#[tauri::command]
pub fn unmount_mapping(_id: String) -> CommandResult<()> {
    Err(
        EngineError::NotImplemented("unmount orchestration is tracked in TODO.md".into())
            .to_string(),
    )
}
