//! The núcleo as a library: every module except the HTTP door and `main`.
//!
//! Split out on 2026-10-07 so an edit to `http.rs` (the most-changed file in the core) recompiles
//! only the binary, and an edit here does not drag the 38k-line router along with it. The
//! binary glob-imports this crate at its root, so `crate::runs::...` inside `http.rs` and
//! `main.rs` still names the same thing it always did.
//!
//! `testkit` exposes the test doubles and fixtures the binary's own tests use. Cargo enables it
//! for every test build through the self dev-dependency in `Cargo.toml`; a release build never
//! sees it.

// The testkit build carries fixtures for the binary's tests and nothing else, so what it leaves
// unused says nothing: dead code is judged by the plain library and its own test build.
#![cfg_attr(all(feature = "testkit", not(test)), allow(dead_code, unused_imports))]
// `pub` here means "visible to the binary", not a published API: these clippy lints only fire on
// exported items, and every item became exported at the split without any of them changing.
#![allow(clippy::new_without_default, clippy::should_implement_trait)]

pub use nucleos_base::agent;
pub mod assistant;
pub mod assistants;
pub mod attention;
pub mod auth;
pub mod autopilot;
pub use nucleos_base::autostart;
pub use nucleos_base::backup;
pub mod brief;
pub mod browser;
pub use nucleos_base::browser_client;
pub use nucleos_base::browser_policy;
pub mod budget;
pub mod calendar;
pub mod capabilities;
pub mod capture;
pub mod chats;
pub mod classifier;
pub mod collision;
pub use nucleos_base::command_reader;
pub use nucleos_base::commands;
pub mod concurrency;
pub mod config;
pub mod consolidate;
pub mod contacts;
pub mod context_refs;
pub mod council;
pub mod daemon_client;
pub mod detect;
pub mod devtime;
pub mod distill;
pub mod door;
pub mod email;
pub mod embed;
pub mod exclusion;
pub use nucleos_base::feed;
pub mod files;
pub use nucleos_base::gate;
pub mod git_exec;
pub mod github;
pub use nucleos_base::handoff;
pub mod health;
pub mod hooks;
pub use nucleos_base::inspect;
pub mod job;
pub use nucleos_base::join;
pub mod judge;
pub mod knowledge;
pub mod land;
pub mod loadout;
pub mod local_agent;
pub use nucleos_base::logging;
pub mod machine_config;
pub mod mailsend;
pub mod map;
pub mod mcp_tools;
pub use nucleos_base::mentions;
pub mod model_catalog;
pub use nucleos_base::notes;
pub mod notify;
pub use nucleos_base::notify_policy;
pub mod onboarding;
pub mod openai_compatible;
pub mod owner_notes;
pub mod ownership;
pub mod pii_shadow;
pub mod presets;
pub use nucleos_base::pressure;
pub mod priority;
pub use nucleos_base::process_tree;
pub mod project;
pub use nucleos_base::project_exit;
pub use nucleos_base::prompt_budget;
pub mod proposals;
pub mod quota;
pub mod quote_guard;
pub use nucleos_base::recurrence;
pub mod redact;
pub mod relay;
pub mod repo_trigger;
pub mod resolver;
pub mod route_advice;
pub mod route_report;
pub use nucleos_base::router_client;
pub mod run_stop;
pub mod runner;
pub mod runs;
pub mod scheduler;
pub use nucleos_base::search;
pub mod seat_advice;
pub use nucleos_base::secrets;
pub mod seed;
pub mod sessions;
pub mod shadow;
pub mod sidecar;
#[cfg(any(test, feature = "testkit"))]
pub mod source_scan;
pub mod speak;
pub mod speed;
pub mod state;
pub use nucleos_base::storage;
pub mod team;
pub use nucleos_base::team_notes;
pub mod test_select;
#[cfg(any(test, feature = "testkit"))]
pub use nucleos_base::testdb;
pub use nucleos_base::tests_map;
pub mod token_efficiency;
pub mod tool_loadout;
pub use nucleos_base::transcribe;
pub mod triage;
pub use nucleos_base::trust;
pub mod vcs;
pub mod verify;
pub mod verify_cli;
pub use nucleos_base::verify_sched;
pub mod voice;
pub use nucleos_base::warm;
pub mod wave;
pub mod web;
pub use nucleos_base::web_client;
pub mod webhook;
pub mod wip;
pub mod workflows;
pub mod worktree;

// The module-map check only reads files, so it runs inside this library's test binary. As an
// integration test under `tests/` it made every `cargo test` build the daemon executable too, a
// second full compile of `http.rs` that no test used.
#[cfg(test)]
#[path = "../tests/module_map.rs"]
mod module_map;

// Same reason, same place: the size check only reads files.
#[cfg(test)]
#[path = "../tests/file_size.rs"]
mod file_size;
