//! Gate processes through the sandbox door (spec `2026-09-14-sandbox-de-so-design`, D3, D13).
//!
//! The gate runner lives in `nucleos-base`, below this crate, so it cannot name the door; it takes
//! a launcher instead (`nucleos_base::gate::Launch`). This module is the launcher every caller in
//! the daemon passes: it derives a [`Profile`] from the run's mode and starts the child with
//! [`spawn_prepared`].
//!
//! Two modes, two scopes:
//! - [`RunMode::RunGate`] measures a run's own worktree, so that worktree is the unit folder and the
//!   profile may write it.
//! - [`RunMode::QueueGate`] still runs in the owner's integration worktree (the queue gate is not
//!   contained yet). The scope names that folder as the owner's checkout, so `profile_for` grants it
//!   to no one: the profile's `writable` never holds it. Nothing here says the queue gate is
//!   contained, and with no backend nothing is enforced either way.
//!
//! D13: this only builds a scope. It creates no folder and reads no environment.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::{Profile, RunMode, RunScope, profile_for, spawn_prepared};
use crate::gate::{ArgvOutcome, GateOutcome, Launched};

/// The scope of a gate process that runs in `dir`.
fn gate_scope(mode: RunMode, dir: &Path) -> RunScope {
    RunScope {
        mode,
        unit_dir: dir.to_path_buf(),
        // The queue gate runs in a folder the owner works in; a run gate's folder is its own.
        main_checkout: (mode == RunMode::QueueGate).then(|| dir.to_path_buf()),
        main_repo_objects: None,
        tree_caches: Vec::new(),
        toolchain: Vec::new(),
        api_domains: Vec::new(),
        registry_domains: Vec::new(),
        run_env: BTreeMap::new(),
        inherit: BTreeMap::new(),
        proxy: None,
    }
}

/// What a gate of `mode` running in `dir` is granted.
pub fn gate_profile(mode: RunMode, dir: &Path) -> Profile {
    profile_for(&gate_scope(mode, dir))
}

/// Starts `command` through the door under `profile`, in the shape the gate runner expects.
fn start(command: Command, profile: &Profile) -> io::Result<Launched> {
    let contained = spawn_prepared(command, profile)?;
    let super::Contained { killer, child } = contained;
    Ok(Launched { killer, child })
}

/// [`crate::gate::run_argv`] with the process started through the door under the profile of `mode`.
pub async fn run_argv(
    mode: RunMode,
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> ArgvOutcome {
    let profile = gate_profile(mode, cwd);
    let launch = move |command: Command| start(command, &profile);
    crate::gate::run_argv(&launch, argv, cwd, env, timeout).await
}

/// [`crate::gate::run_gate`] with the process started through the door under the profile of `mode`.
pub async fn run_gate(
    mode: RunMode,
    worktree: &Path,
    project_root: &Path,
    command: &str,
    timeout: Duration,
) -> GateOutcome {
    let profile = gate_profile(mode, worktree);
    let launch = move |command: Command| start(command, &profile);
    crate::gate::run_gate(&launch, worktree, project_root, command, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Egress;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[tokio::test]
    async fn a_run_gate_starts_through_the_door_with_the_rungate_profile() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let profile = gate_profile(RunMode::RunGate, worktree.path());
        assert_eq!(profile.writable, vec![worktree.path().to_path_buf()]);
        assert_eq!(
            profile.egress,
            Egress::Closed,
            "no domains were put in the scope"
        );
        assert!(
            !profile.env.contains_key("NUCLEOS_DAEMON_TOKEN"),
            "the control token is never granted"
        );

        let outcome = run_argv(
            RunMode::RunGate,
            &argv(&["git", "--version"]),
            worktree.path(),
            &[],
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(outcome.exit_code, Some(0), "tail was: {}", outcome.tail);
        assert!(outcome.error.is_none());

        let verdict = run_gate(
            RunMode::RunGate,
            worktree.path(),
            worktree.path(),
            r#"sh -c "exit 3""#,
            Duration::from_secs(30),
        )
        .await;
        assert!(
            matches!(verdict, GateOutcome::Failed { exit_code: 3, .. }),
            "got {verdict:?}"
        );
    }

    #[tokio::test]
    async fn the_queue_gate_uses_the_queuegate_profile_and_never_the_integration_worktree_as_writable()
     {
        let integration = tempfile::tempdir().expect("create temporary integration worktree");

        let profile = gate_profile(RunMode::QueueGate, integration.path());
        assert!(
            !profile
                .writable
                .iter()
                .any(|path| path.starts_with(integration.path())),
            "the integration worktree must not be writable: {:?}",
            profile.writable
        );
        assert!(
            !profile
                .readable
                .iter()
                .any(|path| path.starts_with(integration.path())),
            "nor readable through the profile: {:?}",
            profile.readable
        );

        // It still runs there: the queue gate is not contained, only profiled.
        let outcome = run_argv(
            RunMode::QueueGate,
            &argv(&["git", "--version"]),
            integration.path(),
            &[],
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(outcome.exit_code, Some(0), "tail was: {}", outcome.tail);
    }
}
