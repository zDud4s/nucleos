//! What tells the hub that a file changed. The hub only knows `WatchFactory`; production uses the
//! `notify` crate, the tests use a fake they can poke.

use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

/// Dropping it releases the watcher.
pub type WatchGuard = Box<dyn Send>;

pub trait WatchFactory: Send + Sync {
    /// Starts watching `paths` (recursively) and reports each changed path on `tx`. An EMPTY path
    /// means "events were lost, rescan everything" (the OS buffer overflowed or the watch broke).
    fn start(&self, paths: &[PathBuf], tx: UnboundedSender<PathBuf>) -> WatchGuard;
}

pub struct NotifyWatch;

/// Turns one `notify` result into reported paths. An EMPTY path means "events were lost": the OS
/// buffer overflowed (`Flag::Rescan`, what Windows `ReadDirectoryChangesW` raises) or the watch
/// itself failed. Reading a file (`Access`) is not a change.
fn forward(result: notify::Result<notify::Event>, tx: &UnboundedSender<PathBuf>) {
    match result {
        Ok(event) => {
            if event.need_rescan() {
                let _ = tx.send(PathBuf::new());
            }
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                let _ = tx.send(path);
            }
        }
        Err(_) => {
            let _ = tx.send(PathBuf::new());
        }
    }
}

impl WatchFactory for NotifyWatch {
    fn start(&self, paths: &[PathBuf], tx: UnboundedSender<PathBuf>) -> WatchGuard {
        use notify::{RecursiveMode, Watcher};
        if paths.is_empty() {
            return Box::new(());
        }
        let handler = move |result: notify::Result<notify::Event>| forward(result, &tx);
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::warn!(%error, "fleet: could not start the file watcher; the 5 s safety net still runs");
                return Box::new(());
            }
        };
        for path in paths {
            if let Err(error) = watcher.watch(path, RecursiveMode::Recursive) {
                tracing::warn!(path = %path.display(), %error, "fleet: could not watch a directory");
            }
        }
        Box::new(watcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_file_written_under_a_watched_directory_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _guard = NotifyWatch.start(&[dir.path().to_path_buf()], tx);

        let file = dir.path().join("session.jsonl");
        std::fs::write(&file, b"{}\n").unwrap();

        let reported = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(path) = rx.recv().await {
                if path.file_name() == file.file_name() {
                    return Some(path);
                }
            }
            None
        })
        .await
        .expect("the watcher reported nothing in 5 s");
        assert!(reported.is_some());
    }

    #[test]
    fn nothing_to_watch_starts_nothing() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let _guard = NotifyWatch.start(&[], tx);
    }

    fn reported(result: notify::Result<notify::Event>) -> Vec<PathBuf> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        forward(result, &tx);
        let mut out = Vec::new();
        while let Ok(path) = rx.try_recv() {
            out.push(path);
        }
        out
    }

    #[test]
    fn a_write_is_reported_by_path_and_a_read_is_not() {
        use notify::event::{AccessKind, DataChange, ModifyKind};
        use notify::{Event, EventKind};
        let write = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
            .add_path(PathBuf::from("/s/a.jsonl"));
        assert_eq!(reported(Ok(write)), vec![PathBuf::from("/s/a.jsonl")]);
        let read =
            Event::new(EventKind::Access(AccessKind::Any)).add_path(PathBuf::from("/s/a.jsonl"));
        assert!(reported(Ok(read)).is_empty());
    }

    #[test]
    fn an_overflow_flag_or_an_error_reports_an_empty_path() {
        use notify::event::Flag;
        use notify::{Event, EventKind};
        let overflow = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(reported(Ok(overflow)), vec![PathBuf::new()]);
        let broken = Err(notify::Error::generic("watch lost"));
        assert_eq!(reported(broken), vec![PathBuf::new()]);
    }
}
