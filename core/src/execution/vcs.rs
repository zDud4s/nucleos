//! §spec fila-vcs
//!
//! The shared-state git/`gh` queue: at most one operation per repository, ever.
//!
//! Two agents deciding to merge at the same moment is the problem this exists for. Git's index and
//! refs are shared state with no lock a second process can wait on politely, so the serialization
//! has to happen before anything reaches an argv: both requests are admitted, and they run one after
//! the other instead of colliding.
//!
//! Exclusivity is the database's job, not a mutex's. A partial unique index over `status = 'running'`
//! holds it, so it survives the daemon restart a mutex would not — and `reconcile_interrupted` is
//! what releases a slot that restart found still held.
//!
//! **What that index is keyed on is the repository, not the project**, and the distinction is the
//! whole of the promise rather than a detail of it. A project is a label somebody chose; a
//! repository is what a merge actually touches, and two labels can name one. `ResolvedRepo` is the
//! only way to obtain the key — git's own canonical common directory, one value for a main checkout
//! and for every linked worktree of it — so a caller cannot lock one repository while running git in
//! another.
//!
//! Requests are typed (`Op`), never command strings: parsing shell is the surface `classifier.rs`
//! exists to keep closed, so the daemon builds every argv itself. This module decides WHEN an
//! operation runs and records how it ended. It never decides whether the actor was allowed to ask
//! (`autopilot.rs`, `budget.rs`, `wip.rs`, `proposals.rs`), and never what a merge should contain.

use serde::{Deserialize, Serialize};
mod command;
mod drain;
#[cfg(any(test, feature = "testkit"))]
mod fake;
mod queue;
mod reconcile;
mod repo;
mod tickets;

pub(crate) use self::command::*;
pub use self::drain::*;
#[cfg(any(test, feature = "testkit"))]
use self::fake::*;
pub use self::queue::*;
pub use self::reconcile::*;
pub use self::repo::*;
pub use self::tickets::*;

/// What was asked for, as data.
///
/// Typed rather than a command string on purpose: a string would have to be parsed, and parsing
/// shell is the surface `classifier.rs` exists to keep closed. The daemon builds every argv.
///
/// **Whoever adds the next variant here owes three things that `Merge` did not — and `Push` is what
/// paying them looks like, so it is worth reading as the worked example rather than as prose.**
///
/// 1. `git_exec::run_git` justified having no process-tree kill with "nothing here hands git a
///    shell". That was true of `merge`, and it stopped being true the moment `Push` landed and git
///    started spawning ssh and credential helpers — the exact case spec §7's hung-command row was
///    written about. That comment would have become wrong without anybody editing it, which is why
///    the obligation was recorded here, where the change had to be made. **Paid:** `process_tree.rs`
///    now holds the one `TreeKiller`, and `run_git` spawns through it.
/// 2. `Merge`'s `source`/`target` reach argv without a `--end-of-options`, and get away with it by
///    accident rather than design: a dashed string can set an option but cannot also name a commit,
///    HEAD in the integration worktree is always detached so `merge`'s upstream fallback dies, and
///    `update-ref` rejects a dashed ref name. A variant with a different argv shape does not inherit
///    any of that. `Branch` is that accident turned into a rule for the two fields `Merge` has; a
///    variant carrying a name of some other kind owes its own type. **Paid:** `Push` names its remote
///    with `Remote`, and its argv carries an explicit `--end-of-options` rather than an argument
///    about why it does not need one.
/// 3. The operation names in `from_request` are matched as `&str`, so adding a variant here does NOT
///    fail to compile there. Whoever adds one must also take its name out of the "not yet" arm by
///    hand, or the queue will go on refusing an operation it has learned to perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Merge {
        source: Branch,
        target: Branch,
    },
    /// **The first operation that leaves the machine, and the only one so far that a human cannot
    /// undo by reaching for the reflog.** A merge the queue got wrong is a local ref somebody moves
    /// back; a push the queue got wrong is on a server other people have already fetched from. That
    /// is the argument for it needing at least what a merge needs by way of consent, never less —
    /// and `runs::queueable_operation` gives it exactly the merge's route, where a human's approval
    /// IS the queueing.
    ///
    /// `branch` rather than a refspec, and no force of any kind. The queue builds
    /// `<sha>:refs/heads/<branch>` itself from the sha the branch names when the operation runs, so
    /// what is published is a value the row records rather than whatever the ref drifted to; a
    /// non-fast-forward is then the remote's refusal to record, not ours to overrule.
    Push {
        remote: Remote,
        branch: Branch,
    },
    /// **The last command on `classifier.rs`'s approval list that was still handed BACK to the
    /// agent**, which is the whole reason it comes before `fetch` and `rebase` in a queue that has
    /// seven operations left to learn. `git push` and `git merge` were the other two and are done;
    /// `gh pr merge`, `npm publish`, `cargo publish` and `deploy` are not git and are not this
    /// pillar's. So of every git command that pauses for a human today, this was the only one where
    /// saying yes still meant the run performed it with its own hands.
    ///
    /// `at` is a BRANCH rather than a commit-ish, and that is a real restriction rather than a
    /// modelling convenience: the executor resolves `refs/heads/<at>`, so `git tag v1 a1b2c3d` is
    /// refused with a message naming the ref it could not resolve instead of being tagged. Accepting
    /// a raw sha would make the type's name a lie — `Branch` would hold things that are not branches
    /// — and the fallback for anyone who wants it is the one every unqueueable spelling gets.
    ///
    /// Lightweight only. `-a`/`-s` need a message, and a message is a quoted shell argument — which
    /// is the one thing `merge_from_command` and its siblings exist to never parse.
    Tag {
        name: TagName,
        at: Branch,
    },
    /// The only operation here that publishes nothing and can undo nothing.
    ///
    /// It is in the queue anyway, and the reason is the one the module header gives rather than a
    /// wish to be complete: a fetch writes the ref store and the object database, which is the shared
    /// state this pillar serialises. Two fetches racing a merge is the same class of collision as two
    /// merges racing each other, and `.git/packed-refs` is not a file two processes negotiate over
    /// politely.
    ///
    /// **`remote` and nothing else.** No branch, so the remote's own configured refspec decides what
    /// arrives — which is what the person who ran `git remote add` chose, and not something the queue
    /// should second-guess. No `--prune`, which deletes tracking refs; no `--tags`, which fetches a
    /// namespace nobody asked about. Each of those is a different operation wearing this one's name.
    Fetch {
        remote: Remote,
    },
    /// Deleting a branch, in the one spelling that cannot destroy unmerged work.
    ///
    /// `--delete` and never `-D`. The difference is the whole reason this is queueable at all: `-d`
    /// asks git to refuse when the branch holds commits no other branch has, and `-D` asks it not to
    /// care. The safe spelling has a guard that is *git's own*, computed from the commit graph the
    /// daemon does not have to model — so the queue can offer this without owning the question of
    /// what is safe to lose.
    ///
    /// The sha the branch pointed at is recorded, and that is what makes the row an undo: `git branch
    /// <name> <sha>` restores exactly what was removed. It is the one operation whose `result_sha`
    /// describes something that no longer exists, which is precisely when a person needs it.
    ///
    /// **`rename` because `rename_all = "snake_case"` would spell this variant `branch_delete` while
    /// `kind()` spells it `branch-delete`, and one row would then carry both.** What that costs is
    /// legibility rather than correctness, and the distinction is worth stating exactly because the
    /// first version of this comment got it wrong: `from_stored` re-derives `kind()` from the parsed
    /// value rather than reading the payload's tag, so the mismatched row parses back perfectly well
    /// — measured, by a mutation that removed this line and left the round-trip test green. What it
    /// leaves behind is a row whose `op` column says one word and whose `args` payload says another,
    /// for anyone reading the queue or filtering it by JSON path. The hyphen wins because it is the
    /// spelling `from_request` takes from a caller.
    ///
    /// Every other variant is a single word, which is why this appears here first — and it will
    /// appear again for whoever adds a second multi-word one.
    #[serde(rename = "branch-delete")]
    BranchDelete {
        branch: Branch,
    },
    /// Replaying `branch` onto `onto`, and **the only operation here that this pillar's publish
    /// model cannot carry all the way.**
    ///
    /// The model is: compute where nobody is standing, then publish with a command that REFUSES
    /// rather than destroys. `compute_merge` earns the second half by construction — `--no-ff` makes
    /// the old tip the merge's first parent, so `publish` is always a fast-forward, and
    /// `merge --ff-only` in the holder's worktree declines on its own if anything is in the way.
    ///
    /// A rebase breaks that by definition. The rebased tip and the old tip are divergent, so no
    /// fast-forward exists, and there is no git command that moves a checkout across a divergence
    /// while refusing to destroy: `reset --hard` never refuses, and `checkout` refuses but does not
    /// move the branch. **So when somebody holds the branch, this operation is `Blocked` rather than
    /// implemented with a reset.** That is not a gap to be filled later — a queue that hard-resets a
    /// directory a person may be standing in is a different promise from the one this pillar makes,
    /// and `worktree.rs` guards its own deletes with `is_dangerous_removal_path` on exactly that
    /// reasoning.
    ///
    /// What is left is the case that is both safe and common enough to be worth having: a branch
    /// nobody has open, computed in the integration worktree and published by the compare-and-swap
    /// `publish_by_update_ref` already performs. A run's own branch, held by its own paused
    /// worktree, is refused — and refusing it is right rather than merely safe, since rewriting a
    /// branch under a paused run is what would corrupt its state on resume.
    Rebase {
        branch: Branch,
        onto: Branch,
    },
    /// Undoing a merge that is already on `target` (spec 2026-10-05 §6.2, D4), **and the only
    /// operation here that the daemon queues for itself**: the post-merge gate enqueues it when a
    /// bisection confirms a culprit and the project's `revert_on_red` is on. It is not in
    /// `GIT_OP_KINDS`, so no project declares it, and `from_request` refuses it, so no tool call
    /// asks for it. It is not unreachable from outside, though: `POST /vcs/requests`
    /// deserializes an `Op` directly, so a Run-scoped key can submit one (it is held at
    /// `awaiting_approval`) and a Control or Admin key can submit one too.
    ///
    /// `-m 1` and nothing else: the queue's merges are always `--no-ff`, so the first parent is the
    /// line the merge landed on. The revert is a new commit on top of `target`, which makes the
    /// publish a fast-forward like any merge's, and the target moving meanwhile is requeued the
    /// same way. `merge_sha` is an id and not a name, hence its own type.
    Revert {
        merge_sha: CommitSha,
        target: Branch,
    },
}
/// A branch name the daemon is willing to put on a git command line.
///
/// A newtype rather than a validating constructor on `Op`, because the validation has to hold on
/// every route into the queue and there are three: the flat parameters an MCP tool call carries, a
/// raw `POST /vcs/requests` body that deserializes an `Op` directly (`http.rs`), and `Op::from_stored`
/// reading a row back. A `Deserialize` that validates covers all three; a checked constructor covers
/// the first only, which is how `{"op":"merge","source":"--upload-pack=x"}` would have got through.
///
/// The leading-dash rule is the load-bearing one. `Op`'s own doc comment records that `Merge`'s
/// arguments survive today by accident — a dashed string can set an option but cannot also name a
/// commit, and the integration worktree's HEAD is always detached so `merge`'s upstream fallback
/// dies. This is that accident replaced by a rule.
///
/// **This is an argv guard and not a ref validator, and the difference has to be said out loud
/// because the name does not say it.** Empty, a leading `-`, whitespace, control characters — that
/// is the whole list. `feat/x;rm -rf`, `..`, `@{u}`, `HEAD`, a name carrying `~ ^ : ? * [`, a name
/// ending in `.lock`: every one of them passes here, and `git check-ref-format` rejects several. That
/// is fine, and it is fine for a reason rather than by luck. `git_exec::run_git` builds an argv and
/// spawns it with no shell anywhere in the path, so a `;` is not a separator — it is one more
/// character in a ref name, git looks for a branch spelled that way, finds none, and the row records
/// what it said. Git stays the authority on which names resolve; this type only decides which ones
/// may be handed to it. Whoever wants the other guarantee wants `git check-ref-format --branch` or a
/// character allowlist, and owes it its own check rather than a quiet widening of this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Branch(String);
impl Branch {
    pub fn new(value: &str) -> Result<Self, String> {
        argv_safe(value, "branch name").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// PURE: the whole of the argv rule, shared by every name the queue puts on a git command line.
///
/// Shared as a function rather than by making one type serve two roles, and the difference is what a
/// reader can conclude: `Branch` and `Remote` happen to be checked for the same three properties
/// today, and nothing says they must stay that way — a remote is a config key and a branch is a ref,
/// and they answer to different authorities. One type would have made "the queue accepts this
/// remote" and "the queue accepts this branch" literally the same sentence.
fn argv_safe(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("a {what} may not be empty"));
    }
    if value.starts_with('-') {
        return Err(format!("a {what} may not start with '-': {value}"));
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        // Both halves are named, because the message is what a caller reads: told only "whitespace"
        // about a name carrying an ESC it would go looking for a space that is not there.
        return Err(format!(
            "a {what} may not contain whitespace or control characters: {value}"
        ));
    }
    Ok(value.to_owned())
}
/// A remote the daemon is willing to name on a git command line.
///
/// Its own type because `Op`'s doc comment says a variant carrying a name of some other kind owes
/// one, and for the reason `argv_safe` gives about not collapsing two roles into one.
///
/// **An argv guard, not a remote validator.** `origin`, `upstream`, and equally a name no `git
/// remote` in the repository has ever heard of, all pass here — git is the authority on which
/// remotes exist, and a push to one that does not is a failed row carrying git's own message. What
/// this refuses is a name that could act as an option. A URL passes too, and that is worth saying
/// out loud rather than discovering: `git push https://…` is legal, so a caller can push to a
/// destination the repository never configured. It is exactly as legal as the command the caller
/// could have run by hand, and the queue's job here is serialization rather than policy — `wip.rs`
/// and `proposals.rs` are where "may this actor ask for this" is decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Remote(String);
impl Remote {
    pub fn new(value: &str) -> Result<Self, String> {
        argv_safe(value, "remote name").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Remote {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Remote::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
/// The `Branch` counterpart, and it exists for the same reason: tests build remotes from literals.
#[cfg(any(test, feature = "testkit"))]
impl From<&str> for Remote {
    fn from(value: &str) -> Self {
        Remote::new(value).expect("a test used an invalid remote name literal")
    }
}
/// A tag name the daemon is willing to create.
///
/// The third type over `argv_safe`, and the one whose separateness is easiest to justify: `Branch`
/// and `Remote` both name something that already EXISTS and that git will resolve or refuse. A tag
/// name names something this operation brings into being, in a namespace nothing else here writes.
/// One type for all three would have made "the queue accepts this tag" the same sentence as "the
/// queue accepts this branch", and they answer to different authorities the day either rule moves.
///
/// **An argv guard, not a ref validator** — the sentence `Branch` writes out at length, and it holds
/// here with one consequence worth naming rather than leaving to be discovered: `v1..2`, `a~1` and
/// `x.lock` all pass, and `git tag` refuses each of them itself. The row then records git's own
/// message. What this refuses is a name that could act as an option, which is the only thing a
/// string reaching an argv can do that git cannot answer for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TagName(String);
impl TagName {
    pub fn new(value: &str) -> Result<Self, String> {
        argv_safe(value, "tag name").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for TagName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        TagName::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
/// The `Branch` counterpart, for the same reason: tests build tag names from literals.
#[cfg(any(test, feature = "testkit"))]
impl From<&str> for TagName {
    fn from(value: &str) -> Self {
        TagName::new(value).expect("a test used an invalid tag name literal")
    }
}
/// A full commit id the daemon is willing to put on a git command line.
///
/// Its own type because the argv gains a sha and not a ref: `Branch` would accept `master`, a short
/// id or anything not starting with a dash, and a revert of the wrong thing is not a failure git
/// reports. Exactly 40 (SHA-1) or 64 (SHA-256) lowercase hex digits, validated on every route in —
/// including `Deserialize`, which is how a stored or posted row arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitSha(String);
impl CommitSha {
    pub fn new(value: &str) -> Result<Self, String> {
        let well_formed = matches!(value.len(), 40 | 64)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if well_formed {
            Ok(Self(value.to_owned()))
        } else {
            Err(format!(
                "a commit id is 40 or 64 lowercase hex digits, not {value:?}"
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for CommitSha {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CommitSha::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
/// The `Branch` counterpart, for the same reason: tests build commit ids from literals.
#[cfg(any(test, feature = "testkit"))]
impl From<&str> for CommitSha {
    fn from(value: &str) -> Self {
        CommitSha::new(value).expect("a test used an invalid commit id literal")
    }
}
impl<'de> Deserialize<'de> for Branch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Branch::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
/// Tests build branches from literals everywhere. Panicking is right for a literal a developer
/// wrote; production has only the fallible path, and this impl does not exist there.
#[cfg(any(test, feature = "testkit"))]
impl From<&str> for Branch {
    fn from(value: &str) -> Self {
        Branch::new(value).expect("a test used an invalid branch name literal")
    }
}
/// PURE: a caller-supplied branch name for one named role, or why it is not usable as one.
fn named_branch(value: Option<&str>, operation: &str, which: &str) -> Result<Branch, String> {
    let Some(value) = value else {
        return Err(format!("a {operation} needs a {which} branch"));
    };
    Branch::new(value).map_err(|reason| format!("{which}: {reason}"))
}
/// PURE: the same, for the one role that names a remote.
fn named_remote(value: Option<&str>, operation: &str, which: &str) -> Result<Remote, String> {
    let Some(value) = value else {
        return Err(format!("a {operation} needs a {which} remote"));
    };
    Remote::new(value).map_err(|reason| format!("{which}: {reason}"))
}
/// PURE: the same, for the one role that names a tag.
fn named_tag(value: Option<&str>, operation: &str, which: &str) -> Result<TagName, String> {
    let Some(value) = value else {
        return Err(format!("a {operation} needs a {which} tag name"));
    };
    TagName::new(value).map_err(|reason| format!("{which}: {reason}"))
}
impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Merge { .. } => "merge",
            Op::Push { .. } => "push",
            Op::Tag { .. } => "tag",
            Op::Fetch { .. } => "fetch",
            Op::BranchDelete { .. } => "branch-delete",
            Op::Rebase { .. } => "rebase",
            Op::Revert { .. } => "revert",
        }
    }

    /// Builds an operation from the flat parameters a tool call carries.
    ///
    /// Flat rather than the tagged union `Op` serialises to, because the caller on the other side is
    /// a language model reading a tool description: the union is the right wire shape and the wrong
    /// prompt. It is a convenience over `Branch`, not a boundary — the boundary is the type.
    ///
    /// Every rejection names what is wrong, and the rejection for an operation the SPEC lists but
    /// the executor cannot perform yet is deliberately different from the one for a word that is not
    /// an operation at all. A caller told "unknown operation: rebase" would go looking for a typo in
    /// its own request; a caller told "rebase is not queued yet" knows to wait or do something else.
    ///
    /// **`source` and `target` mean the same two things for both operations, and that is why the
    /// push arm reads backwards at first glance.** `source` is what is being moved and `target` is
    /// where it goes: for a merge, a branch into a branch; for a push, a branch to a remote. Naming
    /// the fields after the operation instead would give the tool description two vocabularies for
    /// one pair of parameters, which is the thing this flat shape exists to avoid.
    pub fn from_request(
        operation: &str,
        source: Option<&str>,
        target: Option<&str>,
    ) -> Result<Self, String> {
        match operation.trim().to_ascii_lowercase().as_str() {
            "merge" => Ok(Op::Merge {
                source: named_branch(source, "merge", "source")?,
                target: named_branch(target, "merge", "target")?,
            }),
            "push" => Ok(Op::Push {
                branch: named_branch(source, "push", "source")?,
                remote: named_remote(target, "push", "target")?,
            }),
            // The same reading of `source`/`target` the paragraph above sets out, and a tag is the
            // case that shows it is a rule rather than a coincidence: what MOVES is the branch's tip,
            // and where it GOES is a new name. A tag is a destination in the sense that matters here
            // — a ref this operation writes — which is why it is the target and not the source.
            "tag" => Ok(Op::Tag {
                at: named_branch(source, "tag", "source")?,
                name: named_tag(target, "tag", "target")?,
            }),
            // The two that break the `source`/`target` pair rather than following it, and reading
            // the missing half as an error is the point: a fetch names only where it fetches FROM,
            // and a branch delete names only what goes. Inventing a second parameter to keep the
            // shape symmetrical would give a caller a field it must leave empty and a tool
            // description a word it must ignore.
            "fetch" => Ok(Op::Fetch {
                remote: named_remote(target, "fetch", "target")?,
            }),
            "branch-delete" => Ok(Op::BranchDelete {
                branch: named_branch(source, "branch-delete", "source")?,
            }),
            // `source`/`target` as everywhere else, and a rebase is the case where the pair is least
            // obvious: what MOVES is the branch's commits, and where they GO is on top of `onto`.
            "rebase" => Ok(Op::Rebase {
                branch: named_branch(source, "rebase", "source")?,
                onto: named_branch(target, "rebase", "target")?,
            }),
            // Neither unknown nor deferred: the daemon performs it, and only the daemon asks.
            "revert" => Err(
                "revert is not something a caller asks for: the daemon's post-merge \
                 gate queues it when a bisection confirms a culprit (spec 2026-10-05 §6.2)"
                    .to_owned(),
            ),
            // **What is left on the spec's list of nine is NOT a backlog, and this arm no longer
            // pretends it is.** Every name below has been decided against, each on its own grounds,
            // and the message says so — a caller told "not yet" waits for a version that is never
            // coming, which is a worse answer than a refusal it can act on.
            //
            // - `pr-merge` is the only one of the nine that is not git. It would put a second binary
            //   with its own authentication, its own network failures and its own release cadence
            //   inside the executor — and it would buy none of what this queue is made of, since a
            //   merge on GitHub's servers is not serialised by a lock held on this machine.
            // - `pull` is `fetch` then `merge`, and this queue can already do both. Admitting it as
            //   ONE row would have the queue promise an atomicity it does not have: the two halves
            //   are separate git invocations, another request can be claimed between them only
            //   because it cannot — but a single row that half-succeeded would be recorded as one
            //   failure with no way to say which half. Two rows say exactly what happened, and the
            //   second is `merge` with a source of `origin/<branch>`, which `Branch` already accepts
            //   and `compute_merge` already resolves.
            // - `worktree-add` and `worktree-remove` belong to `worktree.rs`, which owns that
            //   lifecycle entire: the naming scheme `owner_from_dir_name` parses, the orphan sweeper,
            //   the removal backoff, and `is_dangerous_removal_path`. `git_exec.rs` already refuses
            //   to run `worktree prune` up front for this exact reason — "doing that on every merge
            //   would quietly make this module a co-owner of a lifecycle it has no business in" — and
            //   executing the other two here would be that same mistake, made deliberately.
            //
            // Nothing is deferred any more: the spec's nine are six this queue performs and three it
            // has decided against, each on its own grounds.
            other @ ("pull" | "worktree-add" | "worktree-remove" | "pr-merge") => Err(format!(
                "{other} is not an operation this queue performs, and will not become one — see \
                 `Op::from_request` for why. It understands merge, push, tag, fetch, branch-delete \
                 and rebase"
            )),
            other => Err(format!(
                "unknown operation: {other} — the queue understands merge, push, tag, fetch, \
                 branch-delete and rebase"
            )),
        }
    }

    pub fn to_args(&self) -> String {
        serde_json::to_string(self).expect("an Op is always serializable")
    }

    /// `kind` is the column, `args` the JSON payload. They are stored apart so the queue can be
    /// filtered by operation without parsing every row, which means they can also disagree — so
    /// the parse is checked against the column rather than trusted.
    pub fn from_stored(kind: &str, args: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(args).map_err(|error| error.to_string())?;
        if parsed.kind() != kind {
            return Err(format!(
                "stored op column {kind} disagrees with its payload"
            ));
        }
        Ok(parsed)
    }
}
/// Every operation kind this queue can construct, as `Op::kind()` spells them.
///
/// A list rather than an `Op::all()`, because the variants carry data and a catalogue has no
/// arguments to invent. What keeps the two honest is
/// `every_op_kind_is_in_the_declarable_catalogue`, which builds one of each variant and asks: a
/// seventh operation that never reached this list would be one the queue performs and no project
/// can declare, and nothing else in the tree would say so.
pub const GIT_OP_KINDS: [&str; 6] = ["merge", "push", "tag", "fetch", "rebase", "branch-delete"];
/// The operation kinds only the daemon queues. They are outside `GIT_OP_KINDS` on purpose: no
/// project declares them, the picker never draws them and `Op::from_request` refuses them.
/// `every_op_kind_is_in_the_declarable_catalogue` requires every kind to sit in one list or the
/// other. `admit` refuses these kinds when they come from a run or a job origin.
pub const DAEMON_OP_KINDS: [&str; 1] = ["revert"];
/// One operation a project may declare, with the flag the shell draws it by.
///
/// `declarable` is always `true` today and the field is not decoration: the GitHub catalogue's
/// `false` rows are how an operation this build refuses is drawn as a FACT rather than as a
/// checkbox that cannot be ticked. Serving the shape now means the day a ceiling arrives is a
/// change of value and not a change of wire.
///
/// **Not a reuse of `github::OpStanding`/`DeclarableOpView`**: those carry `half`, which separates
/// a read that is in force from an action that is recorded and inert. These six are all writes the
/// queue performs the same way, and a `half` here would be a field with no meaning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeclarableGitOp {
    pub kind: &'static str,
    pub declarable: bool,
}
/// PURE: `GIT_OP_KINDS`, wrapped for the wire a picker draws its checkboxes from.
///
/// The GitHub analogue, `github::every_op`, has to consult two ceilings and stamp a `half` per
/// operation because its catalogue is bigger than what a project may ask for. This queue has no
/// such gap: everything in `GIT_OP_KINDS` is something the queue performs and something a project
/// may declare, in lockstep, so there is nothing here to compute — every row comes back
/// `declarable: true`, and the day that stops being true is the day this stops being a `map`.
pub fn declarable_git_ops() -> Vec<DeclarableGitOp> {
    GIT_OP_KINDS
        .iter()
        .map(|kind| DeclarableGitOp {
            kind,
            declarable: true,
        })
        .collect()
}
/// The statuses a request can no longer leave.
///
/// A positive list rather than `NOT IN ('queued','running','awaiting_approval')`, and
/// `council::prune` argues which way that mistake falls: a status added later and forgotten here
/// simply never ages out and never ends a wait early — a caller kept waiting and a row too many,
/// rather than a row swept while something was still writing to it.
///
/// **`rejected` is in this list and nothing writes it**, which is the one entry worth arguing.
/// `Ticket`'s doc comment records that it is in the `status` column's CHECK constraint and that
/// `cancelled` sat in exactly that position until `cancel_for_run` arrived. Both readers of this
/// list want it there before that happens: a wait on a rejected row would otherwise run to the
/// deadline on a row that can never change, and its tail would never age out. Neither is observable
/// today, and both become wrong silently on the day the status is first written.
/// **`escalated` is terminal, and that is a decision rather than a technicality.** A person owning
/// a conflict is the END of this request: nothing resumes it, and what they decide arrives as a new
/// one. Had it been non-terminal it would need excluding from the retention sweep and from
/// `wait_for`, and the queue would have gained a second kind of liveness to reason about — one where
/// a row is neither finished nor going to move without a human, which is exactly the state that
/// accumulates unnoticed.
pub const TERMINAL_STATUSES: [&str; 7] = [
    "succeeded",
    "failed",
    "blocked",
    "rejected",
    "cancelled",
    "interrupted",
    "escalated",
];
#[cfg(test)]
mod tests {
    // `a_real_merge_lands_through_the_queue` holds `worktree::test_env_lock()`'s `MutexGuard` across
    // every await in it, and that is the point rather than an oversight: the NUCLEOS_WORKTREE_ROOT
    // override it serialises is process-wide, so it has to be held for the whole test. These are
    // `current_thread` tests with no multi-thread runtime to starve, so `await_holding_lock` is a
    // false positive — the same one `job.rs`, `runs.rs`, `worktree.rs` and `git_exec.rs` each carry.
    // An inner attribute, so it must precede every item in the module.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    use std::time::Duration;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// An empty database with the schema exactly as it stood after migration `version`.
    ///
    /// **This is what makes a DATA migration testable at all in this repository.**
    /// `sqlx::migrate!().run()` applies the whole chain against empty tables, so every `UPDATE` in
    /// every migration has always been unreachable by the suite: delete one and nothing goes red.
    /// `email.rs:812` records the same limitation, and the vcs pillar's own handoff carries it as the
    /// one untested guarantee it could not close. Stopping the chain part-way and putting rows in the
    /// gap is all it needed.
    ///
    /// Both helpers now live in `crate::testdb`, on the instruction the note here used to carry:
    /// they are not vcs-specific, and `0129`'s backfill was the second module to need them.
    use crate::testdb::{apply_migrations_after, pool_migrated_through};

    fn repo() -> ResolvedRepo {
        repo_for("alpha")
    }

    /// In tests the repository key is the project name. That keeps every existing `claim_next(&pool,
    /// "alpha")` meaning what it meant, so the rewrite cannot silently swap a key for a label — and it
    /// leaves `two_projects_naming_one_repository_cannot_both_be_running` as the one place where the two
    /// deliberately differ.
    fn repo_for(project: &str) -> ResolvedRepo {
        ResolvedRepo::synthetic(project, "C:/repo", project)
    }

    fn merge_op() -> Op {
        Op::Merge {
            source: "feat/x".into(),
            target: "master".into(),
        }
    }

    async fn status_of(pool: &sqlx::SqlitePool, id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// `expect` rather than defaulting a NULL to `""`: a regression that wrote no reason at all is
    /// exactly what this is for, and collapsing it into an empty string turns that into a bare
    /// `assertion failed` at the call site with no value to read.
    async fn failure_reason_of(pool: &sqlx::SqlitePool, id: i64) -> String {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
        .expect("a failed request records why it failed")
    }

    /// Returns the `Option` rather than `unwrap_or_default()`ing it: NULL and `""` are different
    /// things in this column — NULL means the row never reached an argv — and collapsing them would
    /// erase exactly the distinction its callers are checking.
    async fn output_tail_of(pool: &sqlx::SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("SELECT output_tail FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run_id_of(pool: &sqlx::SqlitePool, id: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT run_id FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A payload `Op::from_stored` accepts, for rows written straight to the table.
    const MERGE_ARGS: &str = r#"{"op":"merge","source":"feat/x","target":"master"}"#;

    /// The one place that knows the column list. `args` is a parameter because the rows worth
    /// writing by hand are exactly the ones `submit` cannot produce — a payload that will not
    /// parse, or a status no caller can reach yet.
    async fn insert(
        pool: &sqlx::SqlitePool,
        project: &str,
        status: &str,
        args: &str,
    ) -> sqlx::Result<i64> {
        // `repo_key` is bound to the same project this is given, for the reason `repo_for` states:
        // in tests the repository key is the project name. Left to the column's `''` default these
        // rows would be invisible to every `claim_next` and would collide with each other on the
        // partial unique index — two failures with nothing to do with what any caller is testing.
        insert_keyed(pool, project, project, status, args).await
    }

    /// `insert` with its two identifying columns pulled apart.
    ///
    /// Every other caller wants them equal, which is why `insert` binds one string to both — and
    /// which is exactly why a test about *which* of them the queue is keyed on cannot go through it.
    async fn insert_keyed(
        pool: &sqlx::SqlitePool,
        project: &str,
        repo_key: &str,
        status: &str,
        args: &str,
    ) -> sqlx::Result<i64> {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, status, created_at)
             VALUES ('merge', ?, ?, 'C:/repo', ?, 'human', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(args)
        .bind(project)
        .bind(repo_key)
        .bind(status)
        .execute(pool)
        .await
        .map(|inserted| inserted.last_insert_rowid())
    }

    /// Exclusivity is the database's job, not a Mutex's: a Mutex does not survive a daemon restart
    /// and this index does. Asserted sequentially on purpose — the constraint is what is under
    /// test, and the pool helper is `max_connections(1)`, so a "concurrent" version would prove
    /// less and flake more.
    #[tokio::test]
    async fn only_one_request_may_run_per_repository() {
        let pool = test_pool().await;

        insert(&pool, "alpha", "running", MERGE_ARGS)
            .await
            .expect("the first running request is allowed");

        let second = insert(&pool, "alpha", "running", MERGE_ARGS).await;
        assert!(
            second.is_err(),
            "a second running request for the same repository must be rejected"
        );

        insert(&pool, "beta", "running", MERGE_ARGS)
            .await
            .expect("a different repository is not blocked by alpha's running request");

        for _ in 0..3 {
            insert(&pool, "alpha", "queued", MERGE_ARGS)
                .await
                .expect("queued requests are not limited — only running is");
        }
    }

    /// **What the backstop is keyed on**, which is the half the test above structurally cannot see.
    ///
    /// It goes through `insert`, which binds `project_id` and `repo_key` to one string by design —
    /// so it pins that the index is unique and never that it is unique *on the repository*.
    /// Re-pointing the index at `project_id`, the column `0048` had it on and `0049` deliberately
    /// moved it off, leaves it green. That is not a small drift: the whole of this module's promise
    /// is that a project is a label somebody chose and a repository is what a merge actually
    /// touches, so two labels naming one repository must not both run. Only two rows where those
    /// two columns disagree can hold it.
    ///
    /// The assertion is on the *constraint*, not on `is_err()`. An insert that failed for any other
    /// reason — a CHECK on `status`, a column list gone stale — satisfies a bare `is_err()` and
    /// proves nothing about the index it claims to be about.
    #[tokio::test]
    async fn the_backstop_locks_on_the_repository_and_not_on_the_project() {
        let pool = test_pool().await;

        insert_keyed(&pool, "alpha", "SHARED", "running", MERGE_ARGS)
            .await
            .expect("the first running request is allowed");

        let rejected = insert_keyed(&pool, "beta", "SHARED", "running", MERGE_ARGS)
            .await
            .expect_err("a second project running against the same repository must be rejected");

        let database_error = rejected
            .as_database_error()
            .expect("the rejection must come from the database, not from sqlx's own plumbing");
        assert!(
            database_error.is_unique_violation(),
            "the rejection must be the unique index and not some other constraint: {database_error}"
        );
        assert!(
            database_error.to_string().contains("repo_key"),
            "the index that rejected this must be the one keyed on repo_key: {database_error}"
        );
    }

    /// The mark has to survive from admission to claim, and those two are a daemon restart apart —
    /// which is why it is written onto the row rather than worked out when the queue gets there.
    /// Everything the resolver's safety rests on hangs off this boolean: a marked source is verified
    /// before it is merged, and an unmarked one is not, so a mark that failed to travel would let the
    /// merge that most needs checking through unchecked.
    #[tokio::test]
    async fn only_a_landing_that_came_out_of_a_resolution_is_claimed_for_verification() {
        let pool = test_pool().await;
        let ordinary = submit(&pool, &repo(), &merge_op(), Origin::Shell)
            .await
            .unwrap();
        let resolved = submit_resolution(&pool, &repo(), &merge_op(), Origin::Shell)
            .await
            .unwrap();

        let first = claim_next(&pool, "alpha")
            .await
            .unwrap()
            .expect("the first request is claimable");
        assert_eq!(first.id, ordinary);
        assert!(
            !first.from_resolution,
            "an ordinary landing must not be held to a resolution's shape — a single-parent tip is \
             what every branch has"
        );
        finish(
            &pool,
            first.id,
            Outcome::Succeeded {
                sha: None,
                output_tail: String::new(),
            },
        )
        .await
        .unwrap();

        let second = claim_next(&pool, "alpha")
            .await
            .unwrap()
            .expect("the second follows once the repository is free");
        assert_eq!(second.id, resolved);
        assert!(
            second.from_resolution,
            "a resolution's landing has to arrive at the executor marked, or nothing verifies it"
        );
    }

    fn steps(dirs: &[&str]) -> SegmentDir {
        SegmentDir::Steps(dirs.iter().map(|dir| (*dir).to_owned()).collect())
    }

    /// The directory a segment runs in is what the line says, not the session's: every followable
    /// `cd` before it, then its own `-C`, which is lifted out so the parsers read the plain shape.
    #[test]
    fn a_segment_carries_the_directory_its_line_moves_it_to() {
        let read = |command: &str| {
            directed_segments(command)
                .into_iter()
                .map(|segment| (segment.dir, segment.command))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            read("git merge master"),
            vec![(SegmentDir::Session, "git merge master".to_owned())]
        );
        assert_eq!(
            read("cd /c/Projects/wt && git merge master"),
            vec![(steps(&["/c/Projects/wt"]), "git merge master".to_owned())]
        );
        assert_eq!(
            read("git -C C:/Projects/wt merge master"),
            vec![(steps(&["C:/Projects/wt"]), "git merge master".to_owned())]
        );
        assert_eq!(
            read("cd \"C:/Projects\" && git -C wt -C sub merge --no-ff master"),
            vec![(
                steps(&["C:/Projects", "wt", "sub"]),
                "git merge --no-ff master".to_owned()
            )]
        );
        assert_eq!(
            read("Set-Location C:/Projects/wt; git branch -d old"),
            vec![(steps(&["C:/Projects/wt"]), "git branch -d old".to_owned())]
        );
        // Other global flags are left where they are, so the parsers still refuse the spelling.
        assert_eq!(
            read("git -c a=b merge master"),
            vec![(SegmentDir::Session, "git -c a=b merge master".to_owned())]
        );

        for lost in [
            "cd && git merge master",
            "cd - && git merge master",
            "cd ~/wt && git merge master",
            "cd $WT && git merge master",
            "cd -P wt && git merge master",
            "popd && git merge master",
            "(cd wt) && git merge master",
            "git -C $WT merge master",
        ] {
            let found = directed_segments(lost);
            let last = found.last().expect("the git segment is still there");
            assert!(
                matches!(last.dir, SegmentDir::Unfollowable(_)),
                "{lost}: {last:?}"
            );
            assert_eq!(last.command, "git merge master", "{lost}");
        }
    }

    #[test]
    fn a_git_bash_drive_path_reads_as_its_windows_spelling() {
        assert_eq!(
            msys_drive_path("/c/Projects/wt").as_deref(),
            Some("C:/Projects/wt")
        );
        assert_eq!(msys_drive_path("/d").as_deref(), Some("D:/"));
        assert_eq!(msys_drive_path("/tmp/x"), None);
        assert_eq!(msys_drive_path("C:/Projects"), None);
        assert_eq!(msys_drive_path("wt"), None);
    }

    #[test]
    fn steps_resolve_from_the_session_each_absolute_or_relative_to_the_last() {
        let session = std::path::Path::new("C:/Projects/nucleos");
        assert_eq!(
            resolve_segment_dir(session, &["../wt".to_owned()]),
            session.join("../wt")
        );
        let absolute = if cfg!(windows) {
            "D:/elsewhere"
        } else {
            "/elsewhere"
        };
        assert_eq!(
            resolve_segment_dir(session, &[absolute.to_owned(), "sub".to_owned()]),
            std::path::Path::new(absolute).join("sub")
        );
        if cfg!(windows) {
            assert_eq!(
                resolve_segment_dir(session, &["/c/Projects/wt".to_owned()]),
                std::path::Path::new("C:/Projects/wt")
            );
        }
    }

    /// Round-tripping through the stored form is the point: the row is the contract between the
    /// submitting process and the worker, which may be a daemon restart apart.
    #[test]
    fn an_operation_round_trips_through_its_stored_form() {
        let op = Op::Merge {
            source: "feat/x".into(),
            target: "master".into(),
        };
        let back =
            Op::from_stored(op.kind(), &op.to_args()).expect("a stored operation must parse back");
        assert_eq!(back, op);
    }

    #[test]
    fn every_op_kind_is_in_the_declarable_catalogue() {
        // One of each variant, so adding a seventh to `Op` fails here rather than shipping an
        // operation the queue performs and no project can declare.
        let one_of_each = [
            Op::Merge {
                source: Branch::new("a").unwrap(),
                target: Branch::new("b").unwrap(),
            },
            Op::Push {
                remote: Remote::new("origin").unwrap(),
                branch: Branch::new("a").unwrap(),
            },
            Op::Tag {
                name: TagName::new("v1").unwrap(),
                at: Branch::new("a").unwrap(),
            },
            Op::Fetch {
                remote: Remote::new("origin").unwrap(),
            },
            Op::BranchDelete {
                branch: Branch::new("a").unwrap(),
            },
            Op::Rebase {
                branch: Branch::new("a").unwrap(),
                onto: Branch::new("b").unwrap(),
            },
            Op::Revert {
                merge_sha: CommitSha::new(&"a".repeat(40)).unwrap(),
                target: Branch::new("b").unwrap(),
            },
        ];
        for op in &one_of_each {
            assert!(
                GIT_OP_KINDS.contains(&op.kind()) || DAEMON_OP_KINDS.contains(&op.kind()),
                "{} is neither declarable nor a daemon-only operation",
                op.kind()
            );
        }
        assert_eq!(
            GIT_OP_KINDS.len() + DAEMON_OP_KINDS.len(),
            one_of_each.len()
        );
    }

    /// D4 of the post-merge gate: a revert is something the daemon queues, never something a
    /// project declares. It must survive storage like any other operation, and it must sit in
    /// the daemon-only vocabulary and OUT of the declarable one the picker draws from.
    #[test]
    fn a_revert_round_trips_through_storage_and_is_not_declarable() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let op = Op::Revert {
            merge_sha: CommitSha::new(sha).unwrap(),
            target: Branch::new("master").unwrap(),
        };

        assert_eq!(op.kind(), "revert");
        let back = Op::from_stored(op.kind(), &op.to_args()).expect("a stored revert parses back");
        assert_eq!(back, op);

        let payload: serde_json::Value = serde_json::from_str(&op.to_args()).unwrap();
        assert_eq!(payload["op"].as_str(), Some("revert"));
        assert_eq!(payload["merge_sha"].as_str(), Some(sha));
        assert_eq!(payload["target"].as_str(), Some("master"));

        assert!(
            !GIT_OP_KINDS.contains(&"revert"),
            "a project must not be able to declare a revert"
        );
        assert!(DAEMON_OP_KINDS.contains(&"revert"));
    }

    /// Nobody asks for a revert through a tool call, and the commit it names is an id and never a
    /// ref: the argv gains a sha, so a branch name, a short id or an option must not get in.
    #[test]
    fn a_revert_is_refused_from_a_tool_call_and_its_sha_must_be_a_full_hex_id() {
        let full = "0123456789abcdef0123456789abcdef01234567";
        let refused = Op::from_request("revert", Some(full), Some("master")).unwrap_err();
        assert!(
            refused.contains("post-merge gate"),
            "the refusal must say who queues it: {refused}"
        );
        assert!(
            !refused.contains("unknown") && !refused.contains("not yet"),
            "neither an unknown operation nor a deferred one: {refused}"
        );

        // A SHA-1 (40) and a SHA-256 (64) object id are the only accepted lengths.
        assert!(CommitSha::new(full).is_ok());
        assert!(CommitSha::new(&"f".repeat(64)).is_ok());
        let bad_ids: Vec<String> = vec![
            String::new(),
            "abc1234".to_string(),
            "master".to_string(),
            "--upstream=x".to_string(),
            "A".repeat(40),
            "g".repeat(40),
            "a".repeat(41),
            "a".repeat(39),
        ];
        for bad in &bad_ids {
            assert!(CommitSha::new(bad).is_err(), "{bad:?} must be refused");
        }

        // The stored form is validated on the way back in as well.
        let stored =
            |sha: &str| format!(r#"{{"op":"revert","merge_sha":"{sha}","target":"master"}}"#);
        assert!(Op::from_stored("revert", &stored(full)).is_ok());
        assert!(Op::from_stored("revert", &stored("master")).is_err());
        assert!(Op::from_stored("revert", &stored("abc1234")).is_err());
    }

    #[test]
    fn an_unknown_operation_is_refused_rather_than_guessed() {
        assert!(Op::from_stored("rm_rf", "{}").is_err());
    }

    /// The column and the payload can disagree — a row edited by hand, or a bug that wrote one
    /// without the other. Trusting the payload would let a `merge` row execute as something else the
    /// moment a second variant exists.
    ///
    /// NOTE: with a single variant this refusal comes from serde's unknown-tag error, not from the
    /// `kind` comparison — every payload that parses at all is a `Merge`, so that branch is
    /// unreachable by construction today. The guard is written now because the moment Chunk 4 adds
    /// `Push` it stops being unreachable and starts being the thing that prevents a merge row from
    /// executing as a push. **Chunk 4 must add the case that actually covers it:**
    /// `Op::from_stored("push", <a merge payload>)`.
    #[test]
    fn a_payload_that_contradicts_its_column_is_refused() {
        assert!(Op::from_stored("merge", r#"{"op":"rm_rf"}"#).is_err());
    }

    /// A human's order in an interactive session already is the approval — asking again two
    /// seconds later is friction with no safety gain.
    #[tokio::test]
    async fn a_human_request_needs_no_second_approval() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        assert_eq!(status_of(&pool, id).await, "queued");
    }

    /// The shell speaks for the human sitting in front of it, so its requests queue on the same
    /// terms rather than asking a second time.
    ///
    /// This is also the only thing that constructs `Origin::Shell` at all, and the `origin` column
    /// is CHECK-constrained: an `as_str` that spelled this variant any other way would fail every
    /// real shell submit at runtime, and nothing else here would notice. Reading the column back is
    /// the half that proves it — the status assertion alone passes for any accepted spelling.
    #[tokio::test]
    async fn a_shell_request_carries_the_same_approval_a_human_s_does() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Shell)
            .await
            .unwrap();

        assert_eq!(status_of(&pool, id).await, "queued");
        let origin: String = sqlx::query_scalar("SELECT origin FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(origin, "shell");
    }

    /// An autonomous run's request is not a human's order; nothing has consented to it yet, so it
    /// must wait for a human before it can queue.
    #[tokio::test]
    async fn an_autonomous_request_waits_for_approval_before_it_can_queue() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        assert_eq!(status_of(&pool, id).await, "awaiting_approval");
    }

    /// A job's id must not land in a column named `run_id`.
    ///
    /// The two ids come from different sequences, so a job written there reads as a run that
    /// happens to share its number — wrong in the way that looks right. Neither admission test
    /// above would notice: both assert only on `status`, so binding NULL always, or binding the
    /// job id too, passes them. This test is the only thing holding that decision in place.
    #[tokio::test]
    async fn only_a_run_puts_its_id_in_run_id() {
        let pool = test_pool().await;

        let from_run = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        let from_job = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Job(7))
            .await
            .unwrap();

        assert_eq!(run_id_of(&pool, from_run).await, Some(7));
        assert_eq!(run_id_of(&pool, from_job).await, None);
    }

    /// The contrast `submit_declared` exists to draw: through the ORDINARY door, `Origin::Run` still
    /// waits for a human, exactly as `an_autonomous_request_waits_for_approval_before_it_can_queue`
    /// above already pins. What follows is the declared door landing the same origin somewhere else.
    #[tokio::test]
    async fn a_declared_git_op_from_a_run_is_queued_rather_than_awaiting_approval() {
        // The row records that a RUN asked — `Origin::Human` would be a lie of the kind this module
        // already refuses for `Shell` — and it starts `queued`, because the project consented before
        // the command existed.
        let pool = test_pool().await;
        let repo = repo_for("alpha");
        let op = Op::Push {
            remote: Remote::new("origin").unwrap(),
            branch: Branch::new("feat/x").unwrap(),
        };
        let id = submit_declared(&pool, &repo, &op, Origin::Run(7))
            .await
            .unwrap();

        assert_eq!(status_of(&pool, id).await, "queued");
        let origin: String = sqlx::query_scalar("SELECT origin FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(origin, "run");
        assert_eq!(run_id_of(&pool, id).await, Some(7));
    }

    /// `run_id` now travels from the claim into the feed row `drain_once` writes for it — the point
    /// of this task. Before it, `drain_once` hard-coded `None` there (see this file's own account of
    /// why that used to be safe, on `finish`'s `Ok(())` arm above), so a declared git op finished
    /// leaving no trace of which run it was for anywhere a person actually looks.
    #[tokio::test]
    async fn a_finished_git_op_names_its_run_in_the_feed() {
        let pool = test_pool().await;
        // A live run, or `reap_requests_of_ended_runs` — `drain_once`'s own pull-half sweep, run
        // before every claim — reaps this request as belonging to a run whose row does not exist,
        // exactly as `a_deleted_runs_queued_git_op_is_reaped_with_the_already_ended_reason` pins.
        // This test is about what a request that DOES get claimed and finished carries, so the run
        // has to still be alive when the drain runs.
        insert_run(&pool, 7, "running").await;
        let repo = repo_for("alpha");
        submit_declared(&pool, &repo, &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let feed_run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            feed_run_id,
            Some(7),
            "the feed row for a declared git op must name the run that asked for it"
        );
    }

    /// The declared door's own version of `cancelling_a_run_cancels_the_requests_it_had_not_started`
    /// — pinned separately because that test does not check the exact reason a reader sees, and the
    /// reason is the whole point of a cancellation nobody watched happen.
    #[tokio::test]
    async fn a_timed_out_runs_queued_git_op_is_cancelled_with_the_run_ended_reason() {
        let pool = test_pool().await;
        insert_run(&pool, 7, "timed_out").await;
        let repo = repo_for("alpha");
        let id = submit_declared(&pool, &repo, &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        assert_eq!(cancel_for_run(&pool, 7).await.unwrap(), 1);
        assert_eq!(status_of(&pool, id).await, "cancelled");
        assert_eq!(
            failure_reason_of(&pool, id).await,
            "the run that asked for this ended before it started"
        );
    }

    /// The declared door's version of `a_request_whose_run_row_is_gone_is_reaped_rather_than_kept_for_ever`
    /// — pinned separately for the same reason the cancellation twin above is: the exact reason is
    /// what a person reads, and nothing else here checks it.
    #[tokio::test]
    async fn a_deleted_runs_queued_git_op_is_reaped_with_the_already_ended_reason() {
        let pool = test_pool().await;
        insert_run(&pool, 7, "running").await;
        let repo = repo_for("alpha");
        let id = submit_declared(&pool, &repo, &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        sqlx::query("DELETE FROM runs WHERE id = 7")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
            1
        );
        assert_eq!(status_of(&pool, id).await, "cancelled");
        assert_eq!(
            failure_reason_of(&pool, id).await,
            "the run that asked for this had already ended when the queue reached it"
        );
    }

    /// `completed` is deliberately absent from `runs::ENDED_RUN_STATUSES` — a run that finished its
    /// work normally asked for this git op and should have it, exactly as this constant's own doc
    /// comment argues. The reaper is what proves that in code, because it is the one of the two
    /// retirement paths that reads run status at all: `cancel_for_run` takes no status and is never
    /// called for a `completed` run in the first place — `runs.rs` gates that call on `ends_the_run`,
    /// which shares this same list — so there is nothing for a `completed`-run test to exercise there.
    #[tokio::test]
    async fn a_completed_runs_declared_git_op_is_not_reaped() {
        assert!(
            !crate::execution::runs::ENDED_RUN_STATUSES.contains(&"completed"),
            "a run that finished normally asked for this merge and should have it — that is why \
             `completed` is deliberately excluded from the list the reaper reads"
        );

        let pool = test_pool().await;
        insert_run(&pool, 7, "completed").await;
        let repo = repo_for("alpha");
        let id = submit_declared(&pool, &repo, &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        assert_eq!(
            reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
            0
        );
        assert_eq!(status_of(&pool, id).await, "queued");
    }

    #[tokio::test]
    async fn the_queue_is_served_in_arrival_order() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert_eq!(claim_next(&pool, "alpha").await.unwrap().unwrap().id, first);
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "the second request must wait: alpha already has one running"
        );

        finish(
            &pool,
            first,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            claim_next(&pool, "alpha").await.unwrap().unwrap().id,
            second
        );
    }

    /// Serializing repositories that cannot touch each other would make this a bottleneck rather than
    /// a brake.
    #[tokio::test]
    async fn separate_repositories_do_not_wait_on_each_other() {
        let pool = test_pool().await;
        submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
        assert!(claim_next(&pool, "beta").await.unwrap().is_some());
    }

    /// Two project ids, one repository. The queue's promise is per REPOSITORY, so the second waits.
    ///
    /// Before this chunk both were claimable at once: the unique index and the claim both filtered on
    /// `project_id` while the git that would run used `project_root`. Inert only because nothing in
    /// production built a request.
    #[tokio::test]
    async fn two_projects_naming_one_repository_cannot_both_be_running() {
        let pool = test_pool().await;
        let alpha = ResolvedRepo::synthetic("alpha", "C:/repo", "SHARED");
        let beta = ResolvedRepo::synthetic("beta", "C:/repo", "SHARED");

        submit(&pool, &alpha, &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &beta, &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(claim_next(&pool, "SHARED").await.unwrap().is_some());
        assert!(
            claim_next(&pool, "SHARED").await.unwrap().is_none(),
            "the second project claimed the repository the first is holding"
        );
    }

    /// Migration 0049's data half, which until now nothing could reach.
    ///
    /// Both `UPDATE`s in that file could be deleted with the whole suite green, and it is the one
    /// instruction in this pillar that runs exactly once, on a real database, with no rehearsal.
    /// What it has to get right is a pair: the backfill must key every existing row, and every row
    /// that was still LIVE must be retired — because a backfilled key is a project LABEL, so a
    /// surviving `queued` row would be claimable under `alpha` while a new request for the same
    /// repository holds its real key, which is two operations against one repository and the exact
    /// defect the migration exists to remove.
    #[tokio::test]
    async fn migration_0049_keys_every_row_and_retires_the_live_ones() {
        let pool = pool_migrated_through(48).await;

        // The pre-0049 shape: no `repo_key` column exists yet, which is itself part of the test —
        // naming it here would fail to compile against the schema this row is written into.
        for (id, status) in [
            (1, "queued"),
            (2, "running"),
            (3, "awaiting_approval"),
            (4, "succeeded"),
            (5, "failed"),
            (6, "cancelled"),
        ] {
            sqlx::query(
                "INSERT INTO vcs_requests
                 (id, op, args, project_id, project_root, origin, status, created_at)
                 VALUES (?, 'merge', ?, 'alpha', 'C:/repo', 'human', ?, '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(MERGE_ARGS)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }

        apply_migrations_after(&pool, 48).await;

        // Named rather than a tuple, for the reason `RequestSummary` gives two hundred lines up:
        // four of these five are `Option<String>` or `String`, so a positional read of the wrong
        // column would compile and pass. Clippy asks for the same thing from the other direction.
        #[derive(Debug, sqlx::FromRow)]
        struct Row {
            repo_key: String,
            status: String,
            failure_reason: Option<String>,
            finished_at: Option<String>,
        }

        let rows: Vec<Row> = sqlx::query_as(
            "SELECT repo_key, status, failure_reason, finished_at FROM vcs_requests ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(
            rows.iter().all(|row| row.repo_key == "alpha"),
            "every row is keyed from its project_id: {rows:?}"
        );

        let statuses: Vec<&str> = rows.iter().map(|row| row.status.as_str()).collect();
        assert_eq!(
            statuses,
            vec![
                "interrupted",
                "interrupted",
                "interrupted",
                "succeeded",
                "failed",
                "cancelled"
            ],
            "the three live rows are retired and the three terminal ones are left exactly as they were"
        );

        let retired = &rows[0];
        assert!(
            retired
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("resubmit")),
            "a retired row has to say why, or its owner cannot know to ask again: {retired:?}"
        );
        // Deliberately NOT stamped, and the migration argues why at length: the daemon does not know
        // when these ended, and SQLite's `datetime('now')` does not even sort with its neighbours.
        assert_eq!(retired.finished_at, None);
    }

    /// `--no-ff` is queued, and it is the only flag that is, because it is the only one that asks
    /// for what this queue performs anyway.
    ///
    /// `compute_merge` runs `git merge --no-ff` unconditionally, so the commit the queue publishes
    /// is always a merge commit. Refusing this spelling handed the merge back to the agent to
    /// perform by hand — the exact outcome the pillar exists to abolish — in exchange for nothing.
    ///
    /// Accepted on either side of the ref because both orders spell the same operation, and refused
    /// anywhere else by shape: a flag cannot stand where the program or the subcommand goes, and a
    /// `source` that ends up holding `--no-ff` is stopped by `Branch`, the argv guard, rather than
    /// by this reader.
    #[test]
    fn a_no_ff_merge_is_queued_because_it_is_what_the_queue_already_does() {
        let expected = Some(Op::Merge {
            source: "feature".into(),
            target: "master".into(),
        });
        assert_eq!(
            merge_from_command("git merge --no-ff feature", "master"),
            expected
        );
        assert_eq!(
            merge_from_command("git merge feature --no-ff", "master"),
            expected
        );

        // The flag is recognised in argument position only. `git --no-ff merge feature` is not a
        // command git would run, and a reader that "helpfully" queued it would be performing
        // something the caller could not have written.
        assert_eq!(
            merge_from_command("git --no-ff merge feature", "master"),
            None
        );
        // Left holding the flag as a ref, the argv guard is what refuses — not the shape.
        assert_eq!(merge_from_command("git merge --no-ff", "master"), None);
        assert_eq!(
            merge_from_command("git merge --no-ff --no-ff", "master"),
            None
        );
    }

    /// The spellings that become a queued merge, and their neighbours that must not.
    ///
    /// Each rejection is a different operation wearing a similar command line, and queueing any of
    /// them would perform something the caller did not write: `--squash` does not merge, `--abort`
    /// unwinds, two refs is an octopus, and `--ff`/`--ff-only` both ask for a fast-forward where
    /// this queue always writes a merge commit. `-X` is there to pin that `Branch` — the argv guard
    /// — is actually applied, and not merely available.
    #[test]
    fn only_a_bare_git_merge_becomes_a_queued_operation() {
        assert_eq!(
            merge_from_command("git merge feature", "master"),
            Some(Op::Merge {
                source: "feature".into(),
                target: "master".into(),
            })
        );
        // The verb is folded because a shell is not case-sensitive about it; the REF is not,
        // because git is, and a queued merge of `Feature` is not a merge of `feature`.
        assert_eq!(
            merge_from_command("GIT MERGE Feature", "master"),
            Some(Op::Merge {
                source: "Feature".into(),
                target: "master".into(),
            })
        );

        for command in [
            "git merge --squash feature",
            "git merge --abort",
            "git merge feature other",
            "git merge",
            "git status",
            "git merge -X",
            // Both ask for a fast-forward when one is possible; `compute_merge` writes a merge
            // commit either way, so queueing these would answer a different question.
            "git merge --ff feature",
            "git merge --ff-only feature",
            // The one accepted flag does not make its compounds acceptable.
            "git merge --no-ff --squash feature",
            "git merge --no-ff feature other",
        ] {
            assert_eq!(merge_from_command(command, "master"), None, "{command}");
        }
    }

    /// **The merge that reached master without touching the queue, and the class it belongs to.**
    ///
    /// Not a hypothetical: `35ec73a` is in the history, written by an editor session that ran
    /// `git merge --no-ff <branch> -m "..."`. Every layer above worked — the PreToolUse hook fired,
    /// its filter matched the verb, the daemon was asked. `merge_from_command` reads three exact
    /// token shapes and a message is not in any of them, so the parsers declined; and declining used
    /// to mean falling out of `unqueueable_but_shared` as `None`, which the gate reads as ALLOW.
    ///
    /// The queue cannot perform this spelling and should not pretend to: `compute_merge` writes its
    /// own message, so a parser that accepted `-m` would drop the caller's and still report success.
    /// Refusing is the honest answer, and the message says which spelling is queueable.
    #[test]
    fn a_merge_spelling_the_queue_cannot_perform_is_refused_rather_than_allowed() {
        let escaped = concat!(
            "git merge --no-ff feat/organograma-do-departamento ",
            r#"-m "merge feat/organograma-do-departamento into master""#
        );
        assert!(
            unqueueable_but_shared(escaped).is_some(),
            "the spelling that actually escaped must be refused"
        );

        for command in [
            "git merge --no-ff feature -m msg",
            "git merge feature -m msg",
            "git merge feature --no-edit",
            "git merge --strategy=ours feature",
            // The verb scan used to read `<path>` here and match no arm at all. `ask_daemon.py` has
            // skipped these flags' values since it was written; this side had not.
            "git -C /somewhere merge --no-ff feature -m msg",
            "git --git-dir /somewhere/.git merge feature -m msg",
        ] {
            assert!(unqueueable_but_shared(command).is_some(), "{command}");
        }

        // Writes nothing another session can see, so it stays this route's business to leave alone.
        // `--ff-only` is here because leaving it passing was a recorded decision, not an oversight.
        for command in [
            "git merge --squash feature",
            "git merge --abort",
            "git merge --continue",
            "git merge --quit",
            "git merge --no-commit feature",
            "git merge --ff-only feature",
        ] {
            assert_eq!(unqueueable_but_shared(command), None, "{command}");
        }
    }

    /// The same inversion for the three verbs that were open in exactly the same way, and the
    /// read-only spellings that must survive it. A gate that refused `git tag` — the listing — would
    /// stop a session at its second command, which is the failure mode this file argues against as
    /// hard as it argues against the hole.
    #[test]
    fn the_other_queue_verbs_fail_closed_while_their_read_only_spellings_do_not() {
        for command in [
            "git fetch --all",
            "git fetch --prune",
            "git fetch origin master",
            "git fetch",
            "git rebase -i master",
            "git rebase --onto master feature",
            "git rebase",
            "git tag -a v1 -m release",
            "git tag --delete v1",
            "git tag -f v1",
            "git tag v1 --force",
            // `-d -f` and `--delete --force` are `-D` written the long way.
            "git branch -d -f feature",
            "git branch --delete --force feature",
            "git branch -D feature",
            // A deletion the parser did not read. The first is the segment an editor session ran by
            // hand on 2026-09-14; the rest are the other ways a `-d` leaves the four-token shape,
            // the last three in spellings only git's own option parser reads as one.
            "git branch -d fix/espera-que-responde 2>&1",
            "git branch -d a b",
            "git -C path branch -d feature",
            "git branch --delete a b",
            "git branch -df feature",
            "git branch -dr origin/feature",
            "git branch --del feature",
        ] {
            assert!(unqueueable_but_shared(command).is_some(), "{command}");
        }

        for command in [
            "git fetch --dry-run origin",
            "git rebase --abort",
            "git rebase --skip",
            "git rebase --continue",
            "git tag",
            "git tag -l",
            r#"git tag --list "v1.*""#,
            "git tag --sort=-creatordate",
            "git branch",
            "git branch --list",
            "git branch -a",
            "git branch -v",
            "git branch -vv",
            "git branch -r",
            "git branch --show-current",
            "git branch --sort=-committerdate",
            // A value handed over after a space, and `-committerdate` holds a `d`.
            "git branch --sort -committerdate",
            "git status",
            "git log --oneline",
            "cargo test",
            // A mention is not a command: the segment must BEGIN with git.
            "echo git merge --no-ff feature -m msg",
        ] {
            assert_eq!(unqueueable_but_shared(command), None, "{command}");
        }
    }

    /// The bypass every parser in this file was open to, and it needed no cleverness to use.
    ///
    /// Each of them matches the whole token list as an exact shape, so a shell operator in front of
    /// the git command makes the list longer and the match fail — `None`, which
    /// `session_git_decision` reads as "not an operation this queue performs" and ALLOWS. A `cd` was
    /// enough. Measured on the running daemon before this existed: `cd repo && git merge master`
    /// came back `allow` from the route whose whole purpose is to refuse exactly that.
    ///
    /// The asserted cases are the shapes that actually occur — a directory change first, a chained
    /// `&&`, one per line — plus an environment assignment, which breaks the match for a different
    /// reason (`program` reads `FOO=bar`) and would otherwise be a second bypass wearing the same
    /// clothes.
    #[test]
    fn a_git_command_behind_a_shell_operator_is_still_the_operation_it_names() {
        let wrapped = [
            "cd /repo && git merge feature",
            "cd /repo\ngit merge feature",
            "true; git merge feature",
            "echo hi | git merge feature",
            "FOO=bar git merge feature",
            "cd /repo && git merge feature && echo done",
        ];
        for command in wrapped {
            // The defect itself, pinned rather than described: handed the whole command, the parser
            // still answers `None`. That is not a thing to fix in the parser — its exact-shape match
            // is what keeps `--squash` working directly — so this line must keep passing, and it is
            // what makes the assertion below about the SPLIT rather than about merge parsing.
            assert_eq!(
                merge_from_command(command, "master"),
                None,
                "unsplit, this is the bypass: {command}"
            );
            let found = shell_segments(command)
                .into_iter()
                .find_map(|segment| merge_from_command(segment, "master"));
            assert_eq!(
                found,
                Some(Op::Merge {
                    source: Branch::new("feature").unwrap(),
                    target: Branch::new("master").unwrap(),
                }),
                "{command}"
            );
        }
    }

    /// And splitting must not turn a command that is NOT a queue operation into one, which is the
    /// cost a looser parser would have carried. The strictness stays where it was: each segment is
    /// still matched as a whole shape, so every spelling the queue declines keeps working directly.
    #[test]
    fn splitting_a_command_does_not_invent_an_operation() {
        for command in [
            "cd /repo && git status",
            "cd /repo && git merge --squash feature",
            "echo git && echo merge && echo feature",
            "cd /repo",
            "",
        ] {
            let found = shell_segments(command)
                .into_iter()
                .find_map(|segment| merge_from_command(segment, "master"));
            assert_eq!(found, None, "{command}");
        }
    }

    /// **The incident of 2026-08-27, pinned: a heredoc body is stdin, and stdin is not a command.**
    ///
    /// A session wrote a file with `python - <<'EOF' … EOF` whose body contained the words of a
    /// merge command — as documentation, for a different agent to read. The old split saw a line of
    /// that body as a segment, `session_git_decision` submitted it, and the queue merged 1016 lines
    /// of unreviewed work into `master`. Nothing was ever executed by the shell.
    ///
    /// Each case below is a real heredoc spelling, and the assertion is the same for all of them:
    /// no operation is found, because none was written.
    #[test]
    fn a_heredoc_body_is_never_a_command() {
        let bodies_that_mention_a_merge = [
            // The incident's own shape.
            "python - <<'EOF'\ngit merge feature\nEOF",
            // Unquoted delimiter.
            "cat <<EOF > notes.md\ngit merge feature\nEOF",
            // `<<-` lets the terminator be indented with tabs, and the body still is not a command.
            "cat <<-EOF\n\tgit merge feature\n\tEOF",
            // A `;` inside the body is not a separator either — the whole line is data.
            "cat <<EOF\ntrue; git merge feature\nEOF",
        ];
        for command in bodies_that_mention_a_merge {
            let found = shell_segments(command)
                .into_iter()
                .find_map(|segment| merge_from_command(segment, "master"));
            assert_eq!(
                found, None,
                "a heredoc body was read as a command: {command}"
            );
        }
    }

    /// Swallowing the body must END at the terminator, or the fix would be a bypass: everything
    /// after a heredoc would stop being read. The merge on the last line is real and must be found.
    #[test]
    fn a_command_after_a_heredoc_is_still_read() {
        let command = "cat <<EOF > notes.md\ngit merge decoy\nEOF\ngit merge feature";
        let found = shell_segments(command)
            .into_iter()
            .find_map(|segment| merge_from_command(segment, "master"));
        assert_eq!(
            found,
            Some(Op::Merge {
                source: Branch::new("feature").unwrap(),
                target: Branch::new("master").unwrap(),
            }),
            "the terminator did not end the body"
        );
    }

    /// A here-string carries its operand on the same line and opens no body, so the line after one
    /// is an ordinary command. Reading `<<<` as a heredoc would swallow the rest of the script.
    #[test]
    fn a_here_string_swallows_nothing() {
        let command = "grep x <<< \"some text\"\ngit merge feature";
        let found = shell_segments(command)
            .into_iter()
            .find_map(|segment| merge_from_command(segment, "master"));
        assert_eq!(
            found,
            Some(Op::Merge {
                source: Branch::new("feature").unwrap(),
                target: Branch::new("master").unwrap(),
            })
        );
    }

    /// Quoted text is data for the same reason a heredoc body is: the shell does not run it, so a
    /// separator inside it separates nothing.
    ///
    /// **`sh -c "git merge feature"` is the case that shows this opens no bypass**, and it is in the
    /// list deliberately. That spelling DOES execute a merge — and it was already invisible here
    /// before this change, because the segment is `sh -c "…"` and `merge_from_command` matches
    /// `[git, merge, source]` and nothing else. Skipping the quotes takes nothing away from a
    /// detection that never existed; what it removes is the pretence of one.
    #[test]
    fn quoted_text_is_not_a_command() {
        for command in [
            "echo \"a; git merge feature\"",
            "echo 'git merge feature'",
            "git commit -m \"revert the git merge feature we did\"",
            "sh -c \"git merge feature\"",
            // A string that spans lines, which is what an embedded script looks like from out here.
            "python -c \"\nx = 'git merge feature'\n\"",
        ] {
            let found = shell_segments(command)
                .into_iter()
                .find_map(|segment| merge_from_command(segment, "master"));
            assert_eq!(found, None, "quoted text was read as a command: {command}");
        }
    }

    /// A worktree with no branch checked out has no merge target, and `HEAD` is exactly what
    /// `rev-parse --abbrev-ref` answers for one. Queueing that would name nothing.
    #[test]
    fn a_detached_head_is_not_a_merge_target() {
        assert_eq!(merge_from_command("git merge feature", "HEAD"), None);
        assert_eq!(merge_from_command("git merge feature", ""), None);
    }

    /// The two shapes a push may take, and the half the command line does not carry.
    ///
    /// `git push origin` names no branch, so the branch is the one the worktree stands on — the same
    /// role the worktree's branch plays as a merge's TARGET, in the opposite position. The two cases
    /// are asserted against different worktree branches on purpose: with both on `main`, an
    /// implementation that ignored the command's second word would pass.
    #[test]
    fn a_push_takes_its_branch_from_the_command_or_from_the_worktree() {
        assert_eq!(
            push_from_command("git push origin", "feat/x"),
            Some(Op::Push {
                remote: "origin".into(),
                branch: "feat/x".into()
            })
        );
        assert_eq!(
            push_from_command("git push origin main", "feat/x"),
            Some(Op::Push {
                remote: "origin".into(),
                branch: "main".into()
            }),
            "an explicit branch is the one asked for, even from a worktree standing elsewhere"
        );
        // Folded for comparison, and the NAMES are not — git is case-sensitive about both a branch
        // and a remote, and a queued push to `Origin` is not a push to `origin`.
        assert_eq!(
            push_from_command("GIT PUSH Origin Main", "feat/x"),
            Some(Op::Push {
                remote: "Origin".into(),
                branch: "Main".into()
            })
        );
    }

    /// Everything the strictest reading leaves out. Each row is a different KIND of exclusion, which
    /// is why they are listed with what they are rather than as a bag of strings.
    #[test]
    fn only_a_bare_git_push_to_a_named_remote_becomes_a_queued_operation() {
        for command in [
            // No argv of its own: what this does is read out of config the daemon does not control.
            "git push",
            // The queue's argv would not set the upstream, so queueing it would succeed at less than
            // was asked and report success.
            "git push -u origin main",
            "git push --set-upstream origin main",
            // Different operations: each destroys or moves what this one only adds to.
            "git push --force origin main",
            "git push --force-with-lease origin main",
            "git push -f origin main",
            "git push --delete origin main",
            "git push --tags origin",
            "git push --all origin",
            "git push --mirror origin",
            // Shape, not flags: a third positional is a second refspec.
            "git push origin main extra",
            // The verb is the verb. A flag in its place is not one.
            "git --no-verify push origin main",
            "gh push origin main",
        ] {
            assert_eq!(push_from_command(command, "master"), None, "{command}");
        }
    }

    /// **`HEAD` is refused on BOTH routes into the branch, and only two assertions can tell that.**
    ///
    /// One arrives from the worktree (`rev-parse --abbrev-ref` says `HEAD` for a detached checkout)
    /// and the other was typed. `git push origin HEAD` is a perfectly ordinary thing for a person to
    /// write and means "whatever I am standing on right now" — which is a sentence with no meaning
    /// left by the time a queued row runs, possibly several operations later.
    #[test]
    fn a_push_never_queues_the_word_head() {
        assert_eq!(push_from_command("git push origin", "HEAD"), None);
        assert_eq!(push_from_command("git push origin", ""), None);
        assert_eq!(push_from_command("git push origin HEAD", "master"), None);
    }

    /// The argv guard is what refuses a dashed name that reached an argument position, exactly as it
    /// does for a merge — and it is `Remote` doing it for the remote, which is the field a `String`
    /// would have let straight through.
    #[test]
    fn a_dashed_name_in_a_push_is_stopped_by_the_type_rather_than_the_shape() {
        assert_eq!(
            push_from_command("git push --receive-pack=touch", "master"),
            None
        );
        assert_eq!(
            push_from_command("git push origin --exec=x", "master"),
            None
        );
    }

    /// The parsers are tried one after another in `runs::queueable_operation`, and this is why that
    /// order cannot matter: each insists on its own subcommand, so no command is more than one.
    ///
    /// Every pair rather than a sample: the property is about the SET, and a chain of `or_else` is
    /// exactly the shape where adding a fourth parser that overlaps an existing one compiles, passes
    /// its own tests, and silently steals commands from whichever came before it.
    #[test]
    fn a_command_is_never_two_operations_at_once() {
        for command in [
            "git merge feature",
            "git push origin main",
            "git tag v1 main",
            "git fetch origin",
            "git branch -d feature",
            "git rebase master",
        ] {
            let matched = [
                merge_from_command(command, "master"),
                push_from_command(command, "master"),
                tag_from_command(command, "master"),
                fetch_from_command(command),
                branch_delete_from_command(command),
                rebase_from_command(command, "master"),
            ]
            .into_iter()
            .flatten()
            .count();
            assert_eq!(matched, 1, "{command} was read by more than one parser");
        }
    }

    /// **What is left of the spec's nine is a set of refusals, not a backlog**, and the message has
    /// to say which — a caller told "not yet" waits for a release that is never coming.
    ///
    /// **Nothing is deferred any more**, and the absence of a "not yet" anywhere is the assertion.
    /// The spec's nine are now six this queue performs and three it has decided against; a caller
    /// told "not yet" would be waiting for a release that is never coming.
    #[test]
    fn the_operations_this_queue_will_never_perform_say_so_rather_than_saying_not_yet() {
        for closed in ["pull", "worktree-add", "worktree-remove", "pr-merge"] {
            let error = Op::from_request(closed, Some("a"), Some("b")).unwrap_err();
            assert!(
                error.contains("will not become one"),
                "{closed} must be refused as a decision, not deferred: {error}"
            );
            assert!(!error.contains("not yet"), "{closed}: {error}");
        }

        // And the whole vocabulary is reachable, which is what says none of the nine is left in
        // limbo: six build, three refuse with a reason, and no name falls through to "unknown".
        for (operation, source, target) in [
            ("merge", Some("feature"), Some("master")),
            ("push", Some("main"), Some("origin")),
            ("tag", Some("main"), Some("v1.0")),
            ("fetch", None, Some("origin")),
            ("branch-delete", Some("feature"), None),
            ("rebase", Some("feature"), Some("master")),
        ] {
            assert!(
                Op::from_request(operation, source, target).is_ok(),
                "{operation} must be one the queue performs"
            );
        }
    }

    /// `git rebase <onto>` reads its two halves the way `merge_from_command` reads its own: the
    /// branch is the one the worktree stands on, and the command names what it goes on top of.
    #[test]
    fn a_rebase_replays_the_worktrees_branch_onto_what_the_command_names() {
        assert_eq!(
            rebase_from_command("git rebase master", "feat/x"),
            Some(Op::Rebase {
                branch: "feat/x".into(),
                onto: "master".into()
            })
        );
        assert_eq!(
            rebase_from_command("GIT REBASE Master", "feat/x"),
            Some(Op::Rebase {
                branch: "feat/x".into(),
                onto: "Master".into()
            })
        );
        // Same guard, same reason as everywhere else: a detached worktree names no branch to replay.
        assert_eq!(rebase_from_command("git rebase master", "HEAD"), None);
        assert_eq!(rebase_from_command("git rebase master", ""), None);
    }

    /// **The one parser with no accepted flag at all**, and each refusal is a different kind.
    #[test]
    fn only_a_bare_git_rebase_onto_one_ref_becomes_a_queued_operation() {
        for command in [
            // Replays onto the configured upstream, in a repository the daemon does not control.
            "git rebase",
            // Opens an editor, for a human at a terminal that does not exist here.
            "git rebase -i master",
            "git rebase --interactive master",
            // Operate on a rebase already in progress — a state this queue never leaves behind.
            "git rebase --continue",
            "git rebase --abort",
            "git rebase --skip",
            // Takes a third ref, which the two-field shape cannot hold.
            "git rebase --onto master feature",
            // Runs an arbitrary command per commit: a shell by another name.
            "git rebase --exec make master",
            // Rewrites every commit rather than replaying them.
            "git rebase --root",
            // Shape, and the verb.
            "git rebase master extra",
            "git --no-verify rebase master",
            "gh rebase master",
        ] {
            assert_eq!(rebase_from_command(command, "feat/x"), None, "{command}");
        }
    }

    /// `git fetch <remote>` exactly, and every neighbouring spelling is a different operation.
    #[test]
    fn only_a_git_fetch_from_one_named_remote_becomes_a_queued_operation() {
        assert_eq!(
            fetch_from_command("git fetch origin"),
            Some(Op::Fetch {
                remote: "origin".into()
            })
        );
        assert_eq!(
            fetch_from_command("GIT FETCH Origin"),
            Some(Op::Fetch {
                remote: "Origin".into()
            })
        );
        for command in [
            // The destination left to config in a repository the daemon does not control.
            "git fetch",
            // Remotes nobody named.
            "git fetch --all",
            // Deletes tracking refs.
            "git fetch --prune origin",
            "git fetch origin --prune",
            // A namespace the refspec deliberately leaves out.
            "git fetch --tags origin",
            // Shape, and the verb.
            "git fetch origin main",
            "git --no-verify fetch origin",
            "gh fetch origin",
        ] {
            assert_eq!(fetch_from_command(command), None, "{command}");
        }
    }

    /// **`-d` and `-D` differ by CASE alone and mean the safe and the unsafe thing**, which makes
    /// this the one parser where folding the flag would be a defect rather than a convenience.
    ///
    /// The other half is that the flag is mandatory: `git branch <name>` CREATES a branch, so a
    /// shape that read the flag as optional would queue a deletion for a command that asked for the
    /// opposite. `classifier.rs` pins that same ambiguity as its reason for listing `git branch` by
    /// exact form only.
    #[test]
    fn deleting_a_branch_is_queued_only_in_the_spelling_that_refuses_unmerged_work() {
        for command in ["git branch -d feature", "git branch --delete feature"] {
            assert_eq!(
                branch_delete_from_command(command),
                Some(Op::BranchDelete {
                    branch: "feature".into()
                }),
                "{command}"
            );
        }
        for command in [
            // The whole point: `-D` asks git to stop answering the question this queue relies on it
            // to answer.
            "git branch -D feature",
            "git branch --delete --force feature",
            "git branch -d -f feature",
            // Not a deletion at all. Reading the flag as optional would turn this into one.
            "git branch feature",
            "git branch",
            // Other mutations of the same subcommand.
            "git branch -m old new",
            "git branch --unset-upstream",
            // Remote-tracking deletion is a different namespace and a different operation.
            "git branch -dr origin/feature",
            // Shape, and the verb.
            "git branch -d one two",
            "gh branch -d feature",
            // The two shapes of 2026-09-14's escape: a redirection after the name, and a global flag
            // before the verb. Declining them is right; what `unqueueable_but_shared` says next is
            // what decides whether they run.
            "git branch -d feature 2>&1",
            "git -C path branch -d feature",
        ] {
            assert_eq!(branch_delete_from_command(command), None, "{command}");
        }
    }

    /// **The line that deleted a branch by hand on 2026-09-14**, and the chains that looked like its
    /// cause.
    ///
    /// They were not the cause: split, a chained `git branch -d <branch>` is the four tokens the
    /// parser reads, and a chain is queued the way
    /// `a_git_command_behind_a_shell_operator_is_still_the_operation_it_names` says every verb is.
    /// What escaped was the redirection on the end. Both halves are pinned, because a deletion has
    /// to land in one of them — read, or refused — and the escape was a line that landed in neither.
    #[test]
    fn a_branch_deletion_is_either_read_or_refused_and_never_neither() {
        let escaped = "git worktree remove C:/Projects/nucleos-espera && echo \"worktree removed\" \
                       && git branch -d fix/espera-que-responde 2>&1 | tail -6";
        let segments = shell_segments(escaped);
        assert_eq!(
            segments
                .iter()
                .copied()
                .find_map(branch_delete_from_command),
            None,
            "the escape was a deletion no parser read: {segments:?}"
        );
        assert!(
            segments
                .iter()
                .copied()
                .find_map(unqueueable_but_shared)
                .is_some(),
            "so it has to be refused, or the gate answers `not mine` again: {segments:?}"
        );

        for chained in [
            "git worktree remove x && git branch -d feature",
            "cd repo && git branch -d feature",
            "cd repo\ngit branch -d feature",
        ] {
            assert_eq!(
                shell_segments(chained)
                    .into_iter()
                    .find_map(branch_delete_from_command),
                Some(Op::BranchDelete {
                    branch: "feature".into()
                }),
                "{chained}"
            );
        }
    }

    /// The two shapes a tag may take, and the half the command line does not carry.
    ///
    /// Asserted against a worktree branch that is NOT the one named, for the reason the push twin
    /// gives: with both spelled `main`, an implementation that ignored the command's third word
    /// would pass.
    #[test]
    fn a_tag_takes_its_branch_from_the_command_or_from_the_worktree() {
        assert_eq!(
            tag_from_command("git tag v1.0", "release/2"),
            Some(Op::Tag {
                name: "v1.0".into(),
                at: "release/2".into()
            })
        );
        assert_eq!(
            tag_from_command("git tag v1.0 main", "release/2"),
            Some(Op::Tag {
                name: "v1.0".into(),
                at: "main".into()
            }),
            "an explicit branch is the one asked for, even from a worktree standing elsewhere"
        );
        // The verb folds; the two NAMES do not. Git is case-sensitive about both, and a tag `V1` is
        // not a tag `v1`.
        assert_eq!(
            tag_from_command("GIT TAG V1 Main", "release/2"),
            Some(Op::Tag {
                name: "V1".into(),
                at: "Main".into()
            })
        );
    }

    /// Everything the strictest reading leaves out. Each row is a different KIND of exclusion, which
    /// is the reason they are grouped rather than listed as a bag of strings.
    #[test]
    fn only_a_bare_git_tag_of_a_new_name_becomes_a_queued_operation() {
        for command in [
            // A READ, and the only bare form among the three parsers that means something else
            // entirely rather than meaning the operation with its arguments left to config.
            "git tag",
            // Annotated and signed tags are a different object, and both need a message — a quoted
            // shell argument, which is the one thing these parsers exist never to read.
            "git tag -a v1 -m release",
            "git tag -s v1 -m release",
            "git tag -m release v1",
            // The destructive spellings: one removes a tag, one moves an existing one.
            "git tag -d v1",
            "git tag -f v1 main",
            "git tag --force v1 main",
            // Reads wearing the write's name. Stopped by `TagName` rather than by the shape, which
            // is the same division of labour `--no-ff` has in the merge parser.
            "git tag -l",
            "git tag --list v*",
            "git tag -n v1",
            "git tag --contains HEAD",
            // Shape: a fourth positional is not a spelling this executes.
            "git tag v1 main extra",
            // The verb is the verb.
            "git --no-verify tag v1",
            "gh tag v1",
        ] {
            assert_eq!(tag_from_command(command, "master"), None, "{command}");
        }
    }

    /// `HEAD` is refused on both routes into the branch, for the reason the push twin states: a
    /// queued row naming it names nothing by the time it runs, and `rev-parse --abbrev-ref` is what
    /// says `HEAD` for a detached worktree.
    #[test]
    fn a_tag_never_queues_the_word_head() {
        assert_eq!(tag_from_command("git tag v1", "HEAD"), None);
        assert_eq!(tag_from_command("git tag v1", ""), None);
        assert_eq!(tag_from_command("git tag v1 HEAD", "master"), None);
    }

    /// A tag name is checked as an argv token by its OWN type, on every route in.
    ///
    /// The JSON half is the one that matters: `Op` derives `Deserialize`, so a raw
    /// `POST /vcs/requests` body reaches `TagName` without passing `from_request` at all. Giving
    /// `Tag` a `String` name would leave every assertion in the flat-builder test green.
    #[test]
    fn a_tag_name_is_an_argv_token_on_every_route_in() {
        assert_eq!(TagName::new("  v1.0  ").unwrap().as_str(), "v1.0");
        for bad in ["", "   ", "--format=x", "-d", "v 1", "v\u{1b}1"] {
            assert!(TagName::new(bad).is_err(), "{bad:?} was accepted");
        }

        let raw = r#"{"op":"tag","name":"--format=touch x","at":"main"}"#;
        assert!(serde_json::from_str::<Op>(raw).is_err());
        assert!(
            Op::from_stored("tag", r#"{"op":"tag","name":"-d","at":"main"}"#).is_err(),
            "a hand-edited row is not trusted either"
        );
    }

    /// **Every variant round-trips AND spells its name the same way in both columns**, checked as a
    /// set rather than one test per variant.
    ///
    /// The second half is the one that had to be added rather than assumed. `kind()` is written by
    /// hand and the serde tag is derived, so the two can disagree — and the round trip does NOT
    /// notice, because `from_stored` re-derives `kind()` from the parsed value instead of reading
    /// the payload's tag. Measured: removing `#[serde(rename = "branch-delete")]` leaves the round
    /// trip green and puts `branch_delete` in the `args` payload of a row whose `op` column says
    /// `branch-delete`. The assertion that sees it is the one comparing the payload's own `op` field
    /// to `kind()`.
    ///
    /// One test over the whole vocabulary catches the next multi-word variant on the day it is
    /// added, where five separate tests would need somebody to remember to write a sixth.
    #[test]
    fn every_operation_round_trips_through_storage() {
        let all = [
            Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            Op::Push {
                remote: "origin".into(),
                branch: "main".into(),
            },
            Op::Tag {
                name: "v1.0".into(),
                at: "main".into(),
            },
            Op::Fetch {
                remote: "origin".into(),
            },
            Op::BranchDelete {
                branch: "feature".into(),
            },
            Op::Rebase {
                branch: "feat/x".into(),
                onto: "master".into(),
            },
            Op::Revert {
                merge_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                target: "master".into(),
            },
        ];

        for op in &all {
            assert_eq!(
                Op::from_stored(op.kind(), &op.to_args()).as_ref().ok(),
                Some(op),
                "{} does not survive its own storage",
                op.kind()
            );
            // The column and the payload are checked against each other, so a mismatched pair is
            // refused rather than silently trusted.
            assert!(Op::from_stored("merge", &op.to_args()).is_err() || op.kind() == "merge");
            // **The assertion the round trip cannot make.** The payload's own tag has to be the
            // same word as the column, or one row carries two spellings of one operation for
            // everybody who reads the queue or filters it by JSON path.
            let payload: serde_json::Value = serde_json::from_str(&op.to_args()).unwrap();
            assert_eq!(
                payload["op"].as_str(),
                Some(op.kind()),
                "the payload's tag and the `op` column must be the same word"
            );
        }

        // And the names are what `from_request` accepts, so the column holds the caller's word.
        let kinds: Vec<&str> = all.iter().map(Op::kind).collect();
        assert_eq!(
            kinds,
            [
                "merge",
                "push",
                "tag",
                "fetch",
                "branch-delete",
                "rebase",
                "revert"
            ],
            "the `op` column's vocabulary is the one the door speaks"
        );
    }

    /// A listing narrowed to a project shows the whole repository that project shares.
    ///
    /// The thing being pinned is that a reader asking "what is queued for alpha" is not shown a
    /// view in which beta's merge into the same branch is invisible. They are in one queue, waiting
    /// on one lock, competing for one set of refs — a listing that split them by label would be
    /// most misleading exactly when it matters, which is when the two are about to collide.
    #[tokio::test]
    async fn a_listing_shows_the_whole_repository_and_not_just_the_project_named() {
        let pool = test_pool().await;
        let alpha = ResolvedRepo::synthetic("alpha", "C:/repo", "SHARED");
        let beta = ResolvedRepo::synthetic("beta", "C:/repo", "SHARED");
        let elsewhere = ResolvedRepo::synthetic("gamma", "C:/other", "OTHER");

        submit(&pool, &alpha, &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &beta, &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &elsewhere, &merge_op(), Origin::Human)
            .await
            .unwrap();

        let listed = list(&pool, Some("alpha")).await.unwrap();
        let projects: Vec<&str> = listed
            .iter()
            .map(|row| row.project_id.as_str())
            .collect::<Vec<_>>();
        assert!(
            projects.contains(&"alpha") && projects.contains(&"beta"),
            "both projects share one repository and one queue: {projects:?}"
        );
        assert!(
            !projects.contains(&"gamma"),
            "widening to the repository must not widen to every repository: {projects:?}"
        );
        assert!(
            listed.iter().all(|row| row.repo_key == "SHARED"),
            "a listing that groups by repository has to say which one"
        );
    }

    /// A project nothing has been queued for lists nothing — not everything.
    ///
    /// The subquery that finds the repository answers NULL for such a project, and `= NULL` is NULL
    /// rather than true, so the row is not returned. Written down as a test because the failure
    /// mode of getting that wrong is not an error: it is a listing that quietly shows every
    /// repository on the machine to a caller who asked about one.
    #[tokio::test]
    async fn a_project_with_nothing_queued_lists_nothing_rather_than_everything() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(list(&pool, Some("never-used")).await.unwrap().is_empty());
        assert_eq!(
            list(&pool, None).await.unwrap().len(),
            1,
            "asking for no project at all still means the whole table"
        );
    }

    /// A row nobody can execute must not take the repository down with it.
    ///
    /// The claim commits before the payload is parsed, so the obvious failure path — return the
    /// error — leaves the row `running` and holds alpha's only slot until the daemon restarts. The
    /// status assertion alone would not catch that: what proves the repository was actually freed
    /// is that the *next* claim returns the following request instead of `None`.
    #[tokio::test]
    async fn a_row_that_cannot_be_parsed_frees_the_repository_instead_of_jamming_it() {
        let pool = test_pool().await;
        // Written directly: `submit` cannot produce this row, which is the point — it comes from a
        // hand edit, or a downgrade that no longer knows an operation a newer build wrote.
        let corrupt = insert(&pool, "alpha", "queued", r#"{"op":"rm_rf"}"#)
            .await
            .unwrap();
        let behind_it = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(
            claim_next(&pool, "alpha").await.is_err(),
            "an unexecutable row is an error, not a wait"
        );
        assert_eq!(status_of(&pool, corrupt).await, "failed");
        // The diagnosis has to survive into the row, or the only account of why this request died
        // is a log line the daemon may have already rotated away.
        let reason: Option<String> =
            sqlx::query_scalar("SELECT failure_reason FROM vcs_requests WHERE id = ?")
                .bind(corrupt)
                .fetch_one(&pool)
                .await
                .unwrap();
        let reason = reason.expect("a failed request records why it failed");
        assert!(
            reason.contains(&corrupt.to_string()) && reason.contains("could not be parsed"),
            "the recorded reason must name the row and say what was wrong: {reason}"
        );
        // The only production producer of `Outcome::Unexecutable`, and the only place its NULL
        // `output_tail` can be caught being written: revert this arm to a `Failed` with an empty
        // tail and every other assertion here still passes.
        assert!(
            output_tail_of(&pool, corrupt).await.is_none(),
            "a row that never reached an argv writes no output tail; that NULL is what tells it \
             apart from an operation that ran and failed"
        );
        assert_eq!(
            claim_next(&pool, "alpha").await.unwrap().unwrap().id,
            behind_it,
            "the queue must move on, not hold alpha until the daemon restarts"
        );
    }

    /// A positional 5-tuple of `(i64, String, String, String, String)` is destructured by position,
    /// so `project_id` and `project_root` — adjacent in both the `RETURNING` list and the pattern —
    /// could be swapped and everything else here would still pass. This is also the only coverage
    /// that `Op` parsing works *through* the claim rather than in isolation.
    #[tokio::test]
    async fn a_claim_carries_the_operation_and_the_repository_it_names() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let claimed = claim_next(&pool, "alpha").await.unwrap().unwrap();
        assert_eq!(
            claimed.op,
            Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            }
        );
        assert_eq!(claimed.project_id, "alpha");
        assert_eq!(claimed.project_root, "C:/repo");
    }

    /// Any non-`running` status frees the partial index, so the ordering test would pass even if
    /// `finish` wrote the wrong status and dropped the sha entirely. What the caller keeps of a
    /// merge is the commit it produced; nothing else asserts it lands.
    #[tokio::test]
    async fn a_succeeded_request_records_the_commit_it_produced() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Fast-forward".into(),
            },
        )
        .await
        .unwrap();

        let (status, sha, exit_code, output_tail, reason): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, result_sha, exit_code, output_tail, failure_reason
               FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(status, "succeeded");
        assert_eq!(sha.as_deref(), Some("abc123"));
        assert_eq!(exit_code, None, "nothing failed, so there is no exit code");
        // Not NULL: a success keeps what it printed, and NULL in this column now means something
        // else entirely — that the row never reached an argv (`Outcome::Unexecutable`).
        assert_eq!(output_tail.as_deref(), Some("Fast-forward"));
        assert_eq!(reason, None);
    }

    /// A successful command's output has somewhere to go. Chunk 1 could not record it: a merge that
    /// succeeded with warnings — a renamed file resolved, a hook's advice — printed them into nothing.
    #[tokio::test]
    async fn a_successful_operation_keeps_what_it_printed() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            output_tail_of(&pool, id).await.as_deref(),
            Some("Merge made by the 'ort' strategy.")
        );
    }

    /// Ages a request by writing its stamps directly. The queue writes `finished_at` itself and has
    /// no way to be told a different one, which is the same reason `runs::prune_transcripts`'s tests
    /// backdate rather than wait.
    async fn backdate(pool: &sqlx::SqlitePool, id: i64, days: i64) {
        let when = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        sqlx::query("UPDATE vcs_requests SET created_at = ?, finished_at = ? WHERE id = ?")
            .bind(&when)
            .bind(&when)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A finished request past the window loses what git printed and keeps everything else.
    ///
    /// **Both halves are asserted, and the second is the one that matters.** Emptying the tail is
    /// what the sweep is for; keeping the row is what the shape was chosen for —
    /// `action_grants.queued_request_id` names these rows by id with no foreign key behind it, so a
    /// DELETE would leave a takeover grant pointing at nothing and `matching_queued_request` would
    /// go on telling a run that a request holds its work. A test that only checked the tail was gone
    /// would pass against exactly that.
    #[tokio::test]
    async fn a_finished_request_loses_what_git_printed_and_keeps_the_row() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await
        .unwrap();
        backdate(&pool, id, 31).await;

        let pruned = prune_output_tails(&pool, 30, chrono::Utc::now())
            .await
            .unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(output_tail_of(&pool, id).await, None);
        let (status, sha): (String, Option<String>) =
            sqlx::query_as("SELECT status, result_sha FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (status.as_str(), sha.as_deref()),
            ("succeeded", Some("abc123")),
            "the row and what it published survive; only the diagnostic ages out"
        );
    }

    /// The three things the sweep must not touch, in one test because each is a different reason.
    ///
    /// A row inside the window is the ordinary case. A row that is still `running` is the dangerous
    /// one — something is writing to it — and it is what a `NOT IN ('queued','running',…)` spelling
    /// would get wrong the day a status is added. And `retain_days <= 0` is the typo guard every
    /// other sweep in this crate carries: it must refuse rather than strip the whole table.
    #[tokio::test]
    async fn the_sweep_leaves_alone_what_is_recent_still_running_or_covered_by_a_zero_window() {
        let pool = test_pool().await;
        let recent = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            recent,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "recent".into(),
            },
        )
        .await
        .unwrap();
        backdate(&pool, recent, 29).await;

        let running = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        sqlx::query("UPDATE vcs_requests SET output_tail = 'in flight' WHERE id = ?")
            .bind(running)
            .execute(&pool)
            .await
            .unwrap();
        backdate(&pool, running, 400).await;
        // `backdate` also rewrites `finished_at`, which a running row would not have. Put it back,
        // so what keeps this row is its STATUS and not a missing stamp — otherwise the assertion
        // below passes against a sweep with no status filter at all.
        sqlx::query("UPDATE vcs_requests SET status = 'running', finished_at = NULL WHERE id = ?")
            .bind(running)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            prune_output_tails(&pool, 30, chrono::Utc::now())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            output_tail_of(&pool, recent).await.as_deref(),
            Some("recent")
        );
        assert_eq!(
            output_tail_of(&pool, running).await.as_deref(),
            Some("in flight")
        );

        // A window of zero is a typo with a plausible-looking value, not an instruction to empty
        // every row on the machine.
        backdate(&pool, recent, 400).await;
        assert_eq!(
            prune_output_tails(&pool, 0, chrono::Utc::now())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            output_tail_of(&pool, recent).await.as_deref(),
            Some("recent")
        );
    }

    /// The sweep reports what it did, not what it could have done.
    ///
    /// Run twice over one aged row: the second pass must report nothing. Without the
    /// `output_tail IS NOT NULL` filter the UPDATE matches the same row every hour for ever, and the
    /// log reports a constant instead of a count — which reads as a sweep that never converges.
    #[tokio::test]
    async fn a_second_sweep_over_the_same_rows_reports_nothing() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await
        .unwrap();
        backdate(&pool, id, 31).await;

        assert_eq!(
            prune_output_tails(&pool, 30, chrono::Utc::now())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            prune_output_tails(&pool, 30, chrono::Utc::now())
                .await
                .unwrap(),
            0,
            "there is nothing left to drop, so the sweep must say so"
        );
    }

    /// **The trap the other three sweeps each pay for in their own doc comment**, pinned here on the
    /// boundary rather than by a coarse test that could not see it.
    ///
    /// `finished_at` is RFC 3339 (`2026-08-01T12:00:00+00:00`); SQLite's `datetime('now','-30 days')`
    /// would produce `2026-07-13 12:00:00`. Compared as TEXT, `T` (0x54) sorts after the space
    /// (0x20) — so within the cutoff's OWN day a row hours too old compares as newer and survives
    /// every sweep for ever. A row aged exactly to the boundary is the only input that tells the two
    /// spellings apart.
    #[tokio::test]
    async fn the_retention_boundary_is_exact_rather_than_to_the_day() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "on the boundary".into(),
            },
        )
        .await
        .unwrap();

        // Thirty days and one hour old: past a window of thirty by an hour, and inside the cutoff's
        // own calendar day, which is where the two spellings disagree.
        let now = chrono::Utc::now();
        let when = (now - chrono::Duration::days(30) - chrono::Duration::hours(1)).to_rfc3339();
        sqlx::query("UPDATE vcs_requests SET finished_at = ? WHERE id = ?")
            .bind(&when)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(prune_output_tails(&pool, 30, now).await.unwrap(), 1);
        assert_eq!(output_tail_of(&pool, id).await, None);
    }

    #[tokio::test]
    async fn a_failed_request_records_why_and_what_it_printed() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        finish(
            &pool,
            id,
            Outcome::Failed {
                reason: "merge conflict".into(),
                exit_code: Some(1),
                output_tail: "CONFLICT (content): Merge conflict in a.txt".into(),
            },
        )
        .await
        .unwrap();

        let (status, sha, exit_code, output_tail, reason): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, result_sha, exit_code, output_tail, failure_reason
               FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(status, "failed");
        assert_eq!(sha, None, "nothing succeeded, so there is no commit");
        assert_eq!(exit_code, Some(1));
        assert_eq!(
            output_tail.as_deref(),
            Some("CONFLICT (content): Merge conflict in a.txt")
        );
        assert_eq!(reason.as_deref(), Some("merge conflict"));
    }

    /// The deferred decision, settled in the row rather than in prose: `output_tail IS NULL` means the
    /// request never reached an argv. Both of these rows read `failed`, so without a structural
    /// discriminator anyone querying failures for execution diagnostics finds entries with no exit code
    /// and no output and no way to tell why.
    #[tokio::test]
    async fn a_request_that_never_ran_is_distinguishable_from_one_that_ran_and_failed() {
        let pool = test_pool().await;

        let never_ran = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            never_ran,
            Outcome::Unexecutable {
                reason: "stored operation could not be parsed".into(),
            },
        )
        .await
        .unwrap();

        let ran = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            ran,
            Outcome::Failed {
                reason: "CONFLICT (content)".into(),
                exit_code: Some(1),
                output_tail: "Automatic merge failed".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(status_of(&pool, never_ran).await, "failed");
        assert_eq!(status_of(&pool, ran).await, "failed");
        assert!(output_tail_of(&pool, never_ran).await.is_none());
        assert!(output_tail_of(&pool, ran).await.is_some());
    }

    /// Only the holder of a claim may end it.
    ///
    /// Task 5's restart reconciliation marks stranded rows `interrupted` and records why. A worker
    /// whose future outlived that reconcile would otherwise flip the row to `succeeded` and NULL
    /// the reason — reporting success for work nobody observed finish, and destroying the only
    /// record that it was ever interrupted.
    #[tokio::test]
    async fn a_request_that_is_no_longer_running_cannot_be_finished() {
        let pool = test_pool().await;
        let id = insert(&pool, "alpha", "interrupted", MERGE_ARGS)
            .await
            .unwrap();

        let late = finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await;

        assert!(matches!(late, Err(sqlx::Error::RowNotFound)));
        assert_eq!(status_of(&pool, id).await, "interrupted");
    }

    // NOTE: the rollback-on-drop half of the claim's cancellation safety is deliberately not tested
    // here, and the reason is narrower than "we lack a harness" — the crate has one. `TempDb`
    // (`storage.rs`, `#[cfg(test)]`) is file-backed with `max_connections(5)`, and its own doc
    // comment advertises this very shape: "a handler parked on a pool while another connection
    // watches it". What is actually missing is a way to stop a future at a *chosen* await:
    // `Waker::noop()` does not give that deterministically, and against `test_pool`'s `:memory:`
    // single connection every attempt ends in `PoolTimedOut` after 30s, because an abandoned claim
    // never returns the one connection there is. So the test would be asserting `sqlx`'s documented
    // guarantee rather than this module's logic: `sqlx-core-0.9.0/src/transaction.rs:265-280`,
    // `impl Drop for Transaction` calls `start_rollback`, which runs "on the next asynchronous
    // invocation of the underlying connection (including if the connection is returned to a pool)".

    /// The queue must never hand out work that cannot execute — a head blocked on a sleeping human
    /// blocks every agent behind it. That is the whole reason approval precedes admission.
    #[tokio::test]
    async fn nothing_awaiting_approval_is_ever_claimable() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_none());
    }

    /// Both branches of the filter, and the ordering, because neither is visible from one row.
    ///
    /// The HTTP test that exercises this route inserts a single request and passes no project, so
    /// `WHERE ?1 IS NULL OR project_id = ?1` never takes its second path there and `ORDER BY id DESC`
    /// cannot be told from `ASC`. `job::list` has the same `Option` filter and covers both — this is
    /// that precedent applied.
    #[tokio::test]
    async fn a_listing_narrows_to_one_repository_and_puts_the_newest_first() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let third = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let everything = list(&pool, None).await.unwrap();
        assert_eq!(
            everything.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![third, second, first],
            "newest first: a caller reading a truncated list must see the present, not history"
        );

        let just_alpha = list(&pool, Some("alpha")).await.unwrap();
        assert_eq!(
            just_alpha.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![third, first]
        );

        // The fields that make a listing answer "what is this queue doing" rather than just
        // "something happened" — and the ones a positional row mapping could silently transpose.
        assert_eq!(just_alpha[0].op, "merge");
        assert_eq!(just_alpha[0].project_id, "alpha");
        assert_eq!(just_alpha[0].origin, "human");
        assert_eq!(just_alpha[0].status, "queued");
        assert!(!just_alpha[0].created_at.is_empty());
    }

    /// A `running` row at startup means the daemon died mid-operation, and nothing can say whether git
    /// finished. Auto-retry is not an option: a re-run `merge` is harmless, a re-run `tag` is not, and
    /// telling them apart from a cold start is guessing. It is recorded and left for a human — the
    /// same call `runs::reconcile_orphaned_runs` makes.
    #[tokio::test]
    async fn a_request_running_at_startup_is_marked_interrupted_not_retried() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();

        let reconciled = reconcile_interrupted(&pool).await.unwrap();

        assert_eq!(status_of(&pool, id).await, "interrupted");
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "an interrupted request must not re-enter the queue by itself"
        );
        // The status and the "does not re-enter the queue" assertions above would both still pass
        // if `reconcile_interrupted` matched every row but wrote `failure_reason` and `finished_at`
        // as NULL, or if it reported the wrong row count to its caller (Task 8 relies on the count
        // to decide whether to log anything). `failure_reason` in particular is the only thing that
        // will ever tell a human this was a restart rather than an ordinary failure.
        assert_eq!(reconciled, 1);
        let (failure_reason, finished_at): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT failure_reason, finished_at FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            failure_reason.as_deref(),
            Some("daemon restarted mid-operation")
        );
        assert!(
            finished_at.is_some(),
            "an interrupted request is terminal and must record when"
        );
    }

    /// The reconcile and a late `finish` have to compose, not merely each be correct.
    ///
    /// `finish`'s `AND status = 'running'` guard was written for this exact collision — a worker
    /// whose future outlived the reconcile — but until now nothing put the two real functions in
    /// sequence: the guard's own test reaches `interrupted` by writing that status by hand. So the
    /// claim held by inspection of two SQL statements and by nothing else, which is how a guard
    /// gets dropped in a refactor that only reads one of them.
    ///
    /// What is actually protected is the audit record: if the late write landed, a row a restart
    /// interrupted would read `succeeded`, and the reason it says so would be gone.
    #[tokio::test]
    async fn a_reconciled_request_cannot_be_finished_by_a_late_worker() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        reconcile_interrupted(&pool).await.unwrap();

        let late = finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: Some("abc123".into()),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await;

        assert!(
            matches!(late, Err(sqlx::Error::RowNotFound)),
            "a finish arriving after the reconcile must be refused, not silently applied"
        );
        assert_eq!(status_of(&pool, id).await, "interrupted");
        let (reason, sha): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT failure_reason, result_sha FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("daemon restarted mid-operation"));
        assert_eq!(
            sha, None,
            "the late write must not have left its sha behind"
        );
    }

    #[tokio::test]
    async fn the_queue_is_drained_in_order_and_each_outcome_recorded() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = FakeVcsExecutor::succeeding_with("abc123");
        drain_once(&pool, "alpha", &executor).await;
        drain_once(&pool, "alpha", &executor).await;

        assert_eq!(status_of(&pool, first).await, "succeeded");
        assert_eq!(status_of(&pool, second).await, "succeeded");
        assert_eq!(executor.calls(), 2);

        // What the executor was handed, not just how often. An executor that ignores its argument
        // reports the same two calls whether the claim gave it the right row or another
        // repository's, so counting alone cannot catch a `claim_next` that returns the wrong one.
        let seen = executor.seen();
        // The order half of this test's name. Both rows end `succeeded` and every per-call
        // assertion below is byte-identical for the two, so without the ids a `claim_next` serving
        // newest-first would pass here unchanged.
        assert_eq!(
            seen.iter().map(|claimed| claimed.id).collect::<Vec<_>>(),
            vec![first, second],
            "the drain must serve the queue in arrival order"
        );
        for claimed in seen {
            assert_eq!(
                claimed.op,
                Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                }
            );
            assert_eq!(claimed.project_id, "alpha");
            assert_eq!(claimed.project_root, "C:/repo");
        }

        // The outcome has to travel from the executor into the row. `drain_once` is the only thing
        // that carries it there, and the status assertions above pass just as well if it drops the
        // sha and writes a canned success of its own.
        let sha: Option<String> =
            sqlx::query_scalar("SELECT result_sha FROM vcs_requests WHERE id = ?")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sha.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn a_failing_operation_is_recorded_and_frees_the_repository() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        drain_once(
            &pool,
            "alpha",
            &FakeVcsExecutor::failing_with("CONFLICT (content)"),
        )
        .await;

        assert_eq!(status_of(&pool, id).await, "failed");
        let reason = failure_reason_of(&pool, id).await;
        assert!(
            reason.contains("CONFLICT"),
            "the recorded reason must be the executor's own: {reason}"
        );

        // The point of this half: a failure must not leave the repository claimed forever.
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
    }

    /// The return value is the only thing Chunk 2's loop can terminate on, and both tests above
    /// discard it — so `true` unconditionally and `false` unconditionally each pass them.
    ///
    /// Neither is harmless. `true` always spins a `while drain_once(..).await {}` at 100% CPU;
    /// `false` always drains one request per poll interval forever, which looks like a slow queue
    /// rather than a bug. `#[must_use]` cannot stand in for this: on an `async fn` it marks the
    /// future, which every caller already awaits.
    #[tokio::test]
    async fn a_drain_says_whether_it_found_anything_to_do() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let busy = FakeVcsExecutor::succeeding_with("abc123");
        assert!(
            drain_once(&pool, "alpha", &busy).await,
            "a drain that claimed and executed a request has done something"
        );

        // A second executor, so the count below is exact rather than merely unchanged.
        let idle = FakeVcsExecutor::succeeding_with("def456");
        assert!(
            !drain_once(&pool, "alpha", &idle).await,
            "there is nothing left to claim, so the caller should wait rather than drain again"
        );
        assert_eq!(
            idle.calls(),
            0,
            "an idle drain must not reach the executor at all"
        );
    }

    /// Spec §6.4 step 5 ends "escreve `result_sha`, `succeeded`, **feed**", and spec §2.1 is why:
    /// this pillar has no view of its own precisely because every transition writes to `feed.rs`,
    /// which the shell already shows. Without this row a merge the daemon performed is invisible to
    /// the person who asked for it.
    #[tokio::test]
    async fn a_finished_request_is_reported_in_the_feed() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let lines: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT summary, subject FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].0.contains("succeeded"), "got: {}", lines[0].0);
        assert_eq!(lines[0].1, Some(format!("vcs:{id}")));
    }

    /// A merge of a job's own branch into a branch outside `nucleos/` is the land the distiller
    /// learns from; any other merge is not, and queues nothing.
    #[tokio::test]
    async fn a_landed_job_branch_is_queued_for_distillation() {
        let pool = test_pool().await;
        // `repo()` resolves to project `alpha`, so the job has to live in the same project.
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (7, 'alpha', 'C:/repo', 'completed', 5, '2026-10-05T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // A feature branch landing is not a job landing.
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;
        let none: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT cause, job_id FROM distill_queue")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(none.is_empty(), "a feature-branch merge queued: {none:?}");

        let op = Op::Merge {
            source: "nucleos/job-7".into(),
            target: "master".into(),
        };
        submit(&pool, &repo(), &op, Origin::Human).await.unwrap();
        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("def456")).await;

        let rows: Vec<(String, String, Option<i64>, Option<i64>)> =
            sqlx::query_as("SELECT cause, project_id, job_id, item_id FROM distill_queue")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![("job_landed".to_string(), "alpha".to_string(), Some(7), None)]
        );
    }

    /// An escalation is the one outcome whose reason has to reach the feed, and the one whose
    /// reader did not ask for it.
    ///
    /// Every other status answers a question somebody is waiting on, so the id and the word send
    /// them to the row. This one tells a person they now own something they never asked about and
    /// are not watching for — and `vcs request 1 escalated` gives them no way to tell whether it is
    /// theirs without going to look. Spec §2.1 leaves this module no view of its own, so if the
    /// reason is not legible here it is legible nowhere.
    #[tokio::test]
    async fn an_escalation_carries_its_reason_into_the_feed_and_the_others_do_not() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(
            &pool,
            "alpha",
            &FakeVcsExecutor::escalating_with("Merge conflict in seed.txt"),
        )
        .await;

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("escalated"), "got: {}", summaries[0]);
        assert!(
            summaries[0].contains("Merge conflict in seed.txt"),
            "the reason has to travel with it: {}",
            summaries[0]
        );

        // And the row says a person owns it, which `failed` could not say.
        let status: String = sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "escalated");
        assert!(
            TERMINAL_STATUSES.contains(&status.as_str()),
            "terminal on purpose: nothing resumes an escalation, so a waiter must not hang on one"
        );
    }

    /// Everything the daemon logged while `body` ran.
    ///
    /// Follows `logging.rs`'s own test rather than `init()`, which installs a *global* subscriber and
    /// would panic the moment a second test did the same; `set_default` is scoped and thread-local,
    /// which is sound here because `#[tokio::test]`'s default runtime polls on the thread that set
    /// it. The writer is the non-blocking one, so the guard has to be dropped before reading back.
    ///
    /// Worth the machinery for exactly one reason: on the refused-write path the log line is not
    /// commentary, it is the only place the outcome still exists.
    async fn logged_during<F: std::future::Future>(body: F) -> String {
        let dir = tempfile::tempdir().unwrap();
        let (writer, flush_on_drop) = tracing_appender::non_blocking(
            tracing_appender::rolling::daily(dir.path(), "test.log"),
        );
        {
            let subscriber = tracing_subscriber::fmt().with_writer(writer).finish();
            let _scope = tracing::subscriber::set_default(subscriber);
            body.await;
        }
        drop(flush_on_drop);
        std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect()
    }

    /// The outcome of a merge the row refuses has nowhere else to go.
    ///
    /// `finish` consumed the outcome before this branch existed, so a real `abc123` was dropped on
    /// the floor: the row read `interrupted`, and a commit the daemon caused was recorded nowhere in
    /// the system. The log line is the whole remedy, which makes it load-bearing rather than
    /// commentary — and the previous version of it announced a jam that does not happen on this
    /// path, sending a reader hunting a stuck queue that is actually fine.
    ///
    /// Asserting on log text is not this crate's habit and should stay rare. It is justified here
    /// because both halves — that the sha survives, and that the message does not misdescribe the
    /// state — are invisible to every other assertion available.
    #[tokio::test]
    async fn an_outcome_the_row_refuses_survives_in_the_log_and_is_not_called_a_jam() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let executor = FakeVcsExecutor::succeeding_until_released("abc123");

        let logged = logged_during(async {
            tokio::join!(drain_once(&pool, "alpha", &executor), async {
                // The reconcile has to land while the operation is running, and both edges of that
                // window are named rather than timed: `entered` is the operation beginning, and it
                // cannot end until `release`. This was a 10ms sleep inside a 100ms delay, which is
                // the same sentence written as a wager on two SQLite writes.
                executor.entered(1).await;
                let reconciled = reconcile_interrupted(&pool).await.unwrap();
                executor.release();
                reconciled
            })
        })
        .await;

        assert!(
            logged.contains("abc123"),
            "the sha the row refused must survive somewhere: {logged}"
        );
        assert!(
            !logged.contains("stays running"),
            "the row is terminal, so the queue is not jammed and must not be reported as one: {logged}"
        );
    }

    /// A restart's reconcile landing while the operation is still running — the collision that makes
    /// `finish`'s refusal a real path rather than a defensive one, seen from the drain's side.
    ///
    /// `a_reconciled_request_cannot_be_finished_by_a_late_worker` covers `finish` refusing directly.
    /// What only this can show is what `drain_once` does with the refusal: it must not treat a
    /// terminal row as a jam, must not lose that the work happened, and must leave the interrupted
    /// row exactly as the reconcile wrote it. The sha the executor produced survives only in a log
    /// line from here — the row is entitled to refuse it, and does.
    ///
    /// The reconcile runs *during* the operation because that is when it really happens, and it can:
    /// the drain holds no pooled connection while it awaits the executor, so a single-connection
    /// pool still answers.
    #[tokio::test]
    async fn a_drain_whose_row_was_reconciled_out_from_under_it_does_not_overwrite_the_record() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        // The reconcile must land strictly inside the operation, so the operation is held open
        // around it rather than given a duration long enough to probably cover it. `reconciled == 1`
        // below is what would fail if that ever stopped being true — loudly, which is how the old
        // 10ms-sleep-inside-100ms version was caught.
        let executor = FakeVcsExecutor::succeeding_until_released("abc123");
        let (drained, reconciled) = tokio::join!(drain_once(&pool, "alpha", &executor), async {
            executor.entered(1).await;
            let reconciled = reconcile_interrupted(&pool).await.unwrap();
            executor.release();
            reconciled
        });

        assert_eq!(
            reconciled, 1,
            "the reconcile must have caught the row mid-operation"
        );
        assert_eq!(executor.calls(), 1);
        assert!(
            drained,
            "the merge ran; a refused terminal write must not be reported as an idle tick"
        );

        let (status, sha, reason): (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, result_sha, failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "interrupted");
        assert_eq!(
            sha, None,
            "the late write must not have left its sha behind"
        );
        assert_eq!(
            reason.as_deref(),
            Some("daemon restarted mid-operation"),
            "the reconcile's account of why must survive the drain arriving after it"
        );

        // The feed append is conditional on the terminal write having succeeded, and this is the
        // only assertion in the module that says so. The row reads `interrupted`; an unconditional
        // append would sit "vcs request 1 succeeded" beside it, in the one surface spec §2.1 says
        // the user actually looks at — a contradiction, and one no status assertion can see because
        // the row is already correct.
        let announced: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            announced.is_empty(),
            "a terminal write the row refused must not be announced as a finish: {announced:?}"
        );

        // And the repository is free: a refused write means the row is terminal, so the queue moves
        // on rather than waiting behind it.
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
    }

    /// The jam window `drain_once`'s doc comment admits to, composed with the thing that closes it.
    ///
    /// Two functions have to agree for that claim to hold, and inspecting either alone does not show
    /// it — the same gap `a_reconciled_request_cannot_be_finished_by_a_late_worker` was written for.
    ///
    /// The NOTE above explains why the *claim's* rollback-on-drop cannot be tested here: an
    /// abandoned claim never returns the single connection a `:memory:` pool has, so every attempt
    /// ends in `PoolTimedOut`. That does not transfer to this case. `claim_next` commits and drops
    /// its `Transaction` before returning, so while the drain is awaiting the executor it holds no
    /// pooled connection at all — dropping it there leaves the pool free to answer the assertions.
    #[tokio::test]
    async fn a_drain_abandoned_mid_operation_jams_the_repository_until_a_restart_reconciles() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        // Never released, so the operation cannot finish at all — as opposed to the 30s this used
        // to sleep, which was a duration chosen to be longer than a guard rather than a statement
        // that finishing is not one of the things that may happen here.
        let executor = FakeVcsExecutor::succeeding_until_released("abc123");
        {
            let drain = drain_once(&pool, "alpha", &executor);
            tokio::pin!(drain);

            // Polled until the executor has actually been ENTERED, and abandoned there — rather
            // than after a fixed slice of wall clock.
            //
            // This was `timeout(50ms, drain)`, which reads like the same thing and is not. Holding
            // the executor open settles only the half AFTER the operation begins; before that,
            // `drain_once` still has a reap and a claim to get through, and 50ms was a real-time
            // budget for two SQLite writes. On a machine also compiling and running the other
            // thousand tests that budget is occasionally missed, and the drain is then abandoned
            // BEFORE the operation — a different scenario wearing this test's name, which is why
            // the flake read `calls(): 0 != 1` rather than anything about jamming.
            //
            // Entering the executor is the property this test is about, so it is what is waited on.
            // The `select!` is still needed to keep the drain being polled — nothing else drives
            // it — and its first arm turns "the drain finished" into a message rather than a
            // confusing later assertion. The outer timeout is not a budget: it is reached only if
            // the drain never arrives at all, and it is there so that failure is a message rather
            // than a hung suite.
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    _ = &mut drain => {
                        panic!("the executor is never released — the drain cannot have finished")
                    }
                    () = executor.entered(1) => {}
                }
            })
            .await
            .expect("the drain never reached the executor");
        }

        // Dropped at the closing brace above, which is the executor's await, so `finish` never ran.
        // This is the documented cost, not a defect: the row is stranded exactly as the doc
        // comment says it is.
        assert_eq!(status_of(&pool, id).await, "running");
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "the stranded row holds this repository's only slot — nothing else may claim it"
        );

        // And the compensator is what releases it, which is the half that makes the window
        // acceptable rather than merely admitted.
        assert_eq!(reconcile_interrupted(&pool).await.unwrap(), 1);
        assert_eq!(status_of(&pool, id).await, "interrupted");
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_some(),
            "once reconciled, the repository is free again"
        );
    }

    /// The worker looks for repositories by their KEY, and in production a key is never a project's
    /// name — it is git's canonical common directory. Polling `DISTINCT project_id` here would hand
    /// `claim_next` a label that no row carries, and the queue would drain nothing, for ever, in
    /// silence.
    ///
    /// **Every other test in this module is structurally blind to that.** `repo_for` makes the key
    /// equal the project name on purpose, so that moving the lock from label to key preserved each
    /// existing assertion's meaning. The cost of that choice is exactly this blindness, and it is
    /// not hypothetical: a mutation that polls `project_id` passed all 41 of the others. This is the
    /// one place where the two must differ.
    #[tokio::test]
    async fn the_worker_looks_for_repositories_by_key_and_not_by_project_name() {
        let pool = test_pool().await;
        let repo = ResolvedRepo::synthetic("alpha", "C:/repo", "a-key-that-is-not-a-project-name");
        let id = submit(&pool, &repo, &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, id).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "the worker never found the repository — it is looking for it by the project's name",
        );
    }

    /// Two repositories, one worker. If it drains them one after the other, neither of these executions
    /// can complete: the fake will not answer until both have arrived.
    #[tokio::test]
    async fn separate_repositories_are_drained_concurrently() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::rendezvous_of(2, "abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, first).await == "succeeded"
                    && status_of(&pool, second).await == "succeeded"
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "both repositories must be in flight at once; a worker that serialises projects deadlocks here",
        );
    }

    /// The loop keeps looping — the property the daemon's whole use of this worker rests on, and the
    /// one both tests around it are structurally blind to.
    ///
    /// They submit everything *before* the spawn, so a worker that polls exactly once and returns
    /// passes them both. In production that worker drains nothing, ever: at startup the queue is
    /// empty, and every request arrives afterwards. The failure would not be "wrong at an edge", it
    /// would be "the pillar does nothing", with the suite green.
    ///
    /// **No timing assertion, and no sleep to let a poll go by.** The first request is what proves
    /// the worker's first pass already happened — it cannot have succeeded otherwise — so the second
    /// is submitted into a worker that is provably past that pass. The only wait is for the second to
    /// finish, inside a budget 10x `WORKER_POLL_INTERVAL`, which is this file's documented margin.
    ///
    /// The second request goes to a **different repository** deliberately. A one-pass worker's
    /// spawned task loops on the repository it was given, so a second request for `alpha` could be
    /// swept up by a task that happened to still be draining — the test would then pass for a reason
    /// that is not the property. `beta` was in no pass that worker ever made, so only another poll
    /// can reach it.
    #[tokio::test]
    async fn a_request_submitted_after_the_worker_started_is_still_drained() {
        let pool = test_pool().await;
        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let drained_once = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, first).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        // Not the property under test — it is the precondition for it. Asserted separately so a
        // worker that never started at all is told apart from one that started and stopped.
        drained_once.expect("the worker's first pass should drain what was queued for it");

        // Submitted only now: the pass above is over, so nothing but the wake from `submit` (or the
        // 30s fallback, far past the budget below) can find this.
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, second).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "the worker must keep polling; one that stops after its first pass would drain nothing \
             the daemon is ever actually asked to do",
        );
    }

    /// The worker sleeps up to `WORKER_POLL_INTERVAL` (30s) when idle, so a request that arrives in
    /// that sleep must be found because `submit` woke the worker, not because a poll came round. The
    /// budget is a fraction of the fallback, so a missing wake fails here instead of passing late.
    #[tokio::test]
    async fn an_enqueue_wakes_an_idle_worker_well_before_the_fallback_poll() {
        let pool = test_pool().await;
        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        // Precondition: one request drained proves the worker is past its first pass and idle.
        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let wait_until = |id: i64| {
            let pool = pool.clone();
            async move {
                loop {
                    if status_of(&pool, id).await == "succeeded" {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), wait_until(first))
            .await
            .expect("the worker's first pass should drain what was queued for it");

        let started = std::time::Instant::now();
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let settled =
            tokio::time::timeout(std::time::Duration::from_secs(5), wait_until(second)).await;
        worker.abort();

        settled.expect("an enqueue must wake the idle worker; the fallback poll is 30s away");
        assert!(
            started.elapsed() < WORKER_POLL_INTERVAL / 2,
            "found by the fallback poll rather than the wake"
        );
    }

    /// And the other half, which the rendezvous cannot show: one repository's requests still run one at
    /// a time, in order.
    ///
    /// **What this holds, exactly, now that mutation has measured it.** Its ordering assertion is
    /// redundant against `the_queue_is_served_in_arrival_order` and
    /// `the_queue_is_drained_in_order_and_each_outcome_recorded` — reversing `claim_next`'s `ORDER BY`
    /// reddens all three. It is kept because those two call `drain_once` by hand, twice, and this is
    /// the only test where the *worker* is what reaches the second request: it pins that a spawned
    /// task drains a repository rather than one request of it.
    ///
    /// It does not pin *when*. Draining one request per tick instead of until empty passes this
    /// unchanged, because the tick is 500ms and the budget below is 5s. Closing that would take an
    /// elapsed-time assertion against `WORKER_POLL_INTERVAL` with roughly 2x of margin, which is
    /// under this file's convention and is the flaky bet
    /// `separate_repositories_are_drained_concurrently` was written to avoid. The gap is named here
    /// rather than papered over.
    #[tokio::test]
    async fn one_repository_is_still_drained_in_order_one_at_a_time() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, second).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect("the worker should drain the queue");
        assert_eq!(status_of(&pool, first).await, "succeeded");
        assert_eq!(
            executor
                .seen()
                .iter()
                .map(|request| request.id)
                .collect::<Vec<_>>(),
            vec![first, second],
            "arrival order, and each one only after the last finished"
        );
    }

    /// The empty-queue common case: the request is already `succeeded` before `wait_for` is ever
    /// called, so the answer must come from the first read — no sleep, no `WAIT_POLL_INTERVAL`
    /// paid at all.
    #[tokio::test]
    async fn a_finished_request_returns_its_outcome_without_waiting() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_secs(10)).await.unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // What this still proves, and what it gave up. A loop that ignored `succeeded` and ran to
        // its deadline would report the same status and sha ten seconds later and still pass every
        // assertion above; a second is nowhere near either side of that. The bound used to be
        // `WAIT_POLL_INTERVAL`, which also caught one stray poll before the first read, but one
        // slow read on a loaded machine outlasts a poll as well, and that bound failed the sibling
        // below with no defect to find (2026-09-13). No wall-clock bound tells "never slept" from
        // "slept once" on a shared machine, so this no longer claims to.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an already-finished request must end the wait, not run it out"
        );
    }

    /// The deadline hands back a ticket, never an error: "still queued" is not a failure, and an agent
    /// told it failed would either give up or retry — both wrong.
    #[tokio::test]
    async fn an_unfinished_request_hands_back_a_ticket_rather_than_failing() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(ticket.status, "queued");
        assert!(ticket.result_sha.is_none());
    }

    /// The two tests above never read `failure_reason` or `id` — both come out `None`/moot in
    /// every case they cover, so a `wait_for` that dropped `failure_reason`, or swapped it for
    /// `output_tail`, would still pass them. A failed request is the only scenario that puts a real
    /// value in that column, so it is the only thing that can catch that class of bug.
    #[tokio::test]
    async fn a_failed_requests_ticket_carries_its_id_and_its_reason() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(
            &pool,
            "alpha",
            &FakeVcsExecutor::failing_with("CONFLICT (content)"),
        )
        .await;

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_secs(10)).await.unwrap();

        assert_eq!(ticket.id, id);
        assert_eq!(ticket.status, "failed");
        assert!(
            ticket.result_sha.is_none(),
            "nothing succeeded, so there is no commit"
        );
        assert_eq!(ticket.failure_reason.as_deref(), Some("CONFLICT (content)"));
        // Without this, a `wait_for` that dropped `failed` from its terminal set would still
        // report the right content ten seconds later once the deadline forced an answer, and every
        // assertion above would still pass. This is what actually proves `failed` ends the wait as
        // fast as `succeeded` does, rather than merely agreeing with it once time runs out. Seconds
        // against one, not 50ms against `WAIT_POLL_INTERVAL`: on a loaded machine a single slow
        // read outlasted the poll interval and failed this with nothing wrong (2026-09-13).
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a failed request must end the wait, not run it out"
        );
    }

    /// Terminal, and terminal in the way that matters: a caller waiting on a blocked request must be
    /// told now, not at the deadline. The elapsed assertion is the whole test — a wait that ran to its
    /// deadline would return the identical ticket.
    #[tokio::test]
    async fn a_blocked_request_ends_the_wait_rather_than_running_it_out() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Blocked {
                reason: "uncommitted changes in the target worktree are in the way".into(),
                output_tail:
                    "error: Your local changes to the following files would be overwritten by merge:\n\tnotes.txt"
                        .into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(status_of(&pool, id).await, "blocked");

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_secs(10)).await.unwrap();
        assert_eq!(ticket.status, "blocked");
        assert!(
            ticket.failure_reason.unwrap().contains("in the way"),
            "the ticket must carry why it is blocked, or the agent cannot act on it"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "blocked is terminal: the wait must not run to its deadline"
        );
    }

    #[tokio::test]
    async fn cancelling_a_run_cancels_the_requests_it_had_not_started() {
        let pool = test_pool().await;
        let queued = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        assert_eq!(cancel_for_run(&pool, 7).await.unwrap(), 1);
        assert_eq!(status_of(&pool, queued).await, "cancelled");
    }

    /// Spec §7 is explicit: an operation already in flight finishes. The queue could not stop it in any
    /// case — the git it spawned belongs to the daemon, not to the run whose context ran out.
    #[tokio::test]
    async fn cancelling_a_run_does_not_touch_an_operation_already_in_flight() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        // Approved, then claimed — the shape Chunk 4 will produce.
        sqlx::query("UPDATE vcs_requests SET status = 'queued' WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        assert_eq!(cancel_for_run(&pool, 7).await.unwrap(), 0);
        assert_eq!(status_of(&pool, id).await, "running");
    }

    #[tokio::test]
    async fn cancelling_a_run_leaves_another_runs_requests_alone() {
        let pool = test_pool().await;
        let mine = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        let theirs = submit(&pool, &repo(), &merge_op(), Origin::Run(8))
            .await
            .unwrap();

        cancel_for_run(&pool, 7).await.unwrap();

        assert_eq!(status_of(&pool, mine).await, "cancelled");
        assert_eq!(status_of(&pool, theirs).await, "awaiting_approval");
    }

    /// A human's request is not a run's request, even when a run is what happens to be ending.
    #[tokio::test]
    async fn a_humans_request_survives_a_run_ending() {
        let pool = test_pool().await;
        let human = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        cancel_for_run(&pool, 7).await.unwrap();

        assert_eq!(status_of(&pool, human).await, "queued");
    }

    /// The smallest `runs` row the table will accept: `prompt`, `status` and `created_at` are its
    /// only NOT NULL columns without a default (`core/migrations/0002_runs.sql`). Written by hand
    /// rather than by spawning a run, because the one column the reaper reads is `status`, and going
    /// through `runs.rs` would drag a whole runner in to set it.
    ///
    /// The id is a parameter rather than returned, so a test can name the same number in the
    /// request's `Origin::Run` and read as one fact what the reaper has to join on.
    async fn insert_run(pool: &sqlx::SqlitePool, id: i64, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, created_at)
             VALUES (?, 'merge it', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Every run status the schema names — `0002_runs.sql`'s own comment, as data.
    ///
    /// Which of them are *ended* is not decided here: `runs::ENDED_RUN_STATUSES` decides, and the two
    /// reaper tables partition this list by it. Kept separate so the reaper's two halves are provably
    /// exhaustive over the vocabulary rather than over whichever cases somebody thought of.
    const EVERY_RUN_STATUS: &[&str] = &[
        "running",
        "completed",
        "failed",
        "timed_out",
        "cancelled",
        "awaiting_approval",
        "interrupted",
    ];

    /// A request a run asked for that is past approval and waiting its turn — the state every one of
    /// these tests is about, and the one `submit` cannot produce directly, since a `Run` request
    /// starts `awaiting_approval`.
    async fn queued_request_for_run(pool: &sqlx::SqlitePool, run_id: i64) -> i64 {
        let id = submit(pool, &repo(), &merge_op(), Origin::Run(run_id))
            .await
            .unwrap();
        sqlx::query("UPDATE vcs_requests SET status = 'queued' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    /// The push half cannot be complete — a run's terminal status is written elsewhere without going
    /// through `finalize_termination`. This is the half that does not depend on anybody remembering.
    ///
    /// **Driven by `runs::ENDED_RUN_STATUSES` itself, never by the statuses written out again here.**
    /// That constant's doc comment names a second copy drifting from it as the precise failure it
    /// exists to prevent, and a list retyped in this test would BE that second copy. It was one:
    /// replacing the reaper's bind loop with four hard-coded strings, two of them nonsense, passed
    /// the whole suite — because `timed_out` was the only status this SQL was ever handed, while
    /// `cancelled`, `failed` and `interrupted` were covered on `ends_the_run`'s side only, and that
    /// is the side the reaper exists because of. Iterating the constant is what stops the coverage
    /// falling behind it again.
    #[tokio::test]
    async fn a_request_whose_run_died_by_some_other_door_is_reaped_before_the_next_claim() {
        assert!(
            !crate::execution::runs::ENDED_RUN_STATUSES.is_empty(),
            "an empty status list would make this test pass by iterating nothing, while the reaper \
             it drives reaped nothing either"
        );
        for status in crate::execution::runs::ENDED_RUN_STATUSES {
            // A database per status rather than one holding them all, so each iteration is exactly
            // the single-status case: one queued request, one drain. Sharing a pool would put every
            // request in one repository's FIFO, where a reaper that swept only the head would be
            // indistinguishable from one that swept them all.
            let pool = test_pool().await;
            insert_run(&pool, 7, status).await;
            let id = queued_request_for_run(&pool, 7).await;

            let executor = FakeVcsExecutor::succeeding_with("abc123");
            assert!(
                !drain_once(&pool, "alpha", &executor).await,
                "{status}: the reap leaves nothing claimable, so the drain must report the queue \
                 empty"
            );

            assert_eq!(
                status_of(&pool, id).await,
                "cancelled",
                "a request whose run is {status} must be reaped"
            );
            // The half that a status assertion alone cannot make: a merge that ran and was then
            // overwritten with `cancelled` would satisfy the line above and still have touched the
            // repository on behalf of a run that no longer exists.
            assert_eq!(
                executor.calls(),
                0,
                "{status}: a dead run's merge must never reach git"
            );
        }
    }

    /// The run's row is gone, and the request has to go with it.
    ///
    /// No foreign key ties `vcs_requests.run_id` to `runs` (`0048_vcs_requests.sql`) and rows really
    /// are deleted from `runs`, so this is an ordinary state rather than a corrupt one. It is also
    /// the *only* state that tells the reaper's predicate apart from its inverse — `EXISTS(ended)`
    /// and `NOT EXISTS(alive)` agree on every row whose run still exists. The version that kept such
    /// a request would leave `drain_once` to claim it and execute a merge on behalf of a run that is
    /// certainly not running, because it is not there.
    ///
    /// The run is inserted and then deleted rather than never written, so the row under test is the
    /// one production produces — a request that named a real run whose record was later removed —
    /// and not a request that named a number nothing ever used.
    #[tokio::test]
    async fn a_request_whose_run_row_is_gone_is_reaped_rather_than_kept_for_ever() {
        let pool = test_pool().await;
        insert_run(&pool, 7, "running").await;
        let id = queued_request_for_run(&pool, 7).await;
        sqlx::query("DELETE FROM runs WHERE id = 7")
            .execute(&pool)
            .await
            .unwrap();

        let executor = FakeVcsExecutor::succeeding_with("abc123");
        assert!(
            !drain_once(&pool, "alpha", &executor).await,
            "the reap leaves nothing claimable, so the drain must report the queue empty"
        );

        assert_eq!(status_of(&pool, id).await, "cancelled");
        assert_eq!(
            executor.calls(),
            0,
            "a merge for a run that no longer exists must never reach git"
        );
    }

    /// The trap, again and one level deeper: a run paused for a human resumes, and its merge must
    /// survive the pause. `status != 'running'` would fail this; a list of ended statuses passes it.
    #[tokio::test]
    async fn a_request_whose_run_is_only_paused_for_approval_is_not_reaped() {
        let pool = test_pool().await;
        insert_run(&pool, 7, "awaiting_approval").await;
        let id = queued_request_for_run(&pool, 7).await;

        assert_eq!(
            reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
            0
        );
        assert_eq!(status_of(&pool, id).await, "queued");
    }

    /// An owner-approved merge outlives the run that asked for it: approving put it back to
    /// `queued` with its `run_id` intact, and neither sweep may then cancel it.
    #[tokio::test]
    async fn an_owner_approved_request_survives_both_run_end_sweeps() {
        let pool = test_pool().await;
        insert_run(&pool, 7, "running").await;
        let id = queued_request_for_run(&pool, 7).await;
        sqlx::query(
            "UPDATE vcs_requests SET status = 'awaiting_owner', pending_map_blob = 'abc' WHERE id = ?",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE runs SET status = ? WHERE id = 7")
            .bind(crate::execution::runs::ENDED_RUN_STATUSES[0])
            .execute(&pool)
            .await
            .unwrap();
        approve_for_owner(&pool, id).await.expect("approve it");

        assert_eq!(
            reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
            0
        );
        assert_eq!(cancel_for_run(&pool, 7).await.unwrap(), 0);
        assert_eq!(status_of(&pool, id).await, "queued");
    }

    /// A request nobody's run owns is nobody's to reap.
    #[tokio::test]
    async fn a_humans_request_is_never_reaped() {
        let pool = test_pool().await;
        // An ended run exists, and is not this request's: without it the test would pass against a
        // reaper that simply found nothing ended, which is not what it claims to check.
        insert_run(&pool, 7, "cancelled").await;
        let human = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert_eq!(
            reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
            0
        );
        assert_eq!(status_of(&pool, human).await, "queued");
    }

    /// The complement of the reaping table, derived rather than retyped, so the two are exhaustive
    /// between them and neither can quietly stop covering a status.
    ///
    /// What must survive is everything `runs::ENDED_RUN_STATUSES` does not name, and spelling those
    /// out here would be a third copy of the list the constant exists to be the only one of. So the
    /// schema's vocabulary is filtered *by* the constant: `running` is a run still working;
    /// `completed` is a run that finished normally and asked for this merge, which is why
    /// `runs.rs`'s exit-code-zero status is deliberately absent from the constant; and
    /// `awaiting_approval` is the trap one level deeper — the case a filter written as
    /// `status != 'running'` would lose. That one keeps its own named test above as well, because it
    /// is the first thing to fail if the reaper's predicate is ever inverted the wrong way, and a
    /// canary is worth having by name.
    #[tokio::test]
    async fn a_request_whose_run_is_still_alive_is_not_reaped() {
        let alive: Vec<&str> = EVERY_RUN_STATUS
            .iter()
            .copied()
            .filter(|status| !crate::execution::runs::ENDED_RUN_STATUSES.contains(status))
            .collect();
        assert!(
            !alive.is_empty(),
            "every status the schema names is now an ended one, so this test would pass by \
             iterating nothing"
        );

        for status in alive {
            let pool = test_pool().await;
            insert_run(&pool, 7, status).await;
            let id = queued_request_for_run(&pool, 7).await;

            assert_eq!(
                reap_requests_of_ended_runs(&pool, "alpha").await.unwrap(),
                0,
                "a run at {status} has not ended, so its request must survive"
            );
            assert_eq!(
                status_of(&pool, id).await,
                "queued",
                "a run at {status} has not ended, so its request must survive"
            );
        }
    }

    /// A cancelled request is terminal, so a caller waiting on one must be answered rather than held to
    /// the deadline. This is the argument `a_blocked_request_ends_the_wait_rather_than_running_it_out`
    /// already makes for `blocked`, and it becomes true for `cancelled` in this task.
    #[tokio::test]
    async fn a_cancelled_request_ends_the_wait_rather_than_running_it_out() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        cancel_for_run(&pool, 7).await.unwrap();

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_secs(5)).await.unwrap();

        assert_eq!(ticket.status, "cancelled");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// A caller waiting on an id nothing ever inserted must not spend the deadline finding that
    /// out. Every id in circulation came from `submit`, which hands one back only after its INSERT
    /// commits, and nothing in this module ever deletes a row — so a missing row can never later
    /// appear, and treating it like "not finished yet" would silently burn the whole wait on a
    /// request that does not exist.
    #[tokio::test]
    async fn waiting_on_an_unknown_id_fails_immediately_rather_than_waiting_out_the_deadline() {
        let pool = test_pool().await;
        let started = std::time::Instant::now();

        let result = wait_for(&pool, 999_999, Duration::from_secs(5)).await;

        assert!(matches!(result, Err(sqlx::Error::RowNotFound)));
        // Two orders of magnitude under the 5s deadline: not a close race, just proof this
        // returned from the first read rather than polling until the deadline passed.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "an id that can never exist must not cost the caller the deadline"
        );
    }

    /// `awaiting_approval` is not treated as done: a human still has to act on it, and an agent
    /// told its request had reached a stable end state would stop watching a request that is very
    /// much still alive. It is handled exactly like `queued` — reported as-is once the deadline
    /// passes, never ending the wait early.
    #[tokio::test]
    async fn a_request_still_awaiting_approval_hands_back_a_ticket_too() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(ticket.status, "awaiting_approval");
        assert!(ticket.result_sha.is_none());
        assert!(ticket.failure_reason.is_none());
    }

    /// The actual point of a bounded wait, not just its two edges: a request that is still queued
    /// when `wait_for` starts but finishes partway through a generous deadline must be reported as
    /// soon as the next poll sees it — "if its turn comes, it gets the result" — not held until the
    /// deadline passes regardless. Neither test above exercises this: one starts already finished,
    /// the other never finishes at all, so a `wait_for` that read the row once and then only ever
    /// re-checked the clock would pass both.
    #[tokio::test]
    async fn a_request_that_finishes_mid_wait_is_reported_before_the_deadline() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(20));
        let started = std::time::Instant::now();
        // The deadline is two orders of magnitude past how long the operation actually takes: what
        // this proves is that `wait_for` returns once the row finishes, not that it merely survives
        // to a deadline that happens to still be far away.
        let (ticket, drained) = tokio::join!(wait_for(&pool, id, Duration::from_secs(30)), async {
            // A head start so `wait_for`'s first read sees "queued", not "running" — the loop, not
            // a lucky initial read, is what has to notice the finish.
            tokio::time::sleep(Duration::from_millis(5)).await;
            drain_once(&pool, "alpha", &executor).await
        });

        assert!(drained, "the operation ran");
        let ticket = ticket.unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // A 6x margin under the 30s deadline: the operation itself finishes around 25ms in and the
        // poll interval catches it shortly after. Seconds, not 500ms: under a full suite on a
        // loaded machine this took 650ms and then 1.19s with nothing wrong (2026-10-15), the same
        // lesson the `failed` test above learned on 2026-09-13. A wait held to its deadline would
        // still take 30s, so 5s keeps the test's whole point.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait_for must return once the request finishes, not hold the caller to the full \
             deadline: took {:?}",
            started.elapsed()
        );
    }

    /// Everything, once: a request submitted through the queue, drained by the real executor, against a
    /// real repository — and the sha in the row is the commit git actually created.
    #[tokio::test]
    async fn a_real_merge_lands_through_the_queue() {
        let _lock = crate::execution::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            crate::execution::git_exec::testkit::repo_with_a_branch_to_merge("nucleos-vcs-e2e-");
        let roots = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::execution::git_exec::tests::WorktreeRootEnv::set(roots.path());

        // Real root, synthetic key: what this test exercises is the executor against a repository
        // that is really there, and `repo_for`'s "the key is the project name" is what keeps
        // `drain_once(&pool, "alpha", ..)` below meaning what it meant.
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        assert!(
            drain_once(
                &pool,
                "alpha",
                &crate::execution::git_exec::GitExecutor::default()
            )
            .await
        );

        assert_eq!(status_of(&pool, id).await, "succeeded");
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert_eq!(
            ticket.result_sha.unwrap(),
            crate::execution::git_exec::tests::sha_of(&repo, "master")
        );
    }

    /// The other half of that wiring, and the half a happy path structurally cannot see: `execute`
    /// has to report what `publish` *answered*, not merely that it called it.
    ///
    /// Measured, not assumed. Rewriting the merge arm to run `publish`, discard its `Outcome` and
    /// return `Succeeded { sha: computed.new }` passes all 1059 other tests in this crate — the e2e
    /// test above included, because on its happy path the publish does land and the two shas agree.
    /// What that would ship is the worst row this pillar can write: `succeeded`, naming a sha that
    /// is on no branch, announced in the feed to the person who asked for it. Only a publish that
    /// refuses tells the two apart, so this drives one — the user is mid-edit on the very file the
    /// merge brings in, which is `a_user_s_uncommitted_file_blocks_the_publish_and_survives_it_untouched`
    /// seen from the queue's side rather than from `publish`'s.
    ///
    /// It also pins the feed's status as *derived* rather than canned, which is the whole reason
    /// `Outcome::status()` exists as one function: the summary here reads `blocked`, so a hardcoded
    /// "succeeded" cannot survive both this and `a_finished_request_is_reported_in_the_feed`.
    #[tokio::test]
    async fn a_merge_the_queue_could_not_publish_is_not_recorded_as_succeeded() {
        let _lock = crate::execution::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = crate::execution::git_exec::testkit::repo_with_a_branch_to_merge(
            "nucleos-vcs-e2e-blocked-",
        );
        let roots = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::execution::git_exec::tests::WorktreeRootEnv::set(roots.path());

        // `feature.txt` is what `feat/x` adds, so the fast-forward has to write it — and it cannot,
        // because the user has an uncommitted copy of it sitting there.
        std::fs::write(repo.join("feature.txt"), "half-finished thought\n").expect("write");
        let before = crate::execution::git_exec::tests::sha_of(&repo, "master");

        // Real root, synthetic key, for the reason the test above states.
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        assert!(
            drain_once(
                &pool,
                "alpha",
                &crate::execution::git_exec::GitExecutor::default()
            )
            .await
        );

        assert_eq!(status_of(&pool, id).await, "blocked");
        assert_eq!(
            crate::execution::git_exec::tests::sha_of(&repo, "master"),
            before,
            "nothing was published, so the row must not claim anything was"
        );
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert!(
            ticket.result_sha.is_none(),
            "nothing landed, so there is no commit to name: {ticket:?}"
        );

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(summaries, vec![format!("vcs request {id} blocked")]);
    }

    /// Design decision #4, and the land design's third test: the target moving once while a merge
    /// is being computed is ordinary contention, not a reason to hand the row back to an asker who
    /// is not watching for it. The SAME row goes back to `queued`, `attempts` becomes 1, and it
    /// lands on the retry — proving the ticket a caller is holding survives the requeue rather than
    /// silently becoming a different row.
    #[tokio::test]
    async fn a_target_that_moved_once_is_requeued_under_the_same_ticket_and_lands_on_retry() {
        let pool = test_pool().await;
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", "C:/repo", "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        let moved = FakeVcsExecutor::failing_with(
            "master moved while the merge was being computed, so it was not published; resubmit",
        );
        assert!(drain_once(&pool, "alpha", &moved).await);
        assert_eq!(
            status_of(&pool, id).await,
            "queued",
            "a moved target is retried by the queue itself, not handed back terminal"
        );
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(attempts, 1);

        let lands = FakeVcsExecutor::succeeding_with("cafe");
        assert!(drain_once(&pool, "alpha", &lands).await);

        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert_eq!(
            ticket.id, id,
            "the same ticket the caller was already holding"
        );
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("cafe"));
    }

    /// F3-10: a revert is the daemon's own operation (spec 2026-10-05 §6.2). `admit` is the one
    /// INSERT behind all four doors, so a run or job origin is refused there and writes no row.
    #[tokio::test]
    async fn a_revert_from_a_run_or_job_origin_is_refused_and_writes_no_row() {
        let pool = test_pool().await;
        let repo = ResolvedRepo::synthetic("alpha", "C:/repo", "alpha");
        let revert = || Op::Revert {
            merge_sha: CommitSha::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            target: Branch::new("master").unwrap(),
        };

        assert!(
            submit(&pool, &repo, &revert(), Origin::Run(7))
                .await
                .is_err()
        );
        assert!(
            submit(&pool, &repo, &revert(), Origin::Job(7))
                .await
                .is_err()
        );
        assert!(
            submit_declared(&pool, &repo, &revert(), Origin::Run(7))
                .await
                .is_err()
        );
        assert!(
            submit_declared(&pool, &repo, &revert(), Origin::Job(7))
                .await
                .is_err()
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "a refused revert leaves nothing in the queue");

        submit_declared(&pool, &repo, &revert(), Origin::Daemon)
            .await
            .expect("the daemon's own revert is still admitted");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// A revert is published by the same compare-and-swap as a merge, so the target moving under
    /// it is the same ordinary contention: the SAME row goes back to `queued` and lands on retry.
    #[tokio::test]
    async fn a_revert_whose_target_moved_is_requeued_like_a_merge() {
        let pool = test_pool().await;
        let revert = Op::Revert {
            merge_sha: CommitSha::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            target: Branch::new("master").unwrap(),
        };
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", "C:/repo", "alpha"),
            &revert,
            Origin::Human,
        )
        .await
        .unwrap();

        let moved = FakeVcsExecutor::failing_with(
            "master moved while the merge was being computed, so it was not published; resubmit",
        );
        assert!(drain_once(&pool, "alpha", &moved).await);
        assert_eq!(
            status_of(&pool, id).await,
            "queued",
            "a moved target is retried by the queue itself, for a revert as for a merge"
        );
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(attempts, 1);

        let lands = FakeVcsExecutor::succeeding_with("cafe");
        assert!(drain_once(&pool, "alpha", &lands).await);
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert_eq!(ticket.id, id);
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("cafe"));
    }

    /// The land design's fourth test: the target moving three times in a row is not contention any
    /// more — it is something writing to it directly, outside the queue — and the row says so
    /// rather than retrying forever. `attempts` stops at the ceiling, `status` is terminal, and the
    /// reason names the real cause instead of the generic "resubmit" nobody was going to act on.
    #[tokio::test]
    async fn a_target_that_keeps_moving_gives_up_at_the_ceiling_and_names_the_real_cause() {
        let pool = test_pool().await;
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", "C:/repo", "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        for attempt in 1..=3 {
            let moved = FakeVcsExecutor::failing_with(
                "master moved while the merge was being computed, so it was not published; resubmit",
            );
            assert!(
                drain_once(&pool, "alpha", &moved).await,
                "attempt {attempt}"
            );
        }

        assert_eq!(status_of(&pool, id).await, "failed");
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(attempts, 3, "the ceiling stops counting once it is reached");
        let reason = failure_reason_of(&pool, id).await;
        assert!(
            reason.contains("3 times in a row"),
            "the row has to say WHY it gave up, not just that it did: {reason}"
        );
        assert!(
            !reason.contains("resubmit"),
            "the generic message is what this replaces — nobody was resubmitting it: {reason}"
        );
    }

    /// A branch name that could be read as an option must not exist, let alone reach argv.
    #[test]
    fn a_branch_name_cannot_be_an_option() {
        for bad in ["-f", "--no-verify", "--upload-pack=x", "--", "", "  "] {
            assert!(
                Branch::new(bad).is_err(),
                "{bad:?} was accepted as a branch name"
            );
        }
    }

    /// Whitespace and control characters go the same way; surrounding whitespace is trimmed rather
    /// than rejected, because a model that sends " master" meant `master`.
    ///
    /// **The two loops are separate because one of the two rules had no coverage at all.** `"a b"`,
    /// `"a\tb"` and `"a\nb"` are each whitespace *and* control-or-not, so the whitespace half of the
    /// predicate catches all three — deleting `|| character.is_control()` left the whole suite
    /// green. The second loop is chosen so it cannot: NUL, BEL, ESC, DEL and the file separator are
    /// `is_control()` and are not `is_whitespace()`, which the guard inside the loop asserts rather
    /// than assumes, so a Unicode table that ever disagreed would say so instead of quietly turning
    /// this back into a duplicate of the first loop. `Branch`'s doc comment calls its four rules
    /// "the whole list"; this is the third of them being held.
    #[test]
    fn a_branch_name_is_trimmed_and_then_must_be_one_word() {
        assert_eq!(Branch::new("  feature  ").unwrap().as_str(), "feature");
        for bad in ["a b", "a\tb", "a\nb"] {
            assert!(Branch::new(bad).is_err(), "{bad:?} was accepted");
        }
        for bad in ["a\u{0}b", "a\u{7}b", "a\u{1b}b", "a\u{7f}b", "a\u{1c}b"] {
            assert!(
                !bad.chars().any(char::is_whitespace),
                "{bad:?} must exercise the control-character rule, and this one is whitespace too"
            );
            assert!(Branch::new(bad).is_err(), "{bad:?} was accepted");
        }
    }

    /// The route the flat builder does NOT cover, and the reason validation lives in the type rather
    /// than in a constructor: `http.rs` deserializes an `Op` straight from the request body.
    #[test]
    fn a_dashed_branch_cannot_arrive_as_json_either() {
        let raw = r#"{"op":"merge","source":"--upload-pack=touch x","target":"master"}"#;

        assert!(serde_json::from_str::<Op>(raw).is_err());
    }

    /// A stored row is not trusted either: `from_stored` parses the same JSON.
    #[test]
    fn a_hand_edited_row_with_a_dashed_branch_will_not_parse() {
        assert!(
            Op::from_stored("merge", r#"{"op":"merge","source":"-f","target":"master"}"#).is_err()
        );
    }

    /// The door's whole vocabulary, stated as a table.
    #[test]
    fn the_queue_speaks_merge_and_push_and_says_so_about_everything_else() {
        assert_eq!(
            Op::from_request("merge", Some("feature"), Some("master")).unwrap(),
            Op::Merge {
                source: "feature".into(),
                target: "master".into()
            }
        );
        // `source` is the thing that moves and `target` is where it goes, for BOTH operations — so a
        // push is a branch to a remote, in that order.
        assert_eq!(
            Op::from_request("push", Some("main"), Some("origin")).unwrap(),
            Op::Push {
                remote: "origin".into(),
                branch: "main".into()
            }
        );

        // A tag is the case that shows the `source`/`target` reading is a rule and not a
        // coincidence: what moves is the branch's tip, and where it goes is a new ref.
        assert_eq!(
            Op::from_request("tag", Some("main"), Some("v1.0")).unwrap(),
            Op::Tag {
                name: "v1.0".into(),
                at: "main".into()
            }
        );

        // The two that break the source/target pair rather than following it, each taking the one
        // parameter it has a use for.
        assert_eq!(
            Op::from_request("fetch", None, Some("origin")).unwrap(),
            Op::Fetch {
                remote: "origin".into()
            }
        );
        assert_eq!(
            Op::from_request("branch-delete", Some("feature"), None).unwrap(),
            Op::BranchDelete {
                branch: "feature".into()
            }
        );

        // An operation the spec lists but this queue has decided against must say WHICH thing is
        // wrong — "unknown operation: pull" would send a caller looking for a typo in its own
        // request, when what it needs to know is to ask for two operations instead of one.
        let error = Op::from_request("pull", Some("feature"), Some("origin")).unwrap_err();
        assert!(
            error.contains("will not become one"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("merge") && error.contains("push") && error.contains("rebase"),
            "the error must name what the queue CAN do: {error}"
        );

        assert!(
            Op::from_request("frobnicate", None, None)
                .unwrap_err()
                .contains("frobnicate")
        );
        assert!(
            Op::from_request("merge", Some("feature"), None)
                .unwrap_err()
                .contains("target")
        );
        assert!(
            Op::from_request("merge", None, Some("master"))
                .unwrap_err()
                .contains("source")
        );
        // The push arm's own two, because it reaches `named_branch`/`named_remote` with different
        // arguments and a swapped pair would still compile.
        assert!(
            Op::from_request("push", Some("main"), None)
                .unwrap_err()
                .contains("remote")
        );
        assert!(
            Op::from_request("push", None, Some("origin"))
                .unwrap_err()
                .contains("branch")
        );
        // And the tag arm's own two, since it reaches a third helper with a third message.
        assert!(
            Op::from_request("tag", Some("main"), None)
                .unwrap_err()
                .contains("tag name")
        );
        assert!(
            Op::from_request("tag", None, Some("v1.0"))
                .unwrap_err()
                .contains("branch")
        );
        assert!(Op::from_request(" Merge ", Some("feature"), Some("master")).is_ok());
        assert!(Op::from_request(" PUSH ", Some("main"), Some("origin")).is_ok());
        assert!(Op::from_request(" Tag ", Some("main"), Some("v1.0")).is_ok());
    }

    /// A remote is checked as an argv token, exactly as a branch is, and by its OWN type.
    ///
    /// The second half is the one worth a test: `Op` derives `Deserialize`, so a raw
    /// `POST /vcs/requests` body reaches `Remote` without passing `Op::from_request` at all — the
    /// route `a_dashed_branch_cannot_arrive_as_json_either` exists for, on the field it does not
    /// cover. Giving `Push` a `String` remote would leave every assertion in the flat-builder test
    /// above green.
    #[test]
    fn a_remote_is_an_argv_token_on_every_route_in() {
        assert_eq!(Remote::new("  origin  ").unwrap().as_str(), "origin");
        for bad in ["", "  ", "--upload-pack=x", "-o", "a b", "a\u{1b}b"] {
            assert!(Remote::new(bad).is_err(), "{bad:?} was accepted");
        }

        let raw = r#"{"op":"push","remote":"--receive-pack=touch x","branch":"main"}"#;
        assert!(serde_json::from_str::<Op>(raw).is_err());
        assert!(
            Op::from_stored("push", r#"{"op":"push","remote":"-o","branch":"main"}"#).is_err(),
            "a hand-edited row is not trusted either"
        );
    }

    // ---- settling: an escalated/blocked row that stopped wanting a person ----

    /// A row admitted through the real INSERT and then moved to `status`.
    async fn row_in_status(
        pool: &sqlx::SqlitePool,
        project: &str,
        source: &str,
        status: &str,
    ) -> i64 {
        let op = Op::Merge {
            source: source.into(),
            target: "master".into(),
        };
        let id = submit(pool, &repo_for(project), &op, Origin::Shell)
            .await
            .expect("admit the merge");
        sqlx::query("UPDATE vcs_requests SET status = ? WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(pool)
            .await
            .expect("move it to its status");
        id
    }

    /// `(settled_at, settled_reason)` as stored. Read by SQL because the summary the listings return
    /// deliberately carries neither.
    async fn settlement_of(pool: &sqlx::SqlitePool, id: i64) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT settled_at, settled_reason FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("read the settlement")
    }

    #[tokio::test]
    async fn a_later_identical_success_settles_an_escalation_and_an_earlier_one_does_not() {
        let pool = test_pool().await;
        // A success BEFORE the escalation is a different event: the branches moved on afterwards.
        let earlier = row_in_status(&pool, "alpha", "feat/x", "succeeded").await;
        let escalated = row_in_status(&pool, "alpha", "feat/x", "escalated").await;
        let blocked = row_in_status(&pool, "alpha", "feat/y", "blocked").await;

        settle_by_record(&pool).await.expect("settle by record");
        assert_eq!(
            settlement_of(&pool, escalated).await.0,
            None,
            "an earlier success says nothing about a later escalation"
        );
        assert_eq!(settlement_of(&pool, blocked).await.0, None);

        // The same operation in ANOTHER project is not the same operation.
        row_in_status(&pool, "beta", "feat/x", "succeeded").await;
        settle_by_record(&pool).await.unwrap();
        assert_eq!(settlement_of(&pool, escalated).await.0, None);

        let later = row_in_status(&pool, "alpha", "feat/x", "succeeded").await;
        assert!(later > escalated && earlier < escalated);
        settle_by_record(&pool).await.unwrap();
        let (at, reason) = settlement_of(&pool, escalated).await;
        assert!(at.is_some(), "a later identical success settles it");
        assert_eq!(reason.as_deref(), Some("superseded"));
        assert_eq!(
            settlement_of(&pool, blocked).await.0,
            None,
            "a different operation is untouched"
        );
        assert_eq!(
            settlement_of(&pool, earlier).await,
            (None, None),
            "a succeeded row is never settled"
        );
    }

    #[tokio::test]
    async fn a_row_handed_to_a_resolution_is_settled_as_resolved() {
        let pool = test_pool().await;
        let handed = row_in_status(&pool, "alpha", "feat/x", "escalated").await;
        let landing = row_in_status(&pool, "alpha", "feat/x", "queued").await;
        let open = row_in_status(&pool, "alpha", "feat/y", "escalated").await;
        sqlx::query("UPDATE vcs_requests SET resolved_by = ? WHERE id = ?")
            .bind(landing)
            .bind(handed)
            .execute(&pool)
            .await
            .unwrap();

        settle_by_record(&pool).await.unwrap();
        assert_eq!(
            settlement_of(&pool, handed).await.1.as_deref(),
            Some("resolved")
        );
        assert_eq!(settlement_of(&pool, open).await, (None, None));
        assert_eq!(
            settlement_of(&pool, landing).await,
            (None, None),
            "the landing row itself is not escalated or blocked"
        );
    }

    /// A merge of `source` into `target`, admitted through the real INSERT and moved to `status`.
    async fn merge_in_status(
        pool: &sqlx::SqlitePool,
        source: &str,
        target: &str,
        status: &str,
    ) -> i64 {
        let op = Op::Merge {
            source: source.into(),
            target: target.into(),
        };
        let id = submit(pool, &repo_for("alpha"), &op, Origin::Shell)
            .await
            .expect("admit the merge");
        sqlx::query("UPDATE vcs_requests SET status = ? WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(pool)
            .await
            .expect("move it to its status");
        id
    }

    /// Hands `escalated` to resolution run `run`, whose worktree is on `branch`.
    async fn hand_to_resolution(pool: &sqlx::SqlitePool, escalated: i64, run: i64, branch: &str) {
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = ? WHERE id = ?")
            .bind(run)
            .bind(escalated)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch,
                                    created_at)
             VALUES ('run', ?, 'alpha', '/repo', '/repo/wt', ?, '2026-10-09T00:00:00Z')",
        )
        .bind(run)
        .bind(branch)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Requests 568/573 in production: the resolution was merged in from the shell, a plain merge,
    /// so `resolved_by` was never written and the escalation stayed open after the work was done.
    #[tokio::test]
    async fn a_resolution_merged_in_by_hand_settles_its_escalation() {
        let pool = test_pool().await;
        let escalated = merge_in_status(&pool, "master", "feat/x", "escalated").await;
        hand_to_resolution(&pool, escalated, 900, "alpha/run-900").await;
        // The resolution branch into some OTHER target says nothing about this conflict.
        merge_in_status(&pool, "alpha/run-900", "feat/y", "succeeded").await;
        settle_by_record(&pool).await.unwrap();
        assert_eq!(settlement_of(&pool, escalated).await, (None, None));

        merge_in_status(&pool, "alpha/run-900", "feat/x", "succeeded").await;
        settle_by_record(&pool).await.unwrap();
        assert_eq!(
            settlement_of(&pool, escalated).await.1.as_deref(),
            Some("resolved")
        );
    }

    #[tokio::test]
    async fn a_target_merged_back_into_its_source_settles_the_conflict_as_landed() {
        let pool = test_pool().await;
        // Before the escalation: a different event, the branches moved on afterwards.
        merge_in_status(&pool, "feat/x", "master", "succeeded").await;
        let escalated = merge_in_status(&pool, "master", "feat/x", "escalated").await;
        // Same direction as the escalation is `superseded`'s business, not this rule's.
        let unrelated = merge_in_status(&pool, "master", "feat/y", "escalated").await;
        settle_by_record(&pool).await.unwrap();
        assert_eq!(settlement_of(&pool, escalated).await, (None, None));

        merge_in_status(&pool, "feat/x", "master", "succeeded").await;
        settle_by_record(&pool).await.unwrap();
        assert_eq!(
            settlement_of(&pool, escalated).await.1.as_deref(),
            Some("landed")
        );
        assert_eq!(settlement_of(&pool, unrelated).await, (None, None));
        assert!(
            feed_of(&pool, escalated).await.iter().any(|(_, summary)| {
                summary.ends_with("its target has since been merged into its source")
            }),
            "the feed says why in words"
        );
    }

    /// The Waiting card's grounds for a decision: which branches, git's output, and the resolution.
    #[tokio::test]
    async fn waiting_on_a_person_carries_what_a_decision_needs() {
        let pool = test_pool().await;
        let escalated = merge_in_status(&pool, "master", "feat/x", "escalated").await;
        sqlx::query("UPDATE vcs_requests SET output_tail = ? WHERE id = ?")
            .bind("CONFLICT (content): Merge conflict in a.rs\n")
            .bind(escalated)
            .execute(&pool)
            .await
            .unwrap();
        hand_to_resolution(&pool, escalated, 901, "alpha/run-901").await;

        let waiting = waiting_on_a_person(&pool).await.unwrap();
        let row = waiting.iter().find(|r| r.id == escalated).expect("listed");
        assert_eq!(row.source.as_deref(), Some("master"));
        assert_eq!(row.target.as_deref(), Some("feat/x"));
        assert!(row.output_tail.as_deref().unwrap().contains("a.rs"));
        assert_eq!(row.resolution_run_id, Some(901));
        assert_eq!(
            row.resolution_status, None,
            "no such run row: absent, not an error"
        );
        assert!(!row.from_resolution);

        let history = list(&pool, None).await.unwrap();
        assert!(
            history
                .iter()
                .all(|r| r.source.is_none() && r.output_tail.is_none()),
            "the 200-row history stays light"
        );
    }

    /// `(kind, summary)` of every feed line under one request's subject, oldest first.
    async fn feed_of(pool: &sqlx::SqlitePool, id: i64) -> Vec<(String, String)> {
        sqlx::query_as("SELECT kind, summary FROM feed WHERE subject = ? ORDER BY id")
            .bind(format!("vcs:{id}"))
            .fetch_all(pool)
            .await
            .expect("read the feed")
    }

    #[tokio::test]
    async fn settling_closes_the_requests_feed_sequence_once() {
        let pool = test_pool().await;
        let dismissed = row_in_status(&pool, "alpha", "feat/x", "escalated").await;
        let superseded = row_in_status(&pool, "alpha", "feat/y", "escalated").await;
        row_in_status(&pool, "alpha", "feat/y", "succeeded").await;

        assert!(settle(&pool, dismissed, "dismissed").await.unwrap());
        assert!(!settle(&pool, dismissed, "merged").await.unwrap());
        settle_by_record(&pool).await.unwrap();
        settle_by_record(&pool).await.unwrap();

        let settled = |lines: Vec<(String, String)>| {
            lines
                .into_iter()
                .filter(|(kind, _)| kind == "vcs_request_settled")
                .map(|(_, summary)| summary)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            settled(feed_of(&pool, dismissed).await),
            vec![format!(
                "vcs request {dismissed} no longer needs a person: dismissed"
            )],
            "one line, from the first settlement only"
        );
        assert_eq!(
            settled(feed_of(&pool, superseded).await),
            vec![format!(
                "vcs request {superseded} no longer needs a person: a later request on the same merge succeeded"
            )]
        );
    }

    #[tokio::test]
    async fn a_settled_row_keeps_its_status_and_its_first_reason() {
        let pool = test_pool().await;
        let escalated = row_in_status(&pool, "alpha", "feat/x", "escalated").await;
        let queued = row_in_status(&pool, "alpha", "feat/y", "queued").await;
        let succeeded = row_in_status(&pool, "alpha", "feat/z", "succeeded").await;

        assert!(settle(&pool, escalated, "merged").await.unwrap());
        let (first_at, first_reason) = settlement_of(&pool, escalated).await;
        assert_eq!(first_reason.as_deref(), Some("merged"));

        assert!(
            !settle(&pool, escalated, "dismissed").await.unwrap(),
            "a second settle changes nothing"
        );
        assert_eq!(
            settlement_of(&pool, escalated).await,
            (first_at, first_reason),
            "neither the moment nor the reason is overwritten"
        );
        assert_eq!(
            status_of(&pool, escalated).await,
            "escalated",
            "settling is a note on the row, never a status change"
        );

        for outside in [queued, succeeded] {
            assert!(!settle(&pool, outside, "dismissed").await.unwrap());
            assert_eq!(settlement_of(&pool, outside).await, (None, None));
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 3, "history is never deleted");
    }

    #[tokio::test]
    async fn waiting_on_a_person_is_uncapped_and_skips_settled_rows() {
        let pool = test_pool().await;
        // More rows than `list`'s 200-row window: the badge used to be capped by accident.
        for n in 0..205 {
            row_in_status(&pool, "alpha", &format!("feat/{n}"), "escalated").await;
        }
        let blocked = row_in_status(&pool, "alpha", "feat/blocked", "blocked").await;
        let settled = row_in_status(&pool, "alpha", "feat/settled", "escalated").await;
        row_in_status(&pool, "alpha", "feat/done", "succeeded").await;
        row_in_status(&pool, "alpha", "feat/failed", "failed").await;
        assert!(settle(&pool, settled, "merged").await.unwrap());

        let waiting = waiting_on_a_person(&pool).await.expect("list the waiting");
        assert_eq!(waiting.len(), 206, "205 escalated + 1 blocked, uncapped");
        assert!(waiting.iter().all(|r| r.id != settled));
        assert!(waiting.iter().any(|r| r.id == blocked));
        assert!(
            waiting
                .iter()
                .all(|r| r.status == "escalated" || r.status == "blocked")
        );
        assert!(
            waiting.windows(2).all(|w| w[0].id > w[1].id),
            "newest first, like the history listing"
        );
    }

    async fn map_blobs_of(pool: &sqlx::SqlitePool, id: i64) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT pending_map_blob, approved_map_blob FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn finished_at_of(pool: &sqlx::SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT finished_at FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn paused_row(pool: &sqlx::SqlitePool, source: &str, blob: &str) -> i64 {
        let id = row_in_status(pool, "alpha", source, "awaiting_owner").await;
        sqlx::query("UPDATE vcs_requests SET pending_map_blob = ? WHERE id = ?")
            .bind(blob)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn the_table_admits_a_request_waiting_for_its_owner() {
        let pool = test_pool().await;
        let id = paused_row(&pool, "feat/x", "abc").await;
        assert_eq!(status_of(&pool, id).await, "awaiting_owner");
        assert_eq!(
            map_blobs_of(&pool, id).await,
            (Some("abc".to_owned()), None)
        );
    }

    #[tokio::test]
    async fn approving_a_paused_request_queues_it_with_the_blob_it_saw() {
        let pool = test_pool().await;
        let id = paused_row(&pool, "feat/x", "abc").await;
        approve_for_owner(&pool, id).await.expect("approve it");
        assert_eq!(status_of(&pool, id).await, "queued");
        assert_eq!(
            map_blobs_of(&pool, id).await.1,
            Some("abc".to_owned()),
            "the approved blob is the one the row paused on"
        );
        assert!(matches!(
            approve_for_owner(&pool, id).await,
            Err(DismissError::NotOpen)
        ));
    }

    #[tokio::test]
    async fn refusing_a_paused_request_ends_it_as_rejected() {
        let pool = test_pool().await;
        let id = paused_row(&pool, "feat/x", "abc").await;
        refuse_for_owner(&pool, id).await.expect("refuse it");
        assert_eq!(status_of(&pool, id).await, "rejected");
        assert!(finished_at_of(&pool, id).await.is_some());
        assert!(failure_reason_of(&pool, id).await.contains("refused"));
        assert!(matches!(
            refuse_for_owner(&pool, id).await,
            Err(DismissError::NotOpen)
        ));
    }

    #[tokio::test]
    async fn owner_decisions_answer_only_a_paused_request() {
        let pool = test_pool().await;
        assert!(matches!(
            approve_for_owner(&pool, 999_999).await,
            Err(DismissError::NotFound)
        ));
        assert!(matches!(
            refuse_for_owner(&pool, 999_999).await,
            Err(DismissError::NotFound)
        ));
        let queued = row_in_status(&pool, "alpha", "feat/q", "queued").await;
        let escalated = row_in_status(&pool, "alpha", "feat/e", "escalated").await;
        for id in [queued, escalated] {
            assert!(matches!(
                approve_for_owner(&pool, id).await,
                Err(DismissError::NotOpen)
            ));
            assert!(matches!(
                refuse_for_owner(&pool, id).await,
                Err(DismissError::NotOpen)
            ));
        }
    }

    #[tokio::test]
    async fn a_request_waiting_for_its_owner_is_waiting_on_a_person() {
        let pool = test_pool().await;
        let paused = paused_row(&pool, "feat/p", "abc").await;
        let escalated = row_in_status(&pool, "alpha", "feat/e", "escalated").await;
        let queued = row_in_status(&pool, "alpha", "feat/q", "queued").await;
        let ids: std::collections::BTreeSet<i64> = waiting_on_a_person(&pool)
            .await
            .expect("list the waiting")
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, std::collections::BTreeSet::from([paused, escalated]));
        assert!(!ids.contains(&queued));
    }

    #[tokio::test]
    async fn the_waiting_count_matches_the_waiting_list() {
        let pool = test_pool().await;
        paused_row(&pool, "feat/p", "abc").await;
        row_in_status(&pool, "alpha", "feat/e", "escalated").await;
        row_in_status(&pool, "alpha", "feat/b", "blocked").await;
        let settled = row_in_status(&pool, "alpha", "feat/s", "escalated").await;
        row_in_status(&pool, "alpha", "feat/q", "queued").await;
        row_in_status(&pool, "alpha", "feat/ok", "succeeded").await;
        assert!(settle(&pool, settled, "merged").await.unwrap());
        let listed = waiting_on_a_person(&pool).await.unwrap().len();
        assert_eq!(listed, 3);
        assert_eq!(count_waiting_on_a_person(&pool).await.unwrap(), listed);
    }

    #[test]
    fn a_pause_for_the_owner_is_its_own_status() {
        let outcome = Outcome::AwaitingOwner {
            map_blob: "abc".to_owned(),
            reason: "r".to_owned(),
        };
        assert_eq!(outcome.status(), "awaiting_owner");
        assert!(!TERMINAL_STATUSES.contains(&"awaiting_owner"));
    }

    #[tokio::test]
    async fn finishing_with_a_pause_records_the_blob_and_no_end() {
        let pool = test_pool().await;
        let id = row_in_status(&pool, "alpha", "feat/x", "running").await;
        finish(
            &pool,
            id,
            Outcome::AwaitingOwner {
                map_blob: "abc".into(),
                reason: "r".into(),
            },
        )
        .await
        .expect("finish with a pause");
        assert_eq!(status_of(&pool, id).await, "awaiting_owner");
        assert_eq!(map_blobs_of(&pool, id).await.0, Some("abc".to_owned()));
        assert_eq!(finished_at_of(&pool, id).await, None);
        assert_eq!(failure_reason_of(&pool, id).await, "r");
    }
}
