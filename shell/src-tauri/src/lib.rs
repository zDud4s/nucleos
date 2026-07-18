// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::WindowEvent;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_daemon_token() -> Result<String, String> {
    // The token lives in the OS Credential Manager (spec §3.4/§5), written by the daemon's core under
    // service "nucleos", key "daemon-token" (Chunk 1 Task 5). The shell runs as the same OS user, so
    // it reads the same entry via keyring — no shared file, no path resolution.
    keyring::Entry::new("nucleos", "daemon-token")
        .and_then(|entry| entry.get_password())
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // Shell-GUI autostart convenience only — the daemon owns its OWN persistence via a
            // Windows Scheduled Task (Part A), independent of this. Enable on first run.
            let autostart_manager = app.autolaunch();
            if !autostart_manager.is_enabled().unwrap_or(false) {
                let _ = autostart_manager.enable();
            }

            // A tray icon is required once window-close hides instead of quits — otherwise there is no
            // way to exit short of Task Manager. Minimal menu: just "Quit".
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit_item])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Hide instead of quit — the app stays alive in the tray. "Quit" here is the shell's
                // own process exit; there is NO child daemon process to kill (the daemon's lifecycle
                // is entirely independent now — Part A).
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![greet, get_daemon_token])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
