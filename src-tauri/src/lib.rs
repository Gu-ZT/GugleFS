mod commands;

use std::path::PathBuf;

use guglefs_core::MappingManager;
use tauri::Manager;

pub struct AppState {
    pub manager: MappingManager,
    pub config_path: PathBuf,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = app
                .path()
                .app_config_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .join("mappings.json");
            let manager = MappingManager::load_from_path(&config_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            app.manage(AppState {
                manager,
                config_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_mappings,
            commands::save_mapping,
            commands::delete_mapping,
            commands::mount_mapping,
            commands::unmount_mapping,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run GugleFS");
}
