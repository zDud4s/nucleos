//! Project-level task claims (spec 2026-10-22-task-claims-design).
//!
//! A problem the daemon detects (an incident such as a red master) or an agent declares becomes a TASK
//! with one owner per project: the first to claim it holds it, and the other worktrees of the same
//! repository see that instead of fixing the same thing in parallel. The store lives in the daemon, not
//! in lock files, because verify, the postgate and land all live in the daemon.
//!
//! `keys` normalises task keys, `store` is the SQL and the compare-and-swap (with `is_live`, the one
//! helper every consumer asks whether a claim is alive), and `sessions` registers interactive sessions
//! and tells them what happened to their claims.

use serde::Serialize;

pub mod keys;
pub mod sessions;
pub mod store;

pub use keys::{KeyError, TaskKey};

/// How long an interactive session's claims outlive its last hook call (spec section 2: 30 minutes).
pub const SESSION_LEASE_SECONDS: i64 = 1800;
/// How long a `probing` row and a master-red re-proof hold their single flight (section 3: 10 minutes).
pub const PROBE_LEASE_SECONDS: i64 = 600;
/// The one live incident key per project; the shas live in `detail`.
pub const MASTER_RED_KEY: &str = "master-red";

/// A closed set of words stored as TEXT. No CHECK constraint in the schema (the rule of 0143), so the
/// parse is what keeps a bad value out of a `TaskRow`.
macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

text_enum!(
    /// `incident`: created by the daemon. `declared`: created by an agent.
    TaskKind { Incident => "incident", Declared => "declared" }
);
text_enum!(
    /// `probing` -> `open` -> `claimed` -> `done` | `abandoned`.
    Status {
        Probing => "probing",
        Open => "open",
        Claimed => "claimed",
        Done => "done",
        Abandoned => "abandoned",
    }
);
text_enum!(
    /// `Verify` appears only on a row in `probing`.
    HolderKind {
        Session => "session",
        Run => "run",
        Job => "job",
        Postgate => "postgate",
        Verify => "verify",
    }
);

/// Who holds a task. The land gate recognises the owner by `worktree`, because `--land` carries no
/// session id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub kind: HolderKind,
    pub id: String,
    pub worktree: String,
}

impl Holder {
    /// The same owner is the same `(kind, id)`; the worktree is where it works, not who it is.
    pub fn same_owner(&self, other: &Holder) -> bool {
        self.kind == other.kind && self.id == other.id
    }
}

/// A task to create (or to merge into the live row with the same key).
#[derive(Debug, Clone)]
pub struct NewTask {
    pub key: TaskKey,
    pub kind: TaskKind,
    pub title: String,
    pub detail: serde_json::Value,
    /// Relative to the project root, `/`-separated (see `keys::normalize_path`).
    pub guard_paths: Vec<String>,
    pub hint_paths: Vec<String>,
}

/// Every column of `task_claims`, typed. The holder is three columns on the row; [`TaskRow::holder`]
/// folds them into a [`Holder`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskRow {
    pub id: i64,
    pub project_id: String,
    pub key: TaskKey,
    pub kind: TaskKind,
    pub title: String,
    pub detail: serde_json::Value,
    pub guard_paths: Vec<String>,
    pub hint_paths: Vec<String>,
    pub status: Status,
    pub holder_kind: Option<HolderKind>,
    pub holder_id: Option<String>,
    pub holder_worktree: Option<String>,
    pub lease_until: Option<String>,
    pub checked_sha: Option<String>,
    pub probe_lease_until: Option<String>,
    pub created_at: String,
    pub claimed_at: Option<String>,
    pub closed_at: Option<String>,
    pub close_note: Option<String>,
}

impl TaskRow {
    /// The current holder, when the row has one (`claimed`, and `probing` with its verify ticket).
    pub fn holder(&self) -> Option<Holder> {
        Some(Holder {
            kind: self.holder_kind?,
            id: self.holder_id.clone()?,
            worktree: self.holder_worktree.clone().unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    Won { task: TaskRow, similar: Vec<TaskRow> },
    Lost { task: TaskRow, holder: Holder, similar: Vec<TaskRow> },
}

impl ClaimOutcome {
    pub fn is_won(&self) -> bool {
        matches!(self, Self::Won { .. })
    }
}

/// What `begin_probe` found for `master-red`.
#[derive(Debug, Clone)]
pub enum ProbeStart {
    /// The caller owns the proof: a fresh `probing` row, or a lapsed one it took over.
    Started(TaskRow),
    /// Another verify's proof is running; the caller marks its group `pending`.
    InFlight(TaskRow),
    /// An incident is already `open` or `claimed`; no proof needed.
    Exists(TaskRow),
}

/// The proof ran the suspect groups on the master's HEAD and they failed there.
#[derive(Debug, Clone)]
pub struct FailedProbe {
    pub master_sha: String,
    pub red_groups: Vec<String>,
    pub red_tests: Vec<String>,
    pub guard_paths: Vec<String>,
}

/// Who closes a task. `Holder`: the owner, by `task_done`. `Green`: the master went green at `sha`
/// (any path: revert, a session's fix, a re-proof). `Reconcile`: the startup sweep.
#[derive(Debug, Clone)]
pub enum Closer<'a> {
    Holder(&'a Holder),
    Green { sha: String },
    Reconcile,
}

/// What the hook tells a session on its next tool call.
#[derive(Debug, Clone)]
pub enum Notice {
    /// To the previous holder: someone else owns the task now.
    ClaimLost { task: TaskRow, new_holder: Holder },
    /// To the holder: the incident closed, "stop your fix".
    IncidentClosed { task: TaskRow, sha: Option<String> },
    /// To the sessions whose verify inherited the incident: "rebase and re-verify". Produced by chunk 4.
    InheritedClosed { task: TaskRow, sha: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_round_trips_through_its_stored_text() {
        for status in [Status::Probing, Status::Open, Status::Claimed, Status::Done, Status::Abandoned] {
            assert_eq!(Status::parse(status.as_str()), Some(status));
        }
        for kind in [HolderKind::Session, HolderKind::Run, HolderKind::Job, HolderKind::Postgate, HolderKind::Verify] {
            assert_eq!(HolderKind::parse(kind.as_str()), Some(kind));
        }
        for kind in [TaskKind::Incident, TaskKind::Declared] {
            assert_eq!(TaskKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Status::parse("nonsense"), None);
    }

    #[test]
    fn serde_uses_the_stored_text() {
        assert_eq!(serde_json::to_string(&Status::Claimed).unwrap(), "\"claimed\"");
        assert_eq!(serde_json::to_string(&HolderKind::Postgate).unwrap(), "\"postgate\"");
    }

    #[test]
    fn owners_compare_by_kind_and_id_not_by_worktree() {
        let here = Holder { kind: HolderKind::Session, id: "s1".into(), worktree: "/a".into() };
        let moved = Holder { worktree: "/b".into(), ..here.clone() };
        let other = Holder { id: "s2".into(), ..here.clone() };
        assert!(here.same_owner(&moved));
        assert!(!here.same_owner(&other));
    }

    fn row_with(holder: Option<(HolderKind, &str, Option<&str>)>) -> TaskRow {
        TaskRow {
            id: 1,
            project_id: "p".into(),
            key: TaskKey::master_red(),
            kind: TaskKind::Incident,
            title: "master red".into(),
            detail: serde_json::json!({}),
            guard_paths: vec![],
            hint_paths: vec![],
            status: if holder.is_some() { Status::Claimed } else { Status::Open },
            holder_kind: holder.map(|h| h.0),
            holder_id: holder.map(|h| h.1.to_owned()),
            holder_worktree: holder.and_then(|h| h.2.map(str::to_owned)),
            lease_until: None,
            checked_sha: None,
            probe_lease_until: None,
            created_at: "t".into(),
            claimed_at: None,
            closed_at: None,
            close_note: None,
        }
    }

    #[test]
    fn a_row_exposes_its_holder_only_when_it_has_one() {
        assert_eq!(row_with(None).holder(), None);
        let held = row_with(Some((HolderKind::Run, "7", Some("/wt")))).holder().unwrap();
        assert_eq!((held.kind, held.id.as_str(), held.worktree.as_str()), (HolderKind::Run, "7", "/wt"));
    }

    #[test]
    fn a_task_row_serialises_with_its_key_as_a_plain_string() {
        let json = serde_json::to_value(row_with(None)).unwrap();
        assert_eq!(json["key"], "master-red");
        assert_eq!(json["status"], "open");
    }
}

/// Fixtures for the tests of the HTTP, MCP and run-lifecycle layers.
#[cfg(test)]
pub mod testing {
    use super::*;
    use sqlx::SqlitePool;

    /// An `open`, unheld `declared` task of project `alpha`. The key is taken as given (no
    /// normalisation): tests name it the way `keys::normalize` would have.
    pub fn row(key: &str, id: i64) -> TaskRow {
        TaskRow {
            id,
            project_id: "alpha".to_owned(),
            key: keys::TaskKey(key.to_owned()),
            kind: TaskKind::Declared,
            title: key.to_owned(),
            detail: serde_json::json!({}),
            guard_paths: Vec::new(),
            hint_paths: Vec::new(),
            status: Status::Open,
            holder_kind: None,
            holder_id: None,
            holder_worktree: None,
            lease_until: None,
            checked_sha: None,
            probe_lease_until: None,
            created_at: "2026-10-22T00:00:00.000000Z".to_owned(),
            claimed_at: None,
            closed_at: None,
            close_note: None,
        }
    }

    pub fn new_task(key: &str, kind: TaskKind) -> NewTask {
        NewTask {
            key: keys::TaskKey(key.to_owned()),
            kind,
            title: key.to_owned(),
            detail: serde_json::json!({}),
            guard_paths: Vec::new(),
            hint_paths: Vec::new(),
        }
    }

    /// Claims `key` (a `declared` task of project `alpha`) for `holder` now; returns the task id.
    pub async fn claimed_by(pool: &SqlitePool, holder: &Holder, key: &str) -> i64 {
        let new = new_task(key, TaskKind::Declared);
        match store::claim(pool, "alpha", new, holder, chrono::Utc::now()).await.unwrap() {
            ClaimOutcome::Won { task, .. } => task.id,
            ClaimOutcome::Lost { .. } => panic!("{key} was already held"),
        }
    }

    /// `(status, holder_id)` as stored, read straight from the table: no `is_live`, no liveness
    /// rules, so a test can tell "released" from "merely not live".
    pub async fn state_of(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, holder_id FROM task_claims WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }
}
