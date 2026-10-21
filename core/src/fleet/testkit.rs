//! Builders and fakes for the fleet tests. `#[cfg(test)]` only.
// Later chunks (book, hub, the adapters) use the builders this chunk does not.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::DateTime;
use tokio::sync::mpsc::UnboundedSender;

use super::fuse::FusedNode;
use super::hub::Clock;
use super::model::{
    Capabilities, Cursor, FleetNode, NodeKey, NodeState, TermKind, TermLine, Timestamp,
    TranscriptPage,
};
use super::source::{
    AgentSource, Edge, LinesRead, NodeRef, Offset, Sighting, SourceError, SourceResult,
};
use super::watch::{WatchFactory, WatchGuard};

/// A fixed instant plus `secs`: every test reasons in offsets from one origin.
pub(crate) fn at(secs: i64) -> Timestamp {
    DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("a valid timestamp")
}

/// A busy, line-bearing sighting with nothing else set; tests mutate the fields they care about.
pub(crate) fn sighting(harness: &str, native_id: &str) -> Sighting {
    Sighting {
        key: NodeKey::new(harness, native_id),
        parent: None,
        harness_label: harness.to_owned(),
        model: None,
        role: None,
        label: native_id.to_owned(),
        project: None,
        state: NodeState::Busy,
        started_at: at(0),
        ended_at: None,
        last_activity_at: at(0),
        run_id: None,
        can_stop: false,
        has_lines: true,
        has_transcript: false,
        usage: None,
    }
}

pub(crate) fn edge(child: (&str, &str), parent: (&str, &str)) -> Edge {
    Edge {
        child: NodeKey::new(child.0, child.1),
        parent: NodeKey::new(parent.0, parent.1),
    }
}

/// A fused node. `parent` is `None` for a root; `root` is the key of the tree's root.
pub(crate) fn fused(
    key: (&str, &str),
    parent: Option<(&str, &str)>,
    root: (&str, &str),
    depth: u32,
    state: NodeState,
) -> FusedNode {
    let key = NodeKey::new(key.0, key.1);
    FusedNode {
        node: FleetNode {
            id: key.id(),
            parent_id: parent.map(|p| NodeKey::new(p.0, p.1).id()),
            root_id: NodeKey::new(root.0, root.1).id(),
            depth,
            harness: key.harness.clone(),
            model: None,
            role: None,
            label: key.native_id.clone(),
            state,
            started_at: at(0),
            ended_at: None,
            pinned: false,
            done_omitted: 0,
            project: None,
            run_id: None,
            capabilities: Capabilities {
                stop: false,
                open_run: false,
                full_transcript: false,
                pin: parent.is_none(),
            },
        },
        key,
        last_activity_at: at(0),
        usage: None,
        lines_source: Some(0),
        transcript_source: None,
        daemon_source: None,
    }
}

pub(crate) fn root_node(key: (&str, &str), state: NodeState) -> FusedNode {
    fused(key, None, key, 0, state)
}

pub(crate) fn child_node(key: (&str, &str), root: (&str, &str), state: NodeState) -> FusedNode {
    fused(key, Some(root), root, 1, state)
}

/// A source the test scripts: set its sightings, push lines, make it fail or hang.
pub(crate) struct FakeSource {
    name: &'static str,
    /// Settable, because real sources grow new watch directories after a `discover()`.
    paths: Mutex<Vec<PathBuf>>,
    sightings: Mutex<Vec<Sighting>>,
    lines: Mutex<HashMap<NodeKey, Vec<TermLine>>>,
    transcript: Mutex<TranscriptPage>,
    pub(crate) fail_discover: AtomicBool,
    /// `discover()` never completes (a hung source; the hub's per-call timeout must cut it).
    pub(crate) hang_discover: AtomicBool,
    pub(crate) discoveries: AtomicUsize,
    pub(crate) reads: AtomicUsize,
}

impl FakeSource {
    /// `paths` empty = an unwatched source (the hub polls it).
    pub(crate) fn new(name: &'static str, paths: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            name,
            paths: Mutex::new(paths),
            sightings: Mutex::new(Vec::new()),
            lines: Mutex::new(HashMap::new()),
            transcript: Mutex::new(TranscriptPage::default()),
            fail_discover: AtomicBool::new(false),
            hang_discover: AtomicBool::new(false),
            discoveries: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set(&self, sightings: Vec<Sighting>) {
        *self.sightings.lock().unwrap() = sightings;
    }

    /// Changes what `watch_paths()` reports from now on.
    pub(crate) fn set_paths(&self, paths: Vec<PathBuf>) {
        *self.paths.lock().unwrap() = paths;
    }

    pub(crate) fn push_line(&self, key: &NodeKey, text: &str) {
        self.push_line_kind(key, TermKind::Text, text);
    }

    pub(crate) fn push_line_kind(&self, key: &NodeKey, kind: TermKind, text: &str) {
        self.lines
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .push(TermLine::new(kind, text));
    }

    pub(crate) fn set_transcript(&self, page: TranscriptPage) {
        *self.transcript.lock().unwrap() = page;
    }

    /// The path `node_ref` reports for a key; a test sends it to the fake watcher.
    pub(crate) fn path_of(&self, key: &NodeKey) -> PathBuf {
        self.paths
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
            .join(format!("{}.jsonl", key.native_id))
    }
}

#[async_trait]
impl AgentSource for FakeSource {
    fn name(&self) -> &'static str {
        self.name
    }
    fn watch_paths(&self) -> Vec<PathBuf> {
        self.paths.lock().unwrap().clone()
    }
    async fn discover(&self, _since: Timestamp) -> SourceResult<Vec<Sighting>> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        if self.hang_discover.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.fail_discover.load(Ordering::SeqCst) {
            return Err(SourceError::msg("scripted failure"));
        }
        Ok(self.sightings.lock().unwrap().clone())
    }
    fn node_ref(&self, key: &NodeKey) -> Option<NodeRef> {
        self.sightings
            .lock()
            .unwrap()
            .iter()
            .any(|s| &s.key == key)
            .then(|| NodeRef {
                key: key.clone(),
                path: Some(self.path_of(key)),
            })
    }
    /// Offsets count lines: `Offset(n)` means "n lines already delivered".
    async fn read_lines(&self, node: &NodeRef, from: Offset) -> SourceResult<LinesRead> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let lines = self
            .lines
            .lock()
            .unwrap()
            .get(&node.key)
            .cloned()
            .unwrap_or_default();
        let start = if from == Offset::TAIL {
            0
        } else {
            (from.0 as usize).min(lines.len())
        };
        Ok(LinesRead {
            offset: Offset(lines.len() as u64),
            lines: lines[start..].to_vec(),
            usage: None,
            last_activity_at: None,
        })
    }
    async fn transcript_page(
        &self,
        _: &NodeRef,
        _: Option<Cursor>,
    ) -> SourceResult<TranscriptPage> {
        Ok(self.transcript.lock().unwrap().clone())
    }
}

/// A watcher that counts how many are alive and lets the test report a changed path.
pub(crate) struct FakeWatch {
    pub(crate) live: Arc<AtomicUsize>,
    pub(crate) started: AtomicUsize,
    /// The paths of the latest `start`.
    pub(crate) last_paths: Mutex<Vec<PathBuf>>,
    tx: Mutex<Option<UnboundedSender<PathBuf>>>,
}

struct Alive(Arc<AtomicUsize>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FakeWatch {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Arc::new(AtomicUsize::new(0)),
            started: AtomicUsize::new(0),
            last_paths: Mutex::new(Vec::new()),
            tx: Mutex::new(None),
        })
    }
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
    pub(crate) fn report(&self, path: PathBuf) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(path);
        }
    }
}

impl WatchFactory for FakeWatch {
    fn start(&self, paths: &[PathBuf], tx: UnboundedSender<PathBuf>) -> WatchGuard {
        *self.last_paths.lock().unwrap() = paths.to_vec();
        self.live.fetch_add(1, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        *self.tx.lock().unwrap() = Some(tx);
        Box::new(Alive(self.live.clone()))
    }
}

/// A clock the test moves by hand.
pub(crate) struct FakeClock(Mutex<Timestamp>);

impl FakeClock {
    pub(crate) fn new(now: Timestamp) -> Arc<Self> {
        Arc::new(Self(Mutex::new(now)))
    }
    pub(crate) fn advance(&self, secs: i64) {
        let mut now = self.0.lock().unwrap();
        *now += chrono::Duration::seconds(secs);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_source_serves_lines_by_count_and_reports_the_next_offset() {
        let source = FakeSource::new("fake", vec![]);
        let key = NodeKey::new("h", "n");
        source.set(vec![sighting("h", "n")]);
        source.push_line(&key, "a");
        source.push_line(&key, "b");
        let node = source.node_ref(&key).unwrap();

        let first = source.read_lines(&node, Offset::TAIL).await.unwrap();
        assert_eq!(first.lines.len(), 2);
        assert_eq!(first.offset, Offset(2));
        source.push_line(&key, "c");
        let next = source.read_lines(&node, first.offset).await.unwrap();
        assert_eq!(next.lines, vec![TermLine::new(TermKind::Text, "c")]);
        assert!(source.node_ref(&NodeKey::new("h", "other")).is_none());
    }

    #[tokio::test]
    async fn the_fake_source_watch_paths_can_grow_and_lines_carry_their_kind() {
        let source = FakeSource::new("fake", vec![PathBuf::from("/a")]);
        assert_eq!(source.watch_paths(), vec![PathBuf::from("/a")]);
        source.set_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(source.watch_paths().len(), 2);

        let key = NodeKey::new("h", "n");
        source.set(vec![sighting("h", "n")]);
        source.push_line_kind(&key, TermKind::Error, "boom");
        let node = source.node_ref(&key).unwrap();
        let read = source.read_lines(&node, Offset::TAIL).await.unwrap();
        assert_eq!(read.lines, vec![TermLine::new(TermKind::Error, "boom")]);
    }

    #[test]
    fn the_fake_watch_counts_live_watchers_and_forwards_reports() {
        let watch = FakeWatch::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let guard = watch.start(&[], tx);
        assert_eq!(watch.live(), 1);
        watch.report(PathBuf::from("/x"));
        assert_eq!(rx.try_recv().unwrap(), PathBuf::from("/x"));
        drop(guard);
        assert_eq!(watch.live(), 0);
    }
}
