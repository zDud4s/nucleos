//! The hub's memory, as a synchronous state machine: known nodes, the capped view the clients
//! see, rings of recent lines, metrics, pins, and the retention rules. Every method takes `now`
//! and returns the events it caused; nothing here waits, reads a file or touches a clock.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::PathBuf;

use super::event::FleetEvent;
use super::fuse::{FusedNode, is_live};
use super::limits::{RETENTION, RING_LINES, SNAPSHOT_TREES, TREE_NODES};
use super::model::{
    FleetNode, NodeId, NodeKey, NodeMetrics, NodeState, TermKind, TermLine, Timestamp,
};
use super::source::{Offset, Usage};

/// Rounds a live node may go unsighted before it is declared lost. Two, so a single failed or
/// racing `discover()` does not flicker a healthy node to `Lost`.
const MISS_LIMIT: u8 = 2;

struct Entry {
    fused: FusedNode,
    /// The node's file, as its adapter reported it; the watcher matches events against it.
    path: Option<PathBuf>,
    offset: Offset,
    ring: VecDeque<TermLine>,
    last_line: Option<TermLine>,
    misses: u8,
    needs_read: bool,
    /// The hub already asked the daemon source for this finished node's run status.
    error_checked: bool,
    /// `last_line` is the `Error` line appended from the run status; a later native read
    /// must not replace it.
    error_marked: bool,
}

pub struct Book {
    known: BTreeMap<NodeId, Entry>,
    /// What clients have been told: capped, with `pinned` and `done_omitted` filled in.
    view: BTreeMap<NodeId, FleetNode>,
    metrics: BTreeMap<NodeId, NodeMetrics>,
    pending_lines: BTreeMap<NodeId, Vec<TermLine>>,
    pending_metrics: BTreeSet<NodeId>,
    pins: BTreeSet<NodeId>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PinError {
    Unknown,
    NotARoot,
}

/// One read the hub must perform.
#[derive(Debug, PartialEq, Eq)]
pub struct ReadJob {
    pub id: NodeId,
    pub key: NodeKey,
    pub source: usize,
    pub offset: Offset,
}

/// What a read produced, already redacted.
pub struct ReadOutcome {
    pub lines: Vec<TermLine>,
    pub offset: Offset,
    pub usage: Option<Usage>,
    pub last_activity_at: Option<Timestamp>,
}

pub enum ReadWhich<'a> {
    /// Only nodes that were never read, or just stopped being alive.
    Pending,
    AllLive,
    /// Live nodes whose lines come from one of these registry indexes.
    LiveOf(&'a BTreeSet<usize>),
    Ids(&'a BTreeSet<NodeId>),
}

impl Book {
    pub fn new(pins: BTreeSet<NodeId>) -> Self {
        Self {
            known: BTreeMap::new(),
            view: BTreeMap::new(),
            metrics: BTreeMap::new(),
            pending_lines: BTreeMap::new(),
            pending_metrics: BTreeSet::new(),
            pins,
        }
    }

    pub fn lookup(&self, id: &str) -> Option<&FusedNode> {
        self.known.get(id).map(|entry| &entry.fused)
    }
}

impl Book {
    pub fn set_path(&mut self, id: &str, path: Option<PathBuf>) {
        if let Some(entry) = self.known.get_mut(id) {
            entry.path = path;
        }
    }

    /// Which known nodes the watcher's paths belong to, and whether any path belonged to none
    /// (a new file, or an empty path meaning the watcher lost events): then discovery must run.
    pub fn match_paths(&self, paths: &HashSet<PathBuf>) -> (BTreeSet<NodeId>, bool) {
        let mut ids = BTreeSet::new();
        let mut unknown = false;
        for path in paths {
            let matched: Vec<&NodeId> = self
                .known
                .iter()
                .filter(|(_, e)| e.path.as_deref() == Some(path.as_path()))
                .map(|(id, _)| id)
                .collect();
            if path.as_os_str().is_empty() || matched.is_empty() {
                unknown = true;
            }
            ids.extend(matched.into_iter().cloned());
        }
        (ids, unknown)
    }

    /// The reads to perform now. Only nodes in the view are read: what no client can see is not
    /// worth a file read. Anything that never had a first read, or just stopped being alive, is
    /// always included.
    pub fn read_jobs(&mut self, which: ReadWhich<'_>) -> Vec<ReadJob> {
        let mut jobs = Vec::new();
        for (id, entry) in self.known.iter_mut() {
            if !self.view.contains_key(id) {
                continue;
            }
            let Some(source) = entry.fused.lines_source else {
                entry.needs_read = false;
                continue;
            };
            let live = is_live(entry.fused.node.state);
            let asked = match &which {
                ReadWhich::Pending => false,
                ReadWhich::AllLive => live,
                ReadWhich::LiveOf(sources) => live && sources.contains(&source),
                ReadWhich::Ids(ids) => ids.contains(id),
            };
            if entry.needs_read || asked {
                entry.needs_read = false;
                jobs.push(ReadJob {
                    id: id.clone(),
                    key: entry.fused.key.clone(),
                    source,
                    offset: entry.offset,
                });
            }
        }
        jobs
    }

    pub fn apply_lines(&mut self, id: &str, read: ReadOutcome, now: Timestamp) {
        let shown: Vec<TermLine> = {
            let Some(entry) = self.known.get_mut(id) else {
                return;
            };
            entry.offset = read.offset;
            entry.ring.extend(read.lines.iter().cloned());
            while entry.ring.len() > RING_LINES {
                entry.ring.pop_front();
            }
            // The run status line stays the last line of a finished node.
            if let (Some(last), false) = (read.lines.last(), entry.error_marked) {
                entry.last_line = Some(last.clone());
            }
            if is_live(entry.fused.node.state) {
                let skip = read.lines.len().saturating_sub(RING_LINES);
                read.lines[skip..].to_vec()
            } else if entry.error_marked {
                Vec::new()
            } else {
                read.lines.last().cloned().into_iter().collect()
            }
        };
        if self.view.contains_key(id) && !shown.is_empty() {
            let pending = self.pending_lines.entry(id.to_owned()).or_default();
            pending.extend(shown);
            let excess = pending.len().saturating_sub(RING_LINES);
            pending.drain(..excess);
        }
        if !read.lines.is_empty() || read.last_activity_at.is_some() || read.usage.is_some() {
            self.note_metrics(id, read.last_activity_at.unwrap_or(now), read.usage.as_ref());
        }
    }

    /// The status reads to perform once the native reads of a pass are applied (contract
    /// amendment 1): finished nodes in the view that a daemon run launched but whose lines come
    /// from a native adapter. The adapter cannot know how the run ended, so the hub asks the
    /// daemon source (`ReadJob.source` is its registry index) for its last line, from the start,
    /// and hands the answer to `apply_error`. Each death is asked once; a node whose closing
    /// native read is still pending waits, so the status line stays last.
    pub fn error_jobs(&mut self) -> Vec<ReadJob> {
        let mut jobs = Vec::new();
        for (id, entry) in self.known.iter_mut() {
            let (Some(daemon), Some(lines)) = (entry.fused.daemon_source, entry.fused.lines_source)
            else {
                continue;
            };
            if lines == daemon
                || is_live(entry.fused.node.state)
                || entry.needs_read
                || entry.error_checked
                || !self.view.contains_key(id)
            {
                continue;
            }
            entry.error_checked = true;
            jobs.push(ReadJob {
                id: id.clone(),
                key: entry.fused.key.clone(),
                source: daemon,
                offset: Offset::TAIL,
            });
        }
        jobs
    }

    /// Appends the `Error` lines of a daemon-source answer (and nothing else) to a finished
    /// node: to its ring and as its last line, and queues them for the next `lines` event.
    pub fn apply_error(&mut self, id: &str, answer: Vec<TermLine>) {
        let errors: Vec<TermLine> = answer
            .into_iter()
            .filter(|line| line.kind == TermKind::Error)
            .collect();
        let Some(entry) = self.known.get_mut(id) else {
            return;
        };
        if errors.is_empty() || is_live(entry.fused.node.state) {
            return;
        }
        entry.ring.extend(errors.iter().cloned());
        while entry.ring.len() > RING_LINES {
            entry.ring.pop_front();
        }
        entry.last_line = errors.last().cloned();
        entry.error_marked = true;
        if self.view.contains_key(id) {
            self.pending_lines
                .entry(id.to_owned())
                .or_default()
                .extend(errors);
        }
    }

    /// The `lines` events queued since the last call, one per node.
    pub fn flush_lines(&mut self) -> Vec<FleetEvent> {
        std::mem::take(&mut self.pending_lines)
            .into_iter()
            .filter(|(id, lines)| self.view.contains_key(id) && !lines.is_empty())
            .map(|(node_id, lines)| FleetEvent::Lines { node_id, lines })
            .collect()
    }

    /// One `metrics` event with every change since the last call (none if nothing moved).
    pub fn flush_metrics(&mut self) -> Vec<FleetEvent> {
        let ids = std::mem::take(&mut self.pending_metrics);
        let metrics: Vec<NodeMetrics> = ids
            .iter()
            .filter(|id| self.view.contains_key(*id))
            .filter_map(|id| self.metrics.get(id).cloned())
            .collect();
        if metrics.is_empty() {
            Vec::new()
        } else {
            vec![FleetEvent::Metrics { metrics }]
        }
    }
}

impl Book {
    pub fn apply_fusion(&mut self, fused: Vec<FusedNode>, now: Timestamp) -> Vec<FleetEvent> {
        let sighted: BTreeSet<NodeId> = fused.iter().map(|f| f.node.id.clone()).collect();
        for mut incoming in fused {
            let id = incoming.node.id.clone();
            settle_end(&mut incoming, self.known.get(&id).map(|e| &e.fused.node), now);
            self.note_metrics(&id, incoming.last_activity_at, incoming.usage.as_ref());
            match self.known.get_mut(&id) {
                Some(entry) => {
                    if is_live(entry.fused.node.state) && !is_live(incoming.node.state) {
                        // One more read to catch the line that closed it.
                        entry.needs_read = true;
                    }
                    if is_live(incoming.node.state) {
                        // A resumed session can die again with a new status.
                        entry.error_checked = false;
                        entry.error_marked = false;
                    }
                    entry.misses = 0;
                    entry.fused = incoming;
                }
                None => {
                    self.known.insert(
                        id,
                        Entry {
                            fused: incoming,
                            path: None,
                            offset: Offset::TAIL,
                            ring: VecDeque::new(),
                            last_line: None,
                            misses: 0,
                            needs_read: true,
                            error_checked: false,
                            error_marked: false,
                        },
                    );
                }
            }
        }
        for (id, entry) in self.known.iter_mut() {
            if sighted.contains(id) || !is_live(entry.fused.node.state) {
                continue;
            }
            entry.misses += 1;
            if entry.misses >= MISS_LIMIT {
                entry.fused.node.state = NodeState::Lost;
                entry.fused.node.ended_at = Some(now);
                entry.fused.node.capabilities.stop = false;
                entry.needs_read = true;
            }
        }
        self.recompute(now)
    }

    pub fn expire(&mut self, now: Timestamp) -> Vec<FleetEvent> {
        self.recompute(now)
    }

    pub fn pins(&self) -> BTreeSet<NodeId> {
        self.pins.clone()
    }

    /// Records a pin without checking it exists. For a pin made while the hub was stopped, after
    /// the caller validated it against a scratch book.
    pub fn pin_raw(&mut self, root_id: &str, on: bool) {
        if on {
            self.pins.insert(root_id.to_owned());
        } else {
            self.pins.remove(root_id);
        }
    }

    pub fn set_pin(
        &mut self,
        root_id: &str,
        on: bool,
        now: Timestamp,
    ) -> Result<Vec<FleetEvent>, PinError> {
        let entry = self.known.get(root_id).ok_or(PinError::Unknown)?;
        if entry.fused.node.parent_id.is_some() {
            return Err(PinError::NotARoot);
        }
        self.pin_raw(root_id, on);
        Ok(self.recompute(now))
    }

    pub fn snapshot(&self) -> FleetEvent {
        let mut nodes: Vec<FleetNode> = self.view.values().cloned().collect();
        nodes.sort_by(|a, b| (&a.root_id, a.depth, &a.id).cmp(&(&b.root_id, b.depth, &b.id)));
        let metrics = nodes
            .iter()
            .filter_map(|n| self.metrics.get(&n.id).cloned())
            .collect();
        let mut lines = BTreeMap::new();
        for node in &nodes {
            let Some(entry) = self.known.get(&node.id) else {
                continue;
            };
            // A finished node carries only its last line; the rest is the transcript's.
            let shown: Vec<TermLine> = if is_live(node.state) {
                entry.ring.iter().cloned().collect()
            } else {
                entry.last_line.iter().cloned().collect()
            };
            if !shown.is_empty() {
                lines.insert(node.id.clone(), shown);
            }
        }
        FleetEvent::Snapshot { nodes, metrics, lines }
    }

    /// Merges a metrics change into the node's record and queues it when something moved.
    /// `last_activity_at` never goes backwards; a `None` field in `usage` never erases a value.
    fn note_metrics(&mut self, id: &str, last_activity: Timestamp, usage: Option<&Usage>) {
        let current = self.metrics.get(id).cloned();
        let mut next = current.clone().unwrap_or(NodeMetrics {
            node_id: id.to_owned(),
            last_activity_at: last_activity,
            context_tokens: None,
            cost_usd: None,
        });
        next.last_activity_at = next.last_activity_at.max(last_activity);
        if let Some(usage) = usage {
            if usage.context_tokens.is_some() {
                next.context_tokens = usage.context_tokens;
            }
            if usage.cost_usd.is_some() {
                next.cost_usd = usage.cost_usd;
            }
        }
        if current.as_ref() != Some(&next) {
            self.metrics.insert(id.to_owned(), next);
            self.pending_metrics.insert(id.to_owned());
        }
    }

    /// Drops expired trees, rebuilds the capped view, and returns the difference from what the
    /// clients were last told.
    fn recompute(&mut self, now: Timestamp) -> Vec<FleetEvent> {
        let retention = chrono::Duration::from_std(RETENTION).expect("retention fits");
        let expired: Vec<NodeId> = self
            .known
            .values()
            .map(|e| &e.fused.node)
            .filter(|n| n.parent_id.is_none() && !is_live(n.state) && !self.pins.contains(&n.id))
            .filter(|n| n.ended_at.is_none_or(|end| now - end >= retention))
            .map(|n| n.id.clone())
            .collect();
        if !expired.is_empty() {
            self.known
                .retain(|_, e| !expired.contains(&e.fused.node.root_id));
            let known = &self.known;
            self.metrics.retain(|id, _| known.contains_key(id));
            self.pending_metrics.retain(|id| known.contains_key(id));
            self.pending_lines.retain(|id, _| known.contains_key(id));
        }

        let new_view = self.build_view();

        let new_roots: BTreeSet<NodeId> = new_view
            .values()
            .filter(|n| n.parent_id.is_none())
            .map(|n| n.id.clone())
            .collect();
        let removed: Vec<NodeId> = self
            .view
            .values()
            .filter(|n| n.parent_id.is_none() && !new_roots.contains(&n.id))
            .map(|n| n.id.clone())
            .collect();
        let inner_vanished = self
            .view
            .values()
            .any(|old| !new_view.contains_key(&old.id) && new_roots.contains(&old.root_id));
        if inner_vanished {
            self.view = new_view;
            self.pending_lines.clear();
            self.pending_metrics.clear();
            return vec![self.snapshot()];
        }

        let mut changed: Vec<&FleetNode> = new_view
            .values()
            .filter(|n| self.view.get(&n.id) != Some(*n))
            .collect();
        changed.sort_by(|a, b| (a.depth, &a.id).cmp(&(b.depth, &b.id)));
        let newly_visible: Vec<NodeId> = changed
            .iter()
            .filter(|n| !self.view.contains_key(&n.id))
            .map(|n| n.id.clone())
            .collect();
        let mut events: Vec<FleetEvent> = removed
            .into_iter()
            .map(|node_id| FleetEvent::Removed { node_id })
            .collect();
        events.extend(changed.into_iter().map(|node| FleetEvent::Node { node: node.clone() }));
        self.view = new_view;

        // A node that just became visible (a new one, or one the caps let in) has metrics and
        // recent lines the clients have not seen.
        for id in newly_visible {
            if self.metrics.contains_key(&id) {
                self.pending_metrics.insert(id.clone());
            }
            if let Some(entry) = self.known.get(&id) {
                let shown: Vec<TermLine> = if is_live(entry.fused.node.state) {
                    entry.ring.iter().cloned().collect()
                } else {
                    entry.last_line.iter().cloned().collect()
                };
                if !shown.is_empty() {
                    self.pending_lines.insert(id, shown);
                }
            }
        }
        let view = &self.view;
        self.pending_lines.retain(|id, _| view.contains_key(id));
        self.pending_metrics.retain(|id| view.contains_key(id));
        events
    }

    fn build_view(&self) -> BTreeMap<NodeId, FleetNode> {
        let mut trees: BTreeMap<NodeId, Vec<&Entry>> = BTreeMap::new();
        for entry in self.known.values() {
            trees
                .entry(entry.fused.node.root_id.clone())
                .or_default()
                .push(entry);
        }
        let mut ranked: Vec<(bool, Timestamp, &NodeId)> = trees
            .iter()
            .filter(|(root, tree)| tree.iter().any(|e| &e.fused.node.id == *root))
            .filter_map(|(root, tree)| {
                tree.iter()
                    .map(|e| e.fused.last_activity_at)
                    .max()
                    .map(|at| (self.pins.contains(root), at, root))
            })
            .collect();
        // Pinned trees take the first places (a pin must survive 50 busier trees), then the most
        // recently active; the id breaks ties so the order is deterministic.
        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(b.2))
        });
        ranked.truncate(SNAPSHOT_TREES);

        let mut view = BTreeMap::new();
        for (_, _, root) in ranked {
            let tree = &trees[root];
            let (kept, omitted) = cap_tree(tree);
            for entry in tree {
                let mut node = entry.fused.node.clone();
                if !kept.contains(&node.id) {
                    continue;
                }
                node.pinned = self.pins.contains(&node.id);
                node.done_omitted = omitted.get(&node.id).copied().unwrap_or(0);
                view.insert(node.id.clone(), node);
            }
        }
        view
    }
}

/// Fills a node's `ended_at` (contract amendment 3) and clears it for a live node. The hub's own
/// observation wins over the sighting's: a node the book saw live gets `now` when it first sees it
/// dead. Already dead in the book: the first stamp stays. Met dead: the sighting's, else last activity.
fn settle_end(incoming: &mut FusedNode, previous: Option<&FleetNode>, now: Timestamp) {
    if is_live(incoming.node.state) {
        incoming.node.ended_at = None;
        return;
    }
    incoming.node.ended_at = Some(match previous {
        Some(prev) if is_live(prev.state) => now,
        Some(prev) => prev
            .ended_at
            .or(incoming.node.ended_at)
            .unwrap_or(incoming.last_activity_at),
        None => incoming.node.ended_at.unwrap_or(incoming.last_activity_at),
    });
}

fn end_of(entry: &Entry) -> Timestamp {
    entry.fused.node.ended_at.unwrap_or(entry.fused.last_activity_at)
}

/// The nodes of one tree that fit the cap, and how many `Done` nodes each kept node stands in for.
/// Live and `Lost` nodes and the root always fit; the newest `Done` fill what is left; a kept node
/// drags its ancestors in. So the result can exceed `TREE_NODES` by the ancestors of kept live
/// nodes (their `Done` ancestors are not counted against the budget): the cap bounds the `Done`
/// nodes, never the live ones, and the overshoot is bounded by those ancestors.
fn cap_tree(tree: &[&Entry]) -> (BTreeSet<NodeId>, BTreeMap<NodeId, u32>) {
    if tree.len() <= TREE_NODES {
        return (
            tree.iter().map(|e| e.fused.node.id.clone()).collect(),
            BTreeMap::new(),
        );
    }
    let by_id: BTreeMap<&str, &Entry> = tree
        .iter()
        .map(|e| (e.fused.node.id.as_str(), *e))
        .collect();
    let mut kept: BTreeSet<NodeId> = BTreeSet::new();
    let mut done: Vec<&Entry> = Vec::new();
    for entry in tree {
        let node = &entry.fused.node;
        if node.state == NodeState::Done && node.parent_id.is_some() {
            done.push(entry);
        } else {
            kept.insert(node.id.clone());
        }
    }
    done.sort_by(|a, b| {
        end_of(b)
            .cmp(&end_of(a))
            .then_with(|| a.fused.node.id.cmp(&b.fused.node.id))
    });
    let budget = TREE_NODES.saturating_sub(kept.len());
    kept.extend(done.iter().take(budget).map(|e| e.fused.node.id.clone()));
    let ancestors: Vec<NodeId> = kept
        .iter()
        .flat_map(|id| ancestors_of(id, &by_id))
        .collect();
    kept.extend(ancestors);

    let mut omitted: BTreeMap<NodeId, u32> = BTreeMap::new();
    for entry in tree {
        if kept.contains(&entry.fused.node.id) {
            continue;
        }
        let mut at = entry.fused.node.parent_id.as_deref();
        while let Some(parent) = at {
            if kept.contains(parent) {
                *omitted.entry(parent.to_owned()).or_default() += 1;
                break;
            }
            at = by_id.get(parent).and_then(|p| p.fused.node.parent_id.as_deref());
        }
    }
    (kept, omitted)
}

fn ancestors_of(id: &str, by_id: &BTreeMap<&str, &Entry>) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut at = by_id.get(id).and_then(|e| e.fused.node.parent_id.as_deref());
    while let Some(parent) = at {
        found.push(parent.to_owned());
        at = by_id.get(parent).and_then(|e| e.fused.node.parent_id.as_deref());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::{at, child_node, root_node};

    const R: (&str, &str) = ("claude-code", "root");

    fn nodes_of(events: &[FleetEvent]) -> Vec<&FleetNode> {
        events
            .iter()
            .filter_map(|e| match e {
                FleetEvent::Node { node } => Some(node),
                _ => None,
            })
            .collect()
    }

    fn removed_of(events: &[FleetEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                FleetEvent::Removed { node_id } => Some(node_id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn snapshot_nodes(book: &Book) -> Vec<String> {
        match book.snapshot() {
            FleetEvent::Snapshot { nodes, .. } => nodes.into_iter().map(|n| n.id).collect(),
            other => panic!("not a snapshot: {other:?}"),
        }
    }

    fn done_root(secs_ended: i64) -> FusedNode {
        let mut root = root_node(R, NodeState::Done);
        root.node.ended_at = Some(at(secs_ended));
        root.last_activity_at = at(secs_ended);
        root
    }

    #[test]
    fn new_nodes_emit_node_events_parents_first() {
        let mut book = Book::new(BTreeSet::new());
        let events = book.apply_fusion(
            vec![
                child_node(("claude-code", "kid"), R, NodeState::Busy),
                root_node(R, NodeState::Busy),
            ],
            at(1),
        );
        let ids: Vec<_> = nodes_of(&events).iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["claude-code:root", "claude-code:kid"]);
    }

    #[test]
    fn an_unchanged_sighting_emits_nothing_and_a_state_change_emits_one_node() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(1));
        assert!(book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(2)).is_empty());

        let events = book.apply_fusion(vec![root_node(R, NodeState::Idle)], at(3));
        assert_eq!(nodes_of(&events).len(), 1);
        assert_eq!(nodes_of(&events)[0].state, NodeState::Idle);
    }

    #[test]
    fn ended_at_is_the_sightings_else_the_moment_the_book_saw_the_end_else_last_activity() {
        // Saw it die: stamped with the time of the round that noticed.
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(10));
        book.apply_fusion(vec![root_node(R, NodeState::Done)], at(20));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(20)));
        // A later round must not move it.
        book.apply_fusion(vec![root_node(R, NodeState::Done)], at(30));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(20)));

        // Met already dead: its last activity.
        let mut book = Book::new(BTreeSet::new());
        let mut dead = root_node(R, NodeState::Lost);
        dead.last_activity_at = at(5);
        book.apply_fusion(vec![dead], at(100));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(5)));

        // Met already dead with an `ended_at` of its own: that one.
        let mut book = Book::new(BTreeSet::new());
        let mut sighted = root_node(R, NodeState::Done);
        sighted.node.ended_at = Some(at(7));
        book.apply_fusion(vec![sighted], at(100));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(7)));
    }

    #[test]
    fn the_books_observation_beats_the_ended_at_a_sighting_carries() {
        // Amendment 3: seen Busy at t=10, then a Done sighting that carries ended_at=15 at t=20:
        // the hub saw it live until it noticed, so the end is t=20.
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(10));
        let mut finished = root_node(R, NodeState::Done);
        finished.node.ended_at = Some(at(15));
        book.apply_fusion(vec![finished], at(20));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(20)));

        // Once dead, the first stamp stays, whatever a later sighting says.
        let mut again = root_node(R, NodeState::Done);
        again.node.ended_at = Some(at(25));
        book.apply_fusion(vec![again], at(30));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, Some(at(20)));
    }

    #[test]
    fn a_live_node_has_no_ended_at_even_if_a_sighting_carries_one() {
        let mut book = Book::new(BTreeSet::new());
        let mut node = root_node(R, NodeState::Busy);
        node.node.ended_at = Some(at(1));
        book.apply_fusion(vec![node], at(2));
        assert_eq!(book.lookup("claude-code:root").unwrap().node.ended_at, None);
    }

    #[test]
    fn a_finished_tree_is_dropped_ten_minutes_after_its_root_ended() {
        let mut book = Book::new(BTreeSet::new());
        let events = book.apply_fusion(vec![done_root(0)], at(1));
        assert_eq!(nodes_of(&events).len(), 1, "it is still inside the window");

        assert!(book.expire(at(599)).is_empty());
        let events = book.expire(at(600));
        assert_eq!(removed_of(&events), ["claude-code:root"]);
        assert!(snapshot_nodes(&book).is_empty());
        assert!(book.lookup("claude-code:root").is_none());
    }

    #[test]
    fn a_finished_tree_met_after_its_window_is_never_shown() {
        let mut book = Book::new(BTreeSet::new());
        let events = book.apply_fusion(vec![done_root(0)], at(10_000));
        assert!(events.is_empty());
    }

    #[test]
    fn a_live_root_never_expires() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Idle)], at(0));
        assert!(book.expire(at(86_400)).is_empty());
        assert_eq!(snapshot_nodes(&book), ["claude-code:root"]);
    }

    #[test]
    fn a_pin_holds_a_finished_tree_and_unpinning_releases_it() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![done_root(0)], at(1));
        let events = book.set_pin("claude-code:root", true, at(2)).unwrap();
        assert_eq!(nodes_of(&events).len(), 1);
        assert!(nodes_of(&events)[0].pinned);

        assert!(book.expire(at(10_000)).is_empty(), "the pin holds it");
        assert_eq!(snapshot_nodes(&book), ["claude-code:root"]);

        let events = book.set_pin("claude-code:root", false, at(10_001)).unwrap();
        assert_eq!(removed_of(&events), ["claude-code:root"], "past its window, it goes at once");
    }

    #[test]
    fn pins_are_validated() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(
            vec![root_node(R, NodeState::Busy), child_node(("claude-code", "kid"), R, NodeState::Busy)],
            at(1),
        );
        assert_eq!(book.set_pin("claude-code:ghost", true, at(2)), Err(PinError::Unknown));
        assert_eq!(book.set_pin("claude-code:kid", true, at(2)), Err(PinError::NotARoot));
    }

    #[test]
    fn a_pin_survives_a_new_book_seeded_with_it() {
        let mut pins = BTreeSet::new();
        pins.insert("claude-code:root".to_owned());
        let mut book = Book::new(pins);
        let events = book.apply_fusion(vec![done_root(0)], at(10_000));
        assert!(nodes_of(&events)[0].pinned, "a pinned old tree is shown and flagged");
    }

    #[test]
    fn a_live_node_that_goes_unsighted_becomes_lost_on_the_second_miss() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(
            vec![root_node(R, NodeState::Busy), child_node(("claude-code", "kid"), R, NodeState::Busy)],
            at(1),
        );
        // The child is missing from this round only.
        assert!(book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(2)).is_empty());
        let events = book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(3));
        let kid = nodes_of(&events)[0].clone();
        assert_eq!(kid.state, NodeState::Lost);
        assert_eq!(kid.ended_at, Some(at(3)));
        assert!(!kid.capabilities.stop);

        // Sighted again in time: the miss counter restarts.
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Busy), child_node(("claude-code", "kid"), R, NodeState::Busy)], at(1));
        book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(2));
        book.apply_fusion(vec![root_node(R, NodeState::Busy), child_node(("claude-code", "kid"), R, NodeState::Busy)], at(3));
        assert!(book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(4)).is_empty());
    }

    #[test]
    fn a_finished_node_that_goes_unsighted_is_kept_until_retention() {
        let mut book = Book::new(BTreeSet::new());
        let mut kid = child_node(("claude-code", "kid"), R, NodeState::Done);
        kid.node.ended_at = Some(at(1));
        book.apply_fusion(vec![root_node(R, NodeState::Busy), kid], at(2));
        for round in 3..10 {
            book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(round));
        }
        assert!(snapshot_nodes(&book).contains(&"claude-code:kid".to_owned()));
    }

    #[test]
    fn the_view_keeps_the_fifty_most_recently_active_trees() {
        let mut book = Book::new(BTreeSet::new());
        let trees: Vec<FusedNode> = (0..60)
            .map(|i| {
                let mut root = root_node(("claude-code", &format!("t{i:02}")), NodeState::Busy);
                root.last_activity_at = at(i);
                root
            })
            .collect();
        let events = book.apply_fusion(trees, at(100));
        assert_eq!(nodes_of(&events).len(), 50);
        let shown = snapshot_nodes(&book);
        assert_eq!(shown.len(), 50);
        assert!(shown.contains(&"claude-code:t59".to_owned()));
        assert!(!shown.contains(&"claude-code:t09".to_owned()), "the ten stalest are left out");

        // A fresher tree displaces the stalest visible one: it is announced, the other removed.
        let mut fresh = root_node(("claude-code", "fresh"), NodeState::Busy);
        fresh.last_activity_at = at(500);
        let events = book.apply_fusion(vec![fresh], at(501));
        assert_eq!(removed_of(&events), ["claude-code:t10"]);
        assert!(nodes_of(&events).iter().any(|n| n.id == "claude-code:fresh"));
    }

    #[test]
    fn pinned_trees_take_the_first_places_of_the_fifty() {
        // t00 is the stalest of sixty live trees, but pinned: it stays and the 50th freshest
        // (t10) is the one left out.
        let mut pins = BTreeSet::new();
        pins.insert("claude-code:t00".to_owned());
        let mut book = Book::new(pins);
        let trees: Vec<FusedNode> = (0..60)
            .map(|i| {
                let mut root = root_node(("claude-code", &format!("t{i:02}")), NodeState::Busy);
                root.last_activity_at = at(i);
                root
            })
            .collect();
        book.apply_fusion(trees, at(100));

        let shown = snapshot_nodes(&book);
        assert_eq!(shown.len(), 50);
        assert!(shown.contains(&"claude-code:t00".to_owned()), "the pinned tree is shown");
        assert!(!shown.contains(&"claude-code:t10".to_owned()), "the pin took one place");
        assert!(shown.contains(&"claude-code:t11".to_owned()));
    }

    /// A root with `done` finished children (`d000`…) whose end times are 0, 1, 2… and `live` busy ones.
    fn big_tree(done: usize, live: usize) -> Vec<FusedNode> {
        let mut nodes = vec![root_node(R, NodeState::Busy)];
        for i in 0..done {
            let mut kid = child_node(("claude-code", &format!("d{i:03}")), R, NodeState::Done);
            kid.node.ended_at = Some(at(i as i64));
            kid.last_activity_at = at(i as i64);
            nodes.push(kid);
        }
        for i in 0..live {
            nodes.push(child_node(("claude-code", &format!("live{i}")), R, NodeState::Busy));
        }
        nodes
    }

    #[test]
    fn a_tree_is_cut_to_five_hundred_nodes_live_first_oldest_done_out_and_the_count_on_the_parent() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(big_tree(600, 3), at(1000));

        let FleetEvent::Snapshot { nodes, .. } = book.snapshot() else { panic!() };
        assert_eq!(nodes.len(), TREE_NODES);
        assert_eq!(nodes.iter().filter(|n| n.id.contains("live")).count(), 3);
        let root = nodes.iter().find(|n| n.id == "claude-code:root").unwrap();
        assert_eq!(root.done_omitted, 600 - (TREE_NODES as u32 - 4));
        assert!(nodes.iter().any(|n| n.id == "claude-code:d599"), "the newest done is kept");
        assert!(!nodes.iter().any(|n| n.id == "claude-code:d000"), "the oldest done is not");
    }

    #[test]
    fn the_ancestors_of_a_kept_node_are_kept_and_the_omitted_count_goes_to_the_nearest_kept_one() {
        let mut book = Book::new(BTreeSet::new());
        let mut nodes = big_tree(520, 0);
        // The oldest done node has a live child: it must stay, or the child would be orphaned.
        let mut old_parent = nodes.remove(1); // d000
        old_parent.node.state = NodeState::Done;
        nodes.push(old_parent);
        nodes.push(crate::fleet::testkit::fused(
            ("claude-code", "grandchild"),
            Some(("claude-code", "d000")),
            R,
            2,
            NodeState::Busy,
        ));
        book.apply_fusion(nodes, at(1000));

        let FleetEvent::Snapshot { nodes, .. } = book.snapshot() else { panic!() };
        assert!(nodes.iter().any(|n| n.id == "claude-code:d000"), "kept as the grandchild's parent");
        assert!(nodes.iter().any(|n| n.id == "claude-code:grandchild"));
        let total_omitted: u32 = nodes.iter().map(|n| n.done_omitted).sum();
        assert_eq!(nodes.len() as u32 + total_omitted, 522, "every node is either shown or counted");
    }

    #[test]
    fn a_non_root_leaving_the_view_answers_with_a_fresh_snapshot() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(big_tree(499, 0), at(1000)); // root + 499 done = 500, all shown
        assert_eq!(snapshot_nodes(&book).len(), 500);

        // One newer done node arrives: the oldest (d000) must leave.
        let mut newer = child_node(("claude-code", "newer"), R, NodeState::Done);
        newer.node.ended_at = Some(at(900));
        newer.last_activity_at = at(900);
        let events = book.apply_fusion(vec![newer], at(1001));

        assert_eq!(events.len(), 1);
        let FleetEvent::Snapshot { nodes, .. } = &events[0] else { panic!("{events:?}") };
        assert_eq!(nodes.len(), 500);
        assert!(!nodes.iter().any(|n| n.id == "claude-code:d000"));
    }

    fn outcome(lines: &[&str], offset: u64) -> ReadOutcome {
        ReadOutcome {
            lines: lines
                .iter()
                .map(|t| TermLine::new(crate::fleet::model::TermKind::Text, t))
                .collect(),
            offset: Offset(offset),
            usage: None,
            last_activity_at: None,
        }
    }

    fn lines_of(events: &[FleetEvent]) -> Vec<(&str, Vec<&str>)> {
        events
            .iter()
            .filter_map(|e| match e {
                FleetEvent::Lines { node_id, lines } => Some((
                    node_id.as_str(),
                    lines.iter().map(|l| l.text.as_str()).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    fn live_book() -> Book {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(1));
        book.flush_lines();
        book.flush_metrics();
        book
    }

    #[test]
    fn new_lines_emit_lines_and_metrics_and_never_a_node() {
        let mut book = live_book();
        book.apply_lines("claude-code:root", outcome(&["a", "b"], 10), at(5));

        assert!(book.apply_fusion(vec![root_node(R, NodeState::Busy)], at(6)).is_empty(), "no node event");
        let lines = book.flush_lines();
        assert_eq!(lines_of(&lines), [("claude-code:root", vec!["a", "b"])]);
        let metrics = book.flush_metrics();
        assert!(matches!(&metrics[..], [FleetEvent::Metrics { metrics }] if metrics[0].last_activity_at == at(5)));
        assert!(book.flush_lines().is_empty() && book.flush_metrics().is_empty(), "drained");
    }

    #[test]
    fn several_reads_inside_a_batch_become_one_ordered_lines_event() {
        let mut book = live_book();
        book.apply_lines("claude-code:root", outcome(&["a"], 2), at(5));
        book.apply_lines("claude-code:root", outcome(&["b", "c"], 6), at(5));
        assert_eq!(lines_of(&book.flush_lines()), [("claude-code:root", vec!["a", "b", "c"])]);
    }

    #[test]
    fn a_big_first_read_emits_only_the_last_twelve_and_the_ring_keeps_twelve() {
        let mut book = live_book();
        let many: Vec<String> = (0..300).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        book.apply_lines("claude-code:root", outcome(&refs, 99), at(5));

        let flushed = book.flush_lines();
        let emitted = lines_of(&flushed);
        assert_eq!(emitted[0].1.len(), RING_LINES);
        assert_eq!(emitted[0].1.last(), Some(&"l299"));
        let FleetEvent::Snapshot { lines, .. } = book.snapshot() else { panic!() };
        assert_eq!(lines["claude-code:root"].len(), RING_LINES);
        assert_eq!(lines["claude-code:root"][0].text, "l288");
    }

    #[test]
    fn a_finished_node_carries_only_its_last_line_in_snapshots_and_events() {
        let mut book = Book::new(BTreeSet::new());
        let mut kid = child_node(("claude-code", "kid"), R, NodeState::Busy);
        kid.last_activity_at = at(1);
        book.apply_fusion(vec![root_node(R, NodeState::Busy), kid], at(1));
        book.apply_lines("claude-code:kid", outcome(&["one", "two", "three"], 14), at(2));
        book.flush_lines();

        let mut done = child_node(("claude-code", "kid"), R, NodeState::Done);
        done.last_activity_at = at(3);
        book.apply_fusion(vec![root_node(R, NodeState::Busy), done], at(3));
        book.apply_lines("claude-code:kid", outcome(&["closing"], 22), at(3));

        assert_eq!(lines_of(&book.flush_lines()), [("claude-code:kid", vec!["closing"])]);
        let FleetEvent::Snapshot { lines, .. } = book.snapshot() else { panic!() };
        assert_eq!(lines["claude-code:kid"].len(), 1);
        assert_eq!(lines["claude-code:kid"][0].text, "closing");
    }

    #[test]
    fn lines_for_a_node_the_book_does_not_know_are_ignored() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![done_root(0)], at(10_000)); // beyond its window: never kept
        book.apply_lines("claude-code:root", outcome(&["x"], 2), at(10_001));
        assert!(book.flush_lines().is_empty());
    }

    #[test]
    fn metrics_merge_and_unchanged_metrics_are_silent() {
        let mut book = live_book();
        let mut read = outcome(&["a"], 2);
        read.usage = Some(Usage { context_tokens: Some(100), cost_usd: None });
        book.apply_lines("claude-code:root", read, at(5));
        let mut read = outcome(&[], 2);
        read.usage = Some(Usage { context_tokens: None, cost_usd: Some(0.25) });
        book.apply_lines("claude-code:root", read, at(6));

        let FleetEvent::Metrics { metrics } = book.flush_metrics().remove(0) else { panic!() };
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].context_tokens, Some(100), "a missing field never erases a value");
        assert_eq!(metrics[0].cost_usd, Some(0.25));
        assert_eq!(metrics[0].last_activity_at, at(6));

        // The same numbers again: nothing to say.
        let mut read = outcome(&[], 2);
        read.usage = Some(Usage { context_tokens: Some(100), cost_usd: Some(0.25) });
        read.last_activity_at = Some(at(6));
        book.apply_lines("claude-code:root", read, at(7));
        assert!(book.flush_metrics().is_empty());
    }

    #[test]
    fn a_discovery_that_only_moves_activity_is_a_metrics_event_not_a_node_event() {
        let mut book = live_book();
        let mut later = root_node(R, NodeState::Busy);
        later.last_activity_at = at(50);
        assert!(book.apply_fusion(vec![later], at(51)).is_empty());
        assert_eq!(book.flush_metrics().len(), 1);
    }

    #[test]
    fn a_node_that_becomes_visible_late_gets_its_recent_lines_and_metrics_queued() {
        // 51 trees: the stalest is hidden. When a fresher tree displaces another, the hidden one
        // is not shown; but a tree that WAS hidden and rises into the view is announced with its ring.
        let mut book = Book::new(BTreeSet::new());
        let trees: Vec<FusedNode> = (0..51)
            .map(|i| {
                let mut root = root_node(("claude-code", &format!("t{i:02}")), NodeState::Busy);
                root.last_activity_at = at(i);
                root
            })
            .collect();
        book.apply_fusion(trees, at(100));
        book.apply_lines("claude-code:t00", outcome(&["hidden"], 7), at(101)); // t00 is not visible
        book.flush_lines();
        book.flush_metrics();

        let mut risen = root_node(("claude-code", "t00"), NodeState::Busy);
        risen.last_activity_at = at(500);
        let events = book.apply_fusion(vec![risen], at(501));
        assert!(nodes_of(&events).iter().any(|n| n.id == "claude-code:t00"));
        assert_eq!(lines_of(&book.flush_lines()), [("claude-code:t00", vec!["hidden"])]);
    }

    #[test]
    fn a_new_node_is_read_from_the_tail_once_and_then_only_when_asked() {
        let mut book = live_book();
        let jobs = book.read_jobs(ReadWhich::Pending);
        assert_eq!(
            jobs,
            vec![ReadJob {
                id: "claude-code:root".into(),
                key: NodeKey::new("claude-code", "root"),
                source: 0,
                offset: Offset::TAIL,
            }]
        );
        book.apply_lines("claude-code:root", outcome(&["a"], 42), at(5));
        assert!(book.read_jobs(ReadWhich::Pending).is_empty());

        let again = book.read_jobs(ReadWhich::AllLive);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].offset, Offset(42), "reads continue from the stored offset");
    }

    #[test]
    fn a_node_that_stops_being_alive_is_read_one_more_time_and_then_never_by_all_live() {
        let mut book = live_book();
        book.read_jobs(ReadWhich::Pending);
        book.apply_lines("claude-code:root", outcome(&["a"], 2), at(2));

        book.apply_fusion(vec![root_node(R, NodeState::Done)], at(3));
        assert_eq!(book.read_jobs(ReadWhich::Pending).len(), 1, "the closing read");
        assert!(book.read_jobs(ReadWhich::AllLive).is_empty());
    }

    #[test]
    fn reads_can_be_scoped_to_sources_or_ids_and_skip_sourceless_nodes() {
        let mut book = Book::new(BTreeSet::new());
        let mut other = child_node(("claude-code", "other"), R, NodeState::Busy);
        other.lines_source = Some(1);
        let mut blind = child_node(("claude-code", "blind"), R, NodeState::Busy);
        blind.lines_source = None;
        book.apply_fusion(vec![root_node(R, NodeState::Busy), other, blind], at(1));
        book.read_jobs(ReadWhich::Pending); // settle the first reads

        let only_one: BTreeSet<usize> = [1].into_iter().collect();
        let jobs = book.read_jobs(ReadWhich::LiveOf(&only_one));
        assert_eq!(jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), ["claude-code:other"]);

        let ids: BTreeSet<NodeId> = ["claude-code:root".to_owned()].into_iter().collect();
        let jobs = book.read_jobs(ReadWhich::Ids(&ids));
        assert_eq!(jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), ["claude-code:root"]);
    }

    #[test]
    fn watcher_paths_are_matched_to_nodes_and_unknown_ones_are_flagged() {
        let mut book = live_book();
        book.set_path("claude-code:root", Some(PathBuf::from("/s/a.jsonl")));

        let known: HashSet<PathBuf> = [PathBuf::from("/s/a.jsonl")].into_iter().collect();
        let (ids, unknown) = book.match_paths(&known);
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), ["claude-code:root"]);
        assert!(!unknown);

        let stray: HashSet<PathBuf> = [PathBuf::from("/s/new.jsonl")].into_iter().collect();
        let (ids, unknown) = book.match_paths(&stray);
        assert!(ids.is_empty() && unknown);

        let overflow: HashSet<PathBuf> = [PathBuf::new()].into_iter().collect();
        assert!(book.match_paths(&overflow).1, "an empty path means events were lost");
    }

    /// A node whose lines come from a native adapter (registry index 1) and that daemon_runs
    /// (index 0) also sighted: the shape of a Claude Code session the daemon launched.
    fn native_with_run(state: NodeState) -> FusedNode {
        let mut node = root_node(R, state);
        node.lines_source = Some(1);
        node.daemon_source = Some(0);
        node
    }

    #[test]
    fn a_finished_run_gets_its_error_line_once_after_the_native_closing_line() {
        // Contract amendment 1 / spec §11: failed, cancelled and timed-out runs end `Done` + Error.
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![native_with_run(NodeState::Busy)], at(1));
        book.read_jobs(ReadWhich::Pending);
        book.apply_lines("claude-code:root", outcome(&["working"], 8), at(2));
        book.flush_lines();
        assert!(book.error_jobs().is_empty(), "a live node has no run status to report");

        book.apply_fusion(vec![native_with_run(NodeState::Done)], at(3));
        assert!(book.error_jobs().is_empty(), "the native closing read comes first");
        book.read_jobs(ReadWhich::Pending);
        book.apply_lines("claude-code:root", outcome(&["all done"], 20), at(3));

        let jobs = book.error_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].id.as_str(), jobs[0].source, jobs[0].offset), ("claude-code:root", 0, Offset::TAIL));
        assert!(book.error_jobs().is_empty(), "asked once");

        // The daemon source answered with its whole last line set; only the Error one is kept.
        let status = vec![
            TermLine::new(crate::fleet::model::TermKind::Final, "ignored"),
            TermLine::new(crate::fleet::model::TermKind::Error, "timed_out"),
        ];
        book.apply_error("claude-code:root", status);
        book.flush_lines();
        let FleetEvent::Snapshot { lines, .. } = book.snapshot() else { panic!() };
        assert_eq!(lines["claude-code:root"].len(), 1);
        assert_eq!(lines["claude-code:root"][0].text, "timed_out");
        assert_eq!(lines["claude-code:root"][0].kind, crate::fleet::model::TermKind::Error);

        // A later native read of the dead node does not push the Error line out.
        book.apply_lines("claude-code:root", outcome(&["late"], 30), at(4));
        let FleetEvent::Snapshot { lines, .. } = book.snapshot() else { panic!() };
        assert_eq!(lines["claude-code:root"][0].text, "timed_out");
    }

    #[test]
    fn the_error_line_is_queued_in_the_next_lines_event() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![native_with_run(NodeState::Done)], at(1));
        book.read_jobs(ReadWhich::Pending);
        book.apply_lines("claude-code:root", outcome(&["bye"], 4), at(1));
        book.flush_lines();
        book.error_jobs();

        book.apply_error(
            "claude-code:root",
            vec![TermLine::new(crate::fleet::model::TermKind::Error, "cancelled")],
        );
        assert_eq!(lines_of(&book.flush_lines()), [("claude-code:root", vec!["cancelled"])]);
    }

    #[test]
    fn a_success_a_run_only_node_and_a_live_node_need_no_error_pass() {
        // Success: daemon_runs answers with `Final`, which is not kept, so nothing is queued.
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![native_with_run(NodeState::Done)], at(1));
        book.read_jobs(ReadWhich::Pending);
        book.flush_lines();
        assert_eq!(book.error_jobs().len(), 1);
        book.apply_error(
            "claude-code:root",
            vec![TermLine::new(crate::fleet::model::TermKind::Final, "All green")],
        );
        assert!(book.flush_lines().is_empty());

        // Run-only: its lines already come from daemon_runs (`Final` / `Error` via read_lines).
        let mut book = Book::new(BTreeSet::new());
        let mut only = root_node(R, NodeState::Done);
        only.lines_source = Some(0);
        only.daemon_source = Some(0);
        book.apply_fusion(vec![only], at(1));
        book.read_jobs(ReadWhich::Pending);
        assert!(book.error_jobs().is_empty());

        // A native node no run launched.
        let mut book = Book::new(BTreeSet::new());
        let mut native = root_node(R, NodeState::Done);
        native.lines_source = Some(1);
        native.daemon_source = None;
        book.apply_fusion(vec![native], at(1));
        book.read_jobs(ReadWhich::Pending);
        assert!(book.error_jobs().is_empty());
    }

    #[test]
    fn a_node_that_comes_back_to_life_can_report_a_new_error_later() {
        let mut book = Book::new(BTreeSet::new());
        book.apply_fusion(vec![native_with_run(NodeState::Done)], at(1));
        book.read_jobs(ReadWhich::Pending);
        assert_eq!(book.error_jobs().len(), 1);

        book.apply_fusion(vec![native_with_run(NodeState::Busy)], at(2));
        book.apply_fusion(vec![native_with_run(NodeState::Done)], at(3));
        book.read_jobs(ReadWhich::Pending);
        assert_eq!(book.error_jobs().len(), 1, "the second death asks again");
    }
}
