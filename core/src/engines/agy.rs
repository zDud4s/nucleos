//! The agy (Antigravity CLI) engine: the opt-in, its warning, the install probe and the runner
//! constructor. Every runner is built only while the opt-in is on.

use std::sync::Arc;

use crate::config::ModelsConfig;
use crate::execution::runner::CommandRunner;
use crate::settings::Scope;

pub use super::agy_setting::AgyDoc;

/// What the owner must accept before the engine is enabled. Shown by `GET /engines/agy`.
pub const WARNING: &str = "Antigravity CLI use here is not supported by Google on consumer plans. Google's FAQ and Terms do not support running agy this way, and your Google account may be suspended. Enable only if you accept that risk.";

/// The version of [`WARNING`]. Raise it when the text changes, so an old acknowledgement stops counting.
pub const WARNING_VERSION: u32 = 1;

/// Whether the opt-in setting is on AND acknowledged at the current [`WARNING_VERSION`]. The one
/// reader of the document; false when it cannot be read. A stored row is not run through `finish`
/// on load, so the acknowledgement is checked here: raising the version revokes.
pub fn enabled() -> bool {
    crate::settings::get_opt::<AgyDoc>(&Scope::Machine)
        .is_some_and(|doc| doc.enabled && doc.acknowledged_warning_version == Some(WARNING_VERSION))
}

/// The model an agy runner uses when nothing pins one.
pub const DEFAULT_MODEL: &str = "agy/gemini-3.8-flash-high";

/// Whether the agy binary resolves on this machine.
pub fn installed() -> bool {
    crate::execution::runner::agy_program().is_some()
}

/// An agy runner for `model` (the catalogue's `agy/` prefix is accepted), or `None` while the
/// opt-in is off. The one place a real `AgyCliRunner` is built.
pub fn runner_for(model: Option<&str>) -> Option<Arc<dyn CommandRunner>> {
    if !enabled() {
        return None;
    }
    Some(Arc::new(crate::execution::runner::AgyCliRunner {
        model: model.unwrap_or(DEFAULT_MODEL).to_string(),
        effort: None,
    }))
}

/// The agy runner, built from the models file (which has no agy key: the default model applies).
pub fn runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    runner_for(None)
}
