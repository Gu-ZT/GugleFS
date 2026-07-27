mod commands;

use guglefs_core::MappingManager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(MappingManager::default())
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
