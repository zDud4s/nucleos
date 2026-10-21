//! Every number of the live fleet tree, in one place (spec 2026-10-20 §5). Nothing else in
//! `fleet/` writes a literal for any of these. The shell's own constants (30 s of client silence,
//! the F9 group threshold of 6, the 60% card zoom) live in `shell/src/fleet/`.

use std::time::Duration;

/// Filesystem events are gathered in windows of this size before a node is read.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(100);
/// Metric changes reach the shell in batches of this size; a metrics event never forces layout.
pub const METRICS_BATCH: Duration = Duration::from_secs(1);
/// New terminal lines reach the shell in batches of this size.
pub const LINES_BATCH: Duration = Duration::from_millis(100);
/// The safety net: a new `discover()` and a read of every live node, for the events the watcher
/// lost (its buffer overflows on Windows).
pub const SAFETY_NET: Duration = Duration::from_secs(5);
/// A `heartbeat` event keeps an idle connection visibly alive.
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// A Codex rollout with no `task_complete` and an mtime newer than this counts as alive.
pub const CODEX_LIVE_MTIME: Duration = Duration::from_secs(2 * 60);
/// A finished tree stays visible this long after its root's `ended_at` (a pin holds it longer).
pub const RETENTION: Duration = Duration::from_secs(10 * 60);
/// At most this many trees in a snapshot, the most recently active first.
pub const SNAPSHOT_TREES: usize = 50;
/// At most this many nodes per tree; live nodes always fit, the oldest `Done` are left out.
pub const TREE_NODES: usize = 500;
/// Terminal lines kept per live node for snapshots.
pub const RING_LINES: usize = 12;
/// A `TermLine` text is cut to this many characters.
pub const LINE_CHARS: usize = 200;
/// A first read of a transcript starts this far from its end.
pub const TAIL_BYTES: u64 = 64 * 1024;
/// One backwards page of a full transcript reads at most this many bytes.
pub const TRANSCRIPT_PAGE_BYTES: u64 = 256 * 1024;
/// Capacity of each client's channel; a client that fills it is sent a fresh snapshot.
pub const CLIENT_CHANNEL: usize = 256;
