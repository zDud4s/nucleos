//! The agy (Antigravity CLI) engine: stubs until the leaves that own it land.
//!
//! Leaf B replaces `enabled` with the opt-in document read, Leaf C wires `runner` to the CLI runner
//! and Leaf D settles the rest. Until then agy is off, absent and has no runner.

use std::sync::Arc;

use crate::config::ModelsConfig;
use crate::execution::runner::CommandRunner;

/// Whether the opt-in setting is on.
pub fn enabled() -> bool {
    false
}

/// Whether the agy binary resolves on this machine.
pub fn installed() -> bool {
    false
}

/// The agy runner, built from the models file.
pub fn runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    None
}
