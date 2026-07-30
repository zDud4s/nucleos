// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
/// Dictation decisions. `pub` because it is genuinely this crate's surface: the platform layer
/// calls into it, and a private module of not-yet-wired functions would be dead code under the
/// `-D warnings` clippy gate that `scripts/gates.sh` now runs over this package.
pub mod voice;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

/// Marker for "autostart has been decided once". Its EXISTENCE is the whole
/// state: after the first launch the answer belongs to the user, whatever it is.
const AUTOSTART_MARKER: &str = "autostart-initialised";

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
            // Windows Scheduled Task (Part A), independent of this.
            //
            // Enabled ONCE, on the first launch, and never asserted again: without the marker
            // this ran on every start, so turning autostart off in Windows Settings lasted
            // exactly until the next launch and looked like the setting was broken. A launch
            // that cannot resolve the config dir simply skips the offer rather than re-enabling.
            if let Ok(config_dir) = app.path().app_config_dir() {
                let marker = config_dir.join(AUTOSTART_MARKER);
                if !marker.exists() {
                    let _ = app.autolaunch().enable();
                    // Written whatever `enable()` returned: the first run has happened, and a
                    // failed attempt is not a licence to keep asking.
                    let _ = std::fs::create_dir_all(&config_dir);
                    let _ = std::fs::write(&marker, b"");
                }
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
        .invoke_handler(tauri::generate_handler![get_daemon_token])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
