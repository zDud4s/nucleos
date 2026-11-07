//! The production gate of a tree branch: at most one verify ticket per branch and HEAD.
//!
//! `gate_job_scope` submits a fresh ticket on every call and waits for it a bounded time. A scoped
//! gate outlasts that wait, so each driver tick used to submit another unit for the same worktree
//! while the first kept running, and a verdict that landed after the wait was dropped. Here the
//! ticket is remembered on the branch (`extra["gate_ticket"]` = `{ticket, head}`): the next gate of
//! the same HEAD reads or awaits that ticket instead of submitting, records its verdict when it
//! finished meanwhile (also across a daemon restart), and submits afresh only when the ticket ended
//! without measuring anything or the branch moved to another HEAD.
//!
//! The ticket is narrowed by the test map's filters (`verify::submit_narrowed`): the tree's land
//! measures every group whole, so a branch's gate only has to tell its own break. A red whose every
//! failed test the live master incident already holds is the base's (`GateVerdict::Inherited`).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::driver::{GateVerdict, NotMeasured, verdict_of};
use super::http::job_lock;
use super::landing::scope_caller_id;
use super::store;
use crate::verify::exec::Executor;
use crate::verify::plan::{Kind, ScopeArg, Ticket};
use crate::verify::{Caller, VerifyArgs, scope_outcome};

/// The key of `Branch.extra` that holds the ticket of the branch's gate.
const KEY: &str = "gate_ticket";

/// The key of `Branch.extra` that holds the ticket of the branch's build check, same shape.
const CHECK_KEY: &str = "gate_check_ticket";

/// The ticket a branch's gate submitted, and the HEAD it measures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stored {
    pub ticket: i64,
    pub head: String,
}

/// What the gate needs from the outside; a fake in the tests, the verify queue in production.
pub(super) trait GateIo {
    async fn stored(&self) -> Result<Option<Stored>, String>;
    async fn remember(&self, stored: Option<Stored>) -> Result<(), String>;
    /// The ticket as it is now, `None` when it vanished.
    async fn read(&self, id: i64) -> Result<Option<Ticket>, String>;
    /// The ticket after waiting up to `wait` for it to finish.
    async fn wait(&self, id: i64, wait: Duration) -> Result<Option<Ticket>, String>;
    /// The base's incident, when every failure of the red `ticket` is already red there.
    async fn inherited(&self, ticket: &Ticket) -> Option<String>;
    /// Submits a new scope ticket and answers its id.
    async fn submit(&self) -> Result<i64, String>;
}

/// The verdict of a finished ticket; a red is asked whether the base already has it.
async fn verdict_from(io: &impl GateIo, ticket: &Ticket) -> Result<GateVerdict, NotMeasured> {
    let verdict = verdict_of(scope_outcome(ticket));
    if let Ok(GateVerdict::Red { .. }) = verdict
        && let Some(note) = io.inherited(ticket).await
    {
        return Ok(GateVerdict::Inherited { note });
    }
    verdict
}

/// Gates one branch at `head`. `Err(Running)` means the ticket has not finished yet: it stays
/// remembered, so the next call picks it up rather than submitting another. `Err(Failed)` means a
/// ticket ended without measuring anything, and each such ticket is answered as `Failed` exactly
/// once: when the wait sees it end it is forgotten, and when a later call finds it ended it is
/// reported even though the fresh unit that replaces it is still running.
pub(super) async fn gate_at_head(
    io: &impl GateIo,
    head: &str,
    wait: Duration,
) -> Result<GateVerdict, NotMeasured> {
    let mut id = None;
    // The reason of a remembered ticket that ended without measuring after the last wait gave up.
    let mut ended = None;
    if let Some(stored) = io.stored().await?
        && stored.head == head
    {
        match io.read(stored.ticket).await? {
            Some(ticket) if !ticket.done => id = Some(stored.ticket),
            Some(ticket) => match verdict_from(io, &ticket).await {
                Ok(verdict) => {
                    if matches!(verdict, GateVerdict::Red { .. }) {
                        io.remember(None).await?;
                    }
                    return Ok(verdict);
                }
                // It ended without measuring: only now is a fresh unit allowed.
                Err(NotMeasured::Failed(reason) | NotMeasured::Running(reason)) => {
                    ended = Some(reason);
                }
            },
            // Vanished (the queue lost it): it too measured nothing, so it counts once.
            None => {
                ended = Some(format!(
                    "scope verification ticket {} vanished",
                    stored.ticket
                ));
            }
        }
    }
    let id = match id {
        Some(id) => id,
        None => {
            let id = io.submit().await?;
            io.remember(Some(Stored {
                ticket: id,
                head: head.to_owned(),
            }))
            .await?;
            id
        }
    };
    let Some(ticket) = io.wait(id, wait).await? else {
        io.remember(None).await?;
        return Err(NotMeasured::Failed(format!(
            "scope verification ticket {id} vanished"
        )));
    };
    if !ticket.done {
        if let Some(reason) = ended {
            return Err(NotMeasured::Failed(format!(
                "{reason}; a fresh unit (ticket {id}) is running"
            )));
        }
        return Err(NotMeasured::Running(format!(
            "scope verification ticket {id} is still running ({} of {} units queued); a later tick reads it",
            ticket.progress.queued, ticket.progress.total
        )));
    }
    let verdict = verdict_from(io, &ticket).await;
    if matches!(verdict, Err(_) | Ok(GateVerdict::Red { .. })) {
        // Answered now, so a later call must not find it and report it again. A red is forgotten
        // too: a re-gate of the same HEAD (after a decision answer, a rescue or a load backoff)
        // measures afresh instead of re-reading a verdict that may have been the machine's.
        io.remember(None).await?;
    }
    verdict
}

/// Gates a branch's HEAD in two steps: its build (`check`, the groups' clippy over all targets)
/// first, its tests only once that is not red. Leaves may not run cargo, so a test helper that no
/// longer compiles used to reach the gate unseen; here it is a red carrying the compiler's output,
/// before any test unit is spent on it. A `NothingRan` check (no group selected) goes on to the tests.
pub(super) async fn gate_compiled_then_tested(
    check: &impl GateIo,
    test: &impl GateIo,
    head: &str,
    wait: Duration,
) -> Result<GateVerdict, NotMeasured> {
    match gate_at_head(check, head, wait).await? {
        GateVerdict::Green | GateVerdict::NothingRan => gate_at_head(test, head, wait).await,
        red => Ok(red),
    }
}

struct VerifyIo<'a> {
    /// The key of `Branch.extra` that holds this step's ticket.
    key: &'static str,
    /// `Kind::Check` for the build step, `Kind::Test` for the tests.
    kind: Kind,
    executor: &'a Arc<Executor>,
    pool: &'a sqlx::SqlitePool,
    job_id: &'a str,
    branch: &'a str,
    worktree: &'a str,
}

impl GateIo for VerifyIo<'_> {
    async fn stored(&self) -> Result<Option<Stored>, String> {
        let job = store::load(self.pool, self.job_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(job
            .and_then(|job| {
                job.manifest
                    .branches
                    .get(self.branch)?
                    .extra
                    .get(self.key)
                    .cloned()
            })
            .and_then(|value| {
                Some(Stored {
                    ticket: value.get("ticket")?.as_i64()?,
                    head: value.get("head")?.as_str()?.to_owned(),
                })
            }))
    }

    async fn remember(&self, stored: Option<Stored>) -> Result<(), String> {
        let lock = job_lock(self.job_id);
        let _guard = lock.lock().await;
        let Some(mut job) = store::load(self.pool, self.job_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let Some(held) = job.manifest.branches.get_mut(self.branch) else {
            return Ok(());
        };
        match stored {
            Some(stored) => {
                held.extra.insert(
                    self.key.to_owned(),
                    json!({ "ticket": stored.ticket, "head": stored.head }),
                );
            }
            None => {
                held.extra.remove(self.key);
            }
        }
        store::update(self.pool, &job)
            .await
            .map_err(|e| e.to_string())
    }

    async fn read(&self, id: i64) -> Result<Option<Ticket>, String> {
        crate::verify::read_ticket(self.pool, id)
            .await
            .map(|read| read.map(|(ticket, _)| ticket))
            .map_err(|e| e.to_string())
    }

    async fn wait(&self, id: i64, wait: Duration) -> Result<Option<Ticket>, String> {
        crate::verify::wait_ticket(self.pool, id, wait)
            .await
            .map(|read| read.map(|(ticket, _)| ticket))
            .map_err(|e| e.to_string())
    }

    async fn inherited(&self, ticket: &Ticket) -> Option<String> {
        crate::verify::inherit::inherited_red(self.pool, ticket, chrono::Utc::now()).await
    }

    async fn submit(&self) -> Result<i64, String> {
        let args = VerifyArgs {
            kind: self.kind,
            scope: ScopeArg::Scope,
            worktree: Some(self.worktree.to_owned()),
            files: None,
            base: None,
            wait: false,
            cover: false,
            task_id: None,
            groups: None,
        };
        let caller = Caller::Job(scope_caller_id(self.job_id));
        crate::verify::submit_narrowed(self.executor, caller, &args)
            .await
            .map_err(|e| e.message().to_owned())
    }
}

/// The HEAD of the worktree, after its uncommitted work is checkpointed (a node leaves it so).
async fn head_of(worktree: &str) -> Result<String, String> {
    crate::execution::worktree::checkpoint(Path::new(worktree))
        .await
        .map_err(|e| e.to_string())?;
    let output = tokio::process::Command::new("git")
        // A repo-local `core.fsmonitor` would run a command on any call that reads the index.
        .args(["-c", "core.fsmonitor="])
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("git rev-parse HEAD: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Gates `branch` of `job_id` over `worktree` through the verify queue, deduplicated per HEAD.
pub(super) async fn gate_branch(
    executor: &Arc<Executor>,
    pool: &sqlx::SqlitePool,
    job_id: &str,
    branch: &str,
    worktree: &str,
    wait: Duration,
) -> Result<GateVerdict, NotMeasured> {
    let head = head_of(worktree).await?;
    let io = |key, kind| VerifyIo {
        key,
        kind,
        executor,
        pool,
        job_id,
        branch,
        worktree,
    };
    gate_compiled_then_tested(
        &io(CHECK_KEY, Kind::Check),
        &io(KEY, Kind::Test),
        &head,
        wait,
    )
    .await
}
