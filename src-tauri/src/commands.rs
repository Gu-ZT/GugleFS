use guglefs_core::{EngineError, MappingConfig, MappingRuntime};
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
