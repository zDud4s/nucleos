//! §spec notch-de-quota
//!
//! Where the quota notch lives: inside the main window, or in a window of its own that floats over
//! every other one (design D8). The drawing is the page's; this module owns only the second window
//! and the one word that says which of the two hosts is in use.
//!
//! **The notch window holds no capability, and needs none.** `capabilities/default.json` stays
//! scoped to `main`. Tauri checks the ACL of an app's *own* commands only when the app ships an
//! ACL manifest for them, or when the caller is a remote origin (tauri 2.11.5,
//! `webview/mod.rs`, the `has_app_acl_manifest || !is_local` guard) — this app ships none, and the
//! notch loads the app's own bundle. So `get_daemon_token` (design D8, wall 4) and the three
//! commands below answer it, while every plugin command, window API and event subscription stays
//! out of its reach. Everything the window has to do to itself is done here, in Rust, for exactly
//! that reason: granting it `core:window` to move itself would be a capability for a job four
//! lines of Rust already do.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The second window's label. `main` is the default one's, which tauri.conf.json never names.
pub const LABEL: &str = "notch";

/// What the page loads in the notch window: the app's own bundle, told by the query which of its
/// two faces to render. One bundle rather than a second entry point, so the notch runs under the
/// same CSP, the same tokens and the same build the CSP gate already checks.
const URL: &str = "index.html?window=notch";

/// Broadcast whenever the mode changes, so the main window can put its contained notch away or
/// take it back without polling. The payload is the mode's word.
pub const MODE_EVENT: &str = "notch://mode";

/// The file whose contents are the mode, in the app's config directory beside the autostart marker.
const MODE_FILE: &str = "notch-mode";

/// Which host draws the notch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Drawn by the main window, at the top of the page area. What Phase 1 shipped.
    Contained,
    /// Drawn by a borderless, always-on-top window of its own, at the top edge of the screen.
    Global,
}

impl Mode {
    /// Reads the word back. Anything that is not exactly `global` is contained: a missing file, an
    /// empty one or a word this build does not know all land on the host that cannot fail to draw,
    /// and that is also the recoil the design names for the second window misbehaving (risk R1).
    pub fn read(word: &str) -> Mode {
        if word.trim() == "global" {
            Mode::Global
        } else {
            Mode::Contained
        }
    }

    /// Strict, for words arriving from the page: an unknown one is refused rather than read as
    /// contained, because a caller asking for a mode that does not exist has a bug worth hearing.
    pub fn parse(word: &str) -> Result<Mode, String> {
        match word {
            "global" => Ok(Mode::Global),
            "contained" => Ok(Mode::Contained),
            other => Err(format!("no notch mode is called {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Contained => "contained",
            Mode::Global => "global",
        }
    }
}

/// The mode on disk. A config directory that cannot be resolved or read answers contained.
pub fn stored(app: &AppHandle) -> Mode {
    app.path()
        .app_config_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join(MODE_FILE)).ok())
        .map_or(Mode::Contained, |word| Mode::read(&word))
}

/// Records the mode and tells every window, without touching the notch window. What a close of
/// the notch window does, because that window is already on its way out.
pub fn record(app: &AppHandle, mode: Mode) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MODE_FILE), mode.as_str()).map_err(|e| e.to_string())?;
    let _ = app.emit(MODE_EVENT, mode.as_str());
    Ok(())
}

/// Puts the notch window up or takes it down, then records the mode.
///
/// In that order, so a window that fails to open is never recorded as the host: the main window
/// would put its own notch away on the broadcast, and the owner would be left with none.
pub fn apply(app: &AppHandle, mode: Mode) -> Result<(), String> {
    match mode {
        Mode::Global => open(app).map_err(|e| e.to_string())?,
        Mode::Contained => {
            if let Some(window) = app.get_webview_window(LABEL) {
                // `destroy`, not `close`: a close comes back through `CloseRequested`, which would
                // record the word a second time.
                window.destroy().map_err(|e| e.to_string())?;
            }
        }
    }
    record(app, mode)
}

/// Opens the notch window, hidden. It is shown by the first `notch_fit` that has something to
/// draw, so a quota that has not answered yet puts nothing on screen — not even an invisible
/// rectangle that swallows clicks at the top of the desktop.
pub fn open(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(());
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(URL.into()))
        .title("NucleOS quota")
        .decorations(false)
        .transparent(true)
        // Without this, Windows draws a one-pixel frame and a drop shadow round an undecorated
        // window, which on a transparent one is a grey box round nothing.
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        // Never takes focus from whatever the owner is typing into. It is read, not used.
        .focused(false)
        .visible(false)
        .inner_size(1.0, 1.0)
        .build()?;
    Ok(())
}

/// Where the notch window sits: centred on the top edge of the work area, which is the screen
/// minus the taskbar. All physical pixels.
pub fn top_centre(area_x: i32, area_y: i32, area_width: u32, window_width: u32) -> (i32, i32) {
    let slack = area_width.saturating_sub(window_width) / 2;
    (area_x + slack as i32, area_y)
}

/// The notch window's size follows its content, in CSS pixels, and it re-centres on every change —
/// which is also how it unfolds when the pointer reaches it: the page grows, and asks to be fitted.
///
/// Zero in either dimension hides the window. That is what the page sends when it has nothing to
/// draw, and a hidden window is the only honest picture of "nothing measured yet".
///
/// Answers only the notch window. The main window calling it would resize itself into a strip.
#[tauri::command]
pub fn notch_fit(window: WebviewWindow, width: f64, height: f64) -> Result<(), String> {
    if window.label() != LABEL {
        return Err("only the notch window is fitted to its content".into());
    }
    if width < 1.0 || height < 1.0 {
        return window.hide().map_err(|e| e.to_string());
    }
    let monitor = window
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten())
        .ok_or("no monitor to put the notch on")?;
    let scale = monitor.scale_factor();
    let size = tauri::PhysicalSize::new(
        (width * scale).ceil() as u32,
        (height * scale).ceil() as u32,
    );
    let area = monitor.work_area();
    let (x, y) = top_centre(
        area.position.x,
        area.position.y,
        area.size.width,
        size.width,
    );
    window.set_size(size).map_err(|e| e.to_string())?;
    window
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    if !window.is_visible().unwrap_or(false) {
        window.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The mode on disk, for a page deciding whether to draw the contained notch.
#[tauri::command]
pub fn notch_mode(app: AppHandle) -> &'static str {
    stored(&app).as_str()
}

/// Moves the notch between its two hosts. Refuses a word that is not a mode; otherwise answers at
/// once, and the outcome arrives as the `MODE_EVENT` broadcast.
///
/// **The work happens on a thread of its own, and the command is synchronous.** A synchronous
/// command runs on the main thread, and building a webview there from inside the IPC call is the
/// deadlock Tauri's own documentation warns about. That was measured, not only read: in the spike
/// that proved this window, the notch docked and never floated again. The usual remedy, an `async`
/// command, was measured too. It floats the notch, and it makes this crate's test binary refuse to
/// load on windows-gnu (`STATUS_ENTRYPOINT_NOT_FOUND`: the test executable carries no manifest,
/// and the async glue drags in an import only the manifest makes resolvable). A plain thread has
/// neither problem.
///
/// A move that fails is still broadcast, as the mode that stayed on disk, so a page that already
/// put its own notch away takes it back instead of drawing it nowhere.
#[tauri::command]
pub fn notch_set_mode(app: AppHandle, mode: String) -> Result<(), String> {
    let mode = Mode::parse(&mode)?;
    std::thread::spawn(move || {
        if apply(&app, mode).is_err() {
            let _ = app.emit(MODE_EVENT, stored(&app).as_str());
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the exact word floats the notch; everything else keeps it where it cannot fail to draw.
    #[test]
    fn anything_but_global_on_disk_reads_as_contained() {
        assert_eq!(Mode::read("global"), Mode::Global);
        assert_eq!(Mode::read("global\n"), Mode::Global);
        for word in ["contained", "", "Global", "floating", "globa"] {
            assert_eq!(Mode::read(word), Mode::Contained, "{word:?}");
        }
    }

    /// The page is held to the two words, and each word survives the round trip through the file.
    #[test]
    fn the_page_may_ask_for_exactly_the_two_modes() {
        for mode in [Mode::Contained, Mode::Global] {
            assert_eq!(Mode::parse(mode.as_str()), Ok(mode));
            assert_eq!(Mode::read(mode.as_str()), mode);
        }
        assert!(Mode::parse("floating").is_err());
    }

    /// Centred on the work area's own origin, which is not the screen's on a second monitor or with
    /// the taskbar at the top.
    #[test]
    fn the_notch_hangs_from_the_middle_of_the_top_edge() {
        assert_eq!(top_centre(0, 0, 1920, 200), (860, 0));
        assert_eq!(top_centre(-1920, 40, 1920, 200), (-1060, 40));
        // Wider than the screen: pinned to the left edge rather than pushed off it.
        assert_eq!(top_centre(0, 0, 100, 200), (0, 0));
    }
}
