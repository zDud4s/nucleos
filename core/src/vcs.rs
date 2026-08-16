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
#[cfg(test)]
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
#[cfg(test)]
impl From<&str> for TagName {
    fn from(value: &str) -> Self {
        TagName::new(value).expect("a test used an invalid tag name literal")
    }
}

impl<'de> Deserialize<'de> for Branch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Branch::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Tests build branches from literals everywhere. Panicking is right for a literal a developer
/// wrote; production has only the fallible path, and this impl does not exist there.
#[cfg(test)]
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

/// Who is asking, which decides whether the request needs a human's sign-off before it may queue.
///
/// A human's order in an interactive session already is the approval — asking again two seconds
/// later is friction with no safety gain. An autonomous run or job's request is not: nothing else
/// in the system has consented to it yet, so it waits. The queue itself never decides consent, only
/// ordering and mutual exclusion — this is where consent, already decided elsewhere, is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Human,
    // **A person's own editor session, asking through its safety hook** — `hooks::session_git_decision`.
    //
    // This said "nothing maps to this, and that is a decision rather than an omission", and it
    // survived on the argument that the schema outranks the mapping. The caller it was waiting for
    // is the one it described almost exactly: *"a credential belonging to an external session in its
    // own right, distinct from both the desktop app's control token and a run's"*. What arrived is
    // one step off that description and the difference is worth keeping straight — the hook presents
    // the control token, so the credential is not a session's own; what is a session's own is the
    // ASKING. `Human` would have been a lie of a readable kind: nobody clicked anything.
    //
    // It stays distinct from `Human` for the reason the audit exists. A `human` row means a person
    // acted in the app; a `shell` row means a person's agent tried to act and was redirected here
    // instead. Those are different events and the queue is the only place that records the second.
    //
    // The Tauri app is NOT this, despite owning the `shell/` directory: it holds the control token
    // and arrives through `vcs_origin` as `Human`, which is why the name was free.
    Shell,
    Run(i64),
    // Constructed by Chunk 4, when jobs submit requests of their own.
    #[allow(dead_code)]
    Job(i64),
}

impl Origin {
    /// The exact spelling the `origin` column's CHECK constraint accepts — do not invent others.
    fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Shell => "shell",
            Origin::Run(_) => "run",
            Origin::Job(_) => "job",
        }
    }

    /// Human and shell requests carry their own approval; run and job requests are autonomous and
    /// have not been approved by anything yet.
    fn needs_approval(self) -> bool {
        matches!(self, Origin::Run(_) | Origin::Job(_))
    }

    /// `run_id` is populated only for `Origin::Run`. A job id written into a column named
    /// `run_id` would silently mislabel it as a run — job ids and run ids come from different
    /// sequences and would collide (see `worktree::Owner::feed_run_id`'s doc comment for the same
    /// mistake made once already). A `job_id` column arrives once jobs actually submit requests,
    /// which is not this chunk.
    fn run_id(self) -> Option<i64> {
        match self {
            Origin::Run(id) => Some(id),
            _ => None,
        }
    }
}

/// A project resolved to the repository it names, with the key the queue locks on.
///
/// Private fields with one production constructor, because the defect this type exists to kill was
/// two fields allowed to disagree: a caller that could set `key` and `root` independently could take
/// the lock on one repository and run git in another, and the row would look entirely ordinary.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    project_id: String,
    root: String,
    key: String,
}

impl ResolvedRepo {
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Tests build repositories that do not exist on disk: what most of them exercise is the SQL,
    /// and making each one create a real git repository would test git twice and slow the suite.
    /// `resolve_repo` is the only constructor compiled into the daemon.
    #[cfg(test)]
    pub fn synthetic(project_id: &str, root: &str, key: &str) -> Self {
        Self {
            project_id: project_id.to_owned(),
            root: root.to_owned(),
            key: key.to_owned(),
        }
    }
}

/// Why a project could not be resolved to a repository.
///
/// Two failure arms rather than one string because the HTTP layer answers them differently and a
/// caller deserves to know which happened: an unknown project is the caller naming something that is
/// not there, and a bad root is the daemon's own recorded state being wrong.
#[derive(Debug)]
pub enum ResolveError {
    UnknownProject,
    NotARepository(String),
    Database(sqlx::Error),
}

// No `Display`: the one consumer, `http.rs`, destructures every arm and formats the inner value
// itself, so a `Display` here would be dead code that clippy cannot see — trait impls are exempt
// from dead-code analysis. Whoever gains a caller that wants to print one whole writes it then.

/// The project a directory belongs to, for a caller that knows only where it is standing.
///
/// `resolve_repo` goes the other way, from a name the caller already had. This is for the caller that
/// has no name at all: an interactive session's safety hook, which knows its `cwd` and nothing else.
/// Both ends meet at the same `ResolvedRepo`, because this returns a project id and hands it straight
/// back to `resolve_repo` rather than building one — the type has one production constructor for a
/// reason, and a second would be free to let `root` and `key` disagree.
///
/// **The match is on the repository, not on the path**, and that is the whole reason a session in a
/// linked worktree resolves at all. `C:\Projects\nucleos-assuntos` is not under `C:\Projects\nucleos`
/// and no prefix test would ever relate them; what relates them is `--git-common-dir`, which both
/// answer identically. Measured on this repo: thirteen worktrees, thirteen different top levels, one
/// common dir. That is also precisely why the queue serialises across them for free — the key it
/// locks on IS that common dir, so `one_running_vcs_request_per_repo` was already counting every
/// session in every worktree before any of them could reach it.
///
/// The parent of the common dir is the main working tree's root. That holds for every repository this
/// queue can serve and fails only for a bare one, which `repo_key` has already refused by here.
pub async fn project_for_worktree(
    pool: &sqlx::SqlitePool,
    worktree_root: &std::path::Path,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let key = crate::git_exec::repo_key(worktree_root, deadline).await?;
    let main_root = std::path::Path::new(&key)
        .parent()
        .ok_or_else(|| format!("{key} has no parent, so it names no working tree"))?
        .to_owned();

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL
         ORDER BY project_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read the project roster: {error}"))?;

    for (project_id, root) in &rows {
        // Canonicalised on both sides rather than compared as written. `canonical` returns a Windows
        // verbatim path and a recorded root is whatever a person typed, so the two spellings reach
        // one directory and compare unequal — the failure `canonical`'s own doc comment warns about.
        // A root that has since been deleted canonicalises to an error and is skipped, not fatal:
        // one stale roster row must not stop the rest of the roster from answering.
        if let Ok(canonical_root) = crate::git_exec::canonical(std::path::Path::new(root)).await
            && std::path::Path::new(&canonical_root) == main_root
        {
            return Ok(project_id.clone());
        }
    }

    Err(format!(
        "no project on the roster is rooted at {} — the queue takes work for projects it knows, and \
         a project in `off` mode has its root cleared, so that is the first thing to check",
        main_root.display()
    ))
}

/// The single production path from a project id to a repository the queue may lock.
///
/// Both halves are needed: `autopilot_state` is the only place a root is recorded, and git is what
/// makes two projects sharing a repository share a lock. It runs a subprocess, so it is neither free
/// nor infallible — that is the trade against keying on a label, which is what it replaces.
pub async fn resolve_repo(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<ResolvedRepo, ResolveError> {
    let root = crate::inspect::project_root(pool, project_id)
        .await
        .map_err(ResolveError::Database)?
        .ok_or(ResolveError::UnknownProject)?;
    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    let key = crate::git_exec::repo_key(std::path::Path::new(&root), deadline)
        .await
        .map_err(ResolveError::NotARepository)?;

    Ok(ResolvedRepo {
        project_id: project_id.to_owned(),
        root,
        key,
    })
}

/// PURE: the merge a shell command asks for, in the queue's own terms, or `None`.
///
/// `git merge X`, in a worktree whose HEAD is on `B`, means "bring X into B" — which is
/// `Merge { source: X, target: B }`, the shape the queue already executes. The queue performs it in
/// the integration worktree and moves the holder's worktree afterwards when the holder is the one
/// standing on `B`, which is the case this whole pillar was written around.
///
/// **The strictest reading that still admits what the queue performs: `git merge <ref>`, with an
/// optional `--no-ff` on either side of the ref, and nothing else.** Not because more could not be
/// parsed, but because everything else is a DIFFERENT operation: `--squash` does not merge at all;
/// `--abort` unwinds one; a second ref is an octopus merge; `--ff` and `--ff-only` both ask for a
/// fast-forward where this queue always writes a merge commit. A caller whose spelling is not one of
/// these keeps exactly the behaviour it has always had, rather than having the queue perform
/// something adjacent to what it wrote.
///
/// **`--no-ff` was in that rejected list, and it was there on a false premise** — worth recording,
/// because the sentence read true and the mistake cost the pillar its whole point for that spelling.
/// It said `--no-ff` wants a merge commit while `publish` is `--ff-only`, which conflates two
/// different things: `compute_merge` runs `git merge --no-ff` unconditionally, so the commit this
/// queue publishes is ALWAYS a merge commit, and `publish`'s `--ff-only` fast-forwards the holder's
/// checkout ONTO that already-computed commit. `--no-ff` is therefore not adjacent to what the queue
/// does — it is a literal spelling of it. Refusing it sent the merge back to be performed by the
/// agent's own hand, which is the one outcome this pillar exists to abolish.
///
/// **This is not the shell parsing `classifier.rs` exists to keep closed, and the difference is
/// where the output goes.** Nothing here reaches an argv: both names pass through `Branch` — the
/// argv guard — and `git_exec` builds its own command line from the typed value. The worst a
/// misreading can do is refuse, or name a branch git will not resolve; it cannot inject. That is
/// also why the verb is folded for comparison and the REF is not: git is case-sensitive about
/// branch names and this must not quietly rename one.
///
/// A `target` of `HEAD` is refused rather than passed on. It is what `rev-parse --abbrev-ref` says
/// for a detached HEAD, and it is meaningless as a merge target besides — the integration worktree's
/// own HEAD is always detached, so publishing "into HEAD" names nothing.
pub fn merge_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    // Matched as whole shapes rather than by filtering the flag out of the token list, and the
    // difference is what a caller can smuggle: a filter would turn `git --no-ff merge feature` —
    // which git itself refuses — into a queued merge, performing something nobody could have run.
    // Here the flag is only ever recognised in argument position, where it is the only thing it can
    // be. A `source` left holding `--no-ff` (`git merge --no-ff`) falls through to `Branch`, which
    // refuses a leading dash; that guard is load-bearing here and not merely nearby.
    let [program, subcommand, source] = match tokens.as_slice() {
        [program, subcommand, source]
        | [program, subcommand, "--no-ff", source]
        | [program, subcommand, source, "--no-ff"] => [program, subcommand, source],
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("merge") {
        return None;
    }
    if current_branch.trim() == "HEAD" {
        return None;
    }
    Some(Op::Merge {
        source: Branch::new(source).ok()?,
        target: Branch::new(current_branch).ok()?,
    })
}

/// PURE: the push a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git push <remote>` and `git push <remote> <branch>`, and nothing else** — the same strictness
/// posture `merge_from_command` argues at length, applied to an operation where being wrong is worse
/// because it is public. What that list leaves out is the interesting part, and each exclusion is a
/// different KIND of thing rather than a longer list of the same one:
///
/// - `git push` alone is refused, and it is the one that looks safest. It has no argv of its own —
///   what it does is read out of `push.default`, `branch.<name>.remote` and the upstream, in a
///   repository the daemon does not control. The queue would have to guess a destination, and the
///   spelling that means "the usual place" to the person who typed it means whatever their config
///   says to us.
/// - `-u` / `--set-upstream` is refused because the queue's argv would not do it. The push would
///   succeed, the upstream would not be set, and the row would say `succeeded` — a silent partial
///   execution, which is worse than a refusal that hands the command back.
/// - `--force`, `--force-with-lease`, `--delete`, `--tags`, `--all`, `--mirror` are refused because
///   each is a different operation, in the sense `merge_from_command` uses the word: they destroy or
///   move things this one only adds to. There is no `--no-ff`-shaped case here — no flag that is
///   merely a literal spelling of what the executor already does — so nothing is admitted beside the
///   bare shapes.
///
/// The branch is the command's own second word when it has one, and otherwise the branch the
/// worktree is standing on. `git push origin feature` from a worktree on `master` is a perfectly
/// ordinary thing to write and does not touch any worktree, so there is no reason to require the two
/// to agree — but `HEAD` is refused in both routes, whether it arrived from a detached worktree or
/// was typed, because a queued row naming `HEAD` names nothing by the time it runs.
pub fn push_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let (program, subcommand, remote, branch) = match tokens.as_slice() {
        [program, subcommand, remote] => (program, subcommand, remote, current_branch.trim()),
        [program, subcommand, remote, branch] => (program, subcommand, remote, *branch),
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("push") {
        return None;
    }
    // Both routes, deliberately: `rev-parse --abbrev-ref` says `HEAD` for a detached worktree, and a
    // caller may equally have typed it. Neither is a branch this queue can push a week later.
    if branch == "HEAD" {
        return None;
    }
    Some(Op::Push {
        remote: Remote::new(remote).ok()?,
        branch: Branch::new(branch).ok()?,
    })
}

/// PURE: the tag a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git tag <name>` and `git tag <name> <branch>`, and nothing else** — the third application of
/// the posture `merge_from_command` argues. The exclusions are worth naming individually because
/// they are not one kind of thing:
///
/// - **`git tag` alone is a READ.** It lists the repository's tags, and refusing it is not a
///   restriction: the fallback hands the run a grant and it lists them itself, which is the right
///   outcome for a command that changes nothing. This is the only one of the three parsers where the
///   bare two-token form means something entirely different from the operation, rather than meaning
///   it with the arguments left to config.
/// - **`-a`, `-s`, `-m` are refused because the queue would have to parse a quoted message**, and
///   not parsing quoted shell is the whole of `classifier.rs`'s reason to exist. A lightweight tag is
///   what this executes and an annotated one is a different object, not a decoration on the same one.
/// - **`-d`, `-f` are the destructive spellings.** One removes a tag and one moves an existing tag
///   to a new commit, which is the tag equivalent of a force push: it invalidates what anybody who
///   already fetched believes. The queue creates; it does not overwrite.
/// - **`-l`, `--list`, `--contains`, `-n` are reads wearing the write's name**, and they are stopped
///   by `TagName` rather than by this list — a leading dash cannot be a tag name. Named here anyway
///   because a reader checking whether they are handled should not have to derive it.
///
/// The branch is the command's own second word when it has one, and otherwise the branch the
/// worktree stands on — the same rule `push_from_command` uses, and `HEAD` is refused on both routes
/// for the same reason: it names nothing by the time a queued row runs.
pub fn tag_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let (program, subcommand, name, at) = match tokens.as_slice() {
        [program, subcommand, name] => (program, subcommand, name, current_branch.trim()),
        [program, subcommand, name, at] => (program, subcommand, name, *at),
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("tag") {
        return None;
    }
    if at == "HEAD" {
        return None;
    }
    Some(Op::Tag {
        name: TagName::new(name).ok()?,
        at: Branch::new(at).ok()?,
    })
}

/// PURE: the rebase a shell command asks for, in the queue's own terms, or `None`.
///
/// `git rebase X`, in a worktree whose HEAD is on `B`, means "replay B onto X" — which is
/// `Rebase { branch: B, onto: X }`, the mirror of how `merge_from_command` reads its own two halves.
///
/// **Everything with a flag is refused, and unlike the other parsers there is no accepted one.**
/// `-i` opens an editor, which is a human sitting at a terminal that does not exist here.
/// `--continue`, `--abort` and `--skip` operate on a rebase already in progress — a state this queue
/// never leaves behind, since a conflicted compute aborts before the row is written. `--onto` takes a
/// third ref and means something the two-field shape cannot hold. `--exec` runs an arbitrary command
/// per commit, which is a shell by another name.
///
/// Bare `git rebase` is refused for `git push`'s reason: it replays onto the configured upstream, in
/// a repository the daemon does not control.
///
/// **Every rebase that arrives through this parser blocks, and that is structural rather than
/// incidental.** The branch is the worktree's own — this function has no other one to name — and a
/// run's worktree is precisely what holds it, so `GitExecutor`'s holder check finds the caller
/// itself and refuses. Measured end to end: an approved `git rebase master` in a run's worktree
/// wrote `blocked`, naming that worktree, and the takeover grant left the run unable to perform it
/// by hand either. That is the correct answer and not a gap — the branch the queue would rewrite is
/// the one the paused run resumes onto, and rewriting it underneath is what would corrupt the run.
/// The publishing half is reached from `POST /vcs/requests`, where a caller names a branch nobody
/// has open; that half is proven too, and moves the ref through `publish_by_update_ref`'s
/// compare-and-swap rather than through the rebase.
pub fn rebase_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, onto] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("rebase") {
        return None;
    }
    if current_branch.trim() == "HEAD" {
        return None;
    }
    Some(Op::Rebase {
        branch: Branch::new(current_branch).ok()?,
        onto: Branch::new(onto).ok()?,
    })
}

/// PURE: the fetch a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git fetch <remote>` exactly.** Bare `git fetch` is refused for `git push`'s reason and not for
/// `git tag`'s: it is not a read wearing the write's name, it is the operation with its destination
/// left to `branch.<name>.remote` and `remote.pushDefault` in a repository the daemon does not
/// control. `--all` fetches from remotes nobody named, `--prune` deletes tracking refs, and `--tags`
/// pulls in a namespace the refspec deliberately leaves out — three different operations, and the
/// argv guard stops none of them, so the shape has to.
pub fn fetch_from_command(command: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, remote] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("fetch") {
        return None;
    }
    Some(Op::Fetch {
        remote: Remote::new(remote).ok()?,
    })
}

/// PURE: the branch deletion a shell command asks for, in the queue's own terms, or `None`.
///
/// **`-d` and `--delete`, never `-D` and never `--delete --force`**, and this is the one parser
/// where the accepted flag is mandatory rather than optional. `git branch <name>` CREATES a branch —
/// `classifier.rs` pins that exact ambiguity as the reason `git branch` is on its
/// `SAFE_EXACT_COMMANDS` list in its listing spellings only — so a shape that read the flag as
/// optional would queue a deletion for a command that asked for a creation.
///
/// The distinction between the two spellings is not stylistic: `-d` refuses when the branch holds
/// commits no other ref reaches, and `-D` deletes anyway. That refusal is git's, computed from the
/// commit graph, and it is the entire reason this operation can be offered at all — the queue never
/// has to decide what is safe to lose. `-D` asks git to stop answering that question, so it falls
/// back to the grant like every other unqueueable spelling.
pub fn branch_delete_from_command(command: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, flag, branch] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("branch") {
        return None;
    }
    // Case-sensitive on purpose, and it is the only place in these parsers where that is load-
    // bearing rather than incidental: `-d` and `-D` differ by case alone and mean the safe and the
    // unsafe thing. Folding here would turn every `-D` into the spelling this queue accepts.
    if *flag != "-d" && !flag.eq_ignore_ascii_case("--delete") {
        return None;
    }
    Some(Op::BranchDelete {
        branch: Branch::new(branch).ok()?,
    })
}

/// Admits a request into the queue and returns its row id. Provenance alone decides the initial
/// status: `Human`/`Shell` already carry their approval and start `queued`; `Run`/`Job` are
/// autonomous and start `awaiting_approval`.
///
/// **Nothing writes the transition out of `awaiting_approval`, and that is settled rather than
/// pending.** This said it belonged to Chunk 4 "alongside the `proposals.rs` wiring that grants it".
/// Chunk 4 landed and took the other road, the one `auth.rs` had already argued for: a run may not
/// queue on its own behalf at all (*"Queueing is Admin's"*), so `resume_approved_run` admits an
/// approved action directly as `Origin::Human` and no row ever starts at `awaiting_approval` in
/// production. `drain_once` carries the full account. This function still only writes the initial
/// state, and the `Run`/`Job` arm is the shape the day a scope exists that may ask for itself.
///
/// The repository arrives resolved rather than as fields to be trusted — see `ResolvedRepo`.
pub async fn submit(
    pool: &sqlx::SqlitePool,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64> {
    submit_on(pool, repo, op, origin).await
}

/// `submit`, against a caller's own executor, so an admission can be part of a larger transaction.
///
/// It exists for one caller: approving a paused run's merge (`runs::resume_approved_run`) has to
/// admit the request in the SAME transaction that approves the proposal and resumes the run.
/// Neither order works outside one: admit-then-commit can queue a merge whose approval then rolls
/// back — an irreversible publication nobody authorised, and a proposal still pending so a human
/// can authorise it a second time — and commit-then-admit can resume a run told its merge is
/// queued when it is not.
///
/// Generic over the executor rather than taking a `&mut Transaction`, because `&SqlitePool` is one
/// too: `submit` is this function, and there is no second copy of the INSERT to drift from it.
pub async fn submit_on<'e, E>(
    executor: E,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let status = if origin.needs_approval() {
        "awaiting_approval"
    } else {
        "queued"
    };
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, run_id, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(op.kind())
    .bind(op.to_args())
    .bind(repo.project_id())
    .bind(repo.root())
    .bind(repo.key())
    .bind(origin.as_str())
    .bind(origin.run_id())
    .bind(status)
    .bind(created_at)
    .execute(executor)
    .await?;
    Ok(result.last_insert_rowid())
}

/// A request the caller now holds: its row is already `running`, so nothing else for the same
/// repository can be claimed until `finish` writes a terminal status.
///
/// It carries everything an execution needs — the operation and where to perform it — because the
/// claim already read that row, and a worker that went back for `project_root` would be reading it
/// at a moment when the row it holds could no longer be trusted to be the same.
#[derive(Debug, Clone)]
pub struct ClaimedRequest {
    pub id: i64,
    pub op: Op,
    pub project_id: String,
    pub project_root: String,
}

/// How a claimed request ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Ran, and did what was asked.
    ///
    /// **`sha` is optional because not every operation produces one**, and that is a fact about the
    /// vocabulary rather than a hedge. A merge, a push, a tag and a branch delete each name exactly
    /// one object id worth recording — what was published, what was sent, what was tagged, what was
    /// removed and could therefore be restored. A `fetch` names none: it moves however many
    /// remote-tracking refs the remote had news about, and picking one of them to put in the column
    /// would be inventing a headline.
    ///
    /// `result_sha` has been nullable since `0048_vcs_requests.sql` and `finish` already writes NULL
    /// for every non-`Succeeded` outcome, so this makes the type agree with the column rather than
    /// changing what the column can hold. An empty string would have been the alternative, and it is
    /// the worse one: a reader cannot tell it from a sha the executor failed to capture.
    Succeeded {
        sha: Option<String>,
        output_tail: String,
    },
    /// Ran and produced its result, which could not be published because the target worktree's
    /// uncommitted files are in the way. Terminal and never retried in a loop (spec §7): a working
    /// copy left dirty over an afternoon would otherwise hold the whole repository's queue.
    ///
    /// **The computed merge is not kept, and nothing here should be read as though it were.** A
    /// blocked row records no `result_sha` — `finish` writes `None` — and nothing ever looks one up,
    /// so a resubmission goes through `compute_merge` again and lands its own commit rather than
    /// publishing that one: a merge commit embeds its committer timestamp, and the clock has moved
    /// (the human had to commit or stash first), so it is not even the same sha. The first is left
    /// unreferenced and is `gc` fodder.
    ///
    /// That is the intended shape rather than a leak, and it is what makes the row terminal
    /// affordable: every object the recompute needs is already in this repository, so redoing it
    /// costs one merge in a worktree nobody is standing in — while the alternative, a terminal row
    /// holding a sha that is on no branch, is something somebody would eventually try to publish.
    Blocked { reason: String, output_tail: String },
    /// Ran and failed. A conflicted merge is this, and so is a raced publish.
    Failed {
        reason: String,
        exit_code: Option<i32>,
        output_tail: String,
    },
    /// Never reached an argv — the row itself was unexecutable, which is a defect in the row and not
    /// a result of the operation.
    ///
    /// Recorded as `failed`, because there is no other honest status for it and adding one would
    /// mean a migration for a case that only a corrupt row can produce. It is told apart in the row
    /// **structurally**, not by reading the prose: every other variant writes an `output_tail`
    /// (possibly empty), and this one writes NULL. `status = 'failed' AND output_tail IS NULL` is
    /// therefore exactly "this row could not be executed", and it is queryable. The status half is
    /// not decoration — `reconcile_interrupted` writes a terminal status without touching this
    /// column, so it leaves NULL on rows where git may well have run, and so does every row still
    /// queued, running or awaiting approval.
    ///
    /// It does *not* mean "no subprocess ran". An operation can fail before reaching one — the
    /// integration worktree turning out not to be a worktree — and that writes an empty tail rather
    /// than NULL, because the row was executable and the environment was not. The two are different
    /// defects and belong to different people.
    Unexecutable { reason: String },
}

impl Outcome {
    /// The `status` column this outcome writes. `Unexecutable` shares `failed` with `Failed`; what
    /// tells them apart in the row is `output_tail IS NULL`, not this.
    ///
    /// One function rather than a string in each of `finish`'s match arms, because `drain_once` now
    /// needs the same answer for the feed: two places deciding what an outcome is called would
    /// drift, and the row and the feed disagreeing is exactly the contradiction the feed exists to
    /// avoid.
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Succeeded { .. } => "succeeded",
            Outcome::Blocked { .. } => "blocked",
            Outcome::Failed { .. } | Outcome::Unexecutable { .. } => "failed",
        }
    }
}

/// Takes the oldest claimable request for one repository and marks it `running`, or returns `None`.
///
/// `None` covers all three ordinary reasons there is nothing to do: nothing is queued, something is
/// already running for this repository, or the only rows are still `awaiting_approval` — a caller
/// waits the same way in each case, so they are not worth distinguishing.
///
/// One conditional `UPDATE`, never a `SELECT` then an `UPDATE`. The gap between those two
/// statements is exactly the race this module exists to remove: both callers would read the same
/// queued head and both would believe they own the repository. Here the winner is decided inside a
/// single statement — `NOT EXISTS` is the arbiter, so a losing caller updates zero rows and simply
/// waits rather than erroring on the unique index. That index is the backstop that makes a bug in
/// this guard impossible to ship silently, not the everyday mechanism.
///
/// `?2` appears twice but is bound once: SQLite numbers placeholder slots by their highest index,
/// not by how often each occurs, so this statement has two parameters and takes exactly two binds.
///
/// The claim, the parse and the release-on-failure are one transaction because they have to be
/// uncancellable together (`core/AGENTS.md` § "Cancellation safety", rule 3). A dropped future
/// stops at its last `.await` and never runs another line, and there are two suspension points
/// between marking a row `running` and deciding it is unexecutable — so a compensating write
/// written as a statement after those awaits is not cleanup, it is happy-path-only code. Inside a
/// transaction the question does not arise: `sqlx`'s `Transaction` rolls back when dropped, so a
/// claim abandoned at *any* await leaves the row exactly `queued`, untouched and claimable on the
/// next poll. It also means a row released this way goes `queued` → `failed` without ever being
/// observably `running`, so no queue view can show a phantom.
///
/// Wrapping the statement does not weaken the `NOT EXISTS` guard: SQLite admits one writer at a
/// time, so a second claimer's UPDATE evaluates the guard against the winner's committed row.
///
/// That holds because the claim is this transaction's **first** statement. `begin()` is deferred, so
/// no lock is taken until the UPDATE takes the write lock outright — there is no read-then-upgrade,
/// and so no `SQLITE_BUSY_SNAPSHOT`. Put a `SELECT` ahead of the claim in here and the argument
/// stops holding: the transaction becomes a reader that must upgrade, and an upgrade can fail
/// outright rather than losing cleanly. A loser that instead exhausts `busy_timeout`
/// (`storage.rs:69`) returns `Err(SQLITE_BUSY)` rather than `Ok(None)` — safe, since it claims
/// nothing, but it is a return shape the pre-transaction code could not produce, and the critical
/// section it waits on is one `serde_json::from_str` plus a commit.
pub async fn claim_next(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
) -> sqlx::Result<Option<ClaimedRequest>> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let claimed: Option<(i64, String, String, String, String)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'running', started_at = ?1
          WHERE id = (
              SELECT id FROM vcs_requests
               WHERE repo_key = ?2 AND status = 'queued'
               ORDER BY id LIMIT 1
          )
            AND NOT EXISTS (
              SELECT 1 FROM vcs_requests WHERE repo_key = ?2 AND status = 'running'
            )
         RETURNING id, op, args, project_id, project_root",
    )
    .bind(started_at)
    .bind(repo_key)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some((id, op, args, project_id, project_root)) = claimed else {
        // Nothing was changed, so the rollback this drop performs is the same as a commit.
        return Ok(None);
    };
    // A row whose stored operation will not parse is an error, never `Ok(None)`: `None` means "come
    // back later", and no amount of waiting makes an unexecutable row executable. Left claimed, it
    // would hold this repository's only slot until the next daemon restart. That is the strong
    // form of the jam `core/AGENTS.md` § "Cancellation safety" describes — and this queue still HAS
    // the strong form, where runs and jobs traded it for numbered slots in migration 0053: a
    // repository has one slot and no ceiling anybody may raise. A queue that can trap the repository
    // it exists to protect is not doing its job.
    match Op::from_stored(&op, &args) {
        Ok(op) => {
            transaction.commit().await?;
            Ok(Some(ClaimedRequest {
                id,
                op,
                project_id,
                project_root,
            }))
        }
        Err(error) => {
            let reason =
                format!("stored operation for vcs request {id} could not be parsed: {error}");
            // Terminal rather than back to `queued`: re-queueing would hand the same unparseable
            // row out again on the next poll, forever. `Unexecutable` rather than `Failed` because
            // this row never reached an argv, and that is what leaves `output_tail` NULL — the
            // structural discriminator `Outcome::Unexecutable`'s doc comment describes. This arm is
            // its only producer.
            let released = match finish(
                &mut *transaction,
                id,
                Outcome::Unexecutable {
                    reason: reason.clone(),
                },
            )
            .await
            {
                Ok(()) => transaction.commit().await,
                Err(error) => Err(error),
            };
            // A lost terminal write is not best-effort bookkeeping (`runs::warn_on_terminal_write_err`
            // makes the same argument), and the caller cannot infer it: it receives the *parse*
            // error and will reasonably read that as "handled, move on". The transaction keeps this
            // from jamming anything — the whole claim rolls back, so the row is `queued` rather
            // than stranded — but it does mean the next poll will hand out the same corrupt row
            // again, and a log line is the only thing that distinguishes that loop from silence.
            if let Err(error) = released {
                tracing::warn!(
                    vcs_request_id = id,
                    %error,
                    "could not record an unparseable vcs request as failed; it stays queued and will be claimed again"
                );
            }
            // The parse error is what propagates either way: it names the defect rather than its
            // symptom, and it is the one that stays true whether or not the release was recorded.
            Err(sqlx::Error::Protocol(reason))
        }
    }
}

/// Releases the repository by writing the claimed request's terminal status.
///
/// The columns an outcome does not carry are written NULL rather than left alone: one statement
/// covers every outcome, and NULL is already what those columns hold for a row that has only ever
/// been queued and claimed.
///
/// Scoped to `status = 'running'`, and a zero-row match is `RowNotFound` rather than a silent
/// `Ok(())` (the convention at `runs.rs:744`). Only the holder of a claim may end it: once Task 5's
/// restart reconciliation can mark a stranded row `interrupted` with the reason why, an unscoped
/// write would let a worker whose future outlived that reconcile flip `interrupted` to `succeeded`
/// and NULL the reason — destroying the only trace of the interruption, and reporting success for
/// work whose outcome nobody actually observed.
///
/// Generic over the executor so the claim can perform its own release inside the transaction that
/// makes the pair uncancellable; callers holding a pool pass `&pool` unchanged.
pub async fn finish<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    id: i64,
    outcome: Outcome,
) -> sqlx::Result<()> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    // Read before the match below consumes the outcome, and from `status()` rather than restated in
    // each arm — see its doc comment for why there is only one place that names a status.
    let status = outcome.status();
    let (result_sha, failure_reason, exit_code, output_tail) = match outcome {
        Outcome::Succeeded { sha, output_tail } => (sha, None, None, Some(output_tail)),
        Outcome::Blocked {
            reason,
            output_tail,
        } => (None, Some(reason), None, Some(output_tail)),
        Outcome::Failed {
            reason,
            exit_code,
            output_tail,
        } => (None, Some(reason), exit_code, Some(output_tail)),
        Outcome::Unexecutable { reason } => (None, Some(reason), None, None),
    };
    let finished = sqlx::query(
        "UPDATE vcs_requests
            SET status = ?, finished_at = ?, result_sha = ?, failure_reason = ?,
                exit_code = ?, output_tail = ?
          WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(finished_at)
    .bind(result_sha)
    .bind(failure_reason)
    .bind(exit_code)
    .bind(output_tail)
    .bind(id)
    .execute(executor)
    .await?;
    if finished.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

/// What `wait_for` hands back: either the row's outcome, if the wait caught it before the deadline,
/// or its current in-flight status if not.
///
/// Serializable because it crosses the boundary Task 8 adds: an agent's blocking merge request gets
/// exactly this back as its HTTP response body, whether the queue answered inside the deadline or
/// not.
///
/// `status` is the same string the `status` column holds rather than an enum: `rejected` is in that
/// column's CHECK constraint even though nothing in this module writes it yet — `cancelled` was too
/// until `cancel_for_run` arrived — and a `Ticket` round-trips whichever one a row holds without this
/// module needing to know what it means.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: i64,
    pub status: String,
    pub result_sha: Option<String>,
    pub failure_reason: Option<String>,
}

/// One row as a queue listing shows it: what was asked, for which repository, by whom, and where it
/// got to.
///
/// A separate type from `Ticket` rather than a reuse of it, because the two answer different
/// questions. A ticket answers "how did MY request end" and needs the result; a listing answers
/// "what is this queue doing" and needs the operation and the project, which a ticket does not
/// carry — reusing it would produce a column of statuses attached to nothing.
///
/// `op` is the `op` column verbatim, not a parsed `Op`. Parsing can fail on a row written by an
/// older version or edited by hand, and one such row must not be able to fail the whole listing —
/// the listing is exactly where somebody would go to find out that a row is wrong.
/// `FromRow` rather than a positional tuple, for the reason `runs.rs` states: a tuple makes the
/// column-order-to-field-order correspondence load-bearing and invisible, and six of these seven
/// fields are `String`, so a swap would compile and pass.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestSummary {
    pub id: i64,
    pub op: String,
    pub project_id: String,
    /// What the queue locked on for this row, and therefore what the listing is grouped by.
    ///
    /// Carried ALONGSIDE `project_id` rather than instead of it. The project is the label a reader
    /// recognises; the key is the only thing that says whether two differently-labelled rows were
    /// competing for the same refs. Without it, a listing that contains another project's work
    /// reads as a bug in the listing rather than as the fact it is reporting.
    pub repo_key: String,
    pub origin: String,
    pub status: String,
    pub created_at: String,
}

/// How many rows a listing returns at most.
///
/// Note what this table is: nothing prunes `vcs_requests`, so it is the permanent history of every
/// git operation this daemon has ever queued, not a snapshot of what is pending. A listing that hits
/// this cap therefore means "the daemon has been running a while" — it is not a finding, and this
/// cap is not a diagnostic. It exists only so one HTTP call cannot return an unbounded response.
///
/// When someone adds retention, or paging, this is where they start.
const LIST_LIMIT: i64 = 200;

/// Newest first, optionally narrowed to one repository.
///
/// Newest first because the question a listing answers is almost always "what just happened", and a
/// caller reading a truncated oldest-first list would be reading history while missing the present.
///
/// **Narrowed by REPOSITORY, though the caller names a project.** The lock this queue takes is on
/// the repository, so two projects pointing at one checkout share a queue and compete for the same
/// refs. A listing that filtered on `project_id` would hand each of them a view with the other's
/// operations missing — which is the single fact about this queue a listing must not hide, and it
/// was the abbreviation this function shipped with while `project_id` and the key were still the
/// same thing.
///
/// The key is read from the TABLE, not from `resolve_repo`, and that is deliberate twice over.
/// A listing must not need a git subprocess: it is where somebody goes when something is already
/// wrong, and a project whose directory has moved or gone would then have no listing at all rather
/// than a listing of what it did. And the row's own `repo_key` is the better authority anyway — it
/// is what the queue actually locked on at the time, which is not necessarily what the disk would
/// say now.
///
/// A project with nothing queued yet makes the subquery NULL, so the comparison is NULL and the
/// listing is empty. That is the right answer and not an accident of SQL: nothing has been queued
/// for it, so there is no repository to widen to.
pub async fn list(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
) -> sqlx::Result<Vec<RequestSummary>> {
    sqlx::query_as(
        "SELECT id, op, project_id, repo_key, origin, status, created_at
           FROM vcs_requests
          WHERE ?1 IS NULL
             OR repo_key = (
                  SELECT repo_key FROM vcs_requests
                   WHERE project_id = ?1
                   ORDER BY id DESC LIMIT 1
                )
          ORDER BY id DESC
          LIMIT ?2",
    )
    .bind(project_id)
    .bind(LIST_LIMIT)
    .fetch_all(pool)
    .await
}

/// The longest a caller that asked to wait is held before it gets a ticket instead.
///
/// Spec decision 3. The common case — an empty queue — answers from the first read and never
/// approaches this. The bad case is two merges queued behind a slow one, and the number exists so
/// that case stops killing the caller's run by timeout: the agent gets a ticket back and decides for
/// itself whether to keep waiting.
///
/// **The constraint to check this against is `state.rs`'s `DEFAULT_PROGRESS_TIMEOUT` (300s), not the
/// 600s wall clock.** A CLI blocked on a call for this long streams no events, and the progress
/// timeout is what kills a run that has gone quiet — so it binds first, and the real margin is
/// roughly 6.7x rather than the 13x the wall clock would suggest. Anyone tempted to lengthen this
/// has to answer to 300s. A wait that outlived the run waiting on it would be worse than no wait.
pub const DEFAULT_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// How often `wait_for` re-checks a row that has not reached a terminal status yet.
///
/// 25ms, chosen from two directions that happen to agree.
///
/// In production it bounds how often one waiting caller queries: against the ~45s deadline this
/// pillar is designed around, 10ms would be roughly 4500 reads per waiting agent and 25ms roughly
/// 1800, while the extra latency it can cost — one interval, for a request that finishes just after
/// a poll — is nothing beside a merge measured in seconds.
///
/// In the tests it is the discrimination margin, and that is the reason it is not smaller.
/// `a_finished_request_returns_its_outcome_without_waiting` proves the answer came from the read
/// *before* the first sleep, and elapsed time is the only evidence of that — a loop that slept first
/// would return the same status, just one interval later. At 10ms the assertion sat exactly on the
/// boundary with no headroom, so ordinary scheduler jitter on a loaded laptop could fail a correct
/// implementation. The interval IS the margin; this file's other timing tests are documented as
/// leaving 10x.
///
/// A `Notify` would remove the wait entirely, but nothing here has a real operation duration yet to
/// make that worth the added machinery — the same tradeoff `drain_once`'s doc comment argues for
/// `VcsExecutor` staying a plain trait rather than a channel.
const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

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
pub const TERMINAL_STATUSES: [&str; 6] = [
    "succeeded",
    "failed",
    "blocked",
    "rejected",
    "cancelled",
    "interrupted",
];

/// Blocks the caller until request `id` reaches a terminal status or `deadline` passes — whichever
/// comes first — and returns a `Ticket` either way.
///
/// Never an error for "still going": a caller told the wait failed would reasonably retry or give
/// up, and both are wrong when the request is simply still queued behind another. The common case —
/// an empty queue — answers from the very first read, before any sleep, so it behaves like an
/// ordinary blocking call. The bad case — two merges queued behind a slow one — stops costing the
/// caller a hard timeout: it gets a ticket back instead and decides for itself whether to keep
/// waiting.
///
/// Terminal means `succeeded`, `failed`, `blocked`, `interrupted`, or `cancelled` — the five statuses
/// `finish`, `reconcile_interrupted`, `cancel_for_run` and `reap_requests_of_ended_runs` actually
/// write today, matching the vocabulary those four already use (see `finish`'s own doc comment, and
/// the interrupted-is-terminal test above). `cancelled` is the one this list gained last, and it has
/// two writers rather than one: `cancel_for_run` retires a request the moment its run is cancelled,
/// and the reaper retires one whose run had already ended by some other door when the queue reached
/// it. Either way the row can no longer change, so a caller waiting on it must be told now rather
/// than at the deadline. `blocked` is as
/// terminal as the other four: the queue never retries it, so a caller held to the deadline would
/// be waiting on a row that can no longer change — and it is the outcome that most needs a human to
/// see it promptly. `queued`, `running` and `awaiting_approval` are treated identically: all three
/// can still change, so none of them ends the wait early, and if the deadline passes while a row is
/// in any of them the ticket just reports whichever one it is. `awaiting_approval` is deliberately
/// not special-cased to end the wait sooner — a human approving mid-wait is exactly the change this
/// loop is built to catch on its next poll, and treating "needs a human" as if it were "done" would
/// tell an agent to stop watching a request that is very much still alive.
///
/// An `id` with no matching row is answered `Err(RowNotFound)` on the very first read, without
/// spending any of the deadline: every id in circulation came from `submit`, which hands one back
/// only after its INSERT has committed, and nothing in this module deletes a row *today*. So a
/// missing row is not "hasn't arrived yet" — it cannot ever arrive — and polling it out to the
/// deadline would just be quietly burning the caller's wait on a request that does not exist.
///
/// The hedge was deliberate: that is a claim about the whole module, not about this function, and
/// the first retention or cleanup pass added anywhere in `vcs.rs` would invalidate it silently — the
/// failure being a caller told "no such request" about one that merely aged out. **Pruning has since
/// arrived and the claim still holds**, because `prune_output_tails` empties `output_tail` and keeps
/// the row rather than deleting it — a shape it was pushed into by `action_grants.queued_request_id`
/// having no foreign key, and which happens to discharge this obligation for free. The hedge stays
/// written down because the next cleanup pass inherits it.
///
/// Reads before it ever sleeps, and every subsequent iteration does the same: the terminal check
/// runs on freshly read data, not on whatever the previous iteration saw, so a row that finishes
/// between two polls is reported the moment the next read sees it rather than after the deadline.
pub async fn wait_for(
    pool: &sqlx::SqlitePool,
    id: i64,
    deadline: std::time::Duration,
) -> sqlx::Result<Ticket> {
    let started = std::time::Instant::now();
    loop {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT status, result_sha, failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;

        let Some((status, result_sha, failure_reason)) = row else {
            return Err(sqlx::Error::RowNotFound);
        };

        let terminal = TERMINAL_STATUSES.contains(&status.as_str());
        if terminal || started.elapsed() >= deadline {
            return Ok(Ticket {
                id,
                status,
                result_sha,
                failure_reason,
            });
        }

        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
}

/// Marks every request still `running` at startup as `interrupted` — the daemon died mid-operation,
/// and nothing can say whether git finished. Called once at startup, the same moment
/// `runs::reconcile_orphaned_runs` runs its counterpart pass over `runs`.
///
/// No auto-retry: a re-run `merge` is harmless, a re-run `tag` is not, and telling the two apart
/// from a cold start is guessing. The row is left `interrupted` with a reason a human can act on,
/// not silently re-queued.
///
/// One statement, not a `SELECT` then an `UPDATE`: nothing else is racing a fresh startup for these
/// rows, so the two-step shape `claim_next`'s doc comment warns against is not the risk here — the
/// single statement is simply the smaller diff to read `RETURNING id, project_id, run_id` off.
///
/// The feed write is best-effort per row (this crate's convention — see `runs.rs`, `job.rs`,
/// `scheduler.rs`): it is observational, so a write it cannot make must not undo the row it is
/// only reporting on. Contrast `finish`, where the terminal write itself is load-bearing.
pub async fn reconcile_interrupted(pool: &sqlx::SqlitePool) -> sqlx::Result<u64> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let reconciled: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'interrupted', finished_at = ?,
                failure_reason = 'daemon restarted mid-operation'
          WHERE status = 'running'
         RETURNING id, project_id, run_id",
    )
    .bind(finished_at)
    .fetch_all(pool)
    .await?;
    for (id, project_id, run_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_interrupted",
            &format!("vcs request {id} interrupted: daemon restarted mid-operation"),
            *run_id,
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

/// Cancels every request run `run_id` asked for that has not started, and returns how many.
///
/// **`running` is deliberately excluded.** Spec §7: an operation already in flight finishes. A merge
/// abandoned half-way is worse than one nobody is waiting for any more, and the queue could not stop
/// it in any case — the git it spawned belongs to the daemon, not to the run whose context ran out.
/// What this reclaims is the place a dead run would otherwise hold in the FIFO.
///
/// `awaiting_approval` is swept alongside `queued`, and that is the arm that matters most: a request
/// nobody has approved yet, belonging to a run that no longer exists, would otherwise sit there until
/// a human approved work for an agent that is gone.
///
/// **Why this cannot race the claim**, which is worth writing down because it is conditional on
/// something a future edit could break: `claim_next` opens a transaction whose FIRST statement is its
/// conditional UPDATE, so it takes SQLite's single write lock outright. Either the claim commits
/// first — the row is `running`, and the filter below excludes it — or this commits first and the
/// claim's subquery finds no `queued` row. There is no window in which a row goes `cancelled` while
/// git is running against it. And even if there were, `finish` is scoped to `status = 'running'`, so
/// a lost race lands in `drain_once`'s existing warn arm rather than overwriting a terminal status.
///
/// The feed write is best-effort per row, this crate's convention for observational writes: a feed
/// row that cannot be written must not undo the cancellation it is only reporting on.
pub async fn cancel_for_run(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<u64> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let cancelled: Vec<(i64, String)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'cancelled', finished_at = ?,
                failure_reason = 'the run that asked for this ended before it started'
          WHERE run_id = ? AND status IN ('queued', 'awaiting_approval')
         RETURNING id, project_id",
    )
    .bind(finished_at)
    .bind(run_id)
    .fetch_all(pool)
    .await?;

    for (id, project_id) in &cancelled {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_cancelled",
            &format!("vcs request {id} cancelled with run {run_id}"),
            Some(run_id),
        )
        .await;
    }

    Ok(cancelled.len() as u64)
}

/// Retires every request in this repository whose submitting run is no longer alive — it ended, or
/// its row is gone — and returns how many. Called by `drain_once` before it claims.
///
/// **This is the pull half of spec §7's "the agent that submitted dies", and it exists because the
/// push half cannot be complete.** `runs::finalize_termination` sweeps the paths that go through it,
/// but a run's terminal status is also written by direct UPDATEs elsewhere, and the next one will be
/// written by somebody who does not know the list exists. Asking here — at the one point that must be
/// correct anyway, because it is where a merge is about to be executed — makes the guarantee true by
/// construction rather than by everyone remembering.
///
/// "Elsewhere" is deliberately not a number. It was written as "four other places" and an audit put
/// the real count at roughly twice that, which is the argument for this function rather than against
/// it — but a count in a comment is a claim that goes stale on its own, and this one would go stale
/// in the direction of sounding smaller than it is.
///
/// `cancel_for_run` is kept alongside it and is not redundant: it makes a cancelled run's requests
/// disappear *immediately*, rather than at the next poll of a repository that may have nothing else
/// queued for hours.
///
/// **The three cases, decided here rather than left to be reassembled by a reader.**
///
/// 1. *The run is alive* — including paused at `awaiting_approval`, which resumes — and its request
///    is **kept**. This is the case the whole shape exists to protect: a merge cancelled out from
///    under a run that was only waiting on a human is work destroyed for no reason.
/// 2. *The run has ended* and its request is **reaped**, because nothing will ever come back for it.
/// 3. *The run's row is gone* and its request is **reaped too** — a row that is not there cannot be
///    running. Nothing ties `run_id` to `runs` (`0048_vcs_requests.sql` writes no foreign key) and
///    rows really are deleted from `runs`, so this is an ordinary state and not a corrupt one. Kept,
///    such a request would eventually be claimed and **executed**: a merge performed against the
///    repository on behalf of a run that no longer exists.
///
/// Case 3 is the only reason the predicate is `NOT EXISTS (… still alive …)` rather than the
/// `EXISTS (… already ended …)` it reads as the obvious spelling of. The two agree on every row
/// whose run row exists, and differ only when it does not — where this direction is the one spec §7
/// asks for.
///
/// The status list comes from `runs::ENDED_RUN_STATUSES` rather than being spelled here. Note what
/// is NOT in it: a run at `awaiting_approval` is paused for a human and will resume, so its merge
/// must survive; and a run that finished its work normally never reaches `finalize_termination` at
/// all, which is correct — it asked for the merge and should have it.
///
/// `running` is excluded by the same status filter `cancel_for_run` uses, and for the same reason
/// spelled out there: an operation already in flight finishes. Here it is also structural — this
/// runs *before* the claim, so there is nothing of this drain's in flight to protect.
///
/// The `IN` clause's placeholders are generated from the constant's length and every element bound,
/// rather than the list being interpolated into the string: a status is data, and a query built by
/// formatting values into SQL is the shape that stops being safe the moment one of them stops being
/// a literal somebody wrote by hand.
pub async fn reap_requests_of_ended_runs(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
) -> sqlx::Result<u64> {
    let placeholders = vec!["?"; crate::runs::ENDED_RUN_STATUSES.len()].join(", ");
    // `run_id IS NOT NULL` is **load-bearing**, and it is the `NOT EXISTS` shape that makes it so.
    // A NULL joins nothing, so for a request no run owns the subquery is empty and `NOT EXISTS` is
    // TRUE — without this line every human's request in the repository would be reaped as though
    // its run had vanished, which is the very case 3 below is about. It reads like the sentence the
    // reaper means (a request no run owns is not one a run can have abandoned) and it is also the
    // guard; `a_humans_request_is_never_reaped` is what holds it.
    //
    // `NOT EXISTS (… alive …)` rather than `EXISTS (… ended …)`: see this function's doc comment.
    // The two differ on exactly one row shape — a `run_id` naming a run row that is gone — and this
    // is the direction that retires it instead of queueing a merge for it for ever.
    let sql = format!(
        "UPDATE vcs_requests
            SET status = 'cancelled', finished_at = ?,
                failure_reason = 'the run that asked for this had already ended when the queue reached it'
          WHERE repo_key = ? AND status IN ('queued', 'awaiting_approval')
            AND run_id IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM runs
                             WHERE runs.id = vcs_requests.run_id
                               AND runs.status NOT IN ({placeholders}))
         RETURNING id, project_id, run_id"
    );

    let finished_at = chrono::Utc::now().to_rfc3339();
    // `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str`, and this string is built at
    // runtime. Audited, and the audit is short: the only interpolation is `placeholders`, which is
    // `n` question marks derived from a compile-time constant's length. No value reaches the SQL —
    // the statuses themselves are bound below, one per placeholder.
    let mut query = sqlx::query_as::<_, (i64, String, Option<i64>)>(sqlx::AssertSqlSafe(sql))
        .bind(finished_at)
        .bind(repo_key.to_string());
    for status in crate::runs::ENDED_RUN_STATUSES {
        query = query.bind(*status);
    }
    let reaped = query.fetch_all(pool).await?;

    // Best-effort per row, this crate's convention for observational writes: a feed row that cannot
    // be written must not undo the cancellation it is only reporting on.
    for (id, project_id, run_id) in &reaped {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_cancelled",
            &format!("vcs request {id} cancelled: the run that asked for it had already ended"),
            *run_id,
        )
        .await;
    }

    Ok(reaped.len() as u64)
}

/// How long a finished request keeps what git printed.
///
/// Thirty days, the same window `runs::prune_transcripts` gives a run's transcript, because it is
/// the same kind of thing: the tail is what a subprocess said, kept so a person can read why an
/// operation ended the way it did, and nobody reads that a month later. What people DO read months
/// later is the metadata — which operation, against which repository, which sha came out — and that
/// is a couple of hundred bytes.
///
/// The bulk is real rather than theoretical: `git_exec::OUTPUT_TAIL_BYTES` caps one tail at 8 KiB,
/// and a conflicted merge in a large repository reaches it.
pub const DEFAULT_OUTPUT_RETENTION_DAYS: i64 = 30;

/// The window, overridable for an operator who wants a different one — the shape
/// `runs::transcript_retention_days` uses, for the reason it gives.
pub fn output_retention_days() -> i64 {
    std::env::var("NUCLEOS_VCS_OUTPUT_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_OUTPUT_RETENTION_DAYS)
}

/// Empties the output of every finished request past the window, leaving the row.
///
/// **The row survives, and here that is not merely the tidier choice — it is the only correct one.**
/// `runs::prune_transcripts` keeps its rows because a feed entry, a job item or a proposal still
/// points at them. This table has a sharper version of the same fact: `action_grants.queued_request_id`
/// (migration 0054) names a request by id and carries **no foreign key** — checked, it is a bare
/// `INTEGER`. So a DELETE would not fail and would not cascade; it would leave a takeover grant
/// pointing at nothing, and `proposals::matching_queued_request` would go on telling a run that
/// request 7 holds its work.
///
/// It also discharges, rather than merely dodging, the obligation `wait_for` records: that function
/// answers `RowNotFound` immediately because a missing row *cannot ever arrive*, and its doc comment
/// says the first pruning pass in this module invalidates that silently. Emptying instead of
/// deleting keeps it true by construction.
///
/// `COALESCE(finished_at, created_at)` because a terminal row with no finish stamp is possible —
/// `reconcile_interrupted` writes one, but a hand-edited or half-written row need not — and ageing
/// such a row from nothing would exempt it for ever.
///
/// The cutoff is RFC 3339 built in Rust and never SQLite's `datetime('now','-N days')`, for the
/// reason `web::prune` sets out in full and the other three sweeps repeat: the two spellings are
/// compared as TEXT and `T` (0x54) sorts after the space (0x20), so within the cutoff's own day a
/// row hours too old compares as newer and survives every sweep for ever.
pub async fn prune_output_tails(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    if retain_days <= 0 {
        // Zero would strip every tail on the machine at the next sweep, which is not a retention
        // policy but a typo with a plausible-looking value. The reading all three existing sweeps
        // make.
        return Ok(0);
    }

    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    // `AssertSqlSafe` because sqlx only trusts `&'static str` and this is built at runtime. The one
    // interpolation is a row of `?` derived from a compile-time constant's length; every status and
    // the cutoff are bound. The same audit `reap_requests_of_ended_runs` writes out above.
    let placeholders = vec!["?"; TERMINAL_STATUSES.len()].join(", ");
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE vcs_requests
            SET output_tail = NULL
          WHERE status IN ({placeholders})
            AND COALESCE(finished_at, created_at) < ?
            AND output_tail IS NOT NULL"
    )));
    for status in TERMINAL_STATUSES {
        update = update.bind(status);
    }
    // `output_tail IS NOT NULL` above is what makes the count mean something: without it every
    // eligible row is rewritten on every hourly sweep for ever, and the log would report the same
    // number until the end of time instead of the number of tails this pass actually dropped.
    Ok(update.bind(&cutoff).execute(pool).await?.rows_affected())
}

/// The núcleo↔git boundary, the same seam `runner.rs` gives the núcleo↔model one: this module
/// decides *when* an operation may run and records how it ended, and this trait is the only thing
/// that knows how to actually perform one. Chunk 1 has only the test double below, which is why
/// nothing here builds an argv yet.
///
/// Chunk 2's real `GitExecutor` belongs in **its own module** (`git_exec.rs`), not in this file.
/// Everything it needs — argv construction, a deadline, an output ceiling, capturing what the
/// subprocess printed — is process transport, and the two comparable concerns in this crate,
/// `gate.rs` and `transcribe.rs`, are each their own module for exactly that reason. Putting it here
/// would make one file both the queue domain and the process transport, which is the coupling
/// `core/AGENTS.md`'s module map exists to prevent.
#[async_trait::async_trait]
pub trait VcsExecutor: Send + Sync {
    /// Performs the claimed operation and reports how it ended.
    ///
    /// An `Outcome` rather than a `Result` because a git command that exits non-zero is not an error
    /// of this call — it is the answer. A conflicted merge is a `Failed` the queue must record
    /// against the row, not a failure to have asked.
    async fn execute(&self, request: &ClaimedRequest) -> Outcome;
}

/// Claims, executes and finalizes exactly one request for one repository, and says whether it found
/// anything to do — so a caller can drain until this returns `false` and only then wait.
///
/// Nothing is propagated, because there is no caller left who could act on a `Result`: this is the
/// step a polling loop repeats. A claim that errored has already dealt with its own row —
/// `claim_next` either records it terminal or rolls the whole claim back to `queued`, so either way
/// this repository is not left holding it. What a failed *terminal* write costs is argued at the
/// call site, where it gets read.
///
/// `true` means a request was claimed and executed — including when the terminal write then failed,
/// because the work did happen and a drain loop must not read that as "the queue was empty". Neither
/// failure path spins: a *refused* write leaves the row terminal, so the next claim moves on to the
/// next request, and a *lost* one leaves it `running`, so the next claim finds the repository busy
/// and returns `false`.
///
/// **Cancellation** (`core/AGENTS.md` § "Cancellation safety"). `claim_next` could make its claim and
/// its compensating release uncancellable by putting them in one transaction; this cannot use the
/// same answer. The await in the middle is git, running for as long as a merge takes, and SQLite
/// admits one writer at a time — a transaction held open across it would stall every other writer in
/// the daemon. Rolling one back would be worse than slow: it would un-claim a row whose git command
/// had already run, erasing the only record that the repository was touched.
///
/// So the window is real and is left open on purpose. A drain dropped between the claim and `finish`
/// leaves its row `running`, and the partial unique index makes that row hold the repository's only
/// slot until the next startup's `reconcile_interrupted` releases it — the jam AGENTS.md describes,
/// in the strong form runs and jobs left behind in migration 0053 and this queue keeps, because a
/// repository has exactly one slot. What keeps it acceptable is who calls this:
/// the only production caller is the **detached task** `run_queue_worker` spawns per repository, and
/// a detached task's future is dropped only at runtime shutdown, which is precisely the case
/// `reconcile_interrupted` exists for — `main.rs` runs that reconcile before it spawns the worker.
///
/// Two consequences of it being *detached* that are easy to get wrong, and one of them is a trap
/// waiting for whoever adds graceful shutdown. **Aborting `run_queue_worker` does not stop a drain
/// already in flight**: the loop owns no handle to the tasks it spawns, so an abort drops the poller
/// and leaves every running merge running — which is what the worker tests do at teardown, and why
/// they are not evidence of a clean stop. And the exposure is now one open window *per repository*
/// rather than one for the daemon, since each repository's drain is its own task.
///
/// Both halves of that are run rather than argued —
/// `a_drain_abandoned_mid_operation_jams_the_repository_until_a_restart_reconciles` drops a drain
/// inside the operation, shows the repository jammed, and then shows the reconcile releasing it.
///
/// **Do not await this inside an HTTP handler.** A client disconnecting mid-merge would strand the
/// row and jam that repository until a restart, and unlike a lost reply nobody would see it happen.
/// A handler that wants a drain goes through `http::uncancellable` (AGENTS.md rule 2) — whose spawn
/// needs owned `'static` arguments, which is a different signature from this one.
///
/// The one thing this call *can* narrow, it does: nothing is awaited between the executor returning
/// and `finish` writing the outcome, so the exposure is the operation itself and not a line longer.
/// An await added there — a feed append, a notification — would widen it for nothing; those belong
/// after the terminal write.
pub async fn drain_once(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
    executor: &dyn VcsExecutor,
) -> bool {
    // Spec §7's pull half, and the reason the other writers of a run's terminal status do not each
    // need a sweep of their own — `reap_requests_of_ended_runs` argues the whole case. It goes
    // before the claim because after it the row would already be `running`, which the reap leaves
    // alone by design.
    //
    // Best-effort, like the feed appends below: a reap that could not run leaves stale requests a
    // human can cancel, whereas returning early on it would stop the queue for this repository
    // outright — including for every request whose run is perfectly alive.
    if let Err(error) = reap_requests_of_ended_runs(pool, repo_key).await {
        tracing::warn!(
            repo_key = %repo_key,
            %error,
            "could not reap the vcs requests of runs that have already ended"
        );
    }
    let claimed = match claim_next(pool, repo_key).await {
        Ok(Some(claimed)) => claimed,
        // Nothing queued, something already running, or only unapproved rows — all "come back
        // later", and the caller waits the same way for each.
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(
                repo_key = %repo_key,
                %error,
                "could not claim the next vcs request"
            );
            return false;
        }
    };
    let id = claimed.id;
    let outcome = executor.execute(&claimed).await;
    // Cloned rather than moved so the failure paths below can still name it. `finish` consumes the
    // outcome, and a refused write would otherwise drop the only copy of a sha that git really
    // produced — leaving a commit the daemon caused recorded nowhere in the system at all.
    match finish(pool, id, outcome.clone()).await {
        // Spec §6.4's fifth step, and spec §2.1's whole argument for this pillar having no view of
        // its own: every transition writes to `feed.rs`, which the shell already shows. Without this
        // row, a merge the daemon performed is invisible to the person who asked for it.
        //
        // Best-effort with `let _`, this crate's convention for observational writes
        // (`reconcile_interrupted` above, and `runs.rs`, `job.rs`, `scheduler.rs`): a feed row that
        // cannot be written must not undo the terminal write it is only reporting on.
        //
        // **Only on `Ok`, and that is not decoration.** `finish` is scoped to `status = 'running'`,
        // so it returns `RowNotFound` when a restart's `reconcile_interrupted` took the row first.
        // An unconditional append would then announce "vcs request 7 succeeded" in the one surface
        // spec §2.1 says the user looks at, while the row itself reads `interrupted`.
        //
        // The status comes from the outcome this function already holds, never from re-reading the
        // row — the row is what the feed is reporting on, and reading it back would report whatever
        // won a race rather than what this operation did.
        //
        // `None` for `run_id`: `ClaimedRequest` does not carry one and `claim_next` does not return
        // one, and widening its `RETURNING` to supply it would buy nothing today.
        //
        // **What that rests on is that nothing in production builds a `Run` request at all today.**
        // The only mapping from a caller to `Origin::Run` is `http.rs`'s `vcs_origin`, and a run
        // token opens exactly one route — `POST /hooks/pretooluse-decision` (`auth.rs`) — which is
        // not the one that reaches `submit`; `vcs_origin`'s own doc comment says as much seven lines
        // above that arm. Every `submit(.., Origin::Run(..))` in the tree is inside a
        // `#[cfg(test)] mod tests`. So `run_id` is NULL on every row this table holds, claimable or
        // not, and this `None` throws nothing away.
        //
        // **This paragraph used to promise that Chunk 4 would open the route to a run scope, and
        // that promise contradicted `auth.rs`.** Chunk 4 has since landed and did the opposite, on
        // purpose: `auth.rs` argues at length that `POST /vcs/requests` is a sibling of
        // `/email/send` rather than of `/runs` — *"Queueing is Admin's"* — and `Scope::Run` still
        // reaches exactly one route. What Chunk 4 opened instead is the door a human already stood
        // at: `runs::resume_approved_run` translates an approved action and admits it in the same
        // transaction, as `Origin::Human`, because a person just authorised it.
        //
        // So `run_id` is NULL on every row in production and this `None` still throws nothing away.
        // Two consequences worth stating rather than leaving to be rediscovered: `Origin::Run`,
        // `needs_approval`, `cancel_for_run` and `reap_requests_of_ended_runs` are correct and
        // DORMANT — they have nothing to match, because no production row carries a `run_id` — and
        // they are kept rather than deleted because they are what the design needs the day a scope
        // is invented that may queue on its own behalf. Whoever invents it changes `auth.rs` first,
        // and this comment second.
        // (`reconcile_interrupted` and `reap_requests_of_ended_runs` do attach one, because they
        // read whole rows rather than a claim.)
        Ok(()) => {
            let _ = crate::feed::append(
                pool,
                Some(claimed.project_id.as_str()),
                "vcs_request_finished",
                &format!("vcs request {id} {}", outcome.status()),
                None,
            )
            .await;
        }
        // Not a lost write: `finish` is scoped to `status = 'running'`, so this is the row being
        // taken out from under the operation — a restart's `reconcile_interrupted` already marked
        // it `interrupted`, the collision `a_reconciled_request_cannot_be_finished_by_a_late_worker`
        // covers. The repository is NOT jammed; the row is terminal and the queue moves on. What
        // is lost is the outcome, which the row is now refusing, so this log line is the only
        // place it survives.
        Err(sqlx::Error::RowNotFound) => tracing::warn!(
            vcs_request_id = id,
            repo_key = %repo_key,
            ?outcome,
            "a vcs request stopped running before its outcome arrived; the row refused it, so it is recorded here"
        ),
        // Anything else is the write itself failing, and that one does jam. This is the write
        // that releases the repository: without it the row stays `running` and every later
        // request for this repository waits behind it until a restart reconciles, so the log
        // line is the only account of why the queue stopped.
        Err(error) => tracing::error!(
            vcs_request_id = id,
            repo_key = %repo_key,
            ?outcome,
            %error,
            "could not record how a vcs request ended; it stays running until the daemon restarts"
        ),
    }
    true
}

/// How often the worker looks for repositories with queued work.
///
/// Much shorter than this daemon's other background loops (`scheduler.rs` 30s, `repo_trigger.rs`
/// 5min, `worktree::run_gc` 30min) because this is the only one a human is actively waiting on: they
/// asked for a merge and are watching for it. The cost of the interval is one query that the
/// `vcs_requests_queued` partial index covers exactly, over an index that is *empty* whenever
/// nothing is queued — which is almost always.
///
/// A `tokio::sync::Notify` would remove the interval entirely and is the obvious next step if this
/// ever shows up in a profile. It is not here yet because it has to be signalled from `submit`, which
/// would give the queue a second way to be woken and a second way to be missed — worth it for real
/// latency, not for 500ms.
const WORKER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Drains every repository with queued work, forever. Spawned once by `main.rs`.
///
/// One task per repository per tick, rather than a loop over repositories: draining them in sequence
/// would make a ten-minute fetch in one project the reason another project's merge is late, and
/// "separate repositories do not wait on each other" is the promise the whole per-repository locking
/// design exists to keep.
///
/// **No bookkeeping of which repositories are already draining, deliberately.** A redundant task for
/// a repository that is already busy is not a hazard: its `claim_next` finds a `running` row, returns
/// `None`, and the task exits — the database is the arbiter, exactly as it is for everything else
/// here. The cost is one index-covered query per tick per busy repository, and what it buys is that
/// there is no in-memory set that can disagree with the database about who holds what.
///
/// One consequence worth naming before somebody reads it as a defect: a redundant claim can also
/// exhaust `busy_timeout` (10s, `storage.rs:69`) against another writer and come back
/// `Err(SQLITE_BUSY)` rather than `Ok(None)` — `claim_next`'s own doc comment describes this. It is
/// still safe, because nothing was claimed and `drain_once` returning `false` ends the loop rather
/// than spinning, but it surfaces as a `could not claim the next vcs request` warning that is
/// expected under contention.
///
/// Each spawned task drains until its repository is empty rather than taking one request, so the
/// second of two queued merges does not wait a tick for no reason.
pub async fn run_queue_worker(pool: sqlx::SqlitePool, executor: std::sync::Arc<dyn VcsExecutor>) {
    let mut interval = tokio::time::interval(WORKER_POLL_INTERVAL);
    loop {
        interval.tick().await;

        let repositories: Vec<String> = match sqlx::query_scalar(
            "SELECT DISTINCT repo_key FROM vcs_requests WHERE status = 'queued'",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(repositories) => repositories,
            // Best-effort, like every other polling loop in this crate: a failed poll is the next
            // tick's problem, not a reason to stop draining every repository for ever.
            Err(error) => {
                tracing::warn!(%error, "vcs: could not look for repositories with queued work");
                continue;
            }
        };

        for repo_key in repositories {
            let pool = pool.clone();
            let executor = std::sync::Arc::clone(&executor);
            tokio::spawn(
                async move { while drain_once(&pool, &repo_key, executor.as_ref()).await {} },
            );
        }
    }
}

/// The test double for `VcsExecutor`. `#[cfg(test)]` because every user of it is a test — building it
/// into the daemon would ship an executor that can report a merge it never performed.
#[cfg(test)]
struct FakeVcsExecutor {
    outcome: Outcome,
    /// How long to take before answering. A real merge takes seconds, and a test about what happens
    /// WHILE one runs — a drain abandoned mid-operation — needs a window to abandon it in. The same
    /// reason `FakeTranscriber` carries one.
    // Qualified rather than imported: the import would be unused in the non-test build.
    delay: std::time::Duration,
    /// A rendezvous every execution must reach before any of them may answer.
    ///
    /// The only way to assert concurrency without betting on a scheduler: `n` executions in flight
    /// at once release each other, and `n - 1` or fewer never return at all. A test that instead
    /// measured elapsed time would be asserting that two things overlapped by looking at how long
    /// they took, which is a guess on a loaded machine; this is the property itself.
    barrier: Option<tokio::sync::Barrier>,
    /// Every request this was handed, in the order it was handed them.
    ///
    /// Recorded rather than counted, because a fake that ignores its argument answers identically
    /// whether the claim gave it the right row or another repository's — and the order is what makes
    /// the drain's FIFO promise checkable at all. Whole `ClaimedRequest`s rather than a tuple of
    /// fields: two adjacent `String`s destructured positionally can be swapped with every assertion
    /// still passing, which is the hazard `claim_next`'s own 5-tuple carries a warning about.
    seen: std::sync::Mutex<Vec<ClaimedRequest>>,
}

#[cfg(test)]
impl FakeVcsExecutor {
    /// `output_tail` is non-empty and deliberately unlike the sha, for the reason
    /// `failing_with`'s doc comment gives about its own two strings: a fake whose two columns
    /// carried the same text could not tell a test that they had been swapped.
    fn succeeding_with(sha: &str) -> Self {
        Self::reporting(Outcome::Succeeded {
            sha: Some(sha.into()),
            output_tail: format!("git printed this while succeeding at {sha}"),
        })
    }

    /// Answers, but not immediately.
    fn succeeding_slowly(sha: &str, delay: std::time::Duration) -> Self {
        Self {
            delay,
            ..Self::succeeding_with(sha)
        }
    }

    /// Answers only once `n` executions are in flight at the same moment — so a caller that runs
    /// them one after the other never gets an answer at all.
    fn rendezvous_of(n: usize, sha: &str) -> Self {
        Self {
            barrier: Some(tokio::sync::Barrier::new(n)),
            ..Self::succeeding_with(sha)
        }
    }

    /// `reason` and `output_tail` are deliberately different strings, so a test using this fake
    /// cannot be blind to those two columns being swapped —
    /// `a_failed_request_records_why_and_what_it_printed` uses distinct ones for the same reason.
    ///
    /// Neither is asserted *through* the drain. What that leaves untested is not whether `finish`
    /// writes the columns, which has direct coverage, but whether `drain_once` forwards the
    /// executor's outcome **whole** rather than rebuilding one of its own — and the drain test's
    /// `result_sha` assertion stands for that, one field deep.
    fn failing_with(reason: &str) -> Self {
        Self::reporting(Outcome::Failed {
            reason: reason.into(),
            exit_code: Some(1),
            output_tail: format!("git printed this while failing: {reason}"),
        })
    }

    fn reporting(outcome: Outcome) -> Self {
        Self {
            outcome,
            delay: std::time::Duration::ZERO,
            barrier: None,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Derived from `seen` rather than kept beside it: a separate counter can drift from the list it
    /// is supposed to describe, and then nothing says which of the two is right.
    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn seen(&self) -> Vec<ClaimedRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl VcsExecutor for FakeVcsExecutor {
    async fn execute(&self, request: &ClaimedRequest) -> Outcome {
        // Recorded before the delay, not after: a drain abandoned mid-operation never reaches the
        // line after the await, and a test of that case still needs to see the executor was entered.
        // The guard is a temporary so it is dropped at the end of this statement — held across the
        // await it would make this future non-`Send`, which `async_trait` requires.
        self.seen.lock().unwrap().push(request.clone());
        tokio::time::sleep(self.delay).await;
        // Held here rather than before the delay so it is the last thing between being entered and
        // answering: whatever else an execution does, it does not finish until its peers arrive.
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        self.outcome.clone()
    }
}

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
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::time::Duration;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
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
    /// The migrator's own list is walked rather than the files read directly, so this cannot drift
    /// from what ships: the SQL is the SQL that will run on the real database, in the order it will
    /// run there. Nothing is written to `_sqlx_migrations` — the bookkeeping is not what is under
    /// test, and a caller finishes the chain with `apply_migrations_after`.
    ///
    /// Not vcs-specific. It lives here because 0049 is the first data migration anybody tried to
    /// test; the second module to need it should move it somewhere neutral rather than copy it.
    pub(crate) async fn pool_migrated_through(version: i64) -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        apply_migrations(&pool, |candidate| candidate <= version).await;
        pool
    }

    pub(crate) async fn apply_migrations_after(pool: &sqlx::SqlitePool, version: i64) {
        apply_migrations(pool, |candidate| candidate > version).await;
    }

    async fn apply_migrations(pool: &sqlx::SqlitePool, wanted: impl Fn(i64) -> bool) {
        for migration in sqlx::migrate!("./migrations").iter() {
            if !wanted(migration.version) {
                continue;
            }
            // `raw_sql` rather than `query`: a migration is many statements, and `query` runs the
            // first and silently drops the rest — which would have made this harness quietly test
            // a fraction of each file.
            sqlx::raw_sql(migration.sql.clone())
                .execute(pool)
                .await
                .unwrap_or_else(|error| panic!("migration {} failed: {error}", migration.version));
        }
    }

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
        ] {
            assert_eq!(branch_delete_from_command(command), None, "{command}");
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
            ["merge", "push", "tag", "fetch", "branch-delete", "rebase"],
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
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("succeeded"), "got: {}", summaries[0]);
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
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(100));

        let logged = logged_during(async {
            tokio::join!(drain_once(&pool, "alpha", &executor), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                reconcile_interrupted(&pool).await.unwrap()
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

        // A 10x margin over the reconcile's own wait, so which lands first is not a race: an
        // in-memory claim takes microseconds, and `reconciled == 1` below fails loudly rather than
        // passing quietly if that ever stops being true.
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(100));
        let (drained, reconciled) = tokio::join!(drain_once(&pool, "alpha", &executor), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            reconcile_interrupted(&pool).await.unwrap()
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

        // Far longer than the guard below, so which of the two fires is not a race.
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_secs(30));
        {
            let drain = drain_once(&pool, "alpha", &executor);
            tokio::pin!(drain);

            // Polled until the executor has actually been ENTERED, and abandoned there — rather
            // than after a fixed slice of wall clock.
            //
            // This was `timeout(50ms, drain)`, which reads like the same thing and is not. The
            // executor's 30s makes it a non-race only for the half AFTER the operation begins;
            // before that, `drain_once` still has a reap and a claim to get through, and 50ms was a
            // real-time budget for two SQLite writes. On a machine also compiling and running the
            // other thousand tests that budget is occasionally missed, and the drain is then
            // abandoned BEFORE the operation — a different scenario wearing this test's name, which
            // is why the flake read `calls(): 0 != 1` rather than anything about jamming.
            //
            // `calls()` is the property this test is about, so it is what is waited on. The outer
            // timeout is not a budget: it is reached only if the drain never arrives at all, and it
            // is there so that failure is a message rather than a hung suite.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        _ = &mut drain => {
                            panic!("the executor sleeps for 30s — the drain cannot have finished")
                        }
                        () = tokio::time::sleep(Duration::from_millis(1)) => {
                            if executor.calls() == 1 {
                                break;
                            }
                        }
                    }
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

        // Submitted only now: the pass above is over, so nothing but a later poll can find this.
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
        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // The name's actual claim: without this, a loop that sleeps before its first read would
        // report the same status and sha 50ms later and still pass every assertion above. A single
        // in-memory read takes microseconds; one `WAIT_POLL_INTERVAL` sleep alone is 10ms, so this
        // is not a close margin — it is the difference between "never slept" and "slept at all".
        assert!(
            started.elapsed() < WAIT_POLL_INTERVAL,
            "an already-finished request must answer from the first read, not pay for a poll"
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
        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(ticket.id, id);
        assert_eq!(ticket.status, "failed");
        assert!(
            ticket.result_sha.is_none(),
            "nothing succeeded, so there is no commit"
        );
        assert_eq!(ticket.failure_reason.as_deref(), Some("CONFLICT (content)"));
        // Without this, a `wait_for` that dropped `failed` from its terminal set would still
        // report the right content 50ms later once the deadline forced an answer, and every
        // assertion above would still pass. This is what actually proves `failed` ends the wait as
        // fast as `succeeded` does, rather than merely agreeing with it once time runs out.
        assert!(
            started.elapsed() < WAIT_POLL_INTERVAL,
            "a failed request must answer from the first read, not pay for a poll"
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
            !crate::runs::ENDED_RUN_STATUSES.is_empty(),
            "an empty status list would make this test pass by iterating nothing, while the reaper \
             it drives reaped nothing either"
        );
        for status in crate::runs::ENDED_RUN_STATUSES {
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
            .filter(|status| !crate::runs::ENDED_RUN_STATUSES.contains(status))
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
        let (ticket, drained) = tokio::join!(wait_for(&pool, id, Duration::from_secs(5)), async {
            // A head start so `wait_for`'s first read sees "queued", not "running" — the loop, not
            // a lucky initial read, is what has to notice the finish.
            tokio::time::sleep(Duration::from_millis(5)).await;
            drain_once(&pool, "alpha", &executor).await
        });

        assert!(drained, "the operation ran");
        let ticket = ticket.unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // A 10x margin under the 5s deadline: the operation itself finishes around 25ms in and a
        // 10ms poll interval should catch it shortly after, so 500ms is nowhere near a close race
        // in either direction.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "wait_for must return once the request finishes, not hold the caller to the full \
             deadline: took {:?}",
            started.elapsed()
        );
    }

    /// Everything, once: a request submitted through the queue, drained by the real executor, against a
    /// real repository — and the sha in the row is the commit git actually created.
    #[tokio::test]
    async fn a_real_merge_lands_through_the_queue() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            crate::git_exec::tests::repo_with_a_branch_to_merge("nucleos-vcs-e2e-");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());

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

        assert!(drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await);

        assert_eq!(status_of(&pool, id).await, "succeeded");
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert_eq!(
            ticket.result_sha.unwrap(),
            crate::git_exec::tests::sha_of(&repo, "master")
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
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            crate::git_exec::tests::repo_with_a_branch_to_merge("nucleos-vcs-e2e-blocked-");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());

        // `feature.txt` is what `feat/x` adds, so the fast-forward has to write it — and it cannot,
        // because the user has an uncommitted copy of it sitting there.
        std::fs::write(repo.join("feature.txt"), "half-finished thought\n").expect("write");
        let before = crate::git_exec::tests::sha_of(&repo, "master");

        // Real root, synthetic key, for the reason the test above states.
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        assert!(drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await);

        assert_eq!(status_of(&pool, id).await, "blocked");
        assert_eq!(
            crate::git_exec::tests::sha_of(&repo, "master"),
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
}
