//! `FleetHub`: the single task behind `GET /fleet/live` (spec §5). Started by the first
//! subscriber, stopped by the last. All decisions live in `Book` and `fuse`; this schedules the
//! passes, performs the reads, redacts, and fans the events out to bounded per-client channels.
//!
//! The state mutex is never held across an `.await`.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::governance::redact::redact_secrets;

use super::FleetEnv;
use super::book::{Book, PinError, ReadOutcome, ReadWhich};
use super::event::FleetEvent;
use super::fuse::{Fused, SourceSightings, fuse};
use super::limits::{
    CLIENT_CHANNEL, HEARTBEAT, LINES_BATCH, METRICS_BATCH, RETENTION, SAFETY_NET, WATCH_DEBOUNCE,
};
use super::model::{Cursor, TermLine, Timestamp, TranscriptPage};
use super::source::{AgentSource, Edge, EdgeSource, Sighting, SourceError, SourceResult};
use super::watch::{NotifyWatch, WatchFactory, WatchGuard};

/// How often a source that has nothing to watch (`daemon_runs`) is asked again. Hub-local: the spec
/// §5 table has no such row, because it assumed every source is a file.
pub const UNWATCHED_POLL: Duration = Duration::from_millis(500);

/// One NDJSON line, already encoded and newline-terminated.
pub type Line = Arc<str>;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        chrono::Utc::now()
    }
}

#[derive(Clone, Debug)]
pub struct HubConfig {
    pub client_channel: usize,
    pub unwatched_poll: Duration,
    /// The first snapshot waits at most this long for the first discovery.
    pub warm_up_timeout: Duration,
    /// Every adapter call (discover, edges, read) is cut after this long; the failure is logged
    /// once and the source counts as having missed the round.
    pub source_timeout: Duration,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            client_channel: CLIENT_CHANNEL,
            unwatched_poll: UNWATCHED_POLL,
            warm_up_timeout: Duration::from_secs(15),
            source_timeout: Duration::from_secs(10),
        }
    }
}

struct Client {
    id: u64,
    tx: mpsc::Sender<Line>,
    lagged: Arc<AtomicBool>,
}

struct State {
    book: Book,
    clients: Vec<Client>,
    next_client: u64,
    task: Option<JoinHandle<()>>,
    watch: Option<WatchGuard>,
    watched: BTreeSet<PathBuf>,
    dirty_tx: Option<mpsc::UnboundedSender<PathBuf>>,
    /// The last sightings of each source, by registry index: a pass that refreshes only some
    /// sources still fuses all of them.
    sightings: Vec<Vec<Sighting>>,
    edges: Vec<Vec<Edge>>,
    /// Failures already logged, so a source that fails every 5 s logs once.
    logged: HashSet<String>,
}

/// Which sources a refresh asks. There is no "unwatched only" variant: the 500 ms tick never
/// calls `discover()` (contract amendment 15), see `poll_unwatched`.
#[derive(Clone, Copy)]
enum Which {
    /// Every source and every edge source (the 5 s net, start-up).
    All,
    /// The sources that have a file to watch; edges are not re-resolved (they query the database).
    Watched,
}

pub struct FleetHub {
    sources: Vec<Arc<dyn AgentSource>>,
    edge_sources: Vec<Arc<dyn EdgeSource>>,
    clock: Arc<dyn Clock>,
    watcher: Arc<dyn WatchFactory>,
    config: HubConfig,
    /// Serialises start-ups: two first subscribers must not both warm the hub.
    gate: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

fn encode(event: &FleetEvent) -> Line {
    let mut text =
        serde_json::to_string(event).unwrap_or_else(|_| r#"{"t":"heartbeat"}"#.to_owned());
    text.push('\n');
    Line::from(text)
}

fn retention() -> chrono::Duration {
    chrono::Duration::from_std(RETENTION).expect("retention fits a chrono duration")
}

fn redact_line(line: TermLine) -> TermLine {
    TermLine::new(line.kind, redact_secrets(&line.text))
}

fn fuse_cached(
    sources: &[Arc<dyn AgentSource>],
    sightings: &[Vec<Sighting>],
    edges: &[Vec<Edge>],
) -> Fused {
    let refs: Vec<SourceSightings<'_>> = sightings
        .iter()
        .enumerate()
        .map(|(index, list)| SourceSightings {
            index,
            name: sources[index].name(),
            sightings: list,
        })
        .collect();
    fuse(&refs, edges)
}

impl FleetHub {
    pub fn new(
        sources: Vec<Arc<dyn AgentSource>>,
        edge_sources: Vec<Arc<dyn EdgeSource>>,
        clock: Arc<dyn Clock>,
        watcher: Arc<dyn WatchFactory>,
        config: HubConfig,
    ) -> Arc<Self> {
        let state = State {
            book: Book::new(BTreeSet::new()),
            clients: Vec::new(),
            next_client: 0,
            task: None,
            watch: None,
            watched: BTreeSet::new(),
            dirty_tx: None,
            sightings: vec![Vec::new(); sources.len()],
            edges: vec![Vec::new(); edge_sources.len()],
            logged: HashSet::new(),
        };
        Arc::new(Self {
            sources,
            edge_sources,
            clock,
            watcher,
            config,
            gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(state),
        })
    }

    pub fn production(env: &FleetEnv) -> Arc<Self> {
        let (agents, edges) = super::sources(env);
        Self::new(
            agents,
            edges,
            Arc::new(SystemClock),
            Arc::new(NotifyWatch),
            HubConfig::default(),
        )
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_running(&self) -> bool {
        self.state().task.is_some()
    }

    fn publish(state: &mut State, events: &[FleetEvent]) {
        if events.is_empty() {
            return;
        }
        let lines: Vec<Line> = events.iter().map(encode).collect();
        state.clients.retain(|client| {
            if client.lagged.load(Ordering::Acquire) {
                return true;
            }
            for line in &lines {
                match client.tx.try_send(line.clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        client.lagged.store(true, Ordering::Release);
                        return true;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return false,
                }
            }
            true
        });
    }

    fn log_once(&self, scope: &str, cause: &SourceError) {
        let mut state = self.state();
        if state.logged.len() > 256 {
            state.logged.clear();
        }
        if state.logged.insert(format!("{scope}: {cause}")) {
            tracing::warn!(source = scope, error = %cause, "fleet source failed; its nodes degrade");
        }
    }

    // ---- subscription -------------------------------------------------------------------

    pub async fn subscribe(self: &Arc<Self>) -> Subscription {
        loop {
            let _gate = self.gate.lock().await;
            let running = self.state().task.is_some();
            if !running {
                // Not racing a deadline: the first snapshot is worth a short wait, a hung
                // adapter is not worth a hung page.
                let _ = tokio::time::timeout(self.config.warm_up_timeout, self.warm_up()).await;
            }
            let mut guard = self.state();
            let state = &mut *guard;
            if !running {
                let (handle, dirty_tx) = self.spawn_loop();
                state.task = Some(handle);
                state.dirty_tx = Some(dirty_tx);
            } else if state.task.is_none() {
                continue; // the last client left between the two looks: start over
            }
            let (tx, rx) = mpsc::channel(self.config.client_channel);
            let lagged = Arc::new(AtomicBool::new(false));
            let id = state.next_client;
            state.next_client += 1;
            // Snapshot and registration in one critical section: nothing can fall in between.
            let first = encode(&state.book.snapshot());
            state.clients.push(Client {
                id,
                tx,
                lagged: lagged.clone(),
            });
            drop(guard);
            if !running {
                self.rewatch();
            }
            return Subscription {
                hub: Arc::clone(self),
                id,
                rx,
                lagged,
                first: Some(first),
            };
        }
    }

    fn unsubscribe(&self, id: u64) {
        let mut guard = self.state();
        let state = &mut *guard;
        state.clients.retain(|client| client.id != id);
        if !state.clients.is_empty() {
            return;
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
        state.watch = None;
        state.watched.clear();
        state.dirty_tx = None;
        let pins = state.book.pins();
        state.book = Book::new(pins);
        state.sightings = vec![Vec::new(); self.sources.len()];
        state.edges = vec![Vec::new(); self.edge_sources.len()];
    }

    /// A snapshot for a client that fell behind, built under the lock that clears its flag.
    fn fresh_snapshot(&self, lagged: &AtomicBool) -> Line {
        let state = self.state();
        let line = encode(&state.book.snapshot());
        lagged.store(false, Ordering::Release);
        line
    }

    // ---- passes -------------------------------------------------------------------------

    /// One adapter call under `source_timeout`. A failure or a timeout is logged once under
    /// `scope` and yields `None`.
    async fn bounded<T>(
        &self,
        scope: &str,
        call: impl std::future::Future<Output = SourceResult<T>>,
    ) -> Option<T> {
        match tokio::time::timeout(self.config.source_timeout, call).await {
            Ok(Ok(value)) => Some(value),
            Ok(Err(error)) => {
                self.log_once(scope, &error);
                None
            }
            Err(_) => {
                let cause = SourceError::msg(format!(
                    "no answer within {} s",
                    self.config.source_timeout.as_secs()
                ));
                self.log_once(scope, &cause);
                None
            }
        }
    }

    async fn discover_one(&self, source: &dyn AgentSource, since: Timestamp) -> Vec<Sighting> {
        self.bounded(source.name(), source.discover(since))
            .await
            .unwrap_or_default()
    }

    async fn edges_all(&self, since: Timestamp) -> Vec<Vec<Edge>> {
        let mut lists = Vec::with_capacity(self.edge_sources.len());
        for source in &self.edge_sources {
            lists.push(
                self.bounded(source.name(), source.edges(since))
                    .await
                    .unwrap_or_default(),
            );
        }
        lists
    }

    /// Re-discovers the chosen sources, re-fuses everything cached, and hands the result to the book.
    async fn refresh(&self, which: Which) {
        let now = self.clock.now();
        let since = now - retention();
        let mut fresh: Vec<(usize, Vec<Sighting>)> = Vec::new();
        for (index, source) in self.sources.iter().enumerate() {
            let wanted = match which {
                Which::All => true,
                Which::Watched => !source.watch_paths().is_empty(),
            };
            if wanted {
                fresh.push((index, self.discover_one(source.as_ref(), since).await));
            }
        }
        let edges = match which {
            Which::Watched => None,
            Which::All => Some(self.edges_all(since).await),
        };
        {
            let mut guard = self.state();
            let state = &mut *guard;
            for (index, sightings) in fresh {
                state.sightings[index] = sightings;
            }
            if let Some(edges) = edges {
                state.edges = edges;
            }
            let mut fused = fuse_cached(&self.sources, &state.sightings, &state.edges);
            for (child, parent) in &fused.ignored_cycles {
                if state.logged.insert(format!("cycle {child}->{parent}")) {
                    tracing::warn!(%child, %parent, "fleet: ignoring an edge that would close a cycle");
                }
            }
            // Contract amendment 10: a label can carry a pasted token (adapters redact too, this
            // is the backstop). Redacted here so the book, the snapshot and every event agree.
            for node in &mut fused.nodes {
                node.node.label = redact_secrets(&node.node.label);
            }
            let events = state.book.apply_fusion(fused.nodes, now);
            Self::publish(state, &events);
        }
        // Contract amendment 4: a discover() can reveal a new directory to watch (a `subagents/`
        // folder); the state lock is released first because `rewatch` takes it.
        self.rewatch();
    }

    /// Fuses a fresh look at every source without touching the running state. For pins and
    /// transcripts asked while nobody is subscribed.
    async fn fuse_once(&self) -> Fused {
        let since = self.clock.now() - retention();
        let mut sightings = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            sightings.push(self.discover_one(source.as_ref(), since).await);
        }
        let edges = self.edges_all(since).await;
        fuse_cached(&self.sources, &sightings, &edges)
    }

    /// The line reads the book asks for.
    async fn read_nodes(&self, which: ReadWhich<'_>) {
        let now = self.clock.now();
        let jobs = self.state().book.read_jobs(which);
        for job in jobs {
            let Some(source) = self.sources.get(job.source) else {
                continue;
            };
            let Some(node) = source.node_ref(&job.key) else {
                continue;
            };
            let scope = format!("{} {}", source.name(), job.id);
            let Some(read) = self
                .bounded(&scope, source.read_lines(&node, job.offset))
                .await
            else {
                continue;
            };
            let outcome = ReadOutcome {
                lines: read.lines.into_iter().map(redact_line).collect(),
                offset: read.offset,
                usage: read.usage,
                last_activity_at: read.last_activity_at,
            };
            let mut state = self.state();
            state.book.set_path(&job.id, node.path.clone());
            state.book.apply_lines(&job.id, outcome, now);
        }
    }

    /// How finished runs ended. The native adapter of a session a daemon run launched cannot know
    /// the run's status, so for such a node the daemon source is read from the start and only its
    /// `Error` line is kept (a success relies on the native `Final`; a run-only node already gets
    /// `Final` / `Error` from `daemon_runs` as its own line source).
    async fn read_errors(&self) {
        let jobs = self.state().book.error_jobs();
        for job in jobs {
            let Some(source) = self.sources.get(job.source) else {
                continue;
            };
            let Some(node) = source.node_ref(&job.key) else {
                continue;
            };
            let scope = format!("{} {} status", source.name(), job.id);
            let Some(read) = self
                .bounded(&scope, source.read_lines(&node, job.offset))
                .await
            else {
                continue;
            };
            let lines = read.lines.into_iter().map(redact_line).collect();
            self.state().book.apply_error(&job.id, lines);
        }
    }

    async fn read_pass(&self, which: ReadWhich<'_>) {
        self.read_nodes(which).await;
        self.read_errors().await;
    }

    async fn warm_up(&self) {
        self.refresh(Which::All).await;
        self.read_pass(ReadWhich::Pending).await;
        // Nobody is subscribed yet: what the passes queued is part of the first snapshot.
        let mut state = self.state();
        state.book.flush_lines();
        state.book.flush_metrics();
    }

    async fn dirty_pass(&self, paths: HashSet<PathBuf>) {
        let overflow = paths.iter().any(|path| path.as_os_str().is_empty());
        let (ids, unknown) = self.state().book.match_paths(&paths);
        if unknown {
            self.refresh(Which::Watched).await;
        }
        if overflow {
            self.read_pass(ReadWhich::AllLive).await;
        } else {
            self.read_pass(ReadWhich::Ids(&ids)).await;
        }
    }

    async fn safety_net(&self) {
        self.refresh(Which::All).await;
        self.read_pass(ReadWhich::AllLive).await;
    }

    /// The 500 ms tick for sources with nothing to watch (`daemon_runs`). Contract amendment 15:
    /// a memory read and nothing else. It reads the lines of the live nodes those sources serve
    /// (`daemon_runs` reads its in-memory `RunTails`), and it does NOT call `discover()` (that
    /// queries the `runs` table) nor resolve edges (batched queries): both wait for the 5 s net, so
    /// a new or finished run reaches the screen within 5 s, its output within 500 ms. The only
    /// non-memory read left in a tick is the one closing read of a node that just ended.
    async fn poll_unwatched(&self) {
        let unwatched: BTreeSet<usize> = self
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.watch_paths().is_empty())
            .map(|(index, _)| index)
            .collect();
        if unwatched.is_empty() {
            return;
        }
        self.read_nodes(ReadWhich::LiveOf(&unwatched)).await;
    }

    fn flush_lines(&self) {
        let mut guard = self.state();
        let state = &mut *guard;
        let events = state.book.flush_lines();
        Self::publish(state, &events);
    }

    fn flush_metrics(&self) {
        let mut guard = self.state();
        let state = &mut *guard;
        let events = state.book.flush_metrics();
        Self::publish(state, &events);
    }

    fn heartbeat(&self) {
        let mut state = self.state();
        Self::publish(&mut state, &[FleetEvent::Heartbeat]);
    }

    /// Test seam: publish a heartbeat now instead of waiting 15 s.
    #[cfg(test)]
    pub(crate) fn emit_heartbeat(&self) {
        self.heartbeat();
    }

    /// (Re)starts the OS watcher when the set of existing directories changed: a directory that
    /// did not exist at start-up (`~/.codex/sessions` on a fresh machine) gets watched once it does.
    ///
    /// Watching is recursive, so a path under another watched path is redundant (and would deliver
    /// every event twice): only the topmost paths are watched, which keeps the set almost constant
    /// when a `subagents/` directory appears under an already watched project. The new watcher is
    /// started outside the state lock (the start can walk a whole tree) and swapped in; the old
    /// one is released after.
    fn rewatch(&self) {
        let existing: BTreeSet<PathBuf> = self
            .sources
            .iter()
            .flat_map(|source| source.watch_paths())
            .filter(|path| path.exists())
            .collect();
        let desired: BTreeSet<PathBuf> = existing
            .iter()
            .filter(|path| !path.ancestors().skip(1).any(|up| existing.contains(up)))
            .cloned()
            .collect();
        let tx = {
            let state = self.state();
            if state.watch.is_some() && state.watched == desired {
                return;
            }
            match state.dirty_tx.clone() {
                Some(tx) => tx,
                None => return,
            }
        };
        let paths: Vec<PathBuf> = desired.iter().cloned().collect();
        let fresh = self.watcher.start(&paths, tx.clone());
        let old = {
            let mut state = self.state();
            if state
                .dirty_tx
                .as_ref()
                .is_some_and(|now| now.same_channel(&tx))
            {
                state.watched = desired;
                state.watch.replace(fresh)
            } else {
                // The hub stopped (or restarted) while the watcher started: it is not wanted.
                drop(fresh);
                None
            }
        };
        drop(old);
    }

    fn spawn_loop(self: &Arc<Self>) -> (JoinHandle<()>, mpsc::UnboundedSender<PathBuf>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let hub = Arc::clone(self);
        (tokio::spawn(async move { hub.run(rx).await }), tx)
    }

    async fn run(self: Arc<Self>, mut dirty_rx: mpsc::UnboundedReceiver<PathBuf>) {
        use tokio::time::{Instant, MissedTickBehavior, interval_at};
        let every = |period: Duration| {
            let mut timer = interval_at(Instant::now() + period, period);
            timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
            timer
        };
        let mut debounce = every(WATCH_DEBOUNCE);
        let mut lines = every(LINES_BATCH);
        let mut metrics = every(METRICS_BATCH);
        let mut net = every(SAFETY_NET);
        let mut beat = every(HEARTBEAT);
        let mut poll = every(self.config.unwatched_poll);
        let mut dirty: HashSet<PathBuf> = HashSet::new();
        loop {
            tokio::select! {
                Some(path) = dirty_rx.recv() => { dirty.insert(path); }
                _ = debounce.tick() => {
                    if !dirty.is_empty() {
                        self.dirty_pass(std::mem::take(&mut dirty)).await;
                    }
                }
                _ = lines.tick() => self.flush_lines(),
                _ = metrics.tick() => self.flush_metrics(),
                _ = net.tick() => self.safety_net().await,
                _ = beat.tick() => self.heartbeat(),
                _ = poll.tick() => self.poll_unwatched().await,
            }
        }
    }
}

/// One connected client. Dropping it leaves the hub; the last drop stops the hub.
pub struct Subscription {
    hub: Arc<FleetHub>,
    id: u64,
    rx: mpsc::Receiver<Line>,
    lagged: Arc<AtomicBool>,
    first: Option<Line>,
}

impl Subscription {
    /// The next NDJSON line, `None` once the hub dropped this client. A client that fell behind
    /// gets a fresh snapshot instead of a hole.
    pub async fn next(&mut self) -> Option<Line> {
        if let Some(first) = self.first.take() {
            return Some(first);
        }
        let line = self.rx.recv().await?;
        if self.lagged.load(Ordering::Acquire) {
            while self.rx.try_recv().is_ok() {}
            return Some(self.hub.fresh_snapshot(&self.lagged));
        }
        Some(line)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TranscriptError {
    UnknownNode,
    NoTranscript,
    /// The adapter failed; the message is for the log, never for the client.
    Failed(String),
}

impl FleetHub {
    /// Pins or unpins a root. Pins live in memory and are lost when the daemon restarts (spec §9).
    pub async fn pin(&self, root_id: &str, on: bool) -> Result<(), PinError> {
        let now = self.clock.now();
        {
            let mut guard = self.state();
            let state = &mut *guard;
            if state.task.is_some() {
                let events = state.book.set_pin(root_id, on, now)?;
                Self::publish(state, &events);
                return Ok(());
            }
        }
        // Nobody is subscribed, so the book is empty: validate against a one-shot look, then keep
        // the pin for when the hub next starts.
        let mut scratch = Book::new(BTreeSet::new());
        scratch.apply_fusion(self.fuse_once().await.nodes, now);
        scratch.set_pin(root_id, on, now)?;
        self.state().book.pin_raw(root_id, on);
        Ok(())
    }

    /// One backwards page of a node's full transcript, with every entry redacted.
    pub async fn transcript(
        &self,
        node_id: &str,
        before: Option<Cursor>,
    ) -> Result<TranscriptPage, TranscriptError> {
        let known = {
            let state = self.state();
            if state.task.is_some() {
                state
                    .book
                    .lookup(node_id)
                    .map(|f| (f.key.clone(), f.transcript_source))
            } else {
                None
            }
        };
        let (key, source_index) = match known {
            Some(found) => found,
            None => {
                let fused = self.fuse_once().await;
                let node = fused
                    .nodes
                    .into_iter()
                    .find(|n| n.node.id == node_id)
                    .ok_or(TranscriptError::UnknownNode)?;
                (node.key, node.transcript_source)
            }
        };
        let source = source_index
            .and_then(|index| self.sources.get(index))
            .ok_or(TranscriptError::NoTranscript)?;
        let node = source.node_ref(&key).ok_or(TranscriptError::NoTranscript)?;
        let mut page = self
            .bounded(source.name(), source.transcript_page(&node, before))
            .await
            .ok_or_else(|| TranscriptError::Failed("the adapter failed or timed out".to_owned()))?;
        for entry in &mut page.entries {
            entry.text = redact_secrets(&entry.text);
            entry.detail = entry.detail.as_deref().map(redact_secrets);
        }
        Ok(page)
    }
}

/// The hub as `AppState` carries it (`state.fleet`): built on first use from the state it is asked
/// with, so a state that never opens the fleet page never starts anything.
pub fn hub(state: &crate::state::AppState) -> Arc<FleetHub> {
    state.fleet.get_or_init(|| {
        let home = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        FleetHub::production(&FleetEnv {
            pool: state.pool.clone(),
            home,
            run_tails: state.run_tails.clone(),
        })
    })
}

/// Tests put their own hub (fake source, clock and watcher) in place.
#[cfg(test)]
pub(crate) fn install(slot: &crate::state::LateSlot, hub: Arc<FleetHub>) {
    slot.set(hub);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::model::{NodeKey, NodeState};
    use crate::fleet::testkit::{FakeClock, FakeSource, FakeWatch, at, sighting};
    use std::time::Duration;

    struct Harness {
        hub: Arc<FleetHub>,
        source: Arc<FakeSource>,
        clock: Arc<FakeClock>,
        watch: Arc<FakeWatch>,
        dir: tempfile::TempDir,
    }

    fn harness_with(config: HubConfig, watched: bool) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let paths = if watched {
            vec![dir.path().to_path_buf()]
        } else {
            vec![]
        };
        let source = FakeSource::new("fake", paths);
        let clock = FakeClock::new(at(1_000));
        let watch = FakeWatch::new();
        let agent: Arc<dyn AgentSource> = source.clone();
        let hub = FleetHub::new(vec![agent], vec![], clock.clone(), watch.clone(), config);
        Harness {
            hub,
            source,
            clock,
            watch,
            dir,
        }
    }

    fn harness() -> Harness {
        harness_with(HubConfig::default(), true)
    }

    fn root(id: &str) -> crate::fleet::source::Sighting {
        let mut s = sighting("claude-code", id);
        s.last_activity_at = at(990);
        s.started_at = at(900);
        s
    }

    /// Every event the subscription has queued, parsed.
    async fn drain(sub: &mut Subscription) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(Some(line)) = tokio::time::timeout(Duration::from_millis(1), sub.next()).await
        {
            out.push(serde_json::from_str(line.trim_end()).expect("a JSON line"));
        }
        out
    }

    fn kinds(events: &[serde_json::Value]) -> Vec<&str> {
        events.iter().map(|e| e["t"].as_str().unwrap()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_subscriber_starts_the_hub_and_the_last_one_stops_it_and_frees_the_watcher() {
        let h = harness();
        h.source.set(vec![root("a")]);
        assert!(!h.hub.is_running());
        assert_eq!(
            h.source
                .discoveries
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "nothing runs with nobody watching"
        );

        let first = h.hub.subscribe().await;
        assert!(h.hub.is_running());
        assert_eq!(h.watch.live(), 1);
        let second = h.hub.subscribe().await;
        assert_eq!(
            h.watch.started.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one watcher for all clients"
        );

        drop(first);
        assert!(h.hub.is_running(), "one client is still there");
        assert_eq!(h.watch.live(), 1);
        drop(second);
        assert!(!h.hub.is_running());
        assert_eq!(h.watch.live(), 0, "the watcher is released");

        let before = h
            .source
            .discoveries
            .load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(
            h.source
                .discoveries
                .load(std::sync::atomic::Ordering::SeqCst),
            before,
            "a stopped hub does not poll"
        );

        let _again = h.hub.subscribe().await;
        assert_eq!(h.watch.live(), 1, "and it starts again");
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_event_is_a_snapshot_with_the_nodes_and_their_recent_lines() {
        let h = harness();
        h.source.set(vec![root("a")]);
        h.source
            .push_line(&NodeKey::new("claude-code", "a"), "hello");

        let mut sub = h.hub.subscribe().await;
        let events = drain(&mut sub).await;

        assert_eq!(kinds(&events[..1]), ["snapshot"]);
        assert_eq!(events[0]["nodes"][0]["id"], "claude-code:a");
        assert_eq!(events[0]["lines"]["claude-code:a"][0]["text"], "hello");
        assert!(events[0]["metrics"].as_array().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn every_line_is_one_json_object_ending_in_a_newline() {
        let h = harness();
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        let first = sub.next().await.unwrap();
        assert!(first.ends_with('\n'));
        assert_eq!(first.matches('\n').count(), 1);
        serde_json::from_str::<serde_json::Value>(&first).unwrap();
        h.hub.heartbeat();
        let beat = sub.next().await.unwrap();
        assert_eq!(&*beat, "{\"t\":\"heartbeat\"}\n");
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_line_emits_lines_and_metrics_and_never_a_node() {
        let h = harness();
        let key = NodeKey::new("claude-code", "a");
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await; // the snapshot

        h.source.push_line(&key, "second");
        h.hub
            .dirty_pass([h.source.path_of(&key)].into_iter().collect())
            .await;
        h.hub.flush_lines();
        h.hub.flush_metrics();

        let events = drain(&mut sub).await;
        assert_eq!(kinds(&events), ["lines", "metrics"]);
        assert_eq!(events[0]["node_id"], "claude-code:a");
        assert_eq!(events[0]["lines"][0]["text"], "second");
    }

    #[tokio::test(start_paused = true)]
    async fn the_watcher_drives_a_read_inside_the_debounce_window() {
        let h = harness();
        let key = NodeKey::new("claude-code", "a");
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        h.source.push_line(&key, "from the watcher");
        h.watch.report(h.source.path_of(&key));
        tokio::time::sleep(Duration::from_millis(350)).await;

        let events = drain(&mut sub).await;
        assert!(kinds(&events).contains(&"lines"), "{events:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_path_nobody_knows_triggers_a_discovery_and_a_lost_buffer_reads_everything() {
        let h = harness();
        let a = NodeKey::new("claude-code", "a");
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        // A new session appears; the watcher only names its file.
        h.source.set(vec![root("a"), root("b")]);
        h.hub
            .dirty_pass([h.dir.path().join("b.jsonl")].into_iter().collect())
            .await;
        let events = drain(&mut sub).await;
        assert!(
            events
                .iter()
                .any(|e| e["t"] == "node" && e["node"]["id"] == "claude-code:b"),
            "{events:?}"
        );

        // The empty path means the OS dropped events: every live node is read.
        h.source.push_line(&a, "late");
        h.hub
            .dirty_pass([std::path::PathBuf::new()].into_iter().collect())
            .await;
        h.hub.flush_lines();
        assert!(kinds(&drain(&mut sub).await).contains(&"lines"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_falls_behind_gets_a_fresh_snapshot_and_then_the_stream_again() {
        let h = harness_with(
            HubConfig {
                client_channel: 4,
                ..HubConfig::default()
            },
            true,
        );
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        assert_eq!(kinds(&drain_n(&mut sub, 1).await), ["snapshot"]);

        for _ in 0..10 {
            h.hub.heartbeat(); // nobody is reading
        }
        let recovered = drain_n(&mut sub, 1).await;
        assert_eq!(kinds(&recovered), ["snapshot"], "a hole is never left");

        h.hub.heartbeat();
        assert_eq!(kinds(&drain_n(&mut sub, 1).await), ["heartbeat"]);
    }

    async fn drain_n(sub: &mut Subscription, n: usize) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for _ in 0..n {
            let line = sub.next().await.expect("an event");
            out.push(serde_json::from_str(line.trim_end()).unwrap());
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn a_secret_in_a_line_is_redacted_before_it_leaves() {
        let h = harness();
        h.source.set(vec![root("a")]);
        h.source.push_line(
            &NodeKey::new("claude-code", "a"),
            "export GH=ghp_abcdefghijklmnopqrstuvwxyz0123456789 now",
        );
        let mut sub = h.hub.subscribe().await;
        let raw = sub.next().await.unwrap();
        assert!(
            !raw.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            "{raw}"
        );
        assert!(raw.contains("[SECRET:github]"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_secret_in_a_label_is_redacted_in_the_snapshot_and_in_node_events() {
        // Contract amendment 10: the hub redacts `FleetNode.label` too (defence in depth behind
        // the adapters' `clean_label`).
        let h = harness();
        let mut leaky = root("a");
        leaky.label = "deploy with ghp_abcdefghijklmnopqrstuvwxyz0123456789".into();
        h.source.set(vec![leaky]);
        let mut sub = h.hub.subscribe().await;
        let first = drain(&mut sub).await;
        assert_eq!(first[0]["nodes"][0]["label"], "deploy with [SECRET:github]");

        let mut renamed = root("a");
        renamed.label = "now ghp_abcdefghijklmnopqrstuvwxyz0123456789".into();
        h.source.set(vec![renamed]);
        h.hub.safety_net().await;
        let events = drain(&mut sub).await;
        let node = events
            .iter()
            .find(|e| e["t"] == "node")
            .expect("a node event");
        assert_eq!(node["node"]["label"], "now [SECRET:github]");
    }

    #[tokio::test(start_paused = true)]
    async fn a_watch_directory_that_appears_after_discovery_is_watched_by_the_next_pass() {
        // Contract amendment 4: `watch_paths()` is re-read after every discover(), not only at the
        // 5 s net, so a new `subagents/` directory is watched within the debounce window.
        let h = harness();
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;
        assert_eq!(h.watch.started.load(std::sync::atomic::Ordering::SeqCst), 1);

        let later = tempfile::tempdir().unwrap();
        h.source
            .set_paths(vec![h.dir.path().to_path_buf(), later.path().to_path_buf()]);
        h.hub
            .dirty_pass([later.path().join("new.jsonl")].into_iter().collect())
            .await;

        assert_eq!(
            h.watch.started.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "re-watched"
        );
        assert_eq!(h.watch.live(), 1, "and the old watcher was released");
    }

    #[tokio::test(start_paused = true)]
    async fn a_watch_path_under_a_watched_one_is_not_watched_and_does_not_rebuild_the_watcher() {
        let h = harness();
        h.source.set(vec![root("a")]);
        let under = h.dir.path().join("sub");
        std::fs::create_dir(&under).unwrap();
        h.source
            .set_paths(vec![h.dir.path().to_path_buf(), under.clone()]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;
        assert_eq!(
            *h.watch.last_paths.lock().unwrap(),
            vec![h.dir.path().to_path_buf()],
            "the recursive ancestor covers the subdirectory"
        );

        let deeper = under.join("subagents");
        std::fs::create_dir(&deeper).unwrap();
        h.source
            .set_paths(vec![h.dir.path().to_path_buf(), under, deeper.clone()]);
        h.hub
            .dirty_pass([deeper.join("new.jsonl")].into_iter().collect())
            .await;
        assert_eq!(
            h.watch.started.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a new directory below a watched one changes nothing"
        );
    }

    fn finished_nine_minutes_ago(h: &Harness) {
        let mut done = root("a");
        done.state = NodeState::Done;
        done.ended_at = Some(at(1_000 - 9 * 60));
        done.last_activity_at = at(1_000 - 9 * 60);
        h.source.set(vec![done]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_finished_tree_leaves_ten_minutes_after_its_root_ended() {
        let h = harness();
        finished_nine_minutes_ago(&h);
        let mut sub = h.hub.subscribe().await;
        assert_eq!(drain(&mut sub).await[0]["nodes"][0]["id"], "claude-code:a");

        h.clock.advance(120);
        h.hub.safety_net().await;
        let events = drain(&mut sub).await;
        assert_eq!(
            events,
            vec![serde_json::json!({"t": "removed", "node_id": "claude-code:a"})]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pin_holds_a_finished_tree_past_its_window() {
        let h = harness();
        finished_nine_minutes_ago(&h);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        h.hub.pin("claude-code:a", true).await.unwrap();
        h.clock.advance(3_600);
        h.hub.safety_net().await;

        let events = drain(&mut sub).await;
        assert!(!kinds(&events).contains(&"removed"), "{events:?}");
        assert!(
            events
                .iter()
                .any(|e| e["t"] == "node" && e["node"]["pinned"] == true)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_source_degrades_its_nodes_to_lost_and_the_hub_keeps_running() {
        let h = harness();
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        h.source
            .fail_discover
            .store(true, std::sync::atomic::Ordering::SeqCst);
        h.hub.safety_net().await;
        assert!(drain(&mut sub).await.is_empty(), "one miss is not enough");
        h.hub.safety_net().await;
        let events = drain(&mut sub).await;
        assert_eq!(events[0]["node"]["state"], "lost");
        assert!(h.hub.is_running());
    }

    #[tokio::test(start_paused = true)]
    async fn a_source_with_nothing_to_watch_is_polled_and_a_watched_one_is_not() {
        let h = harness_with(HubConfig::default(), false);
        let key = NodeKey::new("claude-code", "a");
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        // Contract amendment 15: the 500 ms tick is a memory read of live nodes. It never calls
        // `discover()` (daemon_runs would query `runs`) and never resolves edges; new and finished
        // nodes appear at the next 5 s safety net.
        let discoveries = h
            .source
            .discoveries
            .load(std::sync::atomic::Ordering::SeqCst);
        h.source.push_line(&key, "polled");
        h.hub.poll_unwatched().await;
        h.hub.flush_lines();
        assert!(kinds(&drain(&mut sub).await).contains(&"lines"));
        assert_eq!(
            h.source
                .discoveries
                .load(std::sync::atomic::Ordering::SeqCst),
            discoveries,
            "the tick does no discovery"
        );

        let watched = harness();
        watched.source.set(vec![root("a")]);
        let mut sub = watched.hub.subscribe().await;
        drain(&mut sub).await;
        let reads = watched
            .source
            .reads
            .load(std::sync::atomic::Ordering::SeqCst);
        watched.hub.poll_unwatched().await;
        assert_eq!(
            watched
                .source
                .reads
                .load(std::sync::atomic::Ordering::SeqCst),
            reads
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_connection_gets_a_heartbeat_every_fifteen_seconds() {
        let h = harness();
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        tokio::time::sleep(HEARTBEAT + Duration::from_millis(50)).await;
        let events = drain(&mut sub).await;
        assert_eq!(
            kinds(&events).iter().filter(|k| **k == "heartbeat").count(),
            1,
            "{events:?}"
        );
        tokio::time::sleep(HEARTBEAT).await;
        assert_eq!(
            kinds(&drain(&mut sub).await)
                .iter()
                .filter(|k| **k == "heartbeat")
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_safety_net_finds_a_node_the_watcher_never_reported() {
        let h = harness();
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        h.source.set(vec![root("a"), root("late")]);
        tokio::time::sleep(SAFETY_NET + Duration::from_millis(50)).await;

        let events = drain(&mut sub).await;
        assert!(
            events
                .iter()
                .any(|e| e["t"] == "node" && e["node"]["id"] == "claude-code:late"),
            "{events:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn metrics_are_batched_to_one_event_a_second_and_lines_to_a_tenth() {
        let h = harness();
        let key = NodeKey::new("claude-code", "a");
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        for n in 0..5 {
            h.source.push_line(&key, &format!("line {n}"));
            h.hub
                .dirty_pass([h.source.path_of(&key)].into_iter().collect())
                .await;
        }
        tokio::time::sleep(Duration::from_millis(1_050)).await;

        let events = drain(&mut sub).await;
        let lines: Vec<_> = events.iter().filter(|e| e["t"] == "lines").collect();
        let metrics = events.iter().filter(|e| e["t"] == "metrics").count();
        assert_eq!(
            lines.len(),
            1,
            "five reads inside one batch become one event: {events:?}"
        );
        assert_eq!(lines[0]["lines"].as_array().unwrap().len(), 5);
        assert_eq!(metrics, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_source_is_cut_after_the_source_timeout_and_logged_once() {
        let h = harness_with(
            HubConfig {
                source_timeout: Duration::from_secs(1),
                ..HubConfig::default()
            },
            true,
        );
        h.source.set(vec![root("a")]);
        let mut sub = h.hub.subscribe().await;
        drain(&mut sub).await;

        h.source
            .hang_discover
            .store(true, std::sync::atomic::Ordering::SeqCst);
        h.hub.safety_net().await;
        h.hub.safety_net().await;

        // The warm-up discovery plus the two hung ones: each came back (cut), none stalled the hub.
        assert_eq!(
            h.source
                .discoveries
                .load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(
            h.hub.state().logged.len(),
            1,
            "the same timeout is logged once"
        );
        assert!(h.hub.is_running());
    }

    /// The first snapshot of a hub whose only node is a Claude Code session that a daemon run
    /// launched and that has ended with `status`. Registry: 0 = the native adapter, 1 = daemon_runs.
    async fn snapshot_after_run_ended(status: &str) -> serde_json::Value {
        use crate::fleet::model::TermKind;
        let key = NodeKey::new("claude-code", "a");
        let native = FakeSource::new("native", vec![]);
        let daemon = FakeSource::new(crate::fleet::fuse::DAEMON_RUNS_SOURCE, vec![]);
        let mut seen = root("a");
        seen.state = NodeState::Done;
        seen.ended_at = Some(at(990));
        native.set(vec![seen]);
        native.push_line(&key, "closing words");
        let mut run = sighting("claude-code", "a");
        run.state = NodeState::Done;
        run.ended_at = Some(at(990));
        run.last_activity_at = at(990);
        run.run_id = Some(5);
        daemon.set(vec![run]);
        // What `daemon_runs` really answers for a finished run (`stream::final_line`).
        if status == "completed" {
            daemon.push_line_kind(&key, TermKind::Final, "All green");
        } else {
            daemon.push_line_kind(&key, TermKind::Error, status);
        }
        let (native, daemon): (Arc<dyn AgentSource>, Arc<dyn AgentSource>) = (native, daemon);
        let hub = FleetHub::new(
            vec![native, daemon],
            vec![],
            FakeClock::new(at(1_000)),
            FakeWatch::new(),
            HubConfig::default(),
        );
        let mut sub = hub.subscribe().await;
        drain(&mut sub).await.remove(0)
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_that_did_not_succeed_ends_done_with_an_error_line_and_a_successful_one_does_not()
    {
        for status in ["failed", "cancelled", "timed_out"] {
            let snapshot = snapshot_after_run_ended(status).await;
            assert_eq!(snapshot["nodes"][0]["state"], "done", "{status}");
            let lines = snapshot["lines"]["claude-code:a"]
                .as_array()
                .expect("lines");
            assert_eq!(
                lines.len(),
                1,
                "a finished node shows only its last line: {status}"
            );
            assert_eq!(lines[0]["kind"], "error", "{status}");
            assert_eq!(lines[0]["text"], status);
        }

        let snapshot = snapshot_after_run_ended("completed").await;
        let lines = snapshot["lines"]["claude-code:a"]
            .as_array()
            .expect("lines");
        assert_eq!(
            lines[0]["kind"], "text",
            "success keeps the native closing line"
        );
        assert_eq!(lines[0]["text"], "closing words");
    }

    #[tokio::test(start_paused = true)]
    async fn pinning_validates_the_node_and_works_with_nobody_subscribed() {
        let h = harness();
        let mut done = root("a");
        done.state = NodeState::Done;
        done.ended_at = Some(at(990));
        let mut kid = sighting("claude-code", "kid");
        kid.parent = Some(NodeKey::new("claude-code", "a"));
        h.source.set(vec![done, kid]);

        // Stopped hub: validated against a one-shot look.
        assert_eq!(
            h.hub.pin("claude-code:ghost", true).await,
            Err(PinError::Unknown)
        );
        assert_eq!(
            h.hub.pin("claude-code:kid", true).await,
            Err(PinError::NotARoot)
        );
        h.hub.pin("claude-code:a", true).await.unwrap();
        assert!(!h.hub.is_running());

        // The pin was kept: the next run shows the node pinned.
        let mut sub = h.hub.subscribe().await;
        assert_eq!(drain(&mut sub).await[0]["nodes"][0]["pinned"], true);
        h.hub.pin("claude-code:a", false).await.unwrap();
        let events = drain(&mut sub).await;
        assert!(
            events
                .iter()
                .any(|e| e["t"] == "node" && e["node"]["pinned"] == false),
            "{events:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_transcript_page_is_redacted_and_the_errors_are_distinct() {
        use crate::fleet::model::{TermKind, TranscriptEntry};
        let h = harness();
        let mut with = root("a");
        with.has_transcript = true;
        let without = root("b");
        h.source.set(vec![with, without]);
        h.source.set_transcript(TranscriptPage {
            entries: vec![TranscriptEntry {
                kind: TermKind::Tool,
                text: "Bash echo ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
                detail: Some("ghp_abcdefghijklmnopqrstuvwxyz0123456789".into()),
            }],
            before: Some("120".into()),
        });

        let page = h.hub.transcript("claude-code:a", None).await.unwrap();
        assert!(!page.entries[0].text.contains("ghp_"), "{page:?}");
        assert!(!page.entries[0].detail.as_deref().unwrap().contains("ghp_"));
        assert_eq!(
            page.before.as_deref(),
            Some("120"),
            "the cursor passes through untouched"
        );

        assert_eq!(
            h.hub.transcript("claude-code:b", None).await,
            Err(TranscriptError::NoTranscript)
        );
        assert_eq!(
            h.hub.transcript("claude-code:nope", None).await,
            Err(TranscriptError::UnknownNode)
        );
    }

    // Plain `#[tokio::test]`: `start_paused` would auto-advance the clock while sqlx's pool waits
    // on its worker thread, and `channels::testkit::state()` opens a SQLite pool.
    #[tokio::test]
    async fn the_runtime_builds_one_hub_and_hands_it_out_again() {
        let h = harness();
        let state = crate::channels::testkit::state().await;
        install(&state.fleet, h.hub.clone());
        assert!(Arc::ptr_eq(&hub(&state), &h.hub));
    }
}
