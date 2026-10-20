//! Agent packages: the versioned, file-based source of the factory agents (spec
//! 2026-10-20-agentes-agnosticos-design, §3). Each submodule has exactly one owner.

pub mod capabilities;
pub mod install;
pub mod launch;
pub mod output;
pub mod package;
pub mod prompt;
