//! Transient SQLite faults in the writers of the worktree feed lines.
//!
//! The pool (`nucleos_base::storage`: 12 connections, 3 s acquire timeout, 3 s `busy_timeout`) is
//! sized on purpose and kept inside the hook budget, so a burst (several trees starting in the same
//! second) is ridden out here instead: a short bounded retry, and when it still fails a `_held` feed
//! kind rather than a danger one, because "database is locked" says nothing about the worktree, the
//! gate or the root run.

use std::time::Duration;

/// Pauses between the attempts of [`retry_transient`]: three retries, 1.1 s in total at most.
const BACKOFF_MS: [u64; 3] = [100, 300, 700];

/// Whether an error text says the database was momentarily busy: SQLite's `database is locked`,
/// `database table is locked`, `SQLITE_BUSY`, or the pool out of connections (`pool timed out`).
pub fn is_transient_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "database is locked",
        "database table is locked",
        "sqlite_busy",
        "pool timed out",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

/// [`is_transient_message`] for a typed error: `PoolTimedOut` always, anything else by its text.
pub fn is_transient_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::PoolTimedOut) || is_transient_message(&error.to_string())
}

/// Runs `op`, retrying with a short bounded backoff while it fails with a transient error. Any
/// other error, or the last attempt's, is returned as it is. `op` must be safe to repeat.
pub async fn retry_transient<T, F, Fut>(mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut pauses = BACKOFF_MS.iter();
    loop {
        match op().await {
            Err(error) if is_transient_error(&error) => match pauses.next() {
                Some(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                None => return Err(error),
            },
            other => return other,
        }
    }
}

/// The feed kind to write for a failure whose reason is `reason`: a transient database fault turns
/// the three danger kinds of these writers into their `_held` twin; anything else keeps `kind`.
pub fn feed_kind_for(kind: &'static str, reason: &str) -> &'static str {
    if !is_transient_message(reason) {
        return kind;
    }
    match kind {
        "worktree_provision_failed" => "worktree_provision_held",
        "worktree_gate_failed" => "worktree_gate_held",
        "work_tree_root_start_failed" => "work_tree_root_start_held",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn transient_messages_are_recognised() {
        for text in [
            "error returned from database: (code: 5) database is locked",
            "pool timed out while waiting for an open connection",
            "could not read scope verification ticket 7: pool timed out while waiting",
            "SQLITE_BUSY",
            "database table is locked",
        ] {
            assert!(is_transient_message(text), "{text}");
        }
        for text in ["no space left on device", "branch main not found", ""] {
            assert!(!is_transient_message(text), "{text}");
        }
        assert!(is_transient_error(&sqlx::Error::PoolTimedOut));
        assert!(!is_transient_error(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn a_transient_reason_picks_the_held_kind() {
        let locked = "database is locked";
        assert_eq!(
            feed_kind_for("worktree_provision_failed", locked),
            "worktree_provision_held"
        );
        assert_eq!(
            feed_kind_for(
                "worktree_gate_failed",
                "worktree gate errored: could not read scope verification ticket 3: pool timed out"
            ),
            "worktree_gate_held"
        );
        assert_eq!(
            feed_kind_for("work_tree_root_start_failed", locked),
            "work_tree_root_start_held"
        );
        assert_eq!(
            feed_kind_for("worktree_provision_failed", "disk full"),
            "worktree_provision_failed"
        );
        assert_eq!(feed_kind_for("other_kind", locked), "other_kind");
    }

    #[tokio::test]
    async fn retry_recovers_from_a_transient_error() {
        let calls = AtomicUsize::new(0);
        let result = retry_transient(|| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(sqlx::Error::PoolTimedOut)
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_does_not_repeat_a_permanent_error() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), _> = retry_transient(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::RowNotFound)
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_gives_up_after_the_backoff_is_spent() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), _> = retry_transient(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::PoolTimedOut)
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1 + BACKOFF_MS.len());
    }
}
