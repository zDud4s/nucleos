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

pub mod agent;
pub mod assistant;
pub mod assistants;
pub mod attention;
pub mod auth;
pub mod autopilot;
pub mod autostart;
pub mod backup;
pub mod brief;
pub mod browser;
pub mod browser_client;
pub mod browser_live;
pub mod browser_policy;
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
pub mod command_reader;
pub mod commands;
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
pub mod devtime_store;
pub mod distill;
pub mod distill_model;
pub mod distill_origin;
pub mod door;
pub mod email;
pub mod exclusion;
pub mod feed;
pub mod files;
pub mod gate;
pub mod git_exec;
pub mod github;
pub mod handoff;
pub mod health;
pub mod hooks;
pub mod inspect;
pub mod job;
pub mod join;
pub mod judge;
pub mod knowledge;
pub mod land;
pub mod local_agent;
pub mod logging;
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
pub mod mentions;
pub mod model_catalog;
pub mod notes;
pub mod notify;
pub mod notify_policy;
pub mod onboarding;
pub mod openai_compatible;
pub mod owner_notes;
pub mod ownership;
pub mod pii_shadow;
pub mod presets;
pub mod pressure;
pub mod priority;
pub mod process_tree;
pub mod project_commands;
pub mod project_exit;
pub mod project_map;
pub mod project_policy;
pub mod project_readings;
pub mod project_state;
pub mod prompt_budget;
pub mod proposals;
pub mod quota;
pub mod quota_client;
pub mod recurrence;
pub mod redact;
pub mod relay;
pub mod repo_trigger;
pub mod resolver;
pub mod route_advice;
pub mod route_report;
pub mod router_client;
pub mod run_stop;
pub mod runner;
pub mod runs;
pub mod scheduler;
pub mod search;
pub mod seat_advice;
pub mod secrets;
pub mod seed;
pub mod sessions;
pub mod shadow;
pub mod sidecar;
pub mod speak;
pub mod speed;
pub mod state;
pub mod storage;
pub mod team;
pub mod team_notes;
pub mod team_trigger;
pub mod test_select;
#[cfg(any(test, feature = "testkit"))]
pub mod testdb;
pub mod tests_map;
pub mod token_efficiency;
pub mod transcribe;
pub mod triage;
pub mod trust;
pub mod vcs;
pub mod verify_exec;
pub mod verify_runs;
pub mod verify_sched;
pub mod voice;
pub mod warm;
pub mod wave;
pub mod web;
pub mod web_client;
pub mod webhook;
pub mod wip;
pub mod workflow_graph;
pub mod workflow_materialize;
pub mod workflow_package;
pub mod workflows;
pub mod worktree;
