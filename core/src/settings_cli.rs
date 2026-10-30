//! placeholder, replaced in Step 3

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{self, HistoryRow, Settings};
    use serde_json::json;
    use std::path::Path;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("nucleos-core")
            .chain(parts.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    /// A data directory whose database holds `verify` at version 2 (capacity 2, then capacity 3).
    async fn seeded_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::storage::open(&data_dir::db_path(dir.path())).await.expect("open");
        let handle = Settings::new();
        for capacity in [2, 3] {
            handle
                .put(&pool, &Scope::Machine, "verify", &json!({ "capacity": capacity }), "owner", None)
                .await
                .expect("seed");
        }
        pool.close().await;
        dir
    }

    /// What the database holds for `machine/verify`, read after the CLI has closed its own pool:
    /// the current row (version, capacity) if any, and the history, newest first.
    async fn stored(dir: &Path) -> (Option<(i64, i64)>, Vec<HistoryRow>) {
        let pool = crate::storage::open(&data_dir::db_path(dir)).await.expect("open");
        let snapshot = settings::load_all(&pool).await.expect("load");
        let current = snapshot
            .entry(&Scope::Machine, "verify")
            .map(|entry| (entry.version, entry.body()["capacity"].as_i64().expect("capacity")));
        let history = settings::history(&pool, &Scope::Machine, "verify").await.expect("history");
        pool.close().await;
        (current, history)
    }

    /// A port nothing listens on: bound to get a free number, then released.
    async fn free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        listener.local_addr().expect("addr").port()
    }

    fn reset_machine_verify() -> Invocation {
        Invocation::Reset { scope: Scope::Machine, name: "verify".into() }
    }

    #[test]
    fn a_reset_and_a_rollback_are_parsed_from_the_command_line() {
        assert_eq!(
            invocation(&argv(&["--settings-reset", "machine/verify"])),
            reset_machine_verify()
        );
        assert_eq!(
            invocation(&argv(&["--settings-rollback", "project:BACMAT/autopilot", "4"])),
            Invocation::Rollback {
                scope: Scope::project("BACMAT"),
                name: "autopilot".into(),
                version: 4,
            }
        );
    }

    #[test]
    fn a_command_line_without_the_flags_is_not_a_settings_call() {
        assert_eq!(invocation(&argv(&[])), Invocation::NotSettings);
        assert_eq!(invocation(&argv(&["--land", "x"])), Invocation::NotSettings);
        assert_eq!(invocation(&argv(&["--print-token"])), Invocation::NotSettings);
    }

    #[test]
    fn a_bad_scope_name_version_or_arity_is_a_usage_error_never_a_fall_through() {
        let usage = |parts: &[&str]| matches!(invocation(&argv(parts)), Invocation::Usage(_));
        assert!(usage(&["--settings-reset"]), "no target");
        assert!(usage(&["--settings-reset", "verify"]), "no scope");
        assert!(usage(&["--settings-reset", "machine/"]), "no name");
        assert!(usage(&["--settings-reset", "nowhere/verify"]), "not a scope");
        assert!(usage(&["--settings-reset", "project:/verify"]), "an empty project id");
        assert!(usage(&["--settings-reset", "machine/verify", "extra"]), "a reset takes no version");
        assert!(usage(&["--settings-rollback", "machine/verify"]), "a rollback needs a version");
        assert!(usage(&["--settings-rollback", "machine/verify", "0"]), "versions start at 1");
        assert!(usage(&["--settings-rollback", "machine/verify", "-3"]), "versions are positive");
        assert!(usage(&["--settings-rollback", "machine/verify", "two"]), "versions are numbers");
        assert!(usage(&["--settings-rollback", "machine/verify", "1", "2"]), "too many operands");
        assert!(usage(&["--land", "--settings-reset", "machine/verify"]), "nothing may precede the flag");
        assert!(
            usage(&["--settings-reset", "machine/verify", "--settings-rollback"]),
            "one flag at a time"
        );
    }

    /// The `main.rs` block runs when `invocation` answers anything but `NotSettings`; a flag the
    /// predicate misses would fall through and START A DAEMON, so the two are checked against each other.
    #[test]
    fn the_guard_predicate_and_the_parser_agree() {
        for flag in [RESET_FLAG, ROLLBACK_FLAG] {
            assert!(is_settings_flag(flag));
            assert_ne!(invocation(&argv(&[flag])), Invocation::NotSettings, "{flag} must be caught");
        }
        assert!(!is_settings_flag("--land"));
        assert!(!is_settings_flag("--settings"));
    }

    /// `main.rs` goes on to its other flags and to the daemon exactly when `dispatch` answers `None`.
    #[tokio::test]
    async fn a_command_line_without_the_flags_is_left_to_main() {
        assert_eq!(dispatch(&argv(&[])).await, None);
        assert_eq!(dispatch(&argv(&["--land", "x"])).await, None);
        assert_eq!(dispatch(&argv(&["--print-token"])).await, None);
    }

    #[tokio::test]
    async fn a_reset_deletes_the_document_and_leaves_a_history_row() {
        let dir = seeded_dir().await;
        let outcome = run(dir.path(), free_port().await, reset_machine_verify()).await;
        assert_eq!(outcome.code, EXIT_OK, "stderr: {}", outcome.stderr);
        assert!(outcome.stdout.contains("machine/verify"), "stdout: {}", outcome.stdout);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, None, "the document is absent, so the defaults apply");
        assert_eq!(history.len(), 3, "the two writes plus the deletion");
        assert_eq!(history[0].version, 3);
        assert!(history[0].deleted());
        assert_eq!(history[0].written_by, WRITTEN_BY);
    }

    #[tokio::test]
    async fn a_rollback_writes_the_old_body_as_a_new_version() {
        let dir = seeded_dir().await;
        let call = Invocation::Rollback { scope: Scope::Machine, name: "verify".into(), version: 1 };
        let outcome = run(dir.path(), free_port().await, call).await;
        assert_eq!(outcome.code, EXIT_OK, "stderr: {}", outcome.stderr);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, Some((3, 2)), "version 3 now holds version 1's capacity");
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].body_json, history[2].body_json);
        assert_eq!(history[0].written_by, WRITTEN_BY);
    }

    #[tokio::test]
    async fn a_live_daemon_makes_both_commands_refuse_and_touch_nothing() {
        let dir = seeded_dir().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(daemon_answers_on(port, PROBE_WAIT).await, "the probe sees a listener");

        let reset = run(dir.path(), port, reset_machine_verify()).await;
        assert_eq!(reset.code, EXIT_DAEMON_LIVE);
        assert!(reset.stderr.contains(&port.to_string()), "it names the port: {}", reset.stderr);
        let call = Invocation::Rollback { scope: Scope::Machine, name: "verify".into(), version: 1 };
        assert_eq!(run(dir.path(), port, call).await.code, EXIT_DAEMON_LIVE);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, Some((2, 3)), "unchanged");
        assert_eq!(history.len(), 2, "no row was written");
        drop(listener);
        assert!(!daemon_answers_on(port, PROBE_WAIT).await, "a released port is not a daemon");
    }

    #[tokio::test]
    async fn a_data_dir_without_a_database_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = run(dir.path(), free_port().await, reset_machine_verify()).await;
        assert_eq!(outcome.code, EXIT_NO_DATABASE);
        assert!(outcome.stderr.contains(&dir.path().display().to_string()), "{}", outcome.stderr);
        assert!(!data_dir::db_path(dir.path()).exists(), "storage::open would have created it");
    }

    #[tokio::test]
    async fn the_store_refusing_a_target_is_an_error_exit() {
        let dir = seeded_dir().await;
        let port = free_port().await;
        let unknown = Invocation::Reset { scope: Scope::Machine, name: "nope".into() };
        let outcome = run(dir.path(), port, unknown).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(outcome.stderr.contains("`nope` is not a settings document"), "{}", outcome.stderr);

        let wrong_scope = Invocation::Reset { scope: Scope::project("p1"), name: "verify".into() };
        let outcome = run(dir.path(), port, wrong_scope).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(outcome.stderr.contains("cannot be written at scope `project:p1`"), "{}", outcome.stderr);

        let missing = Invocation::Rollback { scope: Scope::Machine, name: "verify".into(), version: 9 };
        let outcome = run(dir.path(), port, missing).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(outcome.stderr.contains("no version 9"), "{}", outcome.stderr);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, Some((2, 3)));
        assert_eq!(history.len(), 2, "a refused command writes nothing");
    }

    #[tokio::test]
    async fn resetting_a_document_that_is_not_stored_says_the_defaults_already_apply() {
        let dir = seeded_dir().await;
        let port = free_port().await;
        assert_eq!(run(dir.path(), port, reset_machine_verify()).await.code, EXIT_OK);
        let again = run(dir.path(), port, reset_machine_verify()).await;
        assert_eq!(again.code, EXIT_FAILED);
        assert!(again.stderr.contains("defaults already apply"), "{}", again.stderr);
        assert_eq!(stored(dir.path()).await.1.len(), 3, "the second reset wrote nothing");
    }
}
