mod commands;
mod diagnostics;
mod mount_state;
mod security;
mod session_state;
mod updater;

use std::{collections::HashMap, path::PathBuf};

use diagnostics::DiagnosticStore;
use guglefs_core::MappingManager;
use guglefs_mount::SystemMountDriver;
use mount_state::MountStateStore;
use security::SecurityManager;
use session_state::SessionState;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, WindowEvent,
};

struct TrayMenuState {
    open: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

#[tauri::command]
fn set_app_locale(locale: &str, tray: tauri::State<'_, TrayMenuState>) -> Result<(), String> {
    let (open, quit) = if locale == "en" {
        ("Open GugleFS", "Quit")
    } else {
        ("打开 GugleFS", "退出")
    };
    tray.open
        .set_text(open)
        .map_err(|error| error.to_string())?;
    tray.quit
        .set_text(quit)
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub struct AppState {
    pub manager: MappingManager,
    pub config_path: PathBuf,
    pub mount_driver: SystemMountDriver,
    pub mount_state: MountStateStore,
    pub mount_operations: tokio::sync::Mutex<()>,
    pub remote_browsers: tokio::sync::Mutex<HashMap<String, commands::RemoteBrowserSession>>,
    pub diagnostics: DiagnosticStore,
    pub security: SecurityManager,
    pub session_state: SessionState,
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
        .plugin(tauri_plugin_opener::init())
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
            let session_state = SessionState::begin(config_dir.join("session-running"))
                .map_err(std::io::Error::other)?;
            diagnostics.record("application_start", None, "success");
            if session_state.previous_session_unclean() {
                diagnostics.record("previous_session_unclean", None, "detected");
            }
            app.manage(AppState {
                manager,
                config_path,
                mount_driver: SystemMountDriver::default(),
                mount_state,
                mount_operations: tokio::sync::Mutex::new(()),
                remote_browsers: tokio::sync::Mutex::new(HashMap::new()),
                diagnostics,
                security: SecurityManager::default(),
                session_state,
            });

            let open_item = MenuItem::with_id(app, "open", "打开 GugleFS", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;
            app.manage(TrayMenuState {
                open: open_item.clone(),
                quit: quit_item.clone(),
            });
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
            updater::check_for_updates,
            commands::open_fuse_t_installer,
            commands::get_auth_status,
            commands::begin_2fa_setup,
            commands::confirm_2fa_setup,
            commands::unlock_app,
            commands::set_two_factor_enabled,
            commands::lock_app,
            commands::list_mappings,
            commands::export_mappings,
            commands::import_mappings,
            commands::export_diagnostics,
            commands::occupied_drive_letters,
            commands::save_mapping,
            commands::delete_mapping,
            commands::inspect_sftp_host_key,
            commands::import_sftp_known_hosts,
            commands::test_remote_connection,
            commands::open_remote_browser,
            commands::list_remote_directories,
            commands::close_remote_browser,
            commands::detect_sftp_mfa_requirement,
            commands::mount_mapping,
            commands::restore_startup_mappings,
            commands::unmount_mapping,
            set_app_locale,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build GugleFS");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { .. } = event {
            if let Some(state) = app.try_state::<AppState>() {
                if state.session_state.begin_exit() && state.mount_driver.unmount_all().is_ok() {
                    let _ = state.session_state.mark_clean();
                }
            }
        }
    });
}
