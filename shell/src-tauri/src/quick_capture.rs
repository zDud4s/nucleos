//! The quick-capture window: a small always-on-top box, opened by the global capture chord, that
//! takes one note without bringing the main window forward.
//!
//! The drawing is the page's (`shell/src/quick-capture/QuickCapture.tsx`, route `/quick-capture`);
//! this module owns only the window and the one decision of whether to create it or reveal the one
//! that already exists. Unlike the notch window, this one holds a capability
//! (`capabilities/quick-capture.json`): it hides itself from the page on Enter, Esc and blur, and
//! that is a window API, which the ACL only answers for a window that was granted it.

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

/// Opens the quick-capture window, or shows and focuses it when it already exists.
///
/// Errors are returned for the caller to log; there is nothing else useful to do with one, and the
/// main window is deliberately left alone either way.
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

    #[test]
    fn the_box_is_small_and_wider_than_tall() {
        assert!(WIDTH > HEIGHT);
        assert!(WIDTH <= 600.0 && HEIGHT <= 300.0);
    }

    /// The capability is scoped by label, so a renamed label would silently lose its permissions.
    #[test]
    fn the_capability_names_this_window_and_nothing_else() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/quick-capture.json"))
                .expect("quick-capture.json parses");
        assert_eq!(capability["windows"], serde_json::json!([LABEL]));
        let permissions = capability["permissions"].to_string();
        assert!(
            permissions.contains("core:window:allow-hide"),
            "{permissions}"
        );
    }
}
