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
pub mod browser_live;
pub use nucleos_base::browser_policy;
pub mod browser_seat;
pub mod browser_wheel;
pub mod budget;
pub mod calendar;
pub mod capabilities;
pub mod chat_groups;
pub mod chat_notices;
pub mod chat_tasks;
pub mod chats;
pub mod classifier;
pub mod collision;
pub use nucleos_base::command_reader;
pub use nucleos_base::commands;
pub mod concurrency;
pub mod config;
pub mod consolidate;
pub mod contacts;
pub mod council;
pub mod daemon_client;
pub mod detect;
pub mod devtime;
pub mod devtime_lanes;
pub mod devtime_map;
pub mod devtime_parse;
pub mod devtime_precision;
pub mod devtime_rules;
pub mod devtime_rules_a;
pub mod devtime_rules_b;
pub mod devtime_rules_c;
pub mod devtime_rules_cmd;
pub mod devtime_rules_dctx;
pub mod devtime_rules_dflow;
pub mod devtime_rules_f;
#[cfg(test)]
pub mod devtime_rules_fixture;
pub mod devtime_store;
pub mod devtime_unexplained;
pub mod distill;
pub mod distill_model;
pub mod distill_origin;
pub mod door;
pub mod email;
pub mod embed;
pub mod embed_model;
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
pub mod local_agent;
pub use nucleos_base::logging;
pub mod machine_config;
pub mod mailsend;
pub mod map_anchor;
pub mod map_intent;
pub mod map_items;
pub mod map_join;
pub mod map_orphan;
pub mod map_recency;
pub mod map_seam;
pub mod map_stamp;
pub mod map_store;
pub mod map_triage;
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
pub mod project_commands;
pub use nucleos_base::project_exit;
pub mod project_map;
pub mod project_policy;
pub mod project_readings;
pub mod project_state;
pub use nucleos_base::prompt_budget;
pub mod proposals;
pub mod quota;
pub mod quota_client;
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
pub mod team_trigger;
pub mod test_select;
#[cfg(any(test, feature = "testkit"))]
pub use nucleos_base::testdb;
pub use nucleos_base::tests_map;
pub mod token_efficiency;
pub use nucleos_base::transcribe;
pub mod triage;
pub use nucleos_base::trust;
pub mod vcs;
pub mod verify;
pub mod verify_exec;
pub mod verify_fingerprint;
pub mod verify_guard;
pub mod verify_observe;
pub mod verify_plan;
pub mod verify_runs;
pub use nucleos_base::verify_sched;
pub mod verify_store;
pub mod voice;
pub use nucleos_base::warm;
pub mod wave;
pub mod web;
pub use nucleos_base::web_client;
pub mod webhook;
pub mod wip;
pub mod workflow_graph;
pub mod workflow_materialize;
pub mod workflow_package;
pub mod workflows;
pub mod worktree;
