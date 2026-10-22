//! The quick-capture window: a small always-on-top box, opened by the global capture chord, that
//! takes one note without bringing the main window forward.
//!
//! The drawing is the page's (`shell/src/quick-capture/QuickCapture.tsx`, route `/quick-capture`);
//! this module owns only the window and the one decision of whether to create it or reveal the one
//! that already exists. Unlike the notch window, this one holds a capability
//! (`capabilities/quick-capture.json`): it hides itself from the page on Enter, Esc and blur, and
//! that is a window API, which the ACL only answers for a window that was granted it. The grant is
//! exactly `core:window:allow-hide`; focus and blur are plain DOM events and need none.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The window's label. The capability file scopes its permissions to this exact word.
pub const LABEL: &str = "quick-capture";

/// What the page loads: the app's own bundle, told by the query to draw only the capture box. One
/// bundle rather than a second entry point, for the reason `notch::URL` gives.
const URL: &str = "index.html?window=quick-capture";

/// Logical size of the box.
const WIDTH: f64 = 520.0;
const HEIGHT: f64 = 220.0;

/// What the chord does, given whether the window has been created before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// First press: build the window, which shows itself and takes focus.
    Create,
    /// Later presses: the window exists, hidden or not; show it and focus it again.
    Reveal,
}

fn plan(exists: bool) -> Plan {
    if exists {
        Plan::Reveal
    } else {
        Plan::Create
    }
}

/// Builds the window. It blocks until the main thread has created it, so it must never run on the
/// main thread itself: see the "Threading" note in `browser.rs`.
fn build(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(URL.into()))
        .title("NucleOS quick capture")
        .inner_size(WIDTH, HEIGHT)
        .center()
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(true)
        // Same origin as the main window (`useHttpsScheme` in tauri.conf.json): the
        // daemon's CORS allows `https://tauri.localhost` only.
        .use_https_scheme(true)
        .build()?;
    Ok(())
}

/// Opens the quick-capture window, or shows and focuses it when it already exists.
///
/// The chord's handler may run on the main thread, and on Windows building a window from there can
/// deadlock (`browser.rs` records the same hazard for its own build). The first press therefore
/// hands the build to `spawn_blocking` and returns at once; a failed build is logged from there,
/// because the caller has already moved on. Revealing an existing window only queues messages to the
/// main thread, so it stays inline and its errors are returned.
///
/// The main window is deliberately left alone either way.
pub fn open(app: &AppHandle) -> tauri::Result<()> {
    match plan(app.get_webview_window(LABEL).is_some()) {
        Plan::Reveal => {
            if let Some(window) = app.get_webview_window(LABEL) {
                let _ = window.unminimize();
                window.show()?;
                window.set_focus()?;
            }
            Ok(())
        }
        Plan::Create => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = build(&app) {
                    eprintln!("[quick-capture] could not build the window: {error}");
                }
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_press_creates_the_window_and_later_ones_reveal_it() {
        assert_eq!(plan(false), Plan::Create);
        assert_eq!(plan(true), Plan::Reveal);
    }

    #[test]
    fn the_url_asks_the_bundle_for_the_quick_capture_face() {
        assert_eq!(URL, "index.html?window=quick-capture");
        assert!(URL.ends_with(LABEL));
    }

    // Compile-time checks: the sizes are constants, so a runtime `assert!` on them is a constant
    // assertion (clippy `assertions_on_constants`).
    const _: () = assert!(WIDTH > HEIGHT, "the box is wider than tall");
    const _: () = assert!(WIDTH <= 600.0 && HEIGHT <= 300.0, "the box is small");

    /// The first press runs on the chord's handler thread, which may be the main one, where building
    /// a window can deadlock on Windows. There is no app here to raise the chord, so the source is
    /// read: the create arm must hand the build to `spawn_blocking` and not build inline.
    #[test]
    fn the_build_is_not_inlined_into_the_first_press() {
        let source = include_str!("quick_capture.rs");
        let create = source
            .split("Plan::Create => {")
            .nth(1)
            .expect("the create arm exists")
            .split("\n        }")
            .next()
            .expect("the create arm ends");
        assert!(
            create.contains("tauri::async_runtime::spawn_blocking"),
            "{create}"
        );
        assert!(!create.contains("WebviewWindowBuilder"), "{create}");
    }

    /// The capability is scoped by label, so a renamed label would silently lose its permissions.
    /// The permission list is exact: the page needs `hide` and nothing else, and `core:default`
    /// would add event, webview, window-read, app, path and resource APIs it never calls.
    #[test]
    fn the_capability_names_this_window_and_nothing_else() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/quick-capture.json"))
                .expect("quick-capture.json parses");
        assert_eq!(capability["windows"], serde_json::json!([LABEL]));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["core:window:allow-hide"])
        );
    }
}
