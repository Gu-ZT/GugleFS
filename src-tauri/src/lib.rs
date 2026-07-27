mod commands;
mod mount_state;
mod security;

use std::path::PathBuf;

use guglefs_core::MappingManager;
use guglefs_mount::SystemMountDriver;
use mount_state::MountStateStore;
use security::SecurityManager;
use tauri::Manager;

pub struct AppState {
    pub manager: MappingManager,
    pub config_path: PathBuf,
    pub mount_driver: SystemMountDriver,
    pub mount_state: MountStateStore,
    pub mount_operations: tokio::sync::Mutex<()>,
    pub security: SecurityManager,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let config_path = config_dir.join("mappings.json");
            let manager = MappingManager::load_from_path(&config_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let mount_state = MountStateStore::load_from_path(config_dir.join("mount-state.json"))
                .map_err(std::io::Error::other)?;
            app.manage(AppState {
                manager,
                config_path,
                mount_driver: SystemMountDriver::default(),
                mount_state,
                mount_operations: tokio::sync::Mutex::new(()),
                security: SecurityManager::default(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_auth_status,
            commands::begin_2fa_setup,
            commands::confirm_2fa_setup,
            commands::unlock_app,
            commands::lock_app,
            commands::list_mappings,
            commands::save_mapping,
            commands::delete_mapping,
            commands::test_remote_connection,
            commands::mount_mapping,
            commands::restore_startup_mappings,
            commands::unmount_mapping,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run GugleFS");
}
