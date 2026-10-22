//! The núcleo as a library: every module except the HTTP door and `main`.
//!
//! Split out on 2026-10-07 so an edit to `http.rs` (the most-changed file in the core) recompiles
//! only the binary, and an edit here does not drag the 38k-line router along with it. The
//! binary glob-imports this crate at its root, so `crate::execution::runs::...` inside `http.rs` and
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
pub mod agents;
pub mod assistant;
pub mod assistants;
pub mod attention;
pub mod auth;
pub use nucleos_base::autostart;
pub use nucleos_base::backup;
pub mod brief;
pub mod browser;
pub use nucleos_base::browser_client;
pub use nucleos_base::browser_policy;
pub mod capabilities;
pub mod capture;
pub mod channels;
pub mod chats;
pub use nucleos_base::command_reader;
pub use nucleos_base::commands;
pub mod config;
pub mod consolidate;
pub mod context_refs;
pub mod council;
pub mod daemon_client;
pub mod detect;
pub mod distill;
pub mod door;
pub mod embed;
pub mod execution;
pub use nucleos_base::feed;
pub mod files;
pub use nucleos_base::gate;
pub mod governance;
pub use nucleos_base::handoff;
pub mod health;
pub use nucleos_base::inspect;
pub mod integrations;
pub use nucleos_base::join;
pub mod judge;
pub mod knowledge;
pub mod loadout;
pub use nucleos_base::logging;
pub mod machine_config;
pub mod map;
pub mod mcp_tools;
pub use nucleos_base::mentions;
pub mod models;
pub use nucleos_base::notes;
pub use nucleos_base::notify_policy;
pub mod onboarding;
pub mod owner_notes;
pub mod ownership;
pub use nucleos_base::pressure;
pub use nucleos_base::process_tree;
pub mod project;
pub use nucleos_base::project_exit;
pub use nucleos_base::prompt_budget;
pub mod proposals;
pub mod quota;
pub use nucleos_base::recurrence;
pub mod relay;
pub mod repo_trigger;
pub mod resolver;
pub use nucleos_base::router_client;
pub use nucleos_base::search;
pub use nucleos_base::secrets;
pub mod seed;
pub mod sessions;
pub mod shutdown;
pub mod sidecar;
#[cfg(any(test, feature = "testkit"))]
pub mod source_scan;
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
pub use nucleos_base::trust;
pub mod verify;
pub use nucleos_base::verify_sched;
pub use nucleos_base::wake;
pub use nucleos_base::warm;
pub use nucleos_base::web_client;
pub mod wip;
pub mod work_nodes;
pub mod workflow_records;
pub mod workflows;

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

// The phase-4 guards only read files too: no v1 Telegram plumbing left, shared routes still used.
#[cfg(test)]
#[path = "../tests/no_legacy_telegram.rs"]
mod no_legacy_telegram;

#[cfg(test)]
#[path = "../tests/routes_still_used.rs"]
mod routes_still_used;
