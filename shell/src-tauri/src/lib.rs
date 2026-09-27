//! §spec agenticos-foundation-and-autopilot

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
/// The OS calls that carry those decisions out, and nothing else. Holds no rules.
pub mod dictation;
/// What a drop onto the window means, and the only paths this process will read because of one.
pub mod drop;
/// Where the quota notch is drawn: inside the main window, or in a floating window of its own.
pub mod notch;
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

/// What closing the main window does on this platform.
#[derive(Debug, PartialEq, Eq)]
enum CloseAction {
    /// Keep running in the tray; "Show NucleOS" or a left click on the tray brings the window back.
    Hide,
    /// Leave the app.
    Exit,
}

/// Hide where a tray is guaranteed, exit where it is not (spec D9). GNOME without the AppIndicator
/// extension draws no tray at all, and nothing reliable says whether one is there, so on Linux a
/// hidden window could be unreachable. Exiting costs nothing: the daemon's life is independent of
/// the shell's.
const CLOSE: CloseAction = if cfg!(target_os = "linux") {
    CloseAction::Exit
} else {
    CloseAction::Hide
};

/// What a close request does to the window it arrived on.
///
/// The notch is told apart by label because the handler below is shared by every window, and before
/// the notch existed it hid whichever one asked: closing the floating notch would have hidden it
/// and left the owner with no notch at all and no setting that said so (design D8, wall 3).
/// Closing it docks it instead — the notch goes back inside the app, and the mode on disk says so.
fn close_action_for(label: &str) -> Option<CloseAction> {
    if label == notch::LABEL {
        None
    } else {
        Some(CLOSE)
    }
}

/// Brings the main window back: unminimized, shown and focused. Errors are ignored because there is
/// nothing useful to do with one here. The label is the default `main`: tauri.conf.json names none.
fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Linux only: WebKitGTK ships with media streams off and answers no permission request, so
/// `getUserMedia` failed before anyone was asked. Only the app's own bundle loads in this webview
/// (the CSP fences the rest), and only an audio-only user-media request is granted; every other
/// request falls through to WebKitGTK's default, which refuses it.
#[cfg(target_os = "linux")]
fn allow_the_microphone(webview: &webkit2gtk::WebView) {
    use glib::prelude::*;
    use webkit2gtk::{PermissionRequestExt, SettingsExt, UserMediaPermissionRequest, WebViewExt};

    if let Some(settings) = webview.settings() {
        settings.set_enable_media_stream(true);
    }
    webview.connect_permission_request(|_, request| {
        // Both callers (lib/capture.ts, data/conversation.ts) ask for audio alone.
        let audio_only = request.is::<UserMediaPermissionRequest>()
            && request.property::<bool>("is-for-audio-device")
            && !request.property::<bool>("is-for-video-device");
        if audio_only {
            request.allow();
        }
        audio_only
    });
}

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
        // The chords themselves are NOT registered here. They live in `~/.nucleos/voice.yaml`, which only the
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

            // A tray icon is required wherever closing the window hides it: it is the way back to the
            // window and the way out of the app. On Linux closing exits instead (see CLOSE).
            let show_item = MenuItem::with_id(app, "show", "Show NucleOS", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                // A left click shows the window; the menu is on the right button. Linux ignores this
                // setting and reports no tray clicks, so there the menu is the only way.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id() == "show" {
                        show_main(app);
                    } else if event.id() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // The microphone on Linux: see allow_the_microphone.
            #[cfg(target_os = "linux")]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.with_webview(|webview| allow_the_microphone(&webview.inner()));
            }

            // The owner's last choice of host. A notch window that fails to open is not a reason to
            // refuse to start — but it IS a reason to stop calling the notch global: the main
            // window draws its own notch only while the word on disk says `contained`, so a failed
            // open left unrecorded leaves the notch drawn nowhere and the control that would move
            // it back inside the window that does not exist. `restore` records contained instead,
            // which is the recoil the design names for that window misbehaving (risk R1).
            notch::restore(app.handle());

            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => match close_action_for(window.label()) {
                    // Hide instead of quit: the app stays alive in the tray. "Quit" there is the
                    // shell's own process exit; there is NO child daemon process to kill (the
                    // daemon's lifecycle is entirely independent now, Part A).
                    Some(CloseAction::Hide) => {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                    // No guaranteed tray to come back from, so closing leaves the app.
                    Some(CloseAction::Exit) => window.app_handle().exit(0),
                    // The notch: let it close, and dock it.
                    None => {
                        let _ = notch::record(window.app_handle(), notch::Mode::Contained);
                    }
                },
                // The notch window's screen changing under it: a new scale factor, a new resolution,
                // a taskbar that moved, a move it did not ask for. None of them change what the
                // page draws, so its `ResizeObserver` never fires and nothing asks to be fitted —
                // the window keeps a size measured for the old DPI and a position that may now be
                // off-screen. `refit` re-applies the last size the page did ask for.
                //
                // Handled from this app-wide subscription rather than from one registered on the
                // notch window itself: this one already exists and already tells windows apart by
                // label, and the deciding stays in `notch`, which is what `refit` is.
                WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Moved(_) => {
                    if window.label() == notch::LABEL {
                        if let Some(notch) = window.app_handle().get_webview_window(notch::LABEL) {
                            notch::refit(&notch);
                        }
                    }
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
            dictation::voice_hotkeys_unavailable,
            drop::read_dropped,
            notch::notch_fit,
            notch::notch_mode,
            notch::notch_set_mode,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(on_run_event);
}

/// The dock icon: macOS reports a click on it as Reopen, and before this a hidden window stayed hidden.
#[cfg(target_os = "macos")]
fn on_run_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    if let tauri::RunEvent::Reopen { .. } = event {
        show_main(app);
    }
}

/// No other platform has a Reopen event, so nothing is handled: exactly what `Builder::run` did.
#[cfg(not(target_os = "macos"))]
fn on_run_event(_app: &tauri::AppHandle, _event: tauri::RunEvent) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec D9: hide where a tray is guaranteed, exit where it is not.
    #[test]
    fn closing_the_window_hides_only_where_a_tray_is_guaranteed() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            CLOSE,
            CloseAction::Exit,
            "Linux has no guaranteed tray to come back from"
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            CLOSE,
            CloseAction::Hide,
            "the tray brings the window back here"
        );
    }

    /// Closing the floating notch must not reach the main window's rule, or it hides the app.
    #[test]
    fn closing_the_notch_docks_it_and_leaves_the_app_alone() {
        assert_eq!(close_action_for(notch::LABEL), None);
        assert_eq!(close_action_for("main"), Some(CLOSE));
    }

    /// Closing the notch window docks it — and the arm that does it is read here, not only the
    /// decision in front of it. `close_action_for` answering `None` says the notch is not the main
    /// window's case; it says nothing about what happens next, so an arm rewritten to hide the
    /// window would leave the owner with an invisible notch, a file still saying `global`, and
    /// every test in this file green.
    #[test]
    fn the_close_that_docks_the_notch_writes_the_word_and_hides_nothing() {
        let source = include_str!("lib.rs");
        let arm = source
            .split("// The notch: let it close, and dock it.")
            .nth(1)
            .expect("the docking arm is commented as such")
            .split("},")
            .next()
            .expect("the docking arm ends");
        assert!(
            arm.contains("notch::record(window.app_handle(), notch::Mode::Contained)"),
            "{arm}"
        );
        assert!(!arm.contains("hide"), "{arm}");
        assert!(!arm.contains("prevent_close"), "{arm}");
    }

    /// The other notch arm, for the same reason: it is an effect on a window, so there is no app
    /// here to raise the event and nothing but the source says whether the subscription is still
    /// there. It is one line that a refactor drops in silence — and the failure it leaves is a
    /// notch sized for the old DPI, or parked off the edge of a screen that shrank.
    #[test]
    fn the_notch_is_refitted_when_the_screen_changes_under_it() {
        let source = include_str!("lib.rs");
        let arm = source
            .split("WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Moved(_) => {")
            .nth(1)
            .expect("the notch hears the two events that move it without resizing its drawing")
            .split("\n                }")
            .next()
            .expect("the arm ends");
        assert!(arm.contains("window.label() == notch::LABEL"), "{arm}");
        assert!(arm.contains("notch::refit(&notch)"), "{arm}");
    }

    /// The notch reaches the token without a capability of its own (see `notch`), so the one
    /// capability there is must stay scoped to the main window: widening it would hand the floating
    /// notch every plugin command the app has.
    #[test]
    fn the_only_capability_stays_on_the_main_window() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json"))
                .expect("default.json parses");
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
    }

    /// The other half of that argument, and the half no capability file can state: the notch window
    /// is safe without a capability only because this app ships **no ACL manifest of its own**
    /// (see `notch`'s module comment). Tauri builds one from `permissions/`, so the day somebody
    /// adds that directory every command in `invoke_handler` starts being ACL-checked and the
    /// capability above — scoped to `main` — starts refusing the notch window its own commands.
    /// `gen/` is untracked, so nothing else in this suite would notice.
    #[test]
    fn the_app_ships_no_acl_manifest_of_its_own() {
        let permissions = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("permissions");
        assert!(
            !permissions.exists(),
            "{} exists: the notch window now needs a capability of its own, \
             or its commands are refused",
            permissions.display()
        );
    }

    /// Windows: this very binary carries the application manifest, and that is what `build.rs`'s
    /// `manifest_the_test_binaries` is for.
    ///
    /// Without it the loader binds comctl32 5.82 rather than 6, and the first thing in the link
    /// graph to reach a version 6 export kills the whole test binary at load with
    /// `STATUS_ENTRYPOINT_NOT_FOUND` — before `main`, with no test having run and no name to blame.
    /// The failure is silent about its own cause, arrives from a codegen-unit accident rather than
    /// from anything in the source, and costs an afternoon; so the condition is asserted where a
    /// name comes with it. A binary that loaded at all had SOME manifest, which is why this reads
    /// the file rather than trusting its own existence.
    #[cfg(windows)]
    #[test]
    fn this_test_binary_carries_the_application_manifest() {
        let exe = std::env::current_exe().expect("a test binary knows its own path");
        let bytes = std::fs::read(&exe).expect("a test binary can read itself");
        let manifest = b"Microsoft.Windows.Common-Controls";
        assert!(
            bytes.windows(manifest.len()).any(|run| run == manifest),
            "{} carries no application manifest: see build.rs, manifest_the_test_binaries",
            exe.display()
        );
    }

    /// Spec 1.10: without these, macOS refuses `getUserMedia` (capture.ts, conversation.ts).
    #[test]
    fn the_microphone_is_declared_to_macos() {
        let info = include_str!("../Info.plist");
        assert!(info.contains("<key>NSMicrophoneUsageDescription</key>"));
        let entitlements = include_str!("../Entitlements.plist");
        assert!(entitlements.contains("<key>com.apple.security.device.audio-input</key>"));
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json parses");
        assert_eq!(
            conf["bundle"]["macOS"]["entitlements"],
            "./Entitlements.plist"
        );
    }
}
