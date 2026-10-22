//! `nucleos-app`: the top of the núcleo's library, compiled as its own crate
//! (`core/crates/nucleos-app/Cargo.toml` points here).
//!
//! These modules are read by the daemon and by nothing in `nucleos-core`, so an edit to one of them
//! recompiles this crate and the binary only. The glob import below keeps every
//! `crate::state::...`-style path inside them naming the same item it did when they were part of
//! `nucleos-core`; a module of `nucleos-core` that starts reading one of these does not compile,
//! which is the point. Split out on 2026-10-22 (the lib was one dependency cycle of 95% of its
//! lines, so every edit recompiled all of it).

#![cfg_attr(all(feature = "testkit", not(test)), allow(dead_code, unused_imports))]
// Same reason as in `lib.rs`: `pub` here means "visible to the binary".
#![allow(clippy::new_without_default, clippy::should_implement_trait)]

#[allow(unused_imports)]
use nucleos_core::*;

pub mod analytics;
pub mod analytics_pipeline;
pub mod devtime;
pub mod fleet;
pub mod hooks;
pub mod presets;
pub mod price_refresh;
pub mod verify_cli;
