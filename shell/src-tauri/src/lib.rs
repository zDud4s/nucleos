//! §spec agenticos-foundation-and-autopilot

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
/// The OS calls that carry those decisions out, and nothing else. Holds no rules.
pub mod dictation;
/// What a drop onto the window means, and the only paths this process will read because of one.
pub mod drop;
/// Dictation decisions. `pub` because it is genuinely this crate's surface: the platform layer
/// calls into it, and a private module of not-yet-wired functions would be dead code under the
/// `-D warnings` clippy gate that `scripts/gates.sh` now runs over this package.
pub mod voice;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

/// Marker for "autostart has been decided once". Its EXISTENCE is the whole
/// state: after the first launch the answer belongs to the user, whatever it is.
const AUTOSTART_MARKER: &str = "autostart-initialised";

// Left at the crate root's default visibility on purpose: `#[tauri::command]` re-exports helper
// macros with the function's visibility, and `pub(crate)` makes that re-export collide with its own
// definition (E0255). `dictation` reaches it as `crate::get_daemon_token` because a private item in
// the crate root is already visible to every module below it.
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
        // The chords themselves are NOT registered here. They live in `.ai/voice.yaml`, which only the
        // daemon reads, so the Voice tab registers them through `voice_register_hotkeys` once it has
        // read `GET /voice/config` — and a daemon that is not up yet simply means no hotkey yet.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(dictation::Dictation::default())
        .manage(drop::Allowed::default())
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
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    // Hide instead of quit — the app stays alive in the tray. "Quit" here is the
                    // shell's own process exit; there is NO child daemon process to kill (the
                    // daemon's lifecycle is entirely independent now — Part A).
                    api.prevent_close();
                    let _ = window.hide();
                }
                // The OS drop is handled HERE rather than in the page, because the page never sees
                // it: Tauri takes the drop so it can hand over real paths, which is also what makes
                // this side the only honest place to decide which paths are readable afterwards.
                WindowEvent::DragDrop(drag) => match drag {
                    tauri::DragDropEvent::Enter { .. } => {
                        let _ = window.emit("files://drag-enter", ());
                    }
                    tauri::DragDropEvent::Leave => {
                        let _ = window.emit("files://drag-leave", ());
                    }
                    tauri::DragDropEvent::Drop { paths, .. } => {
                        let dropped = drop::accept(&window.state::<drop::Allowed>(), paths);
                        let _ = window.emit("files://dropped", dropped);
                    }
                    // `Over` fires continuously while the cursor moves; the page only needs to know
                    // it is being dragged over, which `Enter` already said.
                    _ => {}
                },
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_daemon_token,
            dictation::voice_hotkey,
            dictation::voice_phase,
            dictation::voice_paste,
            dictation::voice_abandon,
            dictation::voice_register_hotkeys,
            drop::read_dropped,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
