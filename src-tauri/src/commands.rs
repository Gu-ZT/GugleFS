use guglefs_core::{EngineError, MappingConfig, MappingManager, MappingRuntime};
use tauri::State;

type CommandResult<T> = Result<T, String>;

#[tauri::command]
pub fn list_mappings(manager: State<'_, MappingManager>) -> CommandResult<Vec<MappingRuntime>> {
    manager.list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_mapping(
    manager: State<'_, MappingManager>,
    config: MappingConfig,
) -> CommandResult<MappingRuntime> {
    manager.upsert(config).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_mapping(manager: State<'_, MappingManager>, id: String) -> CommandResult<()> {
    manager.remove(&id).map_err(|error| error.to_string())
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
