//! The live fleet tree: every running agent session on this machine, normalised, pushed to the
//! shell over one NDJSON stream. Design: `.ai/specs/2026-10-20-fleet-arvore-viva-design.md`.

pub mod event;
pub mod limits;
pub mod model;
pub mod source;
pub mod tail;
