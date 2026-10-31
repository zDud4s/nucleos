//! `nucleos-core --settings-reset <scope>/<name>` and `nucleos-core --settings-rollback <scope>/<name> <version>`:
//! the recovery door for a settings document that passes validation and still leaves the system unusable
//! (a `gate_command` that breaks every run, say) - spec 2026-10-11 section 6.
//!
//! Both open the database DIRECTLY, with the data directory the daemon would use (`data_dir::data_dir`, the
//! resolution startup shares), and write through `settings::Settings` with `written_by = "cli"`, so the change
//! lands in `settings_history` like any other. That is only safe with no daemon on the database: SQLite's WAL
//! would let both open it, but the live daemon holds a snapshot in memory that would never see the write. So
//! both REFUSE (exit 3) when something answers on the port a daemon of this environment binds - the probe
//! `scripts/run-daemon.ps1` uses (step 2e). `daemon_client::is_primary` is NOT that test: it only looks at the
//! environment.
//!
//! A missing database is an error, never created: `storage::open` would build an empty one at a mistyped path.
//! The logic lives here, testable without a process; [`dispatch`] maps an [`Outcome`] to print and exit, so that
//! `main.rs` (at its file-size ceiling) only gains a three-line call.

use crate::daemon_client;
use crate::data_dir;
use crate::settings::{Scope, Settings, SettingsError};
use crate::storage;
use std::path::Path;
use std::time::Duration;

pub const RESET_FLAG: &str = "--settings-reset";
pub const ROLLBACK_FLAG: &str = "--settings-rollback";

/// `settings_history.written_by` of every write this module makes.
pub const WRITTEN_BY: &str = "cli";

pub const EXIT_OK: i32 = 0;
/// The settings store refused: unknown document, wrong scope, absent document, missing version, database error.
pub const EXIT_FAILED: i32 = 1;
/// Bad arguments. The conventional usage-error code (and `run-daemon.ps1`'s).
pub const EXIT_USAGE: i32 = 2;
/// A daemon answers on this database's port. `run-daemon.ps1` exits 3 for the same reason.
pub const EXIT_DAEMON_LIVE: i32 = 3;
/// No database file at the resolved data directory.
pub const EXIT_NO_DATABASE: i32 = 4;

/// How long the liveness probe waits for the connection (`run-daemon.ps1` waits 500 ms too).
pub const PROBE_WAIT: Duration = Duration::from_millis(500);

pub const USAGE: &str = "\
usage: nucleos-core --settings-reset <scope>/<name>
       nucleos-core --settings-rollback <scope>/<name> <version>

  <scope>/<name>  machine/<name> or project:<id>/<name>, e.g. machine/verify
  --settings-reset     delete the document: the defaults apply again (recorded in the history)
  --settings-rollback  write version <version> back as a NEW version (history is never rewound)

The database is the one the daemon of this environment opens (NUCLEOS_DATA_DIR, else the platform default).
Both refuse while a daemon answers on that environment's port (NUCLEOS_PORT, else 8791): stop it first. With
NUCLEOS_DATA_DIR set and NUCLEOS_PORT unset the port checked is 8791, which may be another daemon's.

exit codes: 0 done | 1 refused by the settings store | 2 bad arguments | 3 a daemon is running | 4 no database";

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Neither flag is present: `main` goes on to the rest of its flags and then to the daemon.
    NotSettings,
    /// A flag is present and the rest does not parse. `run` turns it into exit 2.
    Usage(String),
    Reset {
        scope: Scope,
        name: String,
    },
    Rollback {
        scope: Scope,
        name: String,
        version: i64,
    },
}

/// What a call produced, for `main` to print and exit with.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    fn done(stdout: String) -> Self {
        Self {
            code: EXIT_OK,
            stdout,
            stderr: String::new(),
        }
    }

    fn refused(code: i32, stderr: String) -> Self {
        Self {
            code,
            stdout: String::new(),
            stderr,
        }
    }
}

/// The one predicate `main.rs` and [`invocation`] both rest on: a flag that falls through the `main()` blocks
/// starts a daemon, so the guard and the parser cannot be two lists.
pub fn is_settings_flag(arg: &str) -> bool {
    arg == RESET_FLAG || arg == ROLLBACK_FLAG
}

/// PURE: parse `std::env::args().collect()` (program name first). Strict on purpose - an argument before the
/// flag, a second flag or a stray operand is a usage error, never ignored.
pub fn invocation(args: &[String]) -> Invocation {
    let operands = args.get(1..).unwrap_or(&[]);
    let flags: Vec<usize> = operands
        .iter()
        .enumerate()
        .filter(|(_, arg)| is_settings_flag(arg))
        .map(|(at, _)| at)
        .collect();
    let Some(&at) = flags.first() else {
        return Invocation::NotSettings;
    };
    if flags.len() > 1 {
        return Invocation::Usage(
            "give one of --settings-reset and --settings-rollback, not both".into(),
        );
    }
    if at > 0 {
        return Invocation::Usage(format!(
            "`{}` may not come before {}",
            operands[0], operands[at]
        ));
    }
    let rest = &operands[1..];
    match (operands[0].as_str(), rest) {
        (RESET_FLAG, [target]) => match parse_target(target) {
            Ok((scope, name)) => Invocation::Reset { scope, name },
            Err(reason) => Invocation::Usage(reason),
        },
        (ROLLBACK_FLAG, [target, version]) => {
            match (parse_target(target), parse_version(version)) {
                (Ok((scope, name)), Ok(version)) => Invocation::Rollback {
                    scope,
                    name,
                    version,
                },
                (Err(reason), _) | (_, Err(reason)) => Invocation::Usage(reason),
            }
        }
        (RESET_FLAG, _) => {
            Invocation::Usage("--settings-reset takes exactly one <scope>/<name>".into())
        }
        _ => Invocation::Usage(
            "--settings-rollback takes exactly <scope>/<name> and a version".into(),
        ),
    }
}

/// `machine/verify` -> (`Machine`, `verify`); `project:ID/autopilot` -> (`Project("ID")`, `autopilot`). Splits at the
/// LAST `/`: document names never contain one, a project id might.
fn parse_target(text: &str) -> Result<(Scope, String), String> {
    let (scope_text, name) = text.rsplit_once('/').ok_or_else(|| {
        format!(
            "`{text}` is not <scope>/<name>; write e.g. machine/verify or project:<id>/autopilot"
        )
    })?;
    if name.is_empty() {
        return Err(format!("`{text}` names no document after the last `/`"));
    }
    let scope = scope_text
        .parse::<Scope>()
        .map_err(|error| error.to_string())?;
    Ok((scope, name.to_owned()))
}

fn parse_version(text: &str) -> Result<i64, String> {
    match text.parse::<i64>() {
        Ok(version) if version >= 1 => Ok(version),
        _ => Err(format!(
            "`{text}` is not a version number; versions start at 1"
        )),
    }
}

/// Whether something accepts a TCP connection on `127.0.0.1:<port>` within `wait`. `run-daemon.ps1` step 2e, in Rust.
pub async fn daemon_answers_on(port: u16, wait: Duration) -> bool {
    matches!(
        tokio::time::timeout(wait, tokio::net::TcpStream::connect(("127.0.0.1", port))).await,
        Ok(Ok(_))
    )
}

/// Run a parsed call against the database in `data_dir`, refusing when a daemon answers on `port`.
pub async fn run(data_dir: &Path, port: u16, call: Invocation) -> Outcome {
    let (scope, name, to_version) = match call {
        Invocation::Reset { scope, name } => (scope, name, None),
        Invocation::Rollback {
            scope,
            name,
            version,
        } => (scope, name, Some(version)),
        Invocation::Usage(reason) => {
            return Outcome::refused(EXIT_USAGE, format!("settings: {reason}\n\n{USAGE}\n"));
        }
        Invocation::NotSettings => {
            return Outcome::refused(EXIT_USAGE, format!("settings: nothing to do\n\n{USAGE}\n"));
        }
    };

    if daemon_answers_on(port, PROBE_WAIT).await {
        return Outcome::refused(
            EXIT_DAEMON_LIVE,
            format!(
                "settings: a daemon answers on 127.0.0.1:{port}; it would not see this write. \
                 Stop it first (Ctrl+C its terminal, or `Stop-Process -Id <pid>`) and run this again.\n"
            ),
        );
    }

    let db_path = data_dir::db_path(data_dir);
    if !db_path.is_file() {
        return Outcome::refused(
            EXIT_NO_DATABASE,
            format!(
                "settings: no database at {} (NUCLEOS_DATA_DIR points elsewhere than the daemon you mean?)\n",
                db_path.display()
            ),
        );
    }

    let pool = match storage::open(&db_path).await {
        Ok(pool) => pool,
        Err(error) => {
            return Outcome::refused(
                EXIT_FAILED,
                format!("settings: cannot open {}: {error}\n", db_path.display()),
            );
        }
    };
    let result = apply(&pool, &scope, &name, to_version).await;
    pool.close().await;

    match (result, to_version) {
        (Ok(version), None) => Outcome::done(format!(
            "settings: {scope}/{name} deleted (history version {version}); the defaults apply from the next start\n"
        )),
        (Ok(version), Some(to)) => Outcome::done(format!(
            "settings: {scope}/{name} restored to version {to}, written as version {version}; it applies from the next start\n"
        )),
        (Err(SettingsError::NotFound), None) => Outcome::refused(
            EXIT_FAILED,
            format!("settings: {scope}/{name} is not stored; the defaults already apply\n"),
        ),
        (Err(SettingsError::NotFound), Some(to)) => Outcome::refused(
            EXIT_FAILED,
            format!("settings: {scope}/{name} has no version {to}\n"),
        ),
        (Err(error), _) => Outcome::refused(EXIT_FAILED, format!("settings: {error}\n")),
    }
}

async fn apply(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    name: &str,
    to_version: Option<i64>,
) -> Result<i64, SettingsError> {
    let handle = Settings::new();
    handle.load(pool).await?;
    match to_version {
        None => handle.delete(pool, scope, name, WRITTEN_BY, None).await,
        Some(version) => {
            handle
                .rollback(pool, scope, name, version, WRITTEN_BY, None)
                .await
        }
    }
}

/// The whole `main.rs` side of the recovery commands. `None`: neither flag is on the command line, `main` goes on
/// to its other flags and then to the daemon. `Some(code)`: the command ran (or was refused), its output is
/// printed and flushed, and `main` must exit with `code` - it never starts a daemon.
///
/// `std::process::exit` does not flush a buffered stdout and a pipe is block-buffered, hence the explicit flush.
pub async fn dispatch(args: &[String]) -> Option<i32> {
    use std::io::Write;
    let call = match invocation(args) {
        Invocation::NotSettings => return None,
        call => call,
    };
    let outcome = match data_dir::data_dir() {
        Ok(dir) => run(&dir, daemon_client::port(), call).await,
        Err(reason) => Outcome::refused(EXIT_NO_DATABASE, format!("settings: {reason}\n")),
    };
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    Some(outcome.code)
}

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
        let pool = crate::storage::open(&data_dir::db_path(dir.path()))
            .await
            .expect("open");
        let handle = Settings::new();
        for capacity in [2, 3] {
            handle
                .put(
                    &pool,
                    &Scope::Machine,
                    "verify",
                    &json!({ "capacity": capacity }),
                    "owner",
                    None,
                )
                .await
                .expect("seed");
        }
        pool.close().await;
        dir
    }

    /// What the database holds for `machine/verify`, read after the CLI has closed its own pool:
    /// the current row (version, capacity) if any, and the history, newest first.
    async fn stored(dir: &Path) -> (Option<(i64, i64)>, Vec<HistoryRow>) {
        let pool = crate::storage::open(&data_dir::db_path(dir))
            .await
            .expect("open");
        let snapshot = settings::load_all(&pool).await.expect("load");
        let current = snapshot.entry(&Scope::Machine, "verify").map(|entry| {
            (
                entry.version,
                entry.body()["capacity"].as_i64().expect("capacity"),
            )
        });
        let history = settings::history(&pool, &Scope::Machine, "verify")
            .await
            .expect("history");
        pool.close().await;
        (current, history)
    }

    /// A port nothing listens on: bound to get a free number, then released.
    async fn free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        listener.local_addr().expect("addr").port()
    }

    fn reset_machine_verify() -> Invocation {
        Invocation::Reset {
            scope: Scope::Machine,
            name: "verify".into(),
        }
    }

    #[test]
    fn a_reset_and_a_rollback_are_parsed_from_the_command_line() {
        assert_eq!(
            invocation(&argv(&["--settings-reset", "machine/verify"])),
            reset_machine_verify()
        );
        assert_eq!(
            invocation(&argv(&[
                "--settings-rollback",
                "project:BACMAT/autopilot",
                "4"
            ])),
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
        assert_eq!(
            invocation(&argv(&["--print-token"])),
            Invocation::NotSettings
        );
    }

    #[test]
    fn a_bad_scope_name_version_or_arity_is_a_usage_error_never_a_fall_through() {
        let usage = |parts: &[&str]| matches!(invocation(&argv(parts)), Invocation::Usage(_));
        assert!(usage(&["--settings-reset"]), "no target");
        assert!(usage(&["--settings-reset", "verify"]), "no scope");
        assert!(usage(&["--settings-reset", "machine/"]), "no name");
        assert!(
            usage(&["--settings-reset", "nowhere/verify"]),
            "not a scope"
        );
        assert!(
            usage(&["--settings-reset", "project:/verify"]),
            "an empty project id"
        );
        assert!(
            usage(&["--settings-reset", "machine/verify", "extra"]),
            "a reset takes no version"
        );
        assert!(
            usage(&["--settings-rollback", "machine/verify"]),
            "a rollback needs a version"
        );
        assert!(
            usage(&["--settings-rollback", "machine/verify", "0"]),
            "versions start at 1"
        );
        assert!(
            usage(&["--settings-rollback", "machine/verify", "-3"]),
            "versions are positive"
        );
        assert!(
            usage(&["--settings-rollback", "machine/verify", "two"]),
            "versions are numbers"
        );
        assert!(
            usage(&["--settings-rollback", "machine/verify", "1", "2"]),
            "too many operands"
        );
        assert!(
            usage(&["--land", "--settings-reset", "machine/verify"]),
            "nothing may precede the flag"
        );
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
            assert_ne!(
                invocation(&argv(&[flag])),
                Invocation::NotSettings,
                "{flag} must be caught"
            );
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
        assert!(
            outcome.stdout.contains("machine/verify"),
            "stdout: {}",
            outcome.stdout
        );

        let (current, history) = stored(dir.path()).await;
        assert_eq!(
            current, None,
            "the document is absent, so the defaults apply"
        );
        assert_eq!(history.len(), 3, "the two writes plus the deletion");
        assert_eq!(history[0].version, 3);
        assert!(history[0].deleted());
        assert_eq!(history[0].written_by, WRITTEN_BY);
    }

    #[tokio::test]
    async fn a_rollback_writes_the_old_body_as_a_new_version() {
        let dir = seeded_dir().await;
        let call = Invocation::Rollback {
            scope: Scope::Machine,
            name: "verify".into(),
            version: 1,
        };
        let outcome = run(dir.path(), free_port().await, call).await;
        assert_eq!(outcome.code, EXIT_OK, "stderr: {}", outcome.stderr);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(
            current,
            Some((3, 2)),
            "version 3 now holds version 1's capacity"
        );
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].body_json, history[2].body_json);
        assert_eq!(history[0].written_by, WRITTEN_BY);
    }

    #[tokio::test]
    async fn a_live_daemon_makes_both_commands_refuse_and_touch_nothing() {
        let dir = seeded_dir().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(
            daemon_answers_on(port, PROBE_WAIT).await,
            "the probe sees a listener"
        );

        let reset = run(dir.path(), port, reset_machine_verify()).await;
        assert_eq!(reset.code, EXIT_DAEMON_LIVE);
        assert!(
            reset.stderr.contains(&port.to_string()),
            "it names the port: {}",
            reset.stderr
        );
        let call = Invocation::Rollback {
            scope: Scope::Machine,
            name: "verify".into(),
            version: 1,
        };
        assert_eq!(run(dir.path(), port, call).await.code, EXIT_DAEMON_LIVE);

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, Some((2, 3)), "unchanged");
        assert_eq!(history.len(), 2, "no row was written");
        drop(listener);
        assert!(
            !daemon_answers_on(port, PROBE_WAIT).await,
            "a released port is not a daemon"
        );
    }

    #[tokio::test]
    async fn the_live_daemon_refusal_says_how_to_stop_it_without_schtasks_end() {
        // The task runs under `conhost --headless`, so `schtasks /End` only kills the conhost and the daemon
        // stays alive; `scripts/run-daemon.ps1` already tells the user to `Stop-Process -Id <pid>`.
        let dir = seeded_dir().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let refused = run(dir.path(), port, reset_machine_verify()).await;
        assert_eq!(refused.code, EXIT_DAEMON_LIVE);
        assert!(
            !refused.stderr.contains("schtasks /End"),
            "schtasks /End leaves the daemon alive: {}",
            refused.stderr
        );
        assert!(
            refused.stderr.contains("Stop-Process"),
            "it names Stop-Process: {}",
            refused.stderr
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_data_dir_without_a_database_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = run(dir.path(), free_port().await, reset_machine_verify()).await;
        assert_eq!(outcome.code, EXIT_NO_DATABASE);
        assert!(
            outcome.stderr.contains(&dir.path().display().to_string()),
            "{}",
            outcome.stderr
        );
        assert!(
            !data_dir::db_path(dir.path()).exists(),
            "storage::open would have created it"
        );
    }

    #[tokio::test]
    async fn the_store_refusing_a_target_is_an_error_exit() {
        let dir = seeded_dir().await;
        let port = free_port().await;
        let unknown = Invocation::Reset {
            scope: Scope::Machine,
            name: "nope".into(),
        };
        let outcome = run(dir.path(), port, unknown).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(
            outcome.stderr.contains("`nope` is not a settings document"),
            "{}",
            outcome.stderr
        );

        let wrong_scope = Invocation::Reset {
            scope: Scope::project("p1"),
            name: "verify".into(),
        };
        let outcome = run(dir.path(), port, wrong_scope).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(
            outcome
                .stderr
                .contains("cannot be written at scope `project:p1`"),
            "{}",
            outcome.stderr
        );

        let missing = Invocation::Rollback {
            scope: Scope::Machine,
            name: "verify".into(),
            version: 9,
        };
        let outcome = run(dir.path(), port, missing).await;
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(
            outcome.stderr.contains("no version 9"),
            "{}",
            outcome.stderr
        );

        let (current, history) = stored(dir.path()).await;
        assert_eq!(current, Some((2, 3)));
        assert_eq!(history.len(), 2, "a refused command writes nothing");
    }

    #[tokio::test]
    async fn resetting_a_document_that_is_not_stored_says_the_defaults_already_apply() {
        let dir = seeded_dir().await;
        let port = free_port().await;
        assert_eq!(
            run(dir.path(), port, reset_machine_verify()).await.code,
            EXIT_OK
        );
        let again = run(dir.path(), port, reset_machine_verify()).await;
        assert_eq!(again.code, EXIT_FAILED);
        assert!(
            again.stderr.contains("defaults already apply"),
            "{}",
            again.stderr
        );
        assert_eq!(
            stored(dir.path()).await.1.len(),
            3,
            "the second reset wrote nothing"
        );
    }
}
