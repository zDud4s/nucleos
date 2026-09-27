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

use std::path::Path;
use std::sync::Mutex;

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
    /// Drawn by the main window, against its right edge. What Phase 1 shipped.
    Contained,
    /// Drawn by a borderless, always-on-top window of its own, on the right edge of the screen.
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

    /// The other host. What is in force when the step that was to leave this one failed: an open
    /// that did not come up leaves the main window drawing, and a destroy that did not go through
    /// leaves the floating window up.
    pub fn other(self) -> Mode {
        match self {
            Mode::Contained => Mode::Global,
            Mode::Global => Mode::Contained,
        }
    }
}

/// The mode on disk. A config directory that cannot be resolved or read answers contained.
pub fn stored(app: &AppHandle) -> Mode {
    app.path()
        .app_config_dir()
        .map_or(Mode::Contained, |dir| stored_in(&dir))
}

/// The same read, over a directory named outright.
///
/// Split from `stored` for the one thing an `AppHandle` adds here, which is the path: a test has
/// no app to ask for one, and the round trip through the file is worth proving over a real file
/// rather than over the two words on their own.
pub fn stored_in(dir: &Path) -> Mode {
    std::fs::read_to_string(dir.join(MODE_FILE)).map_or(Mode::Contained, |word| Mode::read(&word))
}

/// Records the mode and tells every window, without touching the notch window. What a close of
/// the notch window does, because that window is already on its way out.
pub fn record(app: &AppHandle, mode: Mode) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    record_in(&dir, mode)?;
    let _ = app.emit(MODE_EVENT, mode.as_str());
    Ok(())
}

/// The write alone, over a directory named outright. See `stored_in`.
pub fn record_in(dir: &Path, mode: Mode) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MODE_FILE), mode.as_str()).map_err(|e| e.to_string())
}

/// A move that did not happen, and the host that is drawing the notch because it did not.
pub struct Failed {
    /// What went wrong, for whoever is reading the logs.
    pub reason: String,
    /// The host actually in force — which is NOT always the word on disk, and that difference is
    /// the whole point of carrying it: a dock that destroyed the window and then failed to write
    /// leaves the file saying `global` while nothing floats any more.
    pub in_force: Mode,
}

/// Puts the notch window up or takes it down, then records the mode.
///
/// In that order, so a window that fails to open is never recorded as the host: the main window
/// would put its own notch away on the broadcast, and the owner would be left with none.
pub fn apply(app: &AppHandle, mode: Mode) -> Result<(), Failed> {
    apply_with(
        mode,
        |mode| match mode {
            Mode::Global => open(app).map_err(|e| e.to_string()),
            Mode::Contained => match app.get_webview_window(LABEL) {
                // `destroy`, not `close`: a close comes back through `CloseRequested`, which would
                // record the word a second time.
                Some(window) => window.destroy().map_err(|e| e.to_string()),
                None => Ok(()),
            },
        },
        |mode| record(app, mode),
    )
}

/// `apply`'s order and its two failure answers, with the two effects passed in.
///
/// Separated because neither effect can be had in a test — there is no app to open a window in —
/// while the ordering and the "which host is drawing now" answer are exactly what a rewrite could
/// quietly get wrong. A test hands it two spies instead.
fn apply_with(
    mode: Mode,
    host: impl FnOnce(Mode) -> Result<(), String>,
    write: impl FnOnce(Mode) -> Result<(), String>,
) -> Result<(), Failed> {
    if let Err(reason) = host(mode) {
        // The window step is what failed, so nothing moved: the other host is still the one
        // drawing, and nothing is written down claiming otherwise.
        return Err(Failed {
            reason,
            in_force: mode.other(),
        });
    }
    // The window did move. A word that cannot be written is then only a word: the host asked for
    // is the one on screen, and it is the one to tell the pages about.
    write(mode).map_err(|reason| Failed {
        reason,
        in_force: mode,
    })
}

/// What the launch leaves in force, once it has tried to put the stored host up.
///
/// A stored `global` whose window did not open is **contained**. The word is what the main window
/// reads to decide whether to draw its own notch, so leaving it at `global` with no window leaves
/// the notch drawn nowhere — and the control that brings it back lives in the window that does not
/// exist, so there is no way out of it either. That is risk R1 exactly, and contained is the
/// recoil the design names for it.
pub fn after_startup(stored: Mode, opened: bool) -> Mode {
    if stored == Mode::Global && opened {
        Mode::Global
    } else {
        Mode::Contained
    }
}

/// Puts the stored host up at launch, and corrects the file when it could not be.
pub fn restore(app: &AppHandle) {
    let stored = stored(app);
    let opened = stored == Mode::Global && open(app).is_ok();
    let in_force = after_startup(stored, opened);
    if in_force != stored {
        // Recorded, not merely remembered in this process: `notch_mode` answers from the file, and
        // a page loading later would otherwise be told the notch is somewhere it is not.
        let _ = record(app, in_force);
    }
}

/// Opens the notch window, hidden. It is shown by the first `notch_fit` that has something to
/// draw, so a quota that has not answered yet puts nothing on screen — not even an invisible
/// rectangle that swallows clicks at the edge of the desktop.
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
    // No listener is registered here. The screen changing under this window — a new scale factor, a
    // new resolution, a taskbar that moved — is heard by the app-wide handler in `lib.rs`, which
    // already exists and already sorts window events by label; this module keeps the deciding, in
    // `refit`. One subscription rather than two, and the one that is already there.
    Ok(())
}

/// Where the notch window sits: against the right edge of the work area — the screen minus the
/// taskbar — with the FOLDED notch centred down it. All physical pixels.
///
/// **`x` is derived from the right edge, and that is what makes the unfold work.** The window's
/// width follows its drawing, so when the pointer arrives and the panel opens, this is called again
/// with a bigger width and returns a smaller `x`: the window grows LEFTWARDS and the edge it hangs
/// from does not move. Computed from the left instead, the same unfold would walk the notch off the
/// side of the screen, which is the one direction there is no room in.
///
/// **`y` is derived from the folded height, and not from the window's.** It used to centre the
/// window it was given, so an unfold that made the drawing taller moved the window UP by half of
/// what it grew — and the rings, which are what the pointer was on, jumped away from under it the
/// moment it arrived. That was the "bumpy" hover the owner reported: the panel opened somewhere
/// other than where they were looking, and it did it again on every pass. Centring on `rest` — the
/// height the page measured while folded — keeps the top edge where the folded notch had it, so the
/// panel opens down and to the left and the first ring stays exactly where it was. A `rest` taller
/// than the window (a provider dropped out while unfolded) is read as the window's own height.
///
/// Opening downwards is bounded by the bottom of the work area: a panel that would run past it is
/// lifted just enough to fit, which only ever happens to a notch dragged most of the way down a
/// short screen.
///
/// Every slack saturates. `fit_size` has already clamped the drawing to the work area, so a window
/// bigger than the area cannot reach here from the app — but a screen that shrinks under a window
/// can, and a notch pinned to the top-left corner is recoverable where one placed at a negative
/// coordinate off the edge is not.
pub fn hang(
    area_x: i32,
    area_y: i32,
    area_width: u32,
    area_height: u32,
    window_width: u32,
    window_height: u32,
    rest_height: u32,
) -> (i32, i32) {
    hang_along(
        area_x,
        area_y,
        area_width,
        area_height,
        window_width,
        window_height,
        rest_height,
        0.5,
    )
}

/// [`hang`], with the folded notch's middle at `along` of the way down the work area rather than
/// at half of it: 0 the top, 1 the bottom. It is where the owner dragged it (`QuotaNotch`), kept
/// by the page and sent with every fit.
///
/// A fraction, not pixels, because it has to survive what pixels do not: a taskbar that moves, a
/// resolution change, the notch moving to a monitor of another height, and the other host — the
/// contained notch hangs from the same fraction of the same work area (`screen-line.ts`). One that
/// is not a number is the middle; one outside [0, 1] is the nearer end. Near an end the folded
/// notch would run off the area, and it stops at the edge instead: the same saturation as the
/// centred case, where a notch taller than the area sits at its top.
#[allow(clippy::too_many_arguments)]
pub fn hang_along(
    area_x: i32,
    area_y: i32,
    area_width: u32,
    area_height: u32,
    window_width: u32,
    window_height: u32,
    rest_height: u32,
    along: f64,
) -> (i32, i32) {
    let along = if along.is_finite() {
        along.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let x = area_width.saturating_sub(window_width);
    let rest = rest_height.min(window_height);
    let wanted = (f64::from(area_height) * along - f64::from(rest) / 2.0).floor();
    let highest_top = area_height.saturating_sub(rest);
    let top = (wanted.max(0.0) as u32).min(highest_top);
    let y = top.min(area_height.saturating_sub(window_height));
    (area_x + x as i32, area_y + y as i32)
}

/// Which window may be fitted to its content: the notch's, and no other. The main window asking
/// would resize itself into a strip against the edge of the screen.
pub fn may_be_fitted(label: &str) -> Result<(), String> {
    if label == LABEL {
        Ok(())
    } else {
        Err(format!(
            "only the notch window is fitted to its content, not {label:?}"
        ))
    }
}

/// The physical box a drawing this big in CSS pixels needs, or `None` for a window that should be
/// hidden instead because there is nothing to draw.
///
/// **Every test here is positive** — `!(width >= 1.0)` rather than `width < 1.0` — because a NaN
/// fails every comparison it is given: `NaN < 1.0` is false, so a negative test lets it straight
/// through, and Rust's float-to-int cast then saturates it. NaN casts to 0 and INFINITY to
/// `u32::MAX`, which is a 0x0 window that is then shown and a transparent, always-on-top,
/// taskbar-less window the size of the desktop that swallows every click on it. The second is
/// unrecoverable from the screen: the pin that docks the notch is under that sheet of glass.
/// Clamping to the work area is the same argument taken to the end — the window is a notch, and
/// nothing the page can measure should be able to make it bigger than the screen it hangs from.
pub fn fit_size(
    width: f64,
    height: f64,
    scale: f64,
    area_width: u32,
    area_height: u32,
) -> Option<(u32, u32)> {
    if !(width >= 1.0 && height >= 1.0) {
        return None;
    }
    // A scale that is not a positive, finite number is no scale at all: CSS pixels are then
    // physical pixels, which is right on every display that reports 1.0 and no worse than a guess
    // anywhere else.
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    Some((
        physical(width, scale, area_width),
        physical(height, scale, area_height),
    ))
}

/// One dimension: scaled, rounded up, at least a pixel and never past the work area.
fn physical(css: f64, scale: f64, area: u32) -> u32 {
    (css * scale).ceil().clamp(1.0, area.max(1) as f64) as u32
}

/// Puts the window round a drawing of this CSS size, against the middle of the right edge of the
/// work area.
///
/// The scale factor is the **window's own** and not the primary monitor's. They are the same
/// number only on a machine with one display or with every display at one DPI; anywhere else the
/// notch was sized for a monitor it is not on — on a two-screen desk, a notch drawn at 125% on the
/// 100% screen. The monitor is the one the window is currently on for the same reason, and the
/// primary is only the fallback for a window the runtime cannot place.
fn place(window: &WebviewWindow, asked: Asked) -> Result<(), String> {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or("no monitor to put the notch on")?;
    let area = monitor.work_area();
    let scale = window
        .scale_factor()
        .unwrap_or_else(|_| monitor.scale_factor());
    let Some((width, height)) = fit_size(
        asked.width,
        asked.height,
        scale,
        area.size.width,
        area.size.height,
    ) else {
        return window.hide().map_err(|e| e.to_string());
    };
    // The folded height goes through the same scaling as the window's. One that is not a size —
    // an older page that sent none, a NaN — is the window's own height, which is the centring
    // this module did before it knew the difference.
    let rest = fit_size(
        asked.width,
        asked.rest,
        scale,
        area.size.width,
        area.size.height,
    )
    .map_or(height, |(_, rest)| rest);
    let size = tauri::PhysicalSize::new(width, height);
    let (x, y) = hang_along(
        area.position.x,
        area.position.y,
        area.size.width,
        area.size.height,
        width,
        height,
        rest,
        asked.along,
    );
    let position = tauri::PhysicalPosition::new(x, y);
    // Written only when it is not already so. `Moved` is one of the events that brings us back here
    // (see the arm in `lib.rs`), and a move that fires another `Moved` would be a loop; comparing
    // first ends it after one round. A size or position the runtime will not report is treated as
    // wrong, which costs a write nobody needed and never a notch left in the wrong place.
    let resized = !matches!(window.inner_size(), Ok(current) if current == size);
    let moved = !matches!(window.outer_position(), Ok(current) if current == position);
    if resized || moved {
        set_bounds(window, position, size)?;
    }
    if !window.is_visible().unwrap_or(false) {
        window.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Moves and resizes the window in ONE step, where the platform has one.
///
/// **Two steps are two frames, and the frame between them is the one the owner sees.** Every unfold
/// both grows the window and moves it left, and `set_size` then `set_position` drew the grown
/// window at the OLD position first — hanging off the right edge of the screen with the rings out
/// of sight — and only then pulled it back into place. `SetWindowPos` takes both at once, so the
/// window goes straight from the folded box to the unfolded one. `SWP_NOACTIVATE` keeps the promise
/// `open` made with `focused(false)`: the notch never takes focus from what the owner is typing
/// into, and a resize is no exception. `SWP_NOZORDER` leaves always-on-top as it was.
#[cfg(windows)]
fn set_bounds(
    window: &WebviewWindow,
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
    let handle = window.hwnd().map_err(|e| e.to_string())?;
    // SAFETY: the handle is the live window Tauri just answered for, on the thread that owns it
    // (a synchronous command, or the window event handler — both the main thread).
    let done = unsafe {
        SetWindowPos(
            handle.0,
            std::ptr::null_mut(),
            position.x,
            position.y,
            size.width as i32,
            size.height as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if done == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// Elsewhere, the two steps: size first, so a runtime that clamps a window to its screen clamps the
/// size it is about to have rather than the one it is leaving.
#[cfg(not(windows))]
fn set_bounds(
    window: &WebviewWindow,
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
) -> Result<(), String> {
    window.set_size(size).map_err(|e| e.to_string())?;
    window.set_position(position).map_err(|e| e.to_string())
}

/// What the page last asked for, in CSS pixels: the drawing's box, how tall it is folded, and how
/// far down the edge the owner put it (a fraction of the work area — see [`hang_along`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Asked {
    width: f64,
    height: f64,
    rest: f64,
    along: f64,
}

/// The last box the page asked for.
///
/// Kept because nothing will ask again when the screen changes under the window: a new scale
/// factor, a new resolution or a taskbar that moved leave the element's CSS box exactly as it was,
/// so the page's `ResizeObserver` never fires. `refit` is what spends it.
static ASKED: Mutex<Option<Asked>> = Mutex::new(None);

/// The notch window's size follows its content, in CSS pixels, and it is re-placed on every change
/// — which is also how it unfolds when the pointer reaches it: the page grows, asks to be fitted,
/// and `hang` opens it leftwards and downwards from the corner the folded notch occupied.
///
/// `rest` is the drawing's height while folded, which is what the window is centred on (see
/// `hang`). Optional, so a page that sends none is centred on its whole height as before. `along`
/// is how far down the edge the owner dragged it, a fraction of the work area; optional, so a page
/// that sends none hangs in the middle as before.
///
/// Zero in either dimension hides the window. That is what the page sends when it has nothing to
/// draw, and a hidden window is the only honest picture of "nothing measured yet".
///
/// Answers only the notch window. The main window calling it would resize itself into a strip.
#[tauri::command]
pub fn notch_fit(
    window: WebviewWindow,
    width: f64,
    height: f64,
    rest: Option<f64>,
    along: Option<f64>,
) -> Result<(), String> {
    may_be_fitted(window.label())?;
    let asked = Asked {
        width,
        height,
        rest: rest.unwrap_or(height),
        along: along.unwrap_or(0.5),
    };
    if let Ok(mut last) = ASKED.lock() {
        *last = Some(asked);
    }
    place(&window, asked)
}

/// Re-applies the last box the page asked for, for a screen that changed under the window.
///
/// Silent: there is nobody to tell. This runs from a window event rather than from a call the page
/// made, and a window left where it was is still a notch.
pub fn refit(window: &WebviewWindow) {
    let asked = ASKED.lock().ok().and_then(|asked| *asked);
    if let Some(asked) = asked {
        let _ = place(window, asked);
    }
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
/// A move that fails is still broadcast — as the host that is **actually drawing**, which is not
/// always the word on disk. A dock that destroyed the floating window and then failed to write the
/// file would otherwise broadcast `global`: the main window keeps its own notch away, and the
/// window it is deferring to is already gone.
#[tauri::command]
pub fn notch_set_mode(app: AppHandle, mode: String) -> Result<(), String> {
    let mode = Mode::parse(&mode)?;
    std::thread::spawn(move || {
        if let Err(failed) = apply(&app, mode) {
            // The correction is written down as well as announced, so the next page to load is
            // told the same thing this one was. If even that fails there is still the broadcast,
            // which at least leaves every window now open drawing the right notch.
            if record(&app, failed.in_force).is_err() {
                let _ = app.emit(MODE_EVENT, failed.in_force.as_str());
            }
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

    /// Against the work area's own right edge and centred down it — neither of which is the
    /// screen's on a second monitor or with the taskbar down one side.
    #[test]
    fn the_notch_hangs_from_the_middle_of_the_right_edge() {
        assert_eq!(hang(0, 0, 1920, 1040, 200, 140, 140), (1720, 450));
        assert_eq!(hang(-1920, 40, 1920, 1000, 200, 140, 140), (-200, 470));
        // Bigger than the work area in both axes: pinned to its top-left corner rather than pushed
        // off the screen. `fit_size` clamps before this is reached, so what this covers is a screen
        // that changed under a window already placed.
        assert_eq!(hang(0, 0, 100, 100, 200, 140, 140), (0, 0));
    }

    /// The property the unfold rests on: a wider drawing opens LEFTWARDS, because `x` is measured
    /// back from the right edge. Written as the edge staying put rather than as two coordinates,
    /// because the coordinates are arithmetic and the edge is the promise.
    #[test]
    fn a_wider_notch_keeps_its_right_edge_where_it_was() {
        let folded = hang(0, 0, 1920, 1040, 46, 96, 96);
        let unfolded = hang(0, 0, 1920, 1040, 240, 96, 96);
        assert_eq!(folded.0 + 46, 1920);
        assert_eq!(unfolded.0 + 240, 1920);
        // And down the edge it has not moved either: the height did not change, so neither did `y`.
        assert_eq!(folded.1, unfolded.1);
    }

    /// The other half of the same promise, and the one the bumpy hover broke: a TALLER drawing
    /// opens downwards from where the folded one's top edge was. Centred on its own height, this
    /// unfold moved the window up by 60 pixels, and the rings the pointer was on went with it.
    #[test]
    fn a_taller_notch_keeps_its_top_edge_where_the_folded_one_had_it() {
        let folded = hang(0, 0, 1920, 1040, 62, 114, 114);
        let unfolded = hang(0, 0, 1920, 1040, 280, 234, 114);
        assert_eq!(folded.1, 463);
        assert_eq!(unfolded.1, folded.1);
        // A folded height taller than the window is read as the window's: a provider that dropped
        // out while the panel was open does not park the notch above the middle.
        assert_eq!(hang(0, 0, 1920, 1040, 62, 114, 400), folded);
    }

    /// Where the owner dragged it: the folded notch's middle at that fraction of the work area. At
    /// one half it is exactly the centred notch, which is what keeps every page that sends no
    /// fraction where it was.
    #[test]
    fn the_notch_hangs_where_it_was_dragged_down_the_edge() {
        // A 140px fold in a 1040px area: middle at 260 for a quarter, so the top at 190.
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 200, 140, 140, 0.25),
            (1720, 190)
        );
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 200, 140, 140, 0.5),
            hang(0, 0, 1920, 1040, 200, 140, 140)
        );
        // The work area's own origin carries through, as it does for the centred notch.
        assert_eq!(
            hang_along(-1920, 40, 1920, 1000, 200, 140, 140, 0.25),
            (-200, 220)
        );
    }

    /// Dragged to an end, the folded notch stops at the edge of the work area instead of hanging
    /// half off it; out of range is the nearer end, and not a number is the middle.
    #[test]
    fn a_notch_dragged_past_an_end_stops_at_the_edge() {
        assert_eq!(hang_along(0, 0, 1920, 1040, 200, 140, 140, 0.0), (1720, 0));
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 200, 140, 140, 1.0),
            (1720, 900)
        );
        assert_eq!(hang_along(0, 0, 1920, 1040, 200, 140, 140, -3.0), (1720, 0));
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 200, 140, 140, 7.0),
            (1720, 900)
        );
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 200, 140, 140, f64::NAN),
            hang(0, 0, 1920, 1040, 200, 140, 140)
        );
        // Unfolded near the bottom, the panel is still lifted to fit rather than run off.
        assert_eq!(
            hang_along(0, 0, 1920, 1040, 280, 300, 140, 1.0),
            (1640, 740)
        );
    }

    /// Opening downwards never runs past the bottom of the work area; the panel is lifted just
    /// enough to fit, and no further.
    #[test]
    fn an_unfold_that_would_run_off_the_bottom_is_lifted_to_fit() {
        // Centred on a 100px fold in a 400px area the top is at 150, and 300px of panel from there
        // would end at 450 — so the panel is lifted to end exactly at the bottom.
        assert_eq!(hang(0, 0, 1920, 400, 280, 300, 100), (1640, 100));
        assert_eq!(hang(0, 20, 1920, 400, 280, 300, 100), (1640, 120));
    }

    /// The main window asking to be fitted would resize itself into a strip.
    #[test]
    fn only_the_notch_window_is_fitted_to_its_content() {
        assert_eq!(may_be_fitted(LABEL), Ok(()));
        for label in ["main", "notch2", "NOTCH", ""] {
            assert!(may_be_fitted(label).is_err(), "{label:?}");
        }
    }

    /// Nothing to draw hides the window, and "nothing" includes every number that is not a size.
    /// A negative test would let NaN through, and the cast would make it a window.
    #[test]
    fn a_drawing_that_is_not_a_size_hides_the_window() {
        for (width, height) in [
            (0.0, 0.0),
            (0.0, 40.0),
            (200.0, 0.0),
            (0.5, 0.5),
            (-200.0, 40.0),
            (f64::NAN, 40.0),
            (200.0, f64::NAN),
            (f64::NEG_INFINITY, 40.0),
        ] {
            assert_eq!(
                fit_size(width, height, 1.0, 1920, 1080),
                None,
                "{width} x {height}"
            );
        }
    }

    /// CSS pixels times the scale, rounded up so a half-pixel of drawing is never cropped.
    #[test]
    fn the_drawing_is_scaled_to_physical_pixels() {
        assert_eq!(fit_size(200.0, 40.0, 1.0, 1920, 1080), Some((200, 40)));
        assert_eq!(fit_size(200.0, 40.0, 1.25, 1920, 1080), Some((250, 50)));
        assert_eq!(fit_size(200.5, 40.1, 1.0, 1920, 1080), Some((201, 41)));
        // A scale that is not a positive, finite number is read as no scale rather than as a size.
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                fit_size(200.0, 40.0, scale, 1920, 1080),
                Some((200, 40)),
                "{scale}"
            );
        }
    }

    /// The notch never grows past the screen it hangs from. Unclamped, an infinity became a
    /// `u32::MAX` window — transparent, always on top, over the whole desktop, and with the pin
    /// that would dock it underneath.
    #[test]
    fn the_notch_is_never_bigger_than_the_work_area() {
        assert_eq!(
            fit_size(f64::INFINITY, 40.0, 1.0, 1920, 1080),
            Some((1920, 40))
        );
        assert_eq!(
            fit_size(1e300, 1e300, 1e300, 1920, 1080),
            Some((1920, 1080))
        );
        assert_eq!(
            fit_size(4000.0, 4000.0, 2.0, 1920, 1080),
            Some((1920, 1080))
        );
        // A work area reported as nothing still leaves a pixel, rather than the 0x0 window that a
        // clamp to zero would show.
        assert_eq!(fit_size(200.0, 40.0, 1.0, 0, 0), Some((1, 1)));
    }

    /// The size the page asked for is kept, because nothing will ask again when the screen changes
    /// under the window: the CSS box does not move, so the page's `ResizeObserver` never fires.
    /// `refit` is what spends it, from the window event arm in `lib.rs`.
    #[test]
    fn the_size_the_page_asked_for_is_kept_for_a_screen_that_changes_later() {
        let mut asked = ASKED.lock().expect("the size is not poisoned");
        assert_eq!(
            *asked, None,
            "nothing has been measured in this test binary"
        );
        let box_ = Asked {
            width: 200.0,
            height: 40.0,
            rest: 40.0,
            along: 0.5,
        };
        *asked = Some(box_);
        assert_eq!(*asked, Some(box_));
        *asked = None;
    }

    /// A directory of this test's own, so the round trip is over a real file and never over the
    /// app's own config directory.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nucleos-notch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The word the page asked for is the word the next launch reads back, including through a
    /// config directory that does not exist yet.
    #[test]
    fn the_mode_survives_the_round_trip_through_the_file() {
        let dir = scratch("round-trip");
        // Nothing written yet: contained, the host that cannot fail to draw.
        assert_eq!(stored_in(&dir), Mode::Contained);
        for mode in [Mode::Global, Mode::Contained, Mode::Global] {
            record_in(&dir, mode).expect("the word is written");
            assert_eq!(stored_in(&dir), mode);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The window goes up before the word is written down, and never the other way round.
    #[test]
    fn the_window_moves_before_the_word_is_recorded() {
        let steps = std::cell::RefCell::new(Vec::new());
        let done = apply_with(
            Mode::Global,
            |mode| {
                steps.borrow_mut().push(format!("window {}", mode.as_str()));
                Ok(())
            },
            |mode| {
                steps.borrow_mut().push(format!("record {}", mode.as_str()));
                Ok(())
            },
        );
        assert!(done.is_ok());
        assert_eq!(steps.into_inner(), ["window global", "record global"]);
    }

    /// A window that never came up is not written down as the host — the main window would put its
    /// own notch away on the broadcast and the owner would be left with none (risk R1).
    #[test]
    fn a_window_that_did_not_open_is_never_recorded_as_the_host() {
        let recorded = std::cell::Cell::new(false);
        let failed = apply_with(
            Mode::Global,
            |_| Err("no webview".into()),
            |_| {
                recorded.set(true);
                Ok(())
            },
        )
        .expect_err("an open that failed is a failure");
        assert!(!recorded.get());
        assert_eq!(failed.in_force, Mode::Contained);
        assert_eq!(failed.reason, "no webview");
    }

    /// What is broadcast after a failure is the host that is drawing, not the word on disk.
    #[test]
    fn a_failure_names_the_host_that_is_actually_drawing() {
        // The window is gone and only the file refused: contained is in force, whatever the file
        // still says.
        let failed = apply_with(Mode::Contained, |_| Ok(()), |_| Err("read-only".into()))
            .expect_err("a write that failed is a failure");
        assert_eq!(failed.in_force, Mode::Contained);
        // The window would not go away: it is still up, so it is still the host.
        let failed = apply_with(Mode::Contained, |_| Err("busy".into()), |_| Ok(()))
            .expect_err("a destroy that failed is a failure");
        assert_eq!(failed.in_force, Mode::Global);
        // The window came up and only the file refused: global is in force.
        let failed = apply_with(Mode::Global, |_| Ok(()), |_| Err("read-only".into()))
            .expect_err("a write that failed is a failure");
        assert_eq!(failed.in_force, Mode::Global);
    }

    /// A launch whose notch window did not open lands on contained, and says so — otherwise the
    /// file keeps saying `global`, the main window draws nothing, and the pin that would fix it is
    /// in the window that does not exist.
    #[test]
    fn a_launch_whose_window_did_not_open_falls_back_to_contained() {
        assert_eq!(after_startup(Mode::Global, true), Mode::Global);
        assert_eq!(after_startup(Mode::Global, false), Mode::Contained);
        assert_eq!(after_startup(Mode::Contained, false), Mode::Contained);
        assert_eq!(after_startup(Mode::Contained, true), Mode::Contained);
    }
}
