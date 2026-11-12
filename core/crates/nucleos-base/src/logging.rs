use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// How many daily log files survive. See `init` for why a ceiling exists at all.
const LOG_RETENTION_DAYS: usize = 14;

/// Initializes a `tracing` subscriber that writes to both stdout and a daily-rotating log file inside
/// `log_dir`, filtered by `RUST_LOG` or `info,sqlx=warn` by default. Returns a `WorkerGuard` that MUST
/// be kept alive for the lifetime of `main()` — the non-blocking file writer flushes on drop.
pub fn init(log_dir: &Path) -> WorkerGuard {
    std::fs::create_dir_all(log_dir).expect("failed to create log directory");
    let stranded = remove_pre_rotation_logs(log_dir);
    if stranded > 0 {
        // Before the subscriber exists, so this cannot be logged. Printed instead of dropped
        // silently: deleting a person's files is not something to do without saying so.
        println!("nucleos-core: removed {stranded} log file(s) from before daily rotation");
    }
    // Rotation is not retention. A daily-rolling appender keeps every day it has ever written, so
    // anything that logs steadily — a sidecar that cannot spawn, a poller warning on every tick —
    // grows this directory until the volume fills. Two weeks is long enough to investigate
    // something that happened over a weekend and short enough to bound.
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("nucleos-core")
        .filename_suffix("log")
        .max_log_files(LOG_RETENTION_DAYS)
        .build(log_dir)
        .expect("failed to build the rolling log appender");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);
    let stdout_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stdout_layer)
        .init();

    guard
}

/// PURE: whether a file name is one this appender wrote before it took its current shape.
///
/// The old name put the date last (`nucleos-core.log.2026-07-22`); the current one puts it in the
/// middle (`nucleos-core.2026-07-22.log`). That matters because `max_log_files` only ever deletes
/// files it RECOGNISES, and it recognises the current pattern — so everything written under the old
/// one sits outside retention permanently. 52 MB of it on the machine where this was found, the
/// largest single file 35 MB.
///
/// Deliberately strict: the prefix, then a date and nothing else. A loose match here deletes
/// somebody's notes out of a directory they did not expect to be swept.
fn is_pre_rotation_log(name: &str) -> bool {
    let Some(date) = name.strip_prefix("nucleos-core.log.") else {
        return false;
    };
    date.len() == 10
        && date.split('-').count() == 3
        && date.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// Removes them, once, at startup. Returns how many went.
///
/// Renaming them into the current pattern would be worse than deleting: it would put pre-rotation
/// days back inside the retention window, and `max_log_files` would then delete RECENT logs to keep
/// the count — losing the days somebody is actually likely to want.
fn remove_pre_rotation_logs(log_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(is_pre_rotation_log)
                && std::fs::remove_file(entry.path()).is_ok()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::{is_pre_rotation_log, remove_pre_rotation_logs};

    /// The sweep has to recognise exactly the shape it was written to recognise, and nothing near
    /// it. A loose match deletes files out of a directory somebody did not expect to be swept.
    #[test]
    fn only_the_pre_rotation_name_is_recognised() {
        assert!(is_pre_rotation_log("nucleos-core.log.2026-07-22"));
        for kept in [
            // The current pattern — retention already owns these.
            "nucleos-core.2026-07-22.log",
            // The live file, and the daemon's other log.
            "nucleos-core.log",
            "nucleos-daemon.log",
            // Right prefix, and then anything at all that is not a date.
            "nucleos-core.log.backup",
            "nucleos-core.log.2026-07-22.zip",
            "nucleos-core.log.",
            // Somebody else's file that merely starts alike.
            "nucleos-core-notes.log.2026-07-22",
            "notes.txt",
        ] {
            assert!(!is_pre_rotation_log(kept), "must not be swept: {kept}");
        }
    }

    #[test]
    fn the_sweep_takes_the_stranded_files_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "nucleos-core.log.2026-07-22",
            "nucleos-core.log.2026-07-26",
            "nucleos-core.2026-08-08.log",
            "nucleos-daemon.log",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }

        assert_eq!(remove_pre_rotation_logs(dir.path()), 2);

        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "what is left: {left:?}");
        assert!(left.contains(&"nucleos-core.2026-08-08.log".to_string()));
        assert!(left.contains(&"nucleos-daemon.log".to_string()));
    }

    /// A directory that is not there is not an error worth taking startup down for.
    #[test]
    fn a_missing_log_directory_sweeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(remove_pre_rotation_logs(&dir.path().join("nowhere")), 0);
    }

    // Deliberately does NOT call `init()` (which installs a *global* subscriber and would panic if a
    // second test set one too). `with_default` scopes a subscriber to this closure instead.
    #[test]
    fn writing_a_log_line_creates_the_daily_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_appender = tracing_appender::rolling::daily(dir.path(), "nucleos-core.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let subscriber = tracing_subscriber::fmt().with_writer(non_blocking).finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("test log line");
        });
        drop(guard); // flush the non-blocking writer before reading the directory back

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(!entries.is_empty(), "expected a rotated log file to exist");
    }
}
