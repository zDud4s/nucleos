//! The bottom of the núcleo: storage, the test database, process and gate plumbing, and the other
//! modules that name nothing above them.
//!
//! Split out of `nucleos-core` on 2026-10-07 so an edit higher up does not recompile these, and an
//! edit here does not have to wait for the 280k lines above them before its own tests run.
//! `nucleos-core`'s library re-exports every module at its root (`pub use nucleos_base::storage;`),
//! so `crate::storage::...` there still names the same thing.
//!
//! `testkit` exposes the fixtures the crates above use in their own tests.

#![cfg_attr(all(feature = "testkit", not(test)), allow(dead_code, unused_imports))]
#![allow(clippy::new_without_default, clippy::should_implement_trait)]

pub mod agent;
pub mod autostart;
pub mod backup;
pub mod browser_client;
pub mod browser_policy;
pub mod command_reader;
pub mod commands;
pub mod feed;
pub mod gate;
pub mod handoff;
pub mod inspect;
pub mod join;
pub mod logging;
pub mod mentions;
pub mod notes;
pub mod notify_policy;
pub mod pressure;
pub mod process_tree;
pub mod project_exit;
pub mod prompt_budget;
pub mod recurrence;
pub mod router_client;
pub mod search;
pub mod secrets;
pub mod storage;
pub mod team_notes;
#[cfg(any(test, feature = "testkit"))]
pub mod testdb;
pub mod tests_map;
pub mod transcribe;
pub mod trust;
pub mod verify_sched;
pub mod warm;
pub mod web_client;
