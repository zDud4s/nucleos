//! Tree jobs: the daemon-side driver of a tree of work nodes, from manifest to landing.

mod auto_default;
mod autofmt;
#[cfg(test)]
mod autofmt_driver_tests;
pub mod branch_start;
pub mod budget;
pub mod checklist;
pub mod close;
pub mod commands;
pub mod decisions;
pub mod docs_paths;
pub mod driver;
pub mod engine;
mod final_verify;
mod gate;
mod gate_summary;
#[cfg(test)]
mod gate_tests;
pub mod handoff_park;
pub mod http;
mod ladder;
pub mod landing;
pub mod model;
pub mod nested_prompt;
pub mod nesting;
#[cfg(test)]
mod nesting_e2e;
pub mod node_run;
pub mod open;
pub mod partition;
mod pending_start;
mod relaunch;
pub mod report;
mod rescue;
mod retry;
pub mod root;
mod root_disk;
mod root_orphan;
pub mod root_pending;
pub mod run_end;
pub mod start_log;
pub mod store;
pub mod watchdog;
pub mod wrap;
