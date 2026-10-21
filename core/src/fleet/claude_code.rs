//! Claude Code as a fleet source: sessions registry + transcripts + subagents + daemon-run roots.
//! Spec `.ai/specs/2026-10-20-fleet-arvore-viva-design.md` §3-§4.

mod record;
mod session;
#[cfg(test)]
mod fixture;

// The FINAL production `use` block of this file (later tasks add none). `Cursor`, `TermLine` and
// `AgentMeta` are first used in Tasks 13-16 and warn as unused until then.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::fleet::FleetEnv;
use crate::fleet::limits;
use crate::fleet::model::{clean_label, Cursor, NodeKey, NodeState, TermLine, Timestamp, TranscriptPage};
use crate::fleet::source::{AgentSource, LinesRead, NodeRef, Offset, Sighting, SourceError, SourceResult, Usage};
use crate::fleet::tail;
use record::TurnMark;
use session::{AgentMeta, ProcessProbe, SessionFile};

const HARNESS: &str = "claude-code";

/// Chars of a session name or subagent description kept as the node label (contract amendment 18).
const LABEL_CHARS: usize = 80;

/// `(mtime, len)` of a file: changes whenever Claude Code rewrites or appends to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    mtime: SystemTime,
    len: u64,
}

impl Stamp {
    fn of(md: &std::fs::Metadata) -> Self {
        Self {
            mtime: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            len: md.len(),
        }
    }
}

fn utc(t: SystemTime) -> Timestamp {
    DateTime::<Utc>::from(t)
}

/// Ids become path components; accept only what Claude Code itself generates (uuids, hex).
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Counters the tests read to prove discovery is incremental (spec §4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
struct Stats {
    session_parses: u32,
    meta_parses: u32,
    subagent_listings: u32,
    tail_reads: u32,
    /// Fallback searches of every project directory for a transcript (Task 10, negative cache).
    project_scans: u32,
}

/// A daemon run that names this session (`runs.session_id`).
#[derive(Clone, Debug)]
struct RunRow {
    id: i64,
    status: String,
    cwd: Option<String>,
    created_at: Option<Timestamp>,
    completed_at: Option<Timestamp>,
}

/// What the last 64 KB of a transcript say.
#[derive(Clone, Debug, Default)]
struct TailFacts {
    model: Option<String>,
    usage: Option<Usage>,
    /// `Some(true)`: last conversation record closed the turn. `None`: none found in the window.
    closed: Option<bool>,
}

struct ChildCache {
    meta: AgentMeta,
    meta_path: PathBuf,
    meta_stamp: Stamp,
    transcript: PathBuf,
    started_at: Timestamp,
    last_activity: Timestamp,
}

/// What a registry entry said about its root. Kept after Claude Code deletes the entry (it does
/// at a clean exit), so a root that stays known as `Done` keeps its label, project and role.
#[derive(Clone, Debug)]
struct Identity {
    name: Option<String>,
    cwd: Option<String>,
    kind: Option<String>,
}

impl Identity {
    fn of(file: &SessionFile) -> Self {
        Self {
            name: file.name.clone(),
            cwd: file.cwd.clone(),
            kind: file.kind.clone(),
        }
    }
}

/// Everything remembered about one root session; survives its registry entry (Claude Code
/// deletes `sessions/<pid>.json` at exit) until it leaves the retention window.
#[derive(Default)]
struct RootCache {
    /// This pass's registry entry and whether its process is the one that wrote it.
    registry: Option<(SessionFile, bool)>,
    /// The stamp of the file `registry` came from (to remember an evicted stale one, see `State`).
    registry_stamp: Option<Stamp>,
    /// Filled whenever a registry entry attaches; the fallback once the entry is gone.
    last_identity: Option<Identity>,
    run: Option<RunRow>,
    transcript: Option<PathBuf>,
    /// `mtime` of `projects/` when the fallback search for the transcript last found nothing;
    /// the search is repeated only when that mtime moves (a new project directory appeared).
    transcript_miss: Option<SystemTime>,
    transcript_stamp: Option<Stamp>,
    tail: TailFacts,
    started_at: Option<Timestamp>,
    /// Transcript mtime.
    last_activity: Option<Timestamp>,
    subagents_mtime: Option<SystemTime>,
    children: HashMap<String, ChildCache>,
}

impl RootCache {
    /// The working directory: the registry entry's (this pass's, else the last one seen), else the
    /// daemon run's.
    fn cwd(&self) -> Option<String> {
        self.last_identity
            .as_ref()
            .and_then(|i| i.cwd.clone())
            .or_else(|| self.run.as_ref().and_then(|r| r.cwd.clone()))
    }
}

#[derive(Default)]
struct State {
    /// One entry per `sessions/*.json` seen: its stamp and what it parsed to (`None` = unreadable).
    sessions: HashMap<PathBuf, (Stamp, Option<SessionFile>)>,
    roots: HashMap<String, RootCache>,
    /// Sessions whose DEAD registry file was evicted as outside the retention window, with the
    /// file's stamp. Without this, a stale `<pid>.json` left on disk would rebuild its root (a
    /// transcript lookup and a tail read) on every 5 s pass; with it, nothing is touched until the
    /// file changes or goes away.
    stale: HashMap<String, Stamp>,
    stats: Stats,
}

/// What `watch_paths` and `node_ref` read. Published at the end of every scan, so those two
/// synchronous methods (called from async code) never wait for the scan mutex, which a scan holds
/// across file IO.
#[derive(Default)]
struct Snapshot {
    /// Directories to watch beyond the fixed `sessions/` and `projects/`: each known root's
    /// project directory and each existing `<session>/subagents/` directory. Sorted, deduplicated.
    watch: Vec<PathBuf>,
    /// Native id (session id or agent id) -> transcript path.
    nodes: HashMap<String, PathBuf>,
}

struct Inner {
    home: PathBuf,
    probe: Arc<dyn ProcessProbe>,
    state: Mutex<State>,
    published: RwLock<Arc<Snapshot>>,
    warned: Mutex<HashSet<String>>,
}

pub struct ClaudeCode {
    pool: SqlitePool,
    inner: Arc<Inner>,
}

impl ClaudeCode {
    pub fn new(env: &FleetEnv) -> Self {
        Self::with_probe(env.pool.clone(), env.home.clone(), Arc::new(session::OsProbe))
    }

    pub(crate) fn with_probe(pool: SqlitePool, home: PathBuf, probe: Arc<dyn ProcessProbe>) -> Self {
        Self {
            pool,
            inner: Arc::new(Inner {
                home,
                probe,
                state: Mutex::new(State::default()),
                published: RwLock::new(Arc::default()),
                warned: Mutex::new(HashSet::new()),
            }),
        }
    }

    #[cfg(test)]
    fn stats(&self) -> Stats {
        self.inner.state.lock().unwrap().stats
    }

    #[cfg(test)]
    fn warnings(&self) -> usize {
        self.inner.warned.lock().unwrap().len()
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One log line per cause for the life of the process (spec §4 "Tolerância").
    fn warn_once(&self, cause: &str) {
        let mut seen = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        if seen.insert(cause.to_owned()) {
            tracing::warn!("claude_code: {cause}");
        }
    }

    fn claude_dir(&self) -> PathBuf {
        self.home.join(".claude")
    }

    /// Lists `sessions/` (tiny: one file per live process) and re-parses only the files whose
    /// stamp changed. Claude Code rewrites these in place, so the directory mtime is no signal.
    fn refresh_registry(&self, st: &mut State) {
        let dir = self.claude_dir().join("sessions");
        let mut present = HashSet::new();
        if let Ok(read) = std::fs::read_dir(&dir) {
            for item in read.flatten() {
                let path = item.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(md) = item.metadata() else { continue };
                let stamp = Stamp::of(&md);
                present.insert(path.clone());
                if st.sessions.get(&path).map(|(s, _)| *s) == Some(stamp) {
                    continue;
                }
                st.stats.session_parses += 1;
                let parsed = std::fs::read_to_string(&path)
                    .map_err(|_| "session file is unreadable")
                    .and_then(|text| session::parse_session(&text));
                let file = match parsed {
                    Ok(file) => {
                        tracing::debug!(version = ?file.version, "claude_code: session file read");
                        Some(file)
                    }
                    Err(cause) => {
                        self.warn_once(cause);
                        None
                    }
                };
                st.sessions.insert(path, (stamp, file));
            }
        }
        st.sessions.retain(|path, _| present.contains(path));
    }

    /// `projects/<cwd with every non-alphanumeric as '-'>/<session>.jsonl`, else any project dir.
    ///
    /// The expected path costs one stat and is checked on every call. The fallback (one stat per
    /// project directory) runs only when the `projects/` directory mtime differs from `*miss`, the
    /// mtime at which the previous fallback found nothing: a transcript that lives in a project
    /// directory that does not follow the cwd is looked for again only once a new project
    /// directory appears. (A transcript created inside an ALREADY existing foreign project
    /// directory is not noticed until then; accepted, the cwd-derived path is the normal case.)
    fn find_transcript(
        &self,
        session_id: &str,
        cwd: Option<&str>,
        miss: &mut Option<SystemTime>,
        stats: &mut Stats,
    ) -> Option<PathBuf> {
        let projects = self.claude_dir().join("projects");
        let file = format!("{session_id}.jsonl");
        if let Some(cwd) = cwd {
            let encoded: String = cwd
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let path = projects.join(encoded).join(&file);
            if path.is_file() {
                *miss = None;
                return Some(path);
            }
        }
        let mtime = std::fs::metadata(&projects).and_then(|md| md.modified()).ok()?;
        if *miss == Some(mtime) {
            return None;
        }
        stats.project_scans += 1;
        let found = std::fs::read_dir(&projects)
            .ok()?
            .flatten()
            .map(|item| item.path().join(&file))
            .find(|path| path.is_file());
        *miss = if found.is_none() { Some(mtime) } else { None };
        found
    }
}

/// Model, newest usage and whether the turn is closed, from the last complete lines. A single
/// line can exceed 64 KB (attachments reach 140 KB), so the window grows until a conversation
/// record is found.
fn tail_facts(path: &Path) -> TailFacts {
    let page = limits::TRANSCRIPT_PAGE_BYTES;
    let mut facts = TailFacts::default();
    for window in [limits::TAIL_BYTES, page, 4 * page] {
        let Ok((lines, _)) = tail::read_lines_before(path, None, window) else {
            break;
        };
        facts = TailFacts::default();
        for line in lines.iter().rev() {
            let Some(parsed) = record::parse_line(line) else { continue };
            if facts.closed.is_none() {
                facts.closed = parsed.mark.map(|m| m == TurnMark::Closed);
            }
            if facts.model.is_none() {
                facts.model = parsed.model;
            }
            if let Some(u) = parsed.usage {
                let have = facts
                    .usage
                    .get_or_insert(Usage { context_tokens: None, cost_usd: None });
                if have.context_tokens.is_none() {
                    have.context_tokens = u.context_tokens;
                }
                if have.cost_usd.is_none() {
                    have.cost_usd = u.cost_usd;
                }
            }
        }
        if facts.closed.is_some() {
            break;
        }
    }
    facts
}

/// The first record's timestamp, else the file's creation/modification time.
fn started_at_of(path: &Path) -> Option<Timestamp> {
    use std::io::{BufRead, BufReader, Read};
    let first = std::fs::File::open(path).ok().and_then(|file| {
        let mut line = String::new();
        // Bounded: a first record can be a 140 KB attachment.
        BufReader::new(file.take(256 * 1024)).read_line(&mut line).ok()?;
        serde_json::from_str::<serde_json::Value>(&line)
            .ok()?
            .get("timestamp")?
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    });
    first.or_else(|| {
        let md = std::fs::metadata(path).ok()?;
        md.created().or_else(|_| md.modified()).ok().map(utc)
    })
}

impl Inner {
    /// Folds this pass's registry entries and daemon runs into the root cache.
    fn attach_sources(&self, st: &mut State, runs: HashMap<String, RunRow>, since: Timestamp) {
        // Two files may name one session (a resumed one): prefer the live one.
        let mut regs: HashMap<String, (SessionFile, bool, Stamp)> = HashMap::new();
        for (stamp, file) in st.sessions.values() {
            let Some(file) = file else { continue };
            let alive = session::is_same_process(file, &*self.probe);
            let keep_existing = matches!(regs.get(&file.session_id), Some((_, true, _))) && !alive;
            if !keep_existing {
                regs.insert(file.session_id.clone(), (file.clone(), alive, *stamp));
            }
        }
        for root in st.roots.values_mut() {
            root.registry = None;
            root.registry_stamp = None;
            root.run = None;
        }
        // Forget stale marks whose registry file is gone.
        st.stale.retain(|id, _| regs.contains_key(id));
        for (id, (file, alive, stamp)) in regs {
            if !safe_id(&id) {
                continue;
            }
            // A dead entry evicted as stale stays out until its file changes (a live one never is).
            if !alive && st.stale.get(&id) == Some(&stamp) {
                continue;
            }
            st.stale.remove(&id);
            let root = st.roots.entry(id).or_default();
            root.last_identity = Some(Identity::of(&file));
            root.registry = Some((file, alive));
            root.registry_stamp = Some(stamp);
        }
        for (id, run) in runs {
            let live = matches!(run.status.as_str(), "running" | "awaiting_approval");
            // Same rule as the SQL below: a terminal run counts by its completion time (creation
            // time when it has none).
            let recent = run.completed_at.or(run.created_at).is_none_or(|t| t >= since);
            if safe_id(&id) && (live || recent) {
                st.roots.entry(id).or_default().run = Some(run);
            }
        }
    }

    /// Transcript path, tail facts and start time of one root; reads only when the stamp moved.
    fn refresh_root(&self, id: &str, root: &mut RootCache, stats: &mut Stats) {
        if !root.transcript.as_ref().is_some_and(|p| p.is_file()) {
            let cwd = root.cwd();
            root.transcript =
                self.find_transcript(id, cwd.as_deref(), &mut root.transcript_miss, stats);
            root.transcript_stamp = None;
        }
        if let Some(path) = root.transcript.clone() {
            match std::fs::metadata(&path) {
                Ok(md) => {
                    let stamp = Stamp::of(&md);
                    if root.transcript_stamp != Some(stamp) {
                        stats.tail_reads += 1;
                        root.tail = tail_facts(&path);
                        root.transcript_stamp = Some(stamp);
                        root.last_activity = Some(utc(stamp.mtime));
                    }
                }
                Err(_) => root.transcript = None,
            }
        }
        if root.started_at.is_none() {
            root.started_at = root
                .registry
                .as_ref()
                .and_then(|(s, _)| s.started_at)
                .or_else(|| root.run.as_ref().and_then(|r| r.created_at))
                .or_else(|| root.transcript.as_deref().and_then(started_at_of));
        }
    }

    fn scan(&self, since: Timestamp, runs: HashMap<String, RunRow>) -> Vec<Sighting> {
        let mut guard = self.lock();
        let st = &mut *guard;
        self.refresh_registry(st);
        self.attach_sources(st, runs, since);
        let mut out = Vec::new();
        let mut expired = Vec::new();
        let ids: Vec<String> = st.roots.keys().cloned().collect();
        for id in ids {
            let Some(root) = st.roots.get_mut(&id) else { continue };
            self.refresh_root(&id, root, &mut st.stats);
            let Some(state) = root_state(root) else {
                expired.push(id);
                continue;
            };
            let alive = matches!(state, NodeState::Busy | NodeState::Idle);
            // Spec §4: nothing without activity in the retention window, unless it is alive.
            if !alive && root_activity(root).is_some_and(|at| at < since) {
                expired.push(id);
                continue;
            }
            self.refresh_children(root, &mut st.stats);
            out.push(root_sighting(&id, root, state));
            child_sightings(&id, root, state, &mut out);
        }
        for id in expired {
            if let Some(root) = st.roots.remove(&id) {
                // A DEAD registry file keeps naming this session on every pass; remember it (with the
                // file's stamp) so `attach_sources` does not rebuild the root every 5 s.
                if let (Some((_, false)), Some(stamp)) = (&root.registry, root.registry_stamp) {
                    st.stale.insert(id, stamp);
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }
}

impl Inner {
    fn read_meta(&self, path: &Path) -> AgentMeta {
        let parsed = std::fs::read_to_string(path)
            .map_err(|_| "agent meta file is unreadable")
            .and_then(|text| session::parse_meta(&text));
        match parsed {
            Ok(meta) => meta,
            // A half-written or odd meta degrades to a bare child; it is re-read when it changes.
            Err(cause) => {
                self.warn_once(cause);
                AgentMeta::default()
            }
        }
    }

    fn load_child(&self, dir: &Path, agent: &str, stats: &mut Stats) -> Option<ChildCache> {
        let meta_path = dir.join(format!("agent-{agent}.meta.json"));
        let meta_stamp = Stamp::of(&std::fs::metadata(&meta_path).ok()?);
        stats.meta_parses += 1;
        let transcript = dir.join(format!("agent-{agent}.jsonl"));
        let last_activity = std::fs::metadata(&transcript)
            .map(|md| utc(Stamp::of(&md).mtime))
            .unwrap_or_else(|_| utc(meta_stamp.mtime));
        Some(ChildCache {
            meta: self.read_meta(&meta_path),
            meta_path,
            meta_stamp,
            started_at: started_at_of(&transcript).unwrap_or(last_activity),
            transcript,
            last_activity,
        })
    }

    /// Spec §4 "Descoberta incremental": the `subagents/` directory is relisted only when ITS mtime
    /// moved; a known child is never re-read from scratch. An unfinished child is re-stat-ed each
    /// pass, because `finished` is written in place and does not touch the directory mtime.
    fn refresh_children(&self, root: &mut RootCache, stats: &mut Stats) {
        let Some(transcript) = root.transcript.as_ref() else {
            return;
        };
        let dir = transcript.with_extension("").join("subagents");
        let Ok(md) = std::fs::metadata(&dir) else {
            root.children.clear();
            root.subagents_mtime = None;
            return;
        };
        let mtime = md.modified().ok();
        if root.subagents_mtime != mtime {
            stats.subagent_listings += 1;
            root.subagents_mtime = mtime;
            if let Ok(read) = std::fs::read_dir(&dir) {
                for item in read.flatten() {
                    let name = item.file_name().to_string_lossy().into_owned();
                    let Some(agent) = name
                        .strip_prefix("agent-")
                        .and_then(|n| n.strip_suffix(".meta.json"))
                    else {
                        continue;
                    };
                    if !safe_id(agent) || root.children.contains_key(agent) {
                        continue;
                    }
                    if let Some(child) = self.load_child(&dir, agent, stats) {
                        root.children.insert(agent.to_owned(), child);
                    }
                }
            }
        }
        for child in root.children.values_mut().filter(|c| !c.meta.is_closed()) {
            if let Ok(md) = std::fs::metadata(&child.meta_path) {
                let stamp = Stamp::of(&md);
                if stamp != child.meta_stamp {
                    stats.meta_parses += 1;
                    child.meta = self.read_meta(&child.meta_path);
                    child.meta_stamp = stamp;
                }
            }
            if let Ok(md) = std::fs::metadata(&child.transcript) {
                child.last_activity = utc(Stamp::of(&md).mtime);
            }
        }
    }
}

fn child_sightings(id: &str, root: &RootCache, root_state: NodeState, out: &mut Vec<Sighting>) {
    let root_key = NodeKey::new(HARNESS, id);
    let root_project = root.cwd().as_deref().and_then(tail::project_of);
    for (agent, child) in &root.children {
        let state = if child.meta.is_closed() {
            NodeState::Done
        } else if matches!(root_state, NodeState::Busy | NodeState::Idle) {
            NodeState::Busy
        } else {
            // Spec §3: unfinished and the root is gone.
            NodeState::Lost
        };
        // The native parent edge: a known parent agent, else the root. An unknown or self
        // `parentAgentId` degrades to the root rather than leaving the child dangling.
        let parent = child
            .meta
            .parent_agent_id
            .as_deref()
            .filter(|p| *p != agent && root.children.contains_key(*p))
            .map(|p| NodeKey::new(HARNESS, p))
            .unwrap_or_else(|| root_key.clone());
        out.push(Sighting {
            key: NodeKey::new(HARNESS, agent),
            parent: Some(parent),
            harness_label: HARNESS.to_owned(),
            model: child.meta.model.clone(),
            role: child.meta.agent_type.clone(),
            // Amendment 18: every label goes through `clean_label` (collapse, redact, cut).
            label: clean_label(
                &child
                    .meta
                    .description
                    .clone()
                    .or_else(|| child.meta.agent_type.clone())
                    .unwrap_or_else(|| agent.clone()),
                LABEL_CHARS,
            ),
            project: root_project.clone(),
            state,
            started_at: child.started_at,
            ended_at: matches!(state, NodeState::Done | NodeState::Lost).then_some(child.last_activity),
            last_activity_at: child.last_activity,
            run_id: None,
            can_stop: false,
            has_lines: true,
            has_transcript: true,
            usage: None,
        });
    }
}

/// Spec §3. Order: a live registry entry decides; else the daemon run's status decides; else the
/// transcript's last conversation record (closed turn = Done, anything else = Lost).
fn root_state(root: &RootCache) -> Option<NodeState> {
    // A root known only from a daemon run needs its transcript on disk: without one there is
    // nothing to read or page, and the run itself is `daemon_runs`' node. (A registry entry, dead
    // or alive, keeps the root: a live process with no transcript yet is still sighted.)
    if root.registry.is_none() && root.transcript.is_none() {
        return None;
    }
    if let Some((file, true)) = &root.registry {
        return Some(if file.status.as_deref() == Some("busy") {
            NodeState::Busy
        } else {
            NodeState::Idle
        });
    }
    if let Some(run) = &root.run {
        match run.status.as_str() {
            "running" => return Some(NodeState::Busy),
            "awaiting_approval" => return Some(NodeState::Idle),
            "interrupted" => return Some(NodeState::Lost),
            "completed" | "failed" | "cancelled" | "timed_out" => return Some(NodeState::Done),
            _ => {}
        }
    }
    // Nothing to show for a root with neither a live process nor a transcript to judge by.
    root.transcript.as_ref()?;
    Some(if root.tail.closed == Some(true) {
        NodeState::Done
    } else {
        NodeState::Lost
    })
}

fn root_activity(root: &RootCache) -> Option<Timestamp> {
    let registry = root.registry.as_ref().and_then(|(s, _)| s.updated_at);
    root.last_activity.into_iter().chain(registry).max()
}

/// Label, role and project come from `last_identity`, so a root keeps them after Claude Code
/// deletes its registry entry at exit.
fn root_sighting(id: &str, root: &RootCache, state: NodeState) -> Sighting {
    let cwd = root.cwd();
    let role = match (&root.last_identity, &root.run) {
        (Some(identity), _) => match identity.kind.as_deref() {
            None | Some("interactive") => Some("chat".to_owned()),
            Some(other) => Some(other.to_owned()),
        },
        (None, Some(_)) => Some("job".to_owned()),
        _ => None,
    };
    // Amendment 18: every label goes through `clean_label` (collapse, redact, cut).
    let label = clean_label(
        &root
            .last_identity
            .as_ref()
            .and_then(|i| i.name.clone())
            .or_else(|| root.run.as_ref().map(|r| format!("run {}", r.id)))
            .unwrap_or_else(|| id.chars().take(8).collect()),
        LABEL_CHARS,
    );
    let activity = root_activity(root).unwrap_or_else(Utc::now);
    let ended = matches!(state, NodeState::Done | NodeState::Lost).then_some(activity);
    Sighting {
        key: NodeKey::new(HARNESS, id),
        parent: None,
        harness_label: HARNESS.to_owned(),
        model: root.tail.model.clone(),
        role,
        label,
        project: cwd.as_deref().and_then(tail::project_of),
        state,
        started_at: root.started_at.unwrap_or(activity),
        ended_at: ended,
        last_activity_at: activity,
        run_id: root.run.as_ref().map(|r| r.id),
        can_stop: false,
        has_lines: root.transcript.is_some(),
        has_transcript: root.transcript.is_some(),
        usage: root.tail.usage.clone(),
    }
}

impl ClaudeCode {
    /// The runs that name a Claude session and are live or inside the retention window. Runs are
    /// the only way to find a root launched in print mode, which has no `sessions/*.json` entry
    /// (spec §4).
    async fn runs_by_session(&self, since: Timestamp) -> HashMap<String, RunRow> {
        type Row = (i64, String, String, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = match sqlx::query_as(
            "SELECT id, session_id, status, cwd, created_at, completed_at FROM runs \
             WHERE session_id IS NOT NULL AND session_id <> '' AND session_id <> 'shared' \
             AND (runner IS NULL OR runner = 'claude') \
             AND (status IN ('running', 'awaiting_approval') \
                  OR julianday(COALESCE(completed_at, created_at)) >= julianday(?)) \
             ORDER BY id DESC",
        )
        .bind(since.to_rfc3339())
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => {
                self.inner.warn_once("the runs table could not be read");
                return HashMap::new();
            }
        };
        let time = |raw: Option<String>| {
            raw.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|t| t.with_timezone(&Utc))
        };
        let mut out = HashMap::new();
        for (id, session_id, status, cwd, created, completed) in rows {
            // Newest first: the first run seen for a session is the one that is current.
            out.entry(session_id).or_insert(RunRow {
                id,
                status,
                cwd,
                created_at: time(created),
                completed_at: time(completed),
            });
        }
        out
    }
}

#[async_trait]
impl AgentSource for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude_code"
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        Vec::new() // Task 17
    }

    async fn discover(&self, since: Timestamp) -> SourceResult<Vec<Sighting>> {
        let inner = self.inner.clone();
        let runs = self.runs_by_session(since).await;
        Ok(tokio::task::spawn_blocking(move || inner.scan(since, runs)).await?)
    }

    fn node_ref(&self, _key: &NodeKey) -> Option<NodeRef> {
        None // Task 17
    }

    async fn read_lines(&self, _node: &NodeRef, _from: Offset) -> SourceResult<LinesRead> {
        Err(SourceError::msg("read_lines: Task 15"))
    }

    async fn transcript_page(&self, _node: &NodeRef, _before: Option<Cursor>) -> SourceResult<TranscriptPage> {
        Err(SourceError::msg("transcript_page: Task 16"))
    }
}

#[cfg(test)]
mod tests {
    // The FINAL `use` block of this module (later tasks add none). `TermKind` / `TranscriptEntry`
    // are first used in Tasks 15-16 and warn as unused until then.
    use super::*;
    use crate::fleet::claude_code::fixture::{self as fx, FakeProbe, Home};
    use crate::fleet::model::{NodeState, TermKind, TranscriptEntry};
    use serde_json::json;

    pub(super) fn since() -> Timestamp {
        Utc::now() - chrono::Duration::minutes(10)
    }

    async fn setup(home: &Home, probe: &Arc<FakeProbe>) -> (ClaudeCode, crate::storage::TempDb) {
        let db = crate::storage::TempDb::new().await;
        let probe: Arc<dyn ProcessProbe> = probe.clone();
        (ClaudeCode::with_probe(db.pool.clone(), home.path(), probe), db)
    }

    fn root_id() -> String {
        format!("claude-code:{}", fx::SESSION)
    }

    fn find<'a>(all: &'a [Sighting], id: &str) -> &'a Sighting {
        all.iter().find(|s| s.key.id() == id).unwrap_or_else(|| {
            panic!("no sighting {id}; have {:?}", all.iter().map(|s| s.key.id()).collect::<Vec<_>>())
        })
    }

    #[tokio::test]
    async fn a_live_registry_root_is_one_busy_sighting_with_its_facts() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;

        let all = cc.discover(since()).await.unwrap();

        assert_eq!(all.len(), 1);
        let root = find(&all, &root_id());
        assert_eq!(root.state, NodeState::Busy);
        assert_eq!(root.label, "proj-1");
        assert_eq!(root.project.as_deref(), Some("proj"));
        assert_eq!(root.model.as_deref(), Some("claude-test-1"));
        assert_eq!(root.role.as_deref(), Some("chat"));
        assert_eq!(root.harness_label, "claude-code");
        assert_eq!(root.parent, None);
        assert_eq!(root.run_id, None);
        assert_eq!(root.ended_at, None);
        assert!(!root.can_stop && root.has_lines && root.has_transcript);
        assert_eq!(root.started_at.timestamp_millis(), 1_792_523_514_511);
        assert_eq!(root.usage.as_ref().and_then(|u| u.context_tokens), Some(330));
    }

    #[tokio::test]
    async fn a_waiting_or_idle_registry_root_is_idle() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "waiting", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &root_id()).state, NodeState::Idle);
    }

    #[tokio::test]
    async fn a_live_root_without_a_transcript_yet_has_no_lines() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        let root = find(&all, &root_id());
        assert_eq!(root.state, NodeState::Busy);
        assert!(!root.has_lines && !root.has_transcript);
    }

    /// A root whose process is gone (no probe entry), transcript given by `lines`.
    async fn dead_root(lines: &[String]) -> (Sighting, ClaudeCode, Home, crate::storage::TempDb) {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, lines);
        let probe = Arc::new(FakeProbe::new());
        let (cc, db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        let root = find(&all, &root_id()).clone();
        (root, cc, home, db)
    }

    #[tokio::test]
    async fn a_dead_pid_with_a_closed_turn_is_done_and_ended_at_the_last_activity() {
        let (root, ..) = dead_root(&fx::closed_turn()).await;
        assert_eq!(root.state, NodeState::Done);
        assert_eq!(root.ended_at, Some(root.last_activity_at));
    }

    #[tokio::test]
    async fn a_dead_pid_with_a_pending_tool_call_is_lost() {
        let (root, ..) = dead_root(&fx::open_turn()).await;
        assert_eq!(root.state, NodeState::Lost);
        assert_eq!(root.ended_at, Some(root.last_activity_at));
    }

    #[tokio::test]
    async fn a_dead_pid_whose_last_record_is_an_unanswered_tool_result_is_lost() {
        let mut lines = fx::open_turn();
        lines.push(fx::tool_result(fx::TS, "toolu_1", "ok", json!({})));
        let (root, ..) = dead_root(&lines).await;
        assert_eq!(root.state, NodeState::Lost);
    }

    #[tokio::test]
    async fn trailing_noise_after_the_final_answer_does_not_reopen_the_turn() {
        let mut lines = fx::closed_turn();
        lines.push(fx::meta_user(fx::TS, "injected"));
        lines.push("{\"type\":\"system\",\"subtype\":\"stop_hook_summary\"}".to_owned());
        let (root, ..) = dead_root(&lines).await;
        assert_eq!(root.state, NodeState::Done);
    }

    #[tokio::test]
    async fn a_recycled_pid_with_a_different_start_time_is_dead() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "999"); // somebody else's process now holds the pid
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &root_id()).state, NodeState::Done);
    }

    #[tokio::test]
    async fn a_root_whose_registry_entry_vanished_stays_known_as_done() {
        let home = Home::new();
        let entry = home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        assert_eq!(find(&cc.discover(since()).await.unwrap(), &root_id()).state, NodeState::Busy);

        // Claude Code exits cleanly: the process and its registry file are both gone.
        std::fs::remove_file(entry).unwrap();
        probe.set(100, session::Proc::Dead);
        let all = cc.discover(since()).await.unwrap();
        let root = find(&all, &root_id());
        assert_eq!(root.state, NodeState::Done);
        assert!(root.ended_at.is_some());
        // The identity the deleted entry carried survives it (`last_identity`).
        assert_eq!(root.label, "proj-1");
        assert_eq!(root.project.as_deref(), Some("proj"));
        assert_eq!(root.role.as_deref(), Some("chat"));
    }

    async fn insert_run(
        pool: &sqlx::SqlitePool,
        id: i64,
        status: &str,
        runner: &str,
        completed_at: Option<chrono::DateTime<Utc>>,
    ) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, session_id, runner, cwd, created_at, completed_at) \
             VALUES (?, 'p', ?, ?, ?, 'c:\\work\\proj', ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(fx::SESSION)
        .bind(runner)
        .bind(Utc::now().to_rfc3339())
        .bind(completed_at.map(|t| t.to_rfc3339()))
        .execute(pool)
        .await
        .expect("insert run");
    }

    /// A print-mode run: a transcript, a run row, and NO registry entry.
    async fn run_root(status: &str, runner: &str, completed: Option<chrono::DateTime<Utc>>) -> Vec<Sighting> {
        let home = Home::new();
        home.transcript(fx::SESSION, &fx::open_turn());
        let probe = Arc::new(FakeProbe::new());
        let (cc, db) = setup(&home, &probe).await;
        insert_run(&db.pool, 5, status, runner, completed).await;
        cc.discover(since()).await.unwrap()
    }

    #[tokio::test]
    async fn a_running_daemon_run_without_a_registry_entry_is_a_busy_root_carrying_its_run_id() {
        let all = run_root("running", "claude", None).await;
        let root = find(&all, &root_id());
        assert_eq!(root.state, NodeState::Busy);
        assert_eq!(root.run_id, Some(5));
        assert_eq!(root.role.as_deref(), Some("job"));
        assert_eq!(root.label, "run 5");
        assert!(!root.can_stop, "only daemon_runs can stop");
        assert!(root.has_lines && root.has_transcript);
        assert_eq!(root.project.as_deref(), Some("proj"));
    }

    #[tokio::test]
    async fn run_states_map_to_node_states() {
        let now = Some(Utc::now());
        for (status, expected) in [
            ("awaiting_approval", NodeState::Idle),
            ("completed", NodeState::Done),
            ("failed", NodeState::Done),
            ("cancelled", NodeState::Done),
            ("timed_out", NodeState::Done),
            ("interrupted", NodeState::Lost),
        ] {
            let all = run_root(status, "claude", now).await;
            assert_eq!(find(&all, &root_id()).state, expected, "run status {status}");
        }
    }

    #[tokio::test]
    async fn a_terminal_run_outside_the_retention_window_is_not_a_root() {
        let long_ago = Some(Utc::now() - chrono::Duration::hours(2));
        assert!(run_root("completed", "claude", long_ago).await.is_empty());
    }

    #[tokio::test]
    async fn another_runner_and_a_run_without_a_transcript_are_not_claude_roots() {
        assert!(run_root("running", "codex", None).await.is_empty());

        // No transcript on disk and no registry entry: nothing to read or page, so no root (the
        // `registry.is_none() && transcript.is_none()` guard at the top of `root_state`).
        let home = Home::new();
        let probe = Arc::new(FakeProbe::new());
        let (cc, db) = setup(&home, &probe).await;
        insert_run(&db.pool, 6, "running", "claude", None).await;
        assert!(cc.discover(since()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_live_run_is_found_however_many_newer_runs_exist() {
        let home = Home::new();
        home.transcript(fx::SESSION, &fx::open_turn());
        let probe = Arc::new(FakeProbe::new());
        let (cc, db) = setup(&home, &probe).await;
        insert_run(&db.pool, 1, "running", "claude", None).await; // the OLDEST row
        let long_ago = (Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        for n in 2..=250_i64 {
            sqlx::query(
                "INSERT INTO runs (id, prompt, status, session_id, runner, created_at, completed_at) \
                 VALUES (?, 'p', 'completed', ?, 'claude', ?, ?)",
            )
            .bind(n)
            .bind(format!("other-session-{n}"))
            .bind(&long_ago)
            .bind(&long_ago)
            .execute(&db.pool)
            .await
            .expect("insert old run");
        }
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(all.len(), 1, "the 249 old runs are filtered in SQL, the live one is not cut off");
        assert_eq!(find(&all, &root_id()).run_id, Some(1));
    }

    #[tokio::test]
    async fn a_live_registry_entry_and_a_run_for_one_session_make_one_root_with_the_run_id() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "idle", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, db) = setup(&home, &probe).await;
        insert_run(&db.pool, 5, "running", "claude", None).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(all.len(), 1);
        let root = find(&all, &root_id());
        assert_eq!((root.state, root.run_id), (NodeState::Idle, Some(5)));
    }

    /// Root alive (busy) with four children: a1 finished, a2 running, a3 under a1, a4 pointing at
    /// an agent that does not exist.
    fn with_children(alive: bool) -> (Home, Arc<FakeProbe>) {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &if alive { fx::closed_turn() } else { fx::open_turn() });
        let s = fx::SESSION;
        home.subagent(s, "a1", json!({"agentType":"Explore","description":"read specs","model":"sonnet","finished":true}), &fx::closed_turn());
        home.subagent(s, "a2", json!({"agentType":"general-purpose","description":"write code"}), &fx::open_turn());
        home.subagent(s, "a3", json!({"agentType":"Explore","description":"deeper","parentAgentId":"a1","spawnDepth":2}), &fx::open_turn());
        home.subagent(s, "a4", json!({"agentType":"Explore","description":"orphan","parentAgentId":"ghost"}), &fx::open_turn());
        let probe = Arc::new(FakeProbe::new());
        if alive {
            probe.alive(100, "777");
        }
        (home, probe)
    }

    fn child_id(agent: &str) -> String {
        format!("claude-code:{agent}")
    }

    #[tokio::test]
    async fn children_hang_off_the_root_or_off_their_parent_agent() {
        let (home, probe) = with_children(true);
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();

        let root_key = NodeKey::new("claude-code", fx::SESSION);
        assert_eq!(find(&all, &child_id("a1")).parent.as_ref(), Some(&root_key));
        assert_eq!(find(&all, &child_id("a2")).parent.as_ref(), Some(&root_key));
        assert_eq!(find(&all, &child_id("a3")).parent.as_ref(), Some(&NodeKey::new("claude-code", "a1")));
        // Unknown parentAgentId: no heuristics, the parent is the root.
        assert_eq!(find(&all, &child_id("a4")).parent.as_ref(), Some(&root_key));
        let a1 = find(&all, &child_id("a1"));
        assert_eq!(a1.model.as_deref(), Some("sonnet"));
        assert_eq!(a1.role.as_deref(), Some("Explore"));
        assert_eq!(a1.label, "read specs");
        assert!(!a1.can_stop && a1.has_lines && a1.has_transcript);
        assert_eq!(a1.project.as_deref(), Some("proj"));
    }

    #[tokio::test]
    async fn child_state_follows_the_finished_marker_and_the_root() {
        let (home, probe) = with_children(true);
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        let a1 = find(&all, &child_id("a1"));
        assert_eq!(a1.state, NodeState::Done);
        assert_eq!(a1.ended_at, Some(a1.last_activity_at));
        let a2 = find(&all, &child_id("a2"));
        assert_eq!((a2.state, a2.ended_at), (NodeState::Busy, None));

        // Same tree, root dead with a pending tool call: unfinished children are Lost.
        let (home, probe) = with_children(false);
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &root_id()).state, NodeState::Lost);
        assert_eq!(find(&all, &child_id("a1")).state, NodeState::Done);
        let a2 = find(&all, &child_id("a2"));
        assert_eq!(a2.state, NodeState::Lost);
        assert!(a2.ended_at.is_some());
    }

    #[tokio::test]
    async fn a_user_stopped_child_is_done_and_a_bare_meta_degrades() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        home.subagent(fx::SESSION, "s1", json!({"agentType":"general-purpose","stoppedByUser":true}), &fx::open_turn());
        home.subagent(fx::SESSION, "b1", json!({}), &[]);
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();

        assert_eq!(find(&all, &child_id("s1")).state, NodeState::Done);
        let bare = find(&all, &child_id("b1"));
        assert_eq!((bare.model.clone(), bare.role.clone()), (None, None));
        assert_eq!(bare.label, "b1");
        assert_eq!(bare.parent, Some(NodeKey::new("claude-code", fx::SESSION)));
    }

    #[tokio::test]
    async fn a_secret_in_a_label_is_redacted_by_clean_label() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let token = format!("ghp_{}", "a".repeat(36));
        home.subagent(
            fx::SESSION,
            "k1",
            json!({"agentType":"Explore","description": format!("push with {token} now")}),
            &fx::open_turn(),
        );
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();

        let label = find(&all, &child_id("k1")).label.clone();
        assert!(label.contains("[SECRET:github]"), "{label}");
        assert!(!label.contains(&token), "{label}");
    }

    #[tokio::test]
    async fn a_run_resolved_root_gets_its_subagents_too() {
        let home = Home::new();
        home.transcript(fx::SESSION, &fx::open_turn());
        home.subagent(fx::SESSION, "a1", json!({"agentType":"Explore","description":"d"}), &fx::open_turn());
        let probe = Arc::new(FakeProbe::new());
        let (cc, db) = setup(&home, &probe).await;
        insert_run(&db.pool, 5, "running", "claude", None).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &root_id()).run_id, Some(5));
        assert_eq!(find(&all, &child_id("a1")).state, NodeState::Busy);
    }

    #[tokio::test]
    async fn a_malformed_meta_file_degrades_to_a_bare_child_and_is_logged_once() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        home.subagent(fx::SESSION, "m1", json!({}), &[]);
        std::fs::write(home.meta_path(fx::SESSION, "m1"), "{\"agentTy").unwrap();
        home.subagent(fx::SESSION, "m2", json!({}), &[]);
        std::fs::write(home.meta_path(fx::SESSION, "m2"), "{\"agentTy").unwrap();
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert!(all.iter().any(|s| s.key.id() == child_id("m1")));
        assert!(all.iter().any(|s| s.key.id() == child_id("m2")));
        assert_eq!(cc.warnings(), 1);
    }

    #[tokio::test]
    async fn a_dead_root_with_no_activity_inside_the_retention_window_is_not_discovered() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        let transcript = home.transcript(fx::SESSION, &fx::closed_turn());
        fx::age(&transcript, 60);
        let probe = Arc::new(FakeProbe::new()); // dead
        let (cc, _db) = setup(&home, &probe).await;
        assert!(cc.discover(since()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_live_root_is_discovered_however_old_its_transcript() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "idle", "777");
        let transcript = home.transcript(fx::SESSION, &fx::closed_turn());
        fx::age(&transcript, 600);
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &root_id()).state, NodeState::Idle);
    }

    #[tokio::test]
    async fn finished_children_of_a_live_root_stay_however_old() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "idle", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let old = home.subagent(fx::SESSION, "a1", json!({"finished":true}), &fx::closed_turn());
        fx::age(&old, 600);
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        assert_eq!(find(&cc.discover(since()).await.unwrap(), &child_id("a1")).state, NodeState::Done);
    }

    #[tokio::test]
    async fn an_expired_cached_root_is_dropped() {
        let home = Home::new();
        let entry = home.session_file(100, fx::SESSION, "busy", "777");
        let transcript = home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        assert_eq!(cc.discover(since()).await.unwrap().len(), 1);

        std::fs::remove_file(entry).unwrap();
        probe.set(100, session::Proc::Dead);
        fx::age(&transcript, 60);
        assert!(cc.discover(since()).await.unwrap().is_empty());
        assert!(cc.inner.lock().roots.is_empty(), "the cache entry should be evicted");
    }

    #[tokio::test]
    async fn an_evicted_stale_session_is_not_rebuilt_until_its_registry_file_changes() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777"); // a leftover: its pid is dead
        let transcript = home.transcript(fx::SESSION, &fx::closed_turn());
        fx::age(&transcript, 60);
        let probe = Arc::new(FakeProbe::new());
        let (cc, _db) = setup(&home, &probe).await;

        assert!(cc.discover(since()).await.unwrap().is_empty());
        assert_eq!(cc.stats().tail_reads, 1, "built once, then evicted as outside the window");
        for _ in 0..3 {
            assert!(cc.discover(since()).await.unwrap().is_empty());
        }
        assert_eq!(cc.stats().tail_reads, 1, "later passes must not re-create the root");
        assert!(cc.inner.lock().roots.is_empty());

        // The file changes (a longer status word, so the stamp differs on any filesystem):
        // the session is evaluated afresh.
        home.session_file(100, fx::SESSION, "waiting", "777");
        assert!(cc.discover(since()).await.unwrap().is_empty());
        assert_eq!(cc.stats().tail_reads, 2);

        // And when the file goes away the mark is forgotten.
        std::fs::remove_file(home.path().join(".claude").join("sessions").join("100.json")).unwrap();
        cc.discover(since()).await.unwrap();
        assert!(cc.inner.lock().stale.is_empty());
    }

    #[tokio::test]
    async fn a_missing_transcript_is_searched_across_project_dirs_only_when_projects_changes() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        let projects = home.path().join(".claude").join("projects");
        std::fs::create_dir_all(projects.join("elsewhere")).unwrap();
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;

        let all = cc.discover(since()).await.unwrap();
        assert!(!find(&all, &root_id()).has_transcript);
        assert_eq!(cc.stats().project_scans, 1);
        cc.discover(since()).await.unwrap();
        cc.discover(since()).await.unwrap();
        assert_eq!(cc.stats().project_scans, 1, "nothing changed under projects/: no new search");

        // The session's transcript shows up in a project directory that does not follow the cwd;
        // creating that directory moves the `projects/` mtime (a short pause keeps the two mtimes
        // apart on filesystems with coarse timestamps).
        std::thread::sleep(std::time::Duration::from_millis(50));
        let moved = projects.join("moved-project");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(
            moved.join(format!("{}.jsonl", fx::SESSION)),
            format!("{}\n", fx::user_prompt(fx::TS, "hi")),
        )
        .unwrap();
        let all = cc.discover(since()).await.unwrap();
        assert!(find(&all, &root_id()).has_transcript);
        assert_eq!(cc.stats().project_scans, 2);
    }

    #[tokio::test]
    async fn an_unchanged_pass_re_reads_nothing_and_a_new_child_costs_one_meta() {
        let (home, probe) = with_children(true);
        let (cc, _db) = setup(&home, &probe).await;
        cc.discover(since()).await.unwrap();
        let first = cc.stats();
        assert_eq!(first.subagent_listings, 1);
        assert_eq!(first.meta_parses, 4);
        assert_eq!(first.session_parses, 1);

        cc.discover(since()).await.unwrap();
        assert_eq!(cc.stats(), first, "nothing changed on disk, nothing may be re-read");

        // A new subagent changes the directory mtime: one relisting, one more meta, others untouched.
        home.subagent(fx::SESSION, "a5", json!({"agentType":"Explore","description":"new"}), &fx::open_turn());
        let all = cc.discover(since()).await.unwrap();
        let after = cc.stats();
        assert_eq!(after.subagent_listings, 2);
        assert_eq!(after.meta_parses, 5);
        assert!(all.iter().any(|s| s.key.id() == child_id("a5")));
    }

    #[tokio::test]
    async fn a_meta_rewritten_in_place_is_noticed_without_relisting_the_directory() {
        let (home, probe) = with_children(true);
        let (cc, _db) = setup(&home, &probe).await;
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &child_id("a2")).state, NodeState::Busy);
        let listings = cc.stats().subagent_listings;

        std::fs::write(
            home.meta_path(fx::SESSION, "a2"),
            json!({"agentType":"general-purpose","description":"write code","finished":true}).to_string(),
        )
        .unwrap();
        let all = cc.discover(since()).await.unwrap();
        assert_eq!(find(&all, &child_id("a2")).state, NodeState::Done);
        assert_eq!(cc.stats().subagent_listings, listings);
    }

    #[tokio::test]
    async fn a_registry_file_rewritten_in_place_is_re_parsed() {
        let home = Home::new();
        home.session_file(100, fx::SESSION, "busy", "777");
        home.transcript(fx::SESSION, &fx::closed_turn());
        let probe = Arc::new(FakeProbe::new());
        probe.alive(100, "777");
        let (cc, _db) = setup(&home, &probe).await;
        assert_eq!(find(&cc.discover(since()).await.unwrap(), &root_id()).state, NodeState::Busy);
        // A longer status word, so the stamp changes even on a coarse-mtime filesystem.
        home.session_file(100, fx::SESSION, "waiting", "777");
        assert_eq!(find(&cc.discover(since()).await.unwrap(), &root_id()).state, NodeState::Idle);
    }

    #[tokio::test]
    async fn a_session_file_that_is_not_json_is_skipped_and_logged_once() {
        let home = Home::new();
        let dir = home.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1.json"), "{\"pid\":1,\"sess").unwrap();
        std::fs::write(dir.join("2.json"), "also not json").unwrap();
        let probe = Arc::new(FakeProbe::new());
        let (cc, _db) = setup(&home, &probe).await;
        assert!(cc.discover(since()).await.unwrap().is_empty());
        assert_eq!(cc.warnings(), 1);
    }
}
