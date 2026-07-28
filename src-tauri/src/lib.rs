mod commands;
mod diagnostics;
mod mount_state;
mod security;

use std::{collections::HashMap, path::PathBuf};

use diagnostics::DiagnosticStore;
use guglefs_core::MappingManager;
use guglefs_mount::SystemMountDriver;
use mount_state::MountStateStore;
use security::SecurityManager;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, WindowEvent,
};

pub struct AppState {
    pub manager: MappingManager,
    pub config_path: PathBuf,
    pub mount_driver: SystemMountDriver,
    pub mount_state: MountStateStore,
    pub mount_operations: tokio::sync::Mutex<()>,
    pub remote_browsers: tokio::sync::Mutex<HashMap<String, commands::RemoteBrowserSession>>,
    pub diagnostics: DiagnosticStore,
    pub security: SecurityManager,
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--tray-only"]),
        ))
        .setup(|app| {
            // 窗口在配置中默认隐藏：开机自启动携带 --tray-only 时保持托盘静默，
            // 正常启动则立即显示主窗口。
            let tray_only = std::env::args().any(|arg| arg == "--tray-only");
            if !tray_only {
                show_main_window(app.handle());
            }

            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let config_path = config_dir.join("mappings.json");
            let manager = MappingManager::load_from_path(&config_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let mount_state = MountStateStore::load_from_path(config_dir.join("mount-state.json"))
                .map_err(std::io::Error::other)?;
            let diagnostics =
                DiagnosticStore::new(config_dir.join("logs")).map_err(std::io::Error::other)?;
            diagnostics.record("application_start", None, "success");
            app.manage(AppState {
                manager,
                config_path,
                mount_driver: SystemMountDriver::default(),
                mount_state,
                mount_operations: tokio::sync::Mutex::new(()),
                remote_browsers: tokio::sync::Mutex::new(HashMap::new()),
                diagnostics,
                security: SecurityManager::default(),
            });

            let open_item = MenuItem::with_id(app, "open", "打开 GugleFS", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;
            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_platform_info,
            commands::open_fuse_t_installer,
            commands::get_auth_status,
            commands::begin_2fa_setup,
            commands::confirm_2fa_setup,
            commands::unlock_app,
            commands::lock_app,
            commands::list_mappings,
            commands::export_mappings,
            commands::import_mappings,
            commands::export_diagnostics,
            commands::occupied_drive_letters,
            commands::save_mapping,
            commands::delete_mapping,
            commands::inspect_sftp_host_key,
            commands::test_remote_connection,
            commands::open_remote_browser,
            commands::list_remote_directories,
            commands::close_remote_browser,
            commands::detect_sftp_mfa_requirement,
            commands::mount_mapping,
            commands::restore_startup_mappings,
            commands::unmount_mapping,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build GugleFS");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { .. } = event {
            if let Some(state) = app.try_state::<AppState>() {
                let _ = state.mount_driver.unmount_all();
            }
        }
    });
}
