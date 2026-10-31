//! The agy (Antigravity CLI) engine: the opt-in and its warning; `installed` and `runner` are stubs
//! until the leaves that own them land.
//!
//! Leaf C wires `runner` to the CLI runner and Leaf D settles the rest.

use std::sync::Arc;

use crate::config::ModelsConfig;
use crate::execution::runner::CommandRunner;
use crate::settings::Scope;

pub(crate) use super::agy_setting::AgyDoc;

/// What the owner must accept before the engine is enabled. Shown by `GET /engines/agy`.
pub const WARNING: &str = "Antigravity CLI use here is not supported by Google on consumer plans. Google's FAQ and Terms do not support running agy this way, and your Google account may be suspended. Enable only if you accept that risk.";

/// The version of [`WARNING`]. Raise it when the text changes, so an old acknowledgement stops counting.
pub const WARNING_VERSION: u32 = 1;

/// Whether the opt-in setting is on. The one reader of the document; false when it cannot be read.
pub fn enabled() -> bool {
    crate::settings::get_opt::<AgyDoc>(&Scope::Machine).is_some_and(|doc| doc.enabled)
}

/// Whether the agy binary resolves on this machine.
pub fn installed() -> bool {
    false
}

/// The agy runner, built from the models file.
pub fn runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    None
}
