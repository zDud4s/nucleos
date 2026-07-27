use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initializes a `tracing` subscriber that writes to both stdout and a daily-rotating log file inside
/// `log_dir`, filtered by `RUST_LOG` or `info,sqlx=warn` by default. Returns a `WorkerGuard` that MUST
/// be kept alive for the lifetime of `main()` — the non-blocking file writer flushes on drop.
pub fn init(log_dir: &Path) -> WorkerGuard {
    std::fs::create_dir_all(log_dir).expect("failed to create log directory");
    let file_appender = tracing_appender::rolling::daily(log_dir, "nucleos-core.log");
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

#[cfg(test)]
mod tests {
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
