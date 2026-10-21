//! What an adapter is, and what it reports (spec §4). Adapters are the only code that knows a
//! harness's on-disk format; everything downstream sees `Sighting`s.

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;

use super::model::{Cursor, NodeKey, NodeState, TermLine, Timestamp, TranscriptPage};

/// What an adapter reports when it cannot do what it was asked (contract amendment 13). `anyhow`
/// is not a dependency of the core and does not become one, so this is the one error type of
/// both traits: a message, plus `From` for the error types adapters meet with `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

pub type SourceResult<T> = Result<T, SourceError>;

impl SourceError {
    /// `SourceError::msg("poisoned")`, `SourceError::msg(format!("no run {id}"))`.
    pub fn msg(m: impl fmt::Display) -> Self {
        Self(m.to_string())
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

impl From<std::io::Error> for SourceError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<sqlx::Error> for SourceError {
    fn from(error: sqlx::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for SourceError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<tokio::task::JoinError> for SourceError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Offset(pub u64);

impl Offset {
    /// "First read: start about 64 KB from the end and skip to the next line."
    pub const TAIL: Offset = Offset(u64::MAX);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub context_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinesRead {
    pub lines: Vec<TermLine>,
    /// The next read starts here (always at a line boundary).
    pub offset: Offset,
    /// The latest usage record seen in this read, if any.
    pub usage: Option<Usage>,
    pub last_activity_at: Option<Timestamp>,
}

/// What an adapter knows about one node. Several sources may sight the same key (fusion).
#[derive(Clone, Debug, PartialEq)]
pub struct Sighting {
    pub key: NodeKey,
    /// The NATIVE edge only (subagent to parent agent or root).
    pub parent: Option<NodeKey>,
    /// `FleetNode.harness`, for display.
    pub harness_label: String,
    pub model: Option<String>,
    pub role: Option<String>,
    pub label: String,
    /// Last path component of the cwd.
    pub project: Option<String>,
    /// Decided by the adapter, per spec §3.
    pub state: NodeState,
    pub started_at: Timestamp,
    /// Adapters set `Some(last_activity_at)` on every `Done`/`Lost` sighting and `None` on a live
    /// one (contract amendment 3). The hub overrides it with its own observation time when it saw
    /// the node live before.
    pub ended_at: Option<Timestamp>,
    pub last_activity_at: Timestamp,
    /// `daemon_runs`, and `claude_code` roots resolved through `runs.session_id`.
    pub run_id: Option<i64>,
    /// True only from `daemon_runs`.
    pub can_stop: bool,
    pub has_lines: bool,
    pub has_transcript: bool,
    pub usage: Option<Usage>,
}

/// The adapter's own handle for a node it sighted.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRef {
    pub key: NodeKey,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub child: NodeKey,
    pub parent: NodeKey,
}

#[async_trait]
pub trait AgentSource: Send + Sync {
    /// Adapter name, for logs ("claude_code").
    fn name(&self) -> &'static str;
    /// Directories to watch. An EMPTY list means "nothing to watch": the hub polls such a source.
    fn watch_paths(&self) -> Vec<PathBuf>;
    /// Nodes with activity at or after `since`, plus every node still alive. Must be incremental
    /// (spec §4): keep per-directory mtimes in `&self` and return the cached sightings for
    /// directories that did not change.
    async fn discover(&self, since: Timestamp) -> SourceResult<Vec<Sighting>>;
    /// Resolve a sighted key to this source's handle (`None` = not mine).
    fn node_ref(&self, key: &NodeKey) -> Option<NodeRef>;
    async fn read_lines(&self, node: &NodeRef, from: Offset) -> SourceResult<LinesRead>;
    async fn transcript_page(
        &self,
        node: &NodeRef,
        before: Option<Cursor>,
    ) -> SourceResult<TranscriptPage>;
}

#[async_trait]
pub trait EdgeSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn edges(&self, since: Timestamp) -> SourceResult<Vec<Edge>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_takes_anything_displayable() {
        assert_eq!(SourceError::msg("plain"), SourceError("plain".to_owned()));
        assert_eq!(SourceError::msg(format!("no run {}", 7)).0, "no run 7");
        assert_eq!(SourceError::msg(42).0, "42");
    }

    #[test]
    fn display_is_the_message_alone_and_it_is_a_std_error() {
        let error = SourceError::msg("boom");
        assert_eq!(error.to_string(), "boom");
        let as_dyn: &dyn std::error::Error = &error;
        assert_eq!(as_dyn.to_string(), "boom");
    }

    #[test]
    fn io_errors_convert_and_question_mark_works_in_a_source_fn() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let expected = io.to_string();
        assert_eq!(SourceError::from(io).0, expected);

        fn open(path: &std::path::Path) -> SourceResult<std::fs::File> {
            Ok(std::fs::File::open(path)?)
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(open(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn json_errors_convert() {
        let parse = serde_json::from_str::<serde_json::Value>("{broken").unwrap_err();
        let expected = parse.to_string();
        assert_eq!(SourceError::from(parse).0, expected);
    }

    #[test]
    fn sqlx_errors_convert() {
        let expected = sqlx::Error::RowNotFound.to_string();
        assert_eq!(SourceError::from(sqlx::Error::RowNotFound).0, expected);
    }

    #[tokio::test]
    async fn join_errors_convert() {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        let joined = handle.await.unwrap_err();
        let expected = joined.to_string();
        assert_eq!(SourceError::from(joined).0, expected);
    }
}
