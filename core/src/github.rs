//! §spec modulo-de-github
//!
//! The GitHub pillar: total capability, autonomy by list.
//!
//! Three pieces that do not know each other. `ReadOp`/`ActOp` are what was asked for, as data;
//! `Policy` answers whether it runs without asking; `execute` answers how. It is the separation
//! `vcs.rs` keeps between WHEN an operation runs and WHETHER the actor was allowed to ask for it.
//!
//! **The partition between `ReadOp` and `ActOp` is a security boundary and not a tidy-up.**
//! `permitted_after_untrusted` (`mcp_tools.rs`) is `tool_effect(name) != Acts` — it reads the table
//! by NAME and never calls `effect_of_call`. A single tool would have to be `ReadsOwn` for the
//! argument-reading arm to run at all, and `ReadsOwn` makes that function return `true`; a turn that
//! had already read a stranger's words would pass the barrier and write to GitHub. Two tools with
//! two parameter types is what makes the barrier work without touching it.
//!
//! Whoever adds a variant owes it to the partition: an operation that acts goes in `ActOp` or it
//! goes nowhere. `read_ops_and_act_ops_partition_op` fails if it does not.
//!
//! Requests are typed, never command strings — the law `vcs.rs` states for its own queue and which
//! holds here unchanged: parsing shell is the surface `classifier.rs` exists to keep closed, so the
//! daemon builds every argv itself. **`ActOp::ApiRead` carries `Vec<String>` rather than a line**, and
//! that is the variant where the law costs most and matters most: the one escape hatch for cases
//! nobody foresaw is exactly where a command string would undo it.

use crate::mcp_tools::ToolEffect;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A read of GitHub. Never acts, whatever the arguments say — which is what lets `github_read` be
/// `ReadsOwn` by name and still be honest.
///
/// The effect is per operation and not per type, because half of these return prose somebody wrote
/// and half return structure. `effect()` is where that lands, and `mcp_tools::effect_of_call` is
/// where it is consulted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReadOp {
    RunList {
        repo: Repo,
    },
    RunStatus {
        repo: Repo,
        id: RunId,
    },
    PrList {
        repo: Repo,
    },
    /// `gh workflow list`. It existed in `READ_CEILING` before it existed here, which made the
    /// ceiling grant a prefix no typed operation could build — and `declarable_github_ops` refused
    /// it for exactly that reason. The variant is what closes the gap; the ceiling is unchanged.
    WorkflowList {
        repo: Repo,
    },
    /// Returns the body of a PR — text somebody wrote. `ReadsUntrusted`, and this is why the effect
    /// is per operation and not per tool.
    PrView {
        repo: Repo,
        number: PrNumber,
    },
    IssueView {
        repo: Repo,
        number: IssueNumber,
    },
    RunLogs {
        repo: Repo,
        id: RunId,
    },
    /// The patch a pull request proposes, as `gh pr diff` gives it: every changed line, written by
    /// whoever opened the pull request, which anyone with a GitHub account may do.
    ///
    /// **It was called `pr_files` for one commit, and the name was the defect.** A caller's whole
    /// interface to this operation is the kind string — the model reading the tool description never
    /// sees this paragraph — and `pr_files` promises a list of names. What arrives is unbounded
    /// attacker-authored text, clipped to the LAST 256 KiB by `clip`, so a hostile pull request can
    /// sit its payload at the tail and push everything before it out of the window. A name that
    /// under-describes its own blast radius is worse here than anywhere else in this enum, because
    /// the thing it under-describes is the injection channel.
    PrDiff {
        repo: Repo,
        number: PrNumber,
    },
    /// The comment thread on a pull request. `PrView` returns the description its author wrote; this
    /// returns everything everybody else wrote underneath it, which is a wider door into the turn
    /// and not a narrower one.
    ///
    /// **`pr_thread` and not `pr_comments`, and the reason is a refusal message rather than a
    /// compare.** Every kind comparison in this module is `==`, so the plural was mechanically safe
    /// beside `ActOp::PrComment`; what it was not safe from is a person. `POST
    /// /projects/{id}/github-ops` refuses an undeclarable kind by listing the declarable ones, so
    /// somebody reaching for this READ was refused and handed a list whose nearest entry was
    /// `pr_comment` — the operation that POSTS under the owner's name, and that runs without asking
    /// the moment it is declared. A refusal that nudges across the partition is worse than no
    /// refusal, and the fix belongs in the name and not in the message.
    PrThread {
        repo: Repo,
        number: PrNumber,
    },
    /// The checks reported against a ref, as `gh pr checks` gives them: a name, a state and a link
    /// per check.
    ///
    /// `Branch` for the ref, the same type and the same field spelling `ActOp::WorkflowRun` uses —
    /// this is `gh`'s argv either way, and a second type for the same guard is how the two come to
    /// disagree.
    ChecksForRef {
        repo: Repo,
        r#ref: Branch,
    },
}

/// An operation that changes something on GitHub's servers.
///
/// Every variant is `Acts`, and that is not a coincidence to be checked but the definition of this
/// half. `github_act` takes this type and nothing else, so the barrier meets it by name on both the
/// local path and the cloud one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ActOp {
    WorkflowRun {
        repo: Repo,
        workflow: WorkflowName,
        r#ref: Branch,
    },
    RunRerun {
        repo: Repo,
        id: RunId,
    },
    PrCreate {
        repo: Repo,
        title: Title,
        body: Body,
        base: Branch,
        head: Branch,
    },
    PrComment {
        repo: Repo,
        number: PrNumber,
        body: Body,
    },
    IssueClose {
        repo: Repo,
        number: IssueNumber,
    },
    /// `gh api`. It exists because the capability is total; it is **never autonomous**, and
    /// `ACTION_CEILING` does not contain it, so not even the owner's file can turn it on.
    ///
    /// `Vec<String>` and not a line: see the module doc.
    ///
    /// **The name is what `--` already did, moved out of this comment and into the type.** Every
    /// caller argument lands after the terminator, so this reaches `gh api` with a path and
    /// positional arguments and no flags — `-X DELETE` arrives as two more positionals and does not
    /// select a method. That is a real narrowing of "total capability" down to reads of the REST
    /// surface, and it is the deliberate reading of the rule that no caller value may ever act as a
    /// flag. Whoever wants the verb wants a typed variant for it, which is the same answer the rest
    /// of this enum gives.
    ///
    /// It was called `Raw` while that narrowing lived only in this comment. `Raw` promises an escape
    /// hatch for anything, which is the opposite of what the caller gets, and a name that
    /// contradicts its own doc is read far more often than the doc is. Renamed while the wire string
    /// had never reached the database — the only moment such a rename costs nothing.
    ///
    /// **A read, and still an `ActOp`, which is not a contradiction.** The partition is not about
    /// what an operation does to GitHub but about what the núcleo can VOUCH for: `ReadOp` is the
    /// closed set whose every argv this module built and can name. An arbitrary REST path is not in
    /// that set, so it stays `Acts` — approval-bearing, and refused after untrusted text — and being
    /// a GET does not earn it the other half's guarantees.
    ApiRead {
        args: Vec<String>,
    },
}

/// The union `execute` consumes.
///
/// Untagged because both halves are already internally tagged on `op` and their kinds are disjoint
/// — `read_ops_and_act_ops_partition_op` is what keeps that true.
///
/// **A caller below the tool boundary inherits no type protection from the partition.** `execute`
/// takes this union by design, so a second caller — the internal trigger of the data-flow's third
/// entry — has to justify its own authorization rather than borrow `github_read`'s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Op {
    Read(ReadOp),
    Act(ActOp),
}

impl ReadOp {
    pub fn kind(&self) -> &'static str {
        match self {
            ReadOp::RunList { .. } => "run_list",
            ReadOp::RunStatus { .. } => "run_status",
            ReadOp::PrList { .. } => "pr_list",
            ReadOp::WorkflowList { .. } => "workflow_list",
            ReadOp::PrView { .. } => "pr_view",
            ReadOp::IssueView { .. } => "issue_view",
            ReadOp::RunLogs { .. } => "run_logs",
            ReadOp::PrDiff { .. } => "pr_diff",
            ReadOp::PrThread { .. } => "pr_thread",
            ReadOp::ChecksForRef { .. } => "checks_for_ref",
        }
    }

    /// What reading this does to the turn.
    ///
    /// Structure — a status, a conclusion, a list of workflow names — is `ReadsOwn`. Prose somebody
    /// wrote is `ReadsUntrusted`, and it burns the turn's right to act, which is the trade the
    /// design accepts on purpose.
    ///
    /// "Structure" and "prose" are the shorthand and not the rule. Three of the four `ReadsOwn`
    /// reads do carry stranger-chosen words; what keeps them on that side is how few and how
    /// short-shaped, and the first comment in the body is where that is set out. Grade a new
    /// variant against that comment, never against this sentence.
    pub fn effect(&self) -> ToolEffect {
        match self {
            // **`ReadsOwn` here does NOT mean "no stranger wrote any of this".** Saying it did would
            // be the most convenient rule to state and it is false, so it is worth killing before
            // somebody grades a seventh read by it. `gh pr list` returns titles, author logins and
            // head branch names chosen by anybody with a GitHub account; `gh run list` and `gh run
            // view` show a run's display title, which for a `pull_request`-triggered run IS the pull
            // request's title. Three of these four already carry a stranger's words. This same file
            // says so 800 lines down, in the sentence that refuses `--limit`: `gh pr list` is called
            // "an injection channel" there, in those words.
            //
            // What actually separates the two arms is how much of a stranger's text can arrive and
            // in what shape. `ReadsOwn` is a deliberate, narrow tolerance: a stranger reaches these
            // four only through short fixed-shape fields — one title, one login, one branch name per
            // row — in a listing whose row count `gh` caps at thirty and which `REFUSED_READ_FLAGS`
            // refuses to let a caller raise. That cap is not a performance detail, it is half of
            // this grading, which is why `--limit` and its `-L` spelling are refused rather than
            // capped. `WorkflowList` is the one with no tolerance to spend: workflow files live on
            // the repository's own branches, so it is committer-authored outright.
            ReadOp::RunList { .. }
            | ReadOp::RunStatus { .. }
            | ReadOp::PrList { .. }
            | ReadOp::WorkflowList { .. } => ToolEffect::ReadsOwn,
            // And the arm below is where that tolerance is gone. Not because the output is long, or
            // free-form, or unparsed — `RunList` is all three — but because a stranger authors it
            // WHOLE and at a length nothing bounds: a patch, a thread, a check's summary. There is
            // no fixed-shape field to point at and no row cap to lean on, so the only honest answer
            // is that the turn has read somebody else's words.
            //
            // `ChecksForRef` is the one that looks structural and is not, and the mechanism is worth
            // getting right because the obvious version of it is wrong. It is NOT that a fork's
            // workflow file runs: for `pull_request` events GitHub takes the workflow from the BASE
            // ref, which is the whole point of that event. It is that a check's name, its summary
            // and its details URL are free text written by whatever produced the check — any GitHub
            // App holding `checks:write` on the repository, and any workflow running on a same-repo
            // pull request branch. The states are GitHub's; the prose beside them is not.
            //
            // What the grading costs, so that it is chosen and not stumbled into: the turn is
            // latched. `effect_of_call` reads this through `effect_of_kind`, the answer comes back
            // fenced by `fence_untrusted`, and `permitted_after_untrusted` refuses every `Acts` tool
            // for the rest of the turn — `github_act` included, and on the cloud path as well as the
            // local one. A run that reads the diff cannot then comment on the pull request. That is
            // the trade, and for text a stranger chose it is the right way round.
            ReadOp::PrView { .. }
            | ReadOp::IssueView { .. }
            | ReadOp::RunLogs { .. }
            | ReadOp::PrDiff { .. }
            | ReadOp::PrThread { .. }
            | ReadOp::ChecksForRef { .. } => ToolEffect::ReadsUntrusted,
        }
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            ReadOp::RunList { repo } => argv(&["run", "list"], [repo_flag(repo)], &[]),
            ReadOp::RunStatus { repo, id } => {
                argv(&["run", "view"], [repo_flag(repo)], &[id.as_str()])
            }
            ReadOp::PrList { repo } => argv(&["pr", "list"], [repo_flag(repo)], &[]),
            ReadOp::WorkflowList { repo } => argv(&["workflow", "list"], [repo_flag(repo)], &[]),
            ReadOp::PrView { repo, number } => {
                argv(&["pr", "view"], [repo_flag(repo)], &[number.as_str()])
            }
            ReadOp::IssueView { repo, number } => {
                argv(&["issue", "view"], [repo_flag(repo)], &[number.as_str()])
            }
            ReadOp::RunLogs { repo, id } => argv(
                &["run", "view"],
                [repo_flag(repo), "--log".to_owned()],
                &[id.as_str()],
            ),
            ReadOp::PrDiff { repo, number } => {
                argv(&["pr", "diff"], [repo_flag(repo)], &[number.as_str()])
            }
            // `--comments` is a boolean this module writes, like `--log` above: it carries no caller
            // value, so it may be a bare flag ahead of the terminator.
            ReadOp::PrThread { repo, number } => argv(
                &["pr", "view"],
                [repo_flag(repo), "--comments".to_owned()],
                &[number.as_str()],
            ),
            ReadOp::ChecksForRef { repo, r#ref } => {
                argv(&["pr", "checks"], [repo_flag(repo)], &[r#ref.as_str()])
            }
        }
    }

    /// One instance of every variant, for the tests that hold this enum to the tables around it.
    ///
    /// A function and not an associated `const`, and the reason is mechanical rather than a
    /// preference: these variants carry `String`s, and a `const ALL: &[ReadOp]` would ask the
    /// compiler to promote values that implement `Drop` to `'static`, which it will not do.
    ///
    /// **The exhaustive `match` below is what makes forgetting a new variant hurt.** Adding one to
    /// the enum stops that match compiling, which puts the author on the line above the list they
    /// also have to extend. It is a nudge and not a proof — Rust has no variant reflection without
    /// a derive macro, and buying one for this would be a dependency for a test.
    pub fn all() -> Vec<Self> {
        let repo = Repo::new("owner/name").expect("the sample repository is valid");
        let every = vec![
            ReadOp::RunList { repo: repo.clone() },
            ReadOp::RunStatus {
                repo: repo.clone(),
                id: RunId::new("1").expect("the sample run id is valid"),
            },
            ReadOp::PrList { repo: repo.clone() },
            ReadOp::WorkflowList { repo: repo.clone() },
            ReadOp::PrView {
                repo: repo.clone(),
                number: PrNumber::new("1").expect("the sample pr number is valid"),
            },
            ReadOp::IssueView {
                repo: repo.clone(),
                number: IssueNumber::new("1").expect("the sample issue number is valid"),
            },
            ReadOp::RunLogs {
                repo: repo.clone(),
                id: RunId::new("1").expect("the sample run id is valid"),
            },
            ReadOp::PrDiff {
                repo: repo.clone(),
                number: PrNumber::new("1").expect("the sample pr number is valid"),
            },
            ReadOp::PrThread {
                repo: repo.clone(),
                number: PrNumber::new("1").expect("the sample pr number is valid"),
            },
            ReadOp::ChecksForRef {
                repo,
                r#ref: Branch::new("main").expect("the sample ref is valid"),
            },
        ];
        for operation in &every {
            match operation {
                ReadOp::RunList { .. }
                | ReadOp::RunStatus { .. }
                | ReadOp::PrList { .. }
                | ReadOp::WorkflowList { .. }
                | ReadOp::PrView { .. }
                | ReadOp::IssueView { .. }
                | ReadOp::RunLogs { .. }
                | ReadOp::PrDiff { .. }
                | ReadOp::PrThread { .. }
                | ReadOp::ChecksForRef { .. } => {}
            }
        }
        every
    }
}

impl ActOp {
    pub fn kind(&self) -> &'static str {
        match self {
            ActOp::WorkflowRun { .. } => "workflow_run",
            ActOp::RunRerun { .. } => "run_rerun",
            ActOp::PrCreate { .. } => "pr_create",
            ActOp::PrComment { .. } => "pr_comment",
            ActOp::IssueClose { .. } => "issue_close",
            ActOp::ApiRead { .. } => "api_read",
        }
    }

    /// Always `Acts`, and the constant answer is the point: this half is defined by acting, so a
    /// variant here can never be graded down into something the untrusted barrier lets through.
    ///
    /// Nothing in production calls it, and that is the correct shape rather than an omission —
    /// `github_act` is `Acts` in `TOOL_EFFECTS` by NAME, which is what the barrier reads. It exists
    /// so `every_operation_declares_an_effect` can hold the enum to that claim instead of the claim
    /// living only in a table.
    #[allow(dead_code)]
    pub fn effect(&self) -> ToolEffect {
        ToolEffect::Acts
    }

    /// The repository this touches, for the sentence a person reads before approving.
    ///
    /// `None` for `ApiRead`, and that is the honest answer rather than a gap: `gh api` names a REST path
    /// and a path is not a repository, however often it happens to contain one.
    pub fn repo(&self) -> Option<&Repo> {
        match self {
            ActOp::WorkflowRun { repo, .. }
            | ActOp::RunRerun { repo, .. }
            | ActOp::PrCreate { repo, .. }
            | ActOp::PrComment { repo, .. }
            | ActOp::IssueClose { repo, .. } => Some(repo),
            ActOp::ApiRead { .. } => None,
        }
    }

    /// One line naming what is being asked for, which is what the approvals list renders.
    ///
    /// A queue whose every row said "github-action" would make a person open each one to find out
    /// what they were agreeing to — the failure `create_team_action_in_transaction` already names
    /// about its own `tool_name` column.
    pub fn describe(&self) -> String {
        match self.repo() {
            Some(repo) => format!(
                "a run asked GitHub for {} on {}",
                self.kind(),
                repo.as_str()
            ),
            None => format!("a run asked GitHub for {}", self.kind()),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            // The workflow name goes AFTER `--`, so the `--ref` flag has to be built in the
            // `--flag=value` spelling ahead of it. A `["--ref", value]` pair would be swallowed as
            // two positionals the moment the terminator moved.
            ActOp::WorkflowRun {
                repo,
                workflow,
                r#ref,
            } => argv(
                &["workflow", "run"],
                [repo_flag(repo), format!("--ref={}", r#ref.as_str())],
                &[workflow.as_str()],
            ),
            ActOp::RunRerun { repo, id } => {
                argv(&["run", "rerun"], [repo_flag(repo)], &[id.as_str()])
            }
            // No positional at all, so no terminator: every caller value is inside a `--flag=value`,
            // where pflag reads the whole remainder as the value and it cannot become an option.
            ActOp::PrCreate {
                repo,
                title,
                body,
                base,
                head,
            } => argv(
                &["pr", "create"],
                [
                    repo_flag(repo),
                    format!("--title={}", title.as_str()),
                    format!("--body={}", body.as_str()),
                    format!("--base={}", base.as_str()),
                    format!("--head={}", head.as_str()),
                ],
                &[],
            ),
            ActOp::PrComment { repo, number, body } => argv(
                &["pr", "comment"],
                [repo_flag(repo), format!("--body={}", body.as_str())],
                &[number.as_str()],
            ),
            ActOp::IssueClose { repo, number } => {
                argv(&["issue", "close"], [repo_flag(repo)], &[number.as_str()])
            }
            ActOp::ApiRead { args } => {
                let mut built = vec!["api".to_owned(), "--".to_owned()];
                built.extend(args.iter().cloned());
                built
            }
        }
    }

    /// One instance of every variant. Same shape and same reasoning as `ReadOp::all`.
    pub fn all() -> Vec<Self> {
        let repo = Repo::new("owner/name").expect("the sample repository is valid");
        let branch = Branch::new("main").expect("the sample branch is valid");
        let body = Body::new("body").expect("the sample body is valid");
        let every = vec![
            ActOp::WorkflowRun {
                repo: repo.clone(),
                workflow: WorkflowName::new("ci.yml").expect("the sample workflow is valid"),
                r#ref: branch.clone(),
            },
            ActOp::RunRerun {
                repo: repo.clone(),
                id: RunId::new("1").expect("the sample run id is valid"),
            },
            ActOp::PrCreate {
                repo: repo.clone(),
                title: Title::new("title").expect("the sample title is valid"),
                body: body.clone(),
                base: branch.clone(),
                head: branch,
            },
            ActOp::PrComment {
                repo: repo.clone(),
                number: PrNumber::new("1").expect("the sample pr number is valid"),
                body,
            },
            ActOp::IssueClose {
                repo,
                number: IssueNumber::new("1").expect("the sample issue number is valid"),
            },
            ActOp::ApiRead { args: Vec::new() },
        ];
        for operation in &every {
            match operation {
                ActOp::WorkflowRun { .. }
                | ActOp::RunRerun { .. }
                | ActOp::PrCreate { .. }
                | ActOp::PrComment { .. }
                | ActOp::IssueClose { .. }
                | ActOp::ApiRead { .. } => {}
            }
        }
        every
    }
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Read(operation) => operation.kind(),
            Op::Act(operation) => operation.kind(),
        }
    }

    // There is deliberately NO `Op::effect()`. The union's effect is never the question anyone
    // should be asking: the untrusted barrier asks `tool_effect` BY NAME, and the per-call answer is
    // `effect_of_call` reading which READ was named. An accessor on the union would be the one shape
    // that reads like an answer to both and is an answer to neither.

    pub fn argv(&self) -> Vec<String> {
        match self {
            Op::Read(operation) => operation.argv(),
            Op::Act(operation) => operation.argv(),
        }
    }

    /// Every operation: the union decision #4 puts one list of names over. `gh_forms` derives the
    /// map from this, and the partition test holds the two halves apart with it.
    ///
    /// It stopped being test-only when the map arrived, and it carried an `#[allow(dead_code)]` for
    /// one more commit because the bin build reached it only through `op_kind_of_gh_command`, which
    /// was itself waiting for its caller. That caller is `Policy::read_is_autonomous`, and it landed
    /// with `Policy::for_project`; the allow came off with the map's own, exactly as the sentence
    /// here promised it would.
    pub fn all() -> Vec<Self> {
        ReadOp::all()
            .into_iter()
            .map(Op::Read)
            .chain(ActOp::all().into_iter().map(Op::Act))
            .collect()
    }
}

/// The flat parameters a `github_read` tool call carries.
///
/// Flat rather than the tagged union `ReadOp` serialises to, for the reason `vcs::Op::from_request`
/// gives about its own: the caller on the other side is a language model reading a tool description,
/// and the union is the right wire shape and the wrong prompt. It is a convenience over the node
/// types, never a boundary — the boundary is the type, and it is still `Repo::new` that decides.
#[derive(Debug, Clone, Default)]
pub struct ReadRequest {
    pub operation: String,
    pub repo: String,
    /// A run id for `run_status` and `run_logs`, a pull request number for `pr_view`, `pr_diff` and
    /// `pr_thread`, an issue number for `issue_view`, a REF for `checks_for_ref`, and nothing at
    /// all for the three listings. One field rather than five, because the model reading this has to
    /// fill in one thing and choosing which name it is called by is not that thing.
    ///
    /// `checks_for_ref` is the arm where the name fits worst and the field still earns its keep: the
    /// alternative is a second optional string that is empty for every other operation, which is how
    /// a caller ends up filling in neither.
    pub id: Option<String>,
}

/// The flat parameters a `github_act` call carries. Wider than its reading sibling because the
/// operations are.
#[derive(Debug, Clone, Default)]
pub struct ActRequest {
    pub operation: String,
    pub repo: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub head: Option<String>,
    pub workflow: Option<String>,
    pub git_ref: Option<String>,
    pub args: Option<Vec<String>>,
}

/// PURE: names the field a caller left out, in the words the caller used for it.
fn required(value: Option<String>, operation: &str, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{operation} needs a {field}"))
}

impl ReadOp {
    /// Builds a read from the flat parameters, or says exactly which field is wrong.
    ///
    /// An operation this module does not know is refused differently from a field that is missing,
    /// and the difference is what the caller does next: told "unknown operation" it goes looking
    /// for a typo in its own request, told "pr_view needs an id" it sends the id.
    pub fn from_request(request: ReadRequest) -> Result<Self, String> {
        let ReadRequest {
            operation,
            repo,
            id,
        } = request;
        let operation = operation.trim().to_ascii_lowercase();
        let repo = Repo::new(&repo)?;
        match operation.as_str() {
            "run_list" => Ok(ReadOp::RunList { repo }),
            "pr_list" => Ok(ReadOp::PrList { repo }),
            "workflow_list" => Ok(ReadOp::WorkflowList { repo }),
            "run_status" => Ok(ReadOp::RunStatus {
                repo,
                id: RunId::new(&required(id, "run_status", "run id")?)?,
            }),
            "run_logs" => Ok(ReadOp::RunLogs {
                repo,
                id: RunId::new(&required(id, "run_logs", "run id")?)?,
            }),
            "pr_view" => Ok(ReadOp::PrView {
                repo,
                number: PrNumber::new(&required(id, "pr_view", "pull request number")?)?,
            }),
            "issue_view" => Ok(ReadOp::IssueView {
                repo,
                number: IssueNumber::new(&required(id, "issue_view", "issue number")?)?,
            }),
            "pr_diff" => Ok(ReadOp::PrDiff {
                repo,
                number: PrNumber::new(&required(id, "pr_diff", "pull request number")?)?,
            }),
            "pr_thread" => Ok(ReadOp::PrThread {
                repo,
                number: PrNumber::new(&required(id, "pr_thread", "pull request number")?)?,
            }),
            // The one operation whose `id` is not a number. The field is still `id`, because the
            // flat parameters exist to give the model ONE thing to fill in — the doc on that field
            // is where the difference is said, not in a second field nobody would know to use.
            "checks_for_ref" => Ok(ReadOp::ChecksForRef {
                repo,
                r#ref: Branch::new(&required(id, "checks_for_ref", "ref")?)?,
            }),
            // Every acting operation is named here rather than falling into the unknown arm, because
            // a caller told "unknown operation: pr_comment" would think it had misspelled something.
            // What is actually true is that it asked the wrong tool, and that is what it is told —
            // the type system already made this unreachable in Rust, and this is the same refusal
            // said in words on the wire.
            other if ActOp::all().iter().any(|op| op.kind() == other) => Err(format!(
                "{other} acts, so it belongs to github_act and not to github_read"
            )),
            other => Err(format!("unknown read operation: {other}")),
        }
    }

    /// The effect of a read named only by its `kind()`, for `mcp_tools::effect_of_call`, which has
    /// arguments rather than an operation.
    ///
    /// Derived from `all()` rather than written out a second time, so the answer here and the answer
    /// from `effect()` cannot drift. `None` is an operation this module does not know, and the
    /// caller turns that into `ReadsUntrusted` — "I could not tell" is not "no".
    pub fn effect_of_kind(kind: &str) -> Option<ToolEffect> {
        ReadOp::all()
            .into_iter()
            .find(|op| op.kind() == kind)
            .map(|op| op.effect())
    }
}

impl ActOp {
    /// Builds an action from the flat parameters. Same refusal discipline as its reading sibling.
    pub fn from_request(request: ActRequest) -> Result<Self, String> {
        let ActRequest {
            operation,
            repo,
            id,
            title,
            body,
            base,
            head,
            workflow,
            git_ref,
            args,
        } = request;
        let operation = operation.trim().to_ascii_lowercase();
        // `api_read` is settled before the repository is, because it is the one operation that names no
        // repository — asking for one first would refuse it for a field it does not have.
        if operation == "api_read" {
            let args = args.unwrap_or_default();
            if args.is_empty() {
                return Err("api_read needs at least an endpoint in args".to_owned());
            }
            return Ok(ActOp::ApiRead { args });
        }
        let repo = Repo::new(&required(repo, &operation, "repository")?)?;
        match operation.as_str() {
            "workflow_run" => Ok(ActOp::WorkflowRun {
                repo,
                workflow: WorkflowName::new(&required(workflow, "workflow_run", "workflow")?)?,
                r#ref: Branch::new(&required(git_ref, "workflow_run", "ref")?)?,
            }),
            "run_rerun" => Ok(ActOp::RunRerun {
                repo,
                id: RunId::new(&required(id, "run_rerun", "run id")?)?,
            }),
            "pr_create" => Ok(ActOp::PrCreate {
                repo,
                title: Title::new(&required(title, "pr_create", "title")?)?,
                body: Body::new(&required(body, "pr_create", "body")?)?,
                base: Branch::new(&required(base, "pr_create", "base branch")?)?,
                head: Branch::new(&required(head, "pr_create", "head branch")?)?,
            }),
            "pr_comment" => Ok(ActOp::PrComment {
                repo,
                number: PrNumber::new(&required(id, "pr_comment", "pull request number")?)?,
                body: Body::new(&required(body, "pr_comment", "body")?)?,
            }),
            "issue_close" => Ok(ActOp::IssueClose {
                repo,
                number: IssueNumber::new(&required(id, "issue_close", "issue number")?)?,
            }),
            other if ReadOp::all().iter().any(|op| op.kind() == other) => Err(format!(
                "{other} only reads, so it belongs to github_read and not to github_act"
            )),
            other => Err(format!("unknown operation: {other}")),
        }
    }
}

/// PURE: assembles a `gh` argv out of three parts that are each safe for a different reason.
///
/// `subcommand` is ours and is literal. `flags` are ours too, and every caller value inside one is
/// in the `--flag=value` spelling, where pflag reads the rest of the token as the value. `trailing`
/// is caller-chosen and positional, so it goes after `--`.
///
/// **`--` and not `--end-of-options`.** The obligation `vcs.rs` records is real and is paid here,
/// but `--end-of-options` is git's `parse-options` convention; `gh` is Cobra/pflag and its
/// terminator is `--`. Writing git's spelling would look like care and would be one more literal
/// argument `gh` hands to the subcommand.
fn argv<const F: usize>(subcommand: &[&str], flags: [String; F], trailing: &[&str]) -> Vec<String> {
    let mut built: Vec<String> = subcommand.iter().map(|part| (*part).to_owned()).collect();
    built.extend(flags);
    if !trailing.is_empty() {
        built.push("--".to_owned());
        built.extend(trailing.iter().map(|part| (*part).to_owned()));
    }
    built
}

fn repo_flag(repo: &Repo) -> String {
    format!("--repo={}", repo.as_str())
}

/// PURE: the whole argv rule for the names that reach a command line as positional words.
///
/// Shared as a function rather than by making one type serve two roles, for the reason `vcs.rs`
/// gives about the same split: a repository and a branch answer to different authorities and
/// nothing says they must stay checked for the same three properties.
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
        return Err(format!(
            "a {what} may not contain whitespace or control characters: {value}"
        ));
    }
    Ok(value.to_owned())
}

/// PURE: the rule for the identifiers GitHub numbers.
///
/// Digits only, which is stricter than "not a flag" and is the right strictness: these name rows on
/// GitHub's side, and a run id that is not a number is a request that was going to fail anyway —
/// refused here it names its own problem instead of arriving as the CLI's.
fn numeric(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("a {what} may not be empty"));
    }
    if !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(format!("a {what} must be a number: {value}"));
    }
    Ok(value.to_owned())
}

/// PURE: one line of text a person reads, with a ceiling GitHub also enforces.
///
/// Refusing here rather than letting the API refuse is worth the duplication: a 422 from `gh` in a
/// run's transcript reads like a broken pillar, and the same refusal at construction names the
/// field.
fn single_line(value: &str, what: &str, ceiling: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("a {what} may not be empty"));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(format!("a {what} may not contain control characters"));
    }
    if value.chars().count() > ceiling {
        return Err(format!(
            "a {what} may not be longer than {ceiling} characters"
        ));
    }
    Ok(value.to_owned())
}

/// A repository, as `owner/name`.
///
/// Its own type because every route into this module names one, and because `owner/name` is a shape
/// a plain string does not carry: told `--repo=name` the CLI would fall back to the checkout's own
/// remote, which is a different repository than the one the caller asked about and no error anywhere
/// would say so.
///
/// **An argv guard and a shape check, not an existence check.** A repository nobody has ever
/// created passes here; GitHub is the authority on which ones exist, and a request for one that
/// does not is a failed call carrying the CLI's own message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo(String);

impl Repo {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = argv_safe(value, "repository")?;
        match value.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self(value))
            }
            _ => Err(format!("a repository must be spelled owner/name: {value}")),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A branch name this module is willing to put on a `gh` command line.
///
/// Its own type rather than `vcs::Branch`, and deliberately so: that one is a guard for git's argv
/// and this one is a guard for `gh`'s. They happen to check the same three properties today and
/// nothing says they must stay that way — sharing the type would make "the queue accepts this
/// branch" and "GitHub accepts this branch" literally the same sentence.
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

/// A workflow, named as `gh workflow run` accepts one: a display name, a file name, or an id.
///
/// Whitespace is allowed, because a workflow called `CI Build` is an ordinary thing and this value
/// is one argv element rather than a line to be split. A leading `-` is refused anyway, and that
/// costs the theoretical workflow named `-x`. It is refused on purpose: the guarantee that no caller
/// value can act as an option should not depend on a `--` staying where somebody put it, which is
/// exactly the way `vcs.rs` watched a comment become wrong without anybody editing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowName(String);

impl WorkflowName {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = single_line(value, "workflow name", 255)?;
        if value.starts_with('-') {
            return Err(format!("a workflow name may not start with '-': {value}"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A pull request title. GitHub's own ceiling is 256 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Title(String);

impl Title {
    pub fn new(value: &str) -> Result<Self, String> {
        single_line(value, "title", 256).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The body of a pull request or a comment: many lines, and the only free text this module puts on
/// a command line.
///
/// Newlines and tabs pass and every other control character does not. It reaches argv only inside
/// `--body=<value>`, so its content cannot become an option however it is spelled. The ceiling is
/// GitHub's own for issue and pull-request bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Body(String);

impl Body {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("a body may not be empty".to_owned());
        }
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err("a body may not contain control characters".to_owned());
        }
        if value.chars().count() > 65_536 {
            return Err("a body may not be longer than 65536 characters".to_owned());
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A workflow run's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunId(String);

impl RunId {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "run id").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A pull request's number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrNumber(String);

impl PrNumber {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "pull request number").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An issue's number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueNumber(String);

impl IssueNumber {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "issue number").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validating `Deserialize` for every node type, and the reason is `vcs.rs`'s: the check has to hold
/// on every route in, and there are three — the flat parameters an MCP tool call carries, a raw
/// `POST /github/requests` body that deserializes an `Op` directly, and a proposal's payload read
/// back. A `Deserialize` that validates covers all three; a checked constructor covers the first
/// only, which is how `{"op":"pr_view","repo":"--upload-pack=x"}` would have got through.
macro_rules! validating_deserialize {
    ($($type:ty),+ $(,)?) => {
        $(
            impl<'de> Deserialize<'de> for $type {
                fn deserialize<D: serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    <$type>::new(&String::deserialize(deserializer)?)
                        .map_err(serde::de::Error::custom)
                }
            }
        )+
    };
}

validating_deserialize!(
    Repo,
    Branch,
    WorkflowName,
    Title,
    Body,
    RunId,
    PrNumber,
    IssueNumber,
);

/// Prefixes eligible for autonomy. **An allowlist**: what is not in it does not pass, which is why
/// `gh api` and `gh auth token` are excluded by absence rather than by a parallel table of
/// exclusions. Two structures deciding one question is how they come to disagree.
///
/// The reasons an allowlist cannot state itself:
///
/// - `gh api` deletes a repository with the same verb it reads an issue with. No prefix analysis
///   separates the two. The capability exists — it can always be asked for — and is never autonomous.
/// - `gh auth token`, `gh auth status`, `gh secret list`, `gh variable list` are *reads*, and they
///   are the worst kind: they print the credential, or its shape, into the agent's context. A read
///   that exfiltrates the credential the module acts with does not belong on a list of safe reads,
///   however much the word suggests otherwise.
///
/// **Only reads of STRUCTURAL shape.** `gh pr view` returns the body of a pull request and is not
/// here under any circumstance; it exists as a `ReadOp`, where the effect is tied to it. Were it
/// here, an agent that wanted a stranger's text and wanted to go on acting would simply use Bash,
/// and the per-operation effect would buy nothing.
pub const READ_CEILING: &[&str] = &[
    "gh run list",
    "gh run view",
    "gh pr list",
    "gh workflow list",
];

/// The `ActOp` kinds eligible for autonomy. `api_read` is outside it and stays outside.
///
/// **`pr_create` is inside, and it publishes to somebody else's server, which is worth saying
/// plainly rather than leaving to be noticed.** It puts a title and a body under the owner's name
/// where people will read them as the owner's words, and closing the pull request afterwards does
/// not unsend that. What makes it admissible anyway is the shape of what it creates: a pull request
/// is a PROPOSAL addressed to a human, and it merges nothing. The repository is in exactly the state
/// it was in a moment before, and the next step belongs to a reviewer who has to press something.
/// That is the same bargain `pr_comment` and `issue_close` already made — public, attributable,
/// bounded — and `pr_create` is not a larger one for being longer.
///
/// It was outside this list while the reasoning stopped at "publishes under the owner's name". That
/// sentence is true and is also true of `pr_comment`, which was inside; the ceiling was drawing a
/// line the two sides of which it could not tell apart.
///
/// `api_read` is refused by a different test and never by that one. It is not "an action that goes
/// further" — it is an arbitrary REST call, so its blast radius is not bounded by its name and no
/// paragraph here can describe what it does. `gh api` deletes a repository with the verb it reads an
/// issue with. Every other entry here names one operation whose worst case can be written down;
/// that is the property the list is selecting for, and `api_read` is the one variant that cannot
/// have it.
pub const ACTION_CEILING: &[&str] = &[
    "workflow_run",
    "run_rerun",
    "pr_create",
    "pr_comment",
    "issue_close",
];

/// Flags that take autonomy away from a prefix that had it.
///
/// A prefix cannot say "`gh run view` without `--log`", because the form that measures it does not
/// see flags — and without this list `gh pr list --json body` matches `gh pr list` and returns the
/// very payload `gh pr view` was excluded for. The ceiling is therefore two constants and not one:
/// an autonomous read must match a `READ_CEILING` prefix AND carry none of these.
///
/// The first seven change the KIND of thing that comes back, unlike `--state` or `--author`, which
/// only choose which. Two need their own reason:
///
/// - `--search` does not change the kind — it chooses which. It is here because a search *over
///   bodies* (`--search "in:body ..."`) returns the existence of the body's content by inference,
///   one answer at a time. It is the same leak, more slowly.
/// - `--limit` was once called harmless, and that was arithmetic rather than analysis: for an
///   injection channel, HOW MANY is the payload. `gh pr list --limit 1000` is a thousand
///   stranger-chosen titles in a call that marks nothing, against `gh`'s default of thirty.
///   Refusing the flag rather than capping it is deliberate — a cap would mean reading a flag's
///   VALUE, and this comparison reads tokens. **`ReadOp::effect` leans on this entry**: `PrList` is
///   `ReadsOwn` because a stranger reaches it through thirty short fields and no further, so the
///   day `--limit` stops being refused is the day that grading stops being true.
///
/// **Every entry has two spellings and the second one does not remember itself.** `-q`, `-t` and
/// `-L` are the short forms of `--jq`, `--template` and `--limit`, and `-L` was added a review after
/// `--limit`, forgotten in the very sentence that exists to say short forms are not forgotten. That
/// is this list's failure mode, recorded here rather than rediscovered.
pub const REFUSED_READ_FLAGS: &[&str] = &[
    "--json",
    "-q",
    "--jq",
    "-t",
    "--template",
    "--log",
    "--log-failed",
    "--search",
    "-L",
    "--limit",
];

/// One row of the map below: what a `gh` command line has to say in order to BE one operation.
///
/// Neither field is written here. Both are read off the operation's own `argv()` — see `gh_forms`.
#[derive(Debug)]
struct GhForm {
    kind: &'static str,
    /// The literal subcommand words, in order: `["run", "view"]`. This module's, never a caller's.
    subcommand: Vec<String>,
    /// The valueless flags this module writes for THIS operation and not for the one it shares a
    /// subcommand with — the `--log` that makes `gh run view` mean `run_logs`. Empty for most rows.
    switches: Vec<String>,
}

impl GhForm {
    /// PURE: whether the words AFTER `gh` ask for this operation.
    ///
    /// The subcommand is compared case-insensitively and the switches are not. That is not an
    /// inconsistency but the same split `read_is_autonomous` already makes, for the reason it gives:
    /// it lowercases before comparing a prefix and reads RAW tokens for the flags, because `-L` is
    /// `gh`'s short `--limit` while `-l` is its short `--label`. Two functions asked to agree about
    /// one command line had better fold case in the same places.
    ///
    /// **A switch this row does not name does not stop the row from matching.** `gh pr list --state
    /// open` is `pr_list` here, and so is `gh pr list --json body`: this step asks only which
    /// operation the words describe. Whether that naming SURVIVES a flag which changes what comes
    /// back is a second question, and `op_kind_of_gh_command` is where it is asked.
    fn matches(&self, arguments: &[String]) -> bool {
        let Some(rest) = arguments.get(self.subcommand.len()..) else {
            return false;
        };
        if !self
            .subcommand
            .iter()
            .zip(arguments)
            .all(|(part, word)| word.eq_ignore_ascii_case(part))
        {
            return false;
        }
        // Everything past `--` is a positional, so a `--log` sitting there is a run id and not a
        // flag. `read_is_autonomous` deliberately does NOT stop at the terminator — for a REFUSAL,
        // reading the whole line is the conservative direction — and the two are the right way
        // round: a refusal should err towards no, and this should err towards the truth about which
        // operation was asked for.
        let flags: Vec<&String> = rest
            .iter()
            .take_while(|word| word.as_str() != "--")
            .collect();
        self.switches
            .iter()
            .all(|switch| flags.iter().any(|word| word_is_flag(word, switch)))
    }
}

/// PURE: the map, derived from the operations instead of written out beside them.
///
/// **Two rules turn an argv into a form, and each is an invariant this file already tests.** A word
/// before the terminator that does not begin with `-` is a subcommand word, because `argv` builds
/// every argv as subcommand, then flags, then `--`, then positionals — which
/// `the_terminator_appears_exactly_where_a_positional_does` holds it to. A flag token carrying no
/// `=` is one of this module's own booleans and therefore says WHICH operation this is; a
/// `--flag=value` carries a caller's string and says nothing, because every operation's `--repo=`
/// looks alike. That second rule is the one sentence of this map not read off the data, and
/// `no_caller_value_reaches_argv_where_it_could_act_as_a_flag` is what keeps it true — that test
/// even spells out the same two booleans, as the list this function computes instead.
///
/// **Built once.** Nothing calls it yet, but its first caller is the classifier's path, which runs
/// for every Bash tool call a run makes — and rebuilding sixteen argvs per command line would be a
/// cost with nothing bought by it. The function is pure and the operations do not change while the
/// process lives, which is the whole precondition a `OnceLock` needs.
fn gh_forms() -> &'static [GhForm] {
    static FORMS: std::sync::OnceLock<Vec<GhForm>> = std::sync::OnceLock::new();
    FORMS.get_or_init(|| {
        Op::all()
            .iter()
            .map(|op| {
                let mut subcommand = Vec::new();
                let mut switches = Vec::new();
                for word in op.argv() {
                    if word == "--" {
                        break;
                    }
                    match (word.starts_with('-'), word.contains('=')) {
                        (true, false) => switches.push(word),
                        (true, true) => {}
                        (false, _) => subcommand.push(word),
                    }
                }
                GhForm {
                    kind: op.kind(),
                    subcommand,
                    switches,
                }
            })
            .collect()
    })
}

/// PURE: which operation a `gh` command line asks for, or `None` for a line no operation builds.
///
/// **This is the map decision #4 moves into the code** —
/// `.ai/specs/2026-09-03-alcada-por-projecto-design.md`, §1.2 for the defect and §2 for the
/// decision. The pillar has two doors and each was governed in its own vocabulary: `ACTION_CEILING`
/// and `action_is_autonomous` speak operation KINDS, `READ_CEILING` and `read_is_autonomous` speak
/// `gh` PREFIXES. Two vocabularies for one question is how the two doors came to answer it
/// differently, and the design measured what that costs an owner: a `.ai/github.yaml` without `gh
/// run view` stops an agent that types it into Bash and hands the same agent the same bytes through
/// `github_read {op: run_status}`. One list of NAMES can govern both doors only if something can say
/// which name a command line spells. This says it.
///
/// **It is not a prefix table, and `gh run view` is the reason.** That one subcommand serves two
/// operations this module grades apart — `run_status` is `ReadsOwn`, `run_logs` is `ReadsUntrusted`
/// — and what separates them is a flag, `--log`, which is why the flag sits in `REFUSED_READ_FLAGS`
/// and why `declarable_github_ops` refuses `run_logs` while admitting the prefix it shares. `gh pr
/// view` is the same shape a second time, `pr_view` against `pr_thread`, separated by `--comments`.
/// Two of them is what makes the pair a rule rather than an exception, and a prefix table would have
/// to pick one of each pair and be wrong about the other.
///
/// **And `gh run view` serves a THIRD spelling, which is why the derivation alone is not enough.**
/// `--log-failed` returns the log of a failed step — a stranger's words — and no `ReadOp` builds it,
/// so no form names it and nothing in `gh_forms` can see it. Left there, this map would answer
/// `run_status` to a line that returns log text: a `ReadsOwn` operation, inside `READ_CEILING`, and
/// one a project may declare. The guard below is what closes that, and it reads
/// `REFUSED_READ_FLAGS` rather than growing a list of its own — that constant is already the
/// module's curated answer to "which flags change the KIND of thing that comes back", in its own
/// words, and a second list would be a second thing to keep in step.
///
/// **An allowlist, and by construction rather than by a second table.** `ReadOp` and `ActOp` are
/// closed sets, so `gh auth token` and `gh secret list` fall outside this map the way they fall
/// outside `READ_CEILING` — by absence, with nothing here to keep in step with a list of exclusions.
/// It is the argument `READ_CEILING`'s own doc makes for itself, inherited.
///
/// **`Policy::read_is_autonomous` is the caller**, and it asks this only after its own refused-flag
/// check has already run. That order is not incidental: a name from this map is not a grant, and the
/// guard below is narrower than the constant it reads — see that function's doc for the half of
/// `REFUSED_READ_FLAGS` this one deliberately does not cover.
pub fn op_kind_of_gh_command(command: &str) -> Option<&'static str> {
    let words = crate::classifier::shell_words(command);
    let (program, arguments) = words.split_first()?;
    if !program.eq_ignore_ascii_case("gh") {
        return None;
    }
    let mut matched: Vec<&GhForm> = gh_forms()
        .iter()
        .filter(|form| form.matches(arguments))
        .collect();
    // Longest subcommand first, then the most switches accounted for: `gh run view --log 1` matches
    // both rows of the pair, and the row that named `--log` is the row that meant it.
    matched.sort_by_key(|form| (form.subcommand.len(), form.switches.len()));
    let best = matched.pop()?;
    let rank = (best.subcommand.len(), best.switches.len());
    if matched
        .last()
        .is_some_and(|next| (next.subcommand.len(), next.switches.len()) == rank)
    {
        // Two operations one line could equally be. There are none today — that is what
        // `no_two_operations_wear_the_same_gh_form` says — and on the day there is one, a guess is a
        // worse answer than no answer: whoever calls this map is deciding autonomy with it.
        return None;
    }
    // The same rule one place further: a flag that changes WHAT COMES BACK changes which operation
    // this is, and a line carrying one the winning form does not itself name is a line this map
    // cannot honestly name either. `--log-failed` is the case that needs it and the case the
    // derivation could never find — see the third paragraph above.
    //
    // The form's OWN switches are exempt, and that exemption is what keeps `run_logs` nameable by
    // the very flag that defines it: `--log` is in `REFUSED_READ_FLAGS` because a PREFIX could not
    // say "`gh run view` without it", which is the problem a name does not have.
    //
    // Scanned across the whole line, terminator and all, where `matches` deliberately stops short of
    // it. The two directions are each right for their own question: naming an operation should err
    // towards the truth about which one it is, and refusing to name one should err towards no.
    let carries_a_flag_of_another_operation = REFUSED_READ_FLAGS.iter().any(|flag| {
        !best.switches.iter().any(|switch| switch == flag)
            && arguments.iter().any(|word| word_is_flag(word, flag))
    });
    if carries_a_flag_of_another_operation {
        return None;
    }
    Some(best.kind)
}

/// PURE: whether this word is `flag`, in either spelling pflag accepts for it.
///
/// One function because it is one rule, and it is held in two places that have to agree about the
/// same command line: `read_is_autonomous` reads it to take autonomy away and `GhForm` reads it to
/// tell two operations apart. Equality alone would let `--json=body` through, which is one character
/// of difference between an implementation that works and one that looks like it does — and a second
/// copy of that character is exactly how the two would come to disagree.
fn word_is_flag(word: &str, flag: &str) -> bool {
    word == flag || word.starts_with(&format!("{flag}="))
}

// **Where the map is going: `gh` in Bash stops being autonomous, and `REFUSED_READ_FLAGS` goes with
// it.** Decision #6 of `.ai/specs/2026-09-03-alcada-por-projecto-design.md`, written down here as
// the destination and deliberately not taken.
//
// **It is not a refusal.** A `gh` line an agent needs can still be ASKED for: it becomes a
// `pending_approval` like every other command this house has no opinion about, and a person
// answers. What ends is the line running with nobody asked, on the strength of a prefix.
//
// **`REFUSED_READ_FLAGS` ends with it, because a typed operation has no flags.** That constant
// exists for exactly one reason, and its own doc says so: a prefix cannot spell "`gh run view`
// without `--log`", so the flags had to be refused beside the prefix. A NAME spells it —
// `run_status` and `run_logs` are two entries and the caller picks one — and there is no `--json`
// for a caller to reach for, because `ReadRequest` has three fields and not one of them is a flag.
// The paragraph in that constant about short forms that do not remember themselves is a paragraph
// about a failure mode the typed door does not have.
//
// **What has to be true first**, so that whoever reads this can tell whether the moment has arrived
// instead of deciding that it has:
//
// - **The typed read door has to consult the list.** Today it does not — `submit` runs every read
//   without asking anything, which its own doc states on purpose — so closing Bash now would WIDEN
//   autonomy rather than narrow it: everything Bash refuses would simply be typed instead, and the
//   owner's file would go from half an effect to none. This is the first condition and not one
//   among several.
// - **The catalogue has to cover what people actually type.** Not "cover `READ_CEILING`" — it
//   already does, and `every_read_ceiling_prefix_is_built_by_a_real_operation` is what says so. The
//   evidence lives on the other side: runs that stop and wait for a person on a `gh` line
//   `op_kind_of_gh_command` cannot name. While those keep arriving the catalogue is short, and the
//   answer is one more `ReadOp` — the way `WorkflowList`, `PrDiff`, `PrThread` and `ChecksForRef`
//   each were, one measured gap at a time.
// - **The tests above have to be measuring something other than themselves.**
//   `one_name_picks_out_the_same_operation_at_both_doors` feeds the map the lines the operations
//   build, so it can never discover a line nobody typed. It proves the map is faithful to the
//   catalogue and says nothing about whether the catalogue is wide enough, and it is that second
//   question this step turns on. A green suite is not the signal.
//
// **And until that day, a name from the map is not a grant.** The paragraph above about
// `REFUSED_READ_FLAGS` ending is true of the DESTINATION and false of every state before it.
// `Policy::for_project` consumes the map while the Bash door is still open, and in that interim a
// caller still holds a `gh` line with flags on it — `read_is_autonomous`'s flag check runs AFTER the
// map and not instead of it. The map's own refused-flag guard covers the flags that change which
// operation a line is; the rest of that constant, `--limit` above all, is a bound on HOW MUCH a
// stranger gets to say, and `ReadOp::effect` leans on it by name. Whoever reads this comment while
// implementing the interim step is the person likeliest to delete the only guard there is.
//
// The day all three hold, the change is small and almost entirely deletion: `READ_CEILING` and
// `REFUSED_READ_FLAGS` go, `read_is_autonomous` goes with them, and `classifier.rs::Segment::
// GithubRead` has nothing left to classify. **The map above goes too**, and that is not a loss but
// the shape of the thing: it exists to read a `gh` command line, and after this step no `gh`
// command line is being read for autonomy — which is also why its refused-flag guard may not
// outlive the constant it consults. That the step is a deletion is the point. It is the proof the
// two vocabularies really did become one, and until then this comment is a plan and not an
// achievement.

/// PURE: the widest policy this codebase can construct — both compiled ceilings, entire.
///
/// It exists so that "inside the ceiling" is asked in exactly the way production asks it, of the
/// same two functions, rather than by a second comparison somebody would have to keep in step.
fn ceiling_policy() -> Policy {
    Policy::from_config(&crate::config::GithubConfig {
        enabled: true,
        autonomous_reads: READ_CEILING
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
        autonomous_actions: ACTION_CEILING
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
    })
}

/// PURE: the READ operations a project may declare, by name.
///
/// **Derived, never written out.** The kinds come off `ReadOp::all()` through `kind()`, and
/// admissibility is asked of `ceiling_policy` — the same question production asks of the owner's
/// file. A hand-written list would be a second spelling of a set that already exists, and a second
/// spelling is how a set drifts.
///
/// Matched on the COMMAND each operation builds and not on its kind, because `READ_CEILING` is
/// written in `gh` prefixes and this half has to be asked in its own vocabulary. That is also what
/// excludes `run_logs`, whose argv carries `--log`: `REFUSED_READ_FLAGS` refuses it here exactly as
/// it refuses the same flag typed into Bash. `pr_view` is excluded the same way and just as
/// deliberately — `READ_CEILING`'s own doc refuses it in words.
///
/// Built once, for `gh_forms`' reason: its caller is the decision path.
fn declarable_read_ops() -> &'static [&'static str] {
    static KINDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        let ceiling = ceiling_policy();
        ReadOp::all()
            .iter()
            .filter(|operation| {
                ceiling.read_is_autonomous(&format!("gh {}", operation.argv().join(" ")))
            })
            .map(|operation| operation.kind())
            .collect()
    })
}

/// PURE: the ACTIONS a project may declare, by name.
///
/// `api_read` is outside `ACTION_CEILING`, so it is outside this, so a project cannot declare it —
/// the ceiling holding across a route that did not exist when it was written.
fn declarable_act_ops() -> &'static [&'static str] {
    static KINDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        let ceiling = ceiling_policy();
        ActOp::all()
            .iter()
            .filter(|operation| ceiling.action_is_autonomous(operation.kind()))
            .map(|operation| operation.kind())
            .collect()
    })
}

/// Every operation kind a project may declare, reads first and actions after.
///
/// One list, because decision #4 gives the owner one list of NAMES. Two halves underneath, because
/// decision #5 keeps the partition in the TYPES and each door may only ever be answered by its own
/// half: `Policy::for_project` narrows against this list and then splits on `declarable_read_ops`,
/// so both decisions are honoured, in that order, by construction rather than by a check anyone
/// could forget.
pub fn declarable_ops() -> Vec<&'static str> {
    declarable_read_ops()
        .iter()
        .chain(declarable_act_ops())
        .copied()
        .collect()
}

/// What runs without asking.
///
/// Built from `.ai/github.yaml` at startup as the MACHINE default, and immutable once built — it
/// does no I/O after construction, which is what lets `classifier::classify` take it by reference
/// and stay pure. There is deliberately no hot reload: a policy a run could reload is a policy a run
/// could change in the middle of itself.
///
/// **A project's own policy is a second value, not a mutation of this one.** `for_project` reads
/// `project_github_ops` and returns a NEW `Policy` with the project's operations laid over the
/// machine default; the value handed to a decision is built for that decision and thrown away
/// after. The immutability above survives intact — what changed is that there is now more than one
/// of these, and §4.4 of `.ai/specs/2026-09-03-alcada-por-projecto-design.md` is where that was
/// decided and where the "read at the moment of the decision, with no cache" rule is argued.
///
/// **Every list here is an intersection with a compiled ceiling, and never a union with one.**
/// Configuration chooses inside what the code fixes, and a project's table is configuration exactly
/// as the file is. `.ai/` is gitignored and travels with nobody; a row in a database travels with
/// nobody either. Neither may be the only thing between an autonomous run and `gh api -X DELETE`.
///
/// It answers WHETHER, never HOW: `execute` builds the argv and this type never sees one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The machine's autonomous reads, as `gh` PREFIXES, narrowed to `READ_CEILING`.
    reads: Vec<String>,
    /// This project's autonomous reads, as operation NAMES, narrowed to `declarable_read_ops`.
    ///
    /// A second field and not more entries in `reads`, because the two are different vocabularies
    /// answering the same question — which is §1.2's whole complaint — and a name is the one of the
    /// two that can tell `run_status` from `run_logs`. `op_kind_of_gh_command` is what turns a
    /// command line into something this list can be asked about.
    ///
    /// It holds only READS. An action's name never reaches it, so `gh pr comment` cannot come
    /// through the reading door on the strength of a project having declared `pr_comment` for the
    /// typed one.
    read_ops: Vec<String>,
    /// Autonomous actions, by kind, narrowed to `ACTION_CEILING`. The machine's and the project's
    /// in ONE list, because for actions the two vocabularies already agree: both are kinds.
    actions: Vec<String>,
    digest: String,
}

impl Policy {
    /// Autonomous in nothing — what an absent, unreadable or malformed file produces, and what every
    /// test that is not about the policy itself should be given.
    pub fn empty() -> Self {
        Self {
            reads: Vec::new(),
            read_ops: Vec::new(),
            actions: Vec::new(),
            digest: digest_of(&[], &[], &[]),
        }
    }

    /// Narrows the owner's two lists to the two ceilings.
    ///
    /// An entry outside a ceiling is **dropped with a warning**: neither ignored in silence (the
    /// owner would hold a policy other than the one they believe they wrote) nor fatal (a typo may
    /// not take the daemon down). The rest of the file stays valid, because one bad line is a
    /// mistake and not a reason to discard the good ones.
    ///
    /// `enabled: false` collapses both lists rather than being carried as a third state. What this
    /// type answers is "does it run without asking", and a pillar the owner switched off answers no
    /// to that in exactly the way an empty list does. A project's rows must not undo that, and they
    /// cannot: `GithubRuntime::policy_for_project` is the door the daemon uses and it is where the
    /// switch is read, because by the time a `Policy` exists the two states are the same value.
    ///
    /// **This is the MACHINE default and it is unchanged by the per-project work.** It declares no
    /// `read_ops` — that field is a project's alone — so a machine with no projects, or a run with
    /// no project, classifies exactly as it did before `for_project` existed.
    pub fn from_config(config: &crate::config::GithubConfig) -> Self {
        if !config.enabled {
            return Self::empty();
        }
        let reads = narrow(&config.autonomous_reads, READ_CEILING, "read");
        let actions = narrow(&config.autonomous_actions, ACTION_CEILING, "action");
        let digest = digest_of(&reads, &[], &actions);
        Self {
            reads,
            read_ops: Vec::new(),
            actions,
            digest,
        }
    }

    /// This project's policy: the machine default with the project's declared operations laid over
    /// it.
    ///
    /// **It ADDS to the machine default; it does not replace it**, and the argument is the table's
    /// own vocabulary. `project_github_ops` holds only operations somebody switched ON — there is no
    /// `deny` here and no room for one, because presence in the table IS the grant. A list that can
    /// only say yes cannot say "and take that other one away", so reading it as a REPLACEMENT would
    /// hand it a power it has no words for: withdrawing a machine-wide grant would happen as a side
    /// effect of declaring something unrelated, and the owner would have no way to write the
    /// opposite down. The shell rules can replace, and that is precisely why they carry a `deny`.
    ///
    /// The empty case then needs no special rule, which is the second half of the argument. Under a
    /// union, a project that declared nothing IS the machine default, and this returns a value equal
    /// to the one it was called on — the non-regression the shell-rules side answered with "empty
    /// means today's behaviour", arrived at here as a consequence instead of as an exception. Read
    /// as a replacement it would have to be an exception, and an exception at zero rows is a cliff
    /// at one: declare a single operation and every machine-wide grant would vanish unannounced.
    ///
    /// **Narrowed on the way out, though `POST /projects/{id}/github-ops` already validated on the
    /// way in.** Two writes the route never saw can reach that table: a row stored before a ceiling
    /// was narrowed, and a row written out of band. The shell-rules side made this argument for its
    /// own fold and it is the stronger one here, because the cost is not a dead rule but a live
    /// grant — `api_read` in a row would otherwise name `gh api -X DELETE` through the map and run
    /// it unasked. The read side is the side that decides, so the read side checks.
    ///
    /// A row outside the ceiling is dropped with a warning and the rest stay valid, which is
    /// `narrow`'s rule and §6 of the design restating it: one bad line is a mistake, not a reason to
    /// discard the good ones.
    ///
    /// **The acting half of the overlay reaches nothing yet, and that is worth knowing before
    /// reading it as live.** `action_is_autonomous` has exactly one production caller — `submit`,
    /// which reads `runtime.policy`, the machine default. `POST /github/requests` is the only way
    /// into `submit`, its body is `{op}` and nothing else, and `github_caller_is_allowed` admits only
    /// the control token and an Admin key — so there is no project on that path, and no run id to
    /// resolve one from either. Naming the project in the body would make it a CLAIM the caller
    /// makes about itself, which is the shape `pretooluse_decision` had to take apart: a caller free
    /// to name any project could borrow that project's grants. That is a decision the design did not
    /// take and this is not the place to take it.
    ///
    /// Merged anyway, and deliberately. The route accepts action kinds, the table stores them, and
    /// `digest` has to say what the effective policy IS rather than what happens to be consulted; on
    /// the day `submit` is handed a project, nothing here changes. Until then a project's declared
    /// ACTIONS are recorded and inert, while its declared READS are live through Bash.
    ///
    /// **One visible consequence of merging them, so nobody has to discover it from a graph.** A
    /// declared action moves the digest — `actions` is always hashed — while changing no verdict
    /// anywhere, so declaring `pr_create` fragments that project's shadow sample for a grant that
    /// does nothing yet. It is the honest answer rather than a bug: the configuration genuinely did
    /// change, and a digest that hid the change would be lying about which policy a row was recorded
    /// under the moment `submit` learns its project. The cost is bounded to
    /// `shadow_readiness` counting one more distinct digest for that project.
    ///
    /// **PRIVATE, and that is the `enabled` hole closed by construction rather than by comment.**
    /// `GithubRuntime::policy_for_project` is the only way in, and it is where the switch is read;
    /// a `pub` constructor here would let a caller outside this module lay a project's rows straight
    /// onto a policy that a switched-off pillar had already collapsed to `empty()`, which is the one
    /// thing `enabled: false` exists to prevent. The tests below still reach it, being in-module.
    async fn for_project(&self, pool: &sqlx::SqlitePool, project_id: &str) -> Self {
        // The SWALLOWING reader, and this is the consumer it was written for. An unreadable table
        // yields no operations, an empty overlay is the machine default, and that is strictly FEWER
        // operations running unasked — the safe direction here. It is the opposite of the shell
        // `deny`, where an empty list would lose a refusal somebody wrote down and the `Result`
        // reader is the only honest one; `project_policy` carries both halves for exactly this
        // reason, and picking the wrong one is how a database hiccup becomes a permission.
        let declared = crate::project_policy::github_ops(pool, project_id).await;
        // One narrowing against the whole declarable set, so a row that is in neither half warns
        // once and by name...
        let kept = narrow(&declared, &declarable_ops(), "project operation");
        // ...and then the partition decision #5 keeps in the types, applied here so that each door
        // holds only what it may answer for.
        //
        // **A `partition` and not two intersections, and that is only exact because of the line
        // above.** `declarable_ops` IS the two halves concatenated, and the halves are disjoint
        // because `ReadOp` and `ActOp` are — `read_ops_and_act_ops_partition_op` is the test that
        // says so — so after `narrow`, "not a read" and "an action" are the same set. Take `narrow`
        // away and they stop being: `api_read` would fall into the acting half and be autonomous
        // there. That is the one ceiling check, deliberately in one place, and this is the sentence
        // that says what depends on it.
        let (read_ops, acted): (Vec<String>, Vec<String>) = kept
            .into_iter()
            .partition(|kind| declarable_read_ops().contains(&kind.as_str()));
        let mut actions = self.actions.clone();
        actions.extend(acted);
        actions.sort();
        actions.dedup();
        let digest = digest_of(&self.reads, &read_ops, &actions);
        Self {
            reads: self.reads.clone(),
            read_ops,
            actions,
            digest,
        }
    }

    /// The effective lists, for the tests that assert what the ceilings let through. Production
    /// asks the two questions below instead, which is why these carry the allow.
    #[allow(dead_code)]
    pub fn autonomous_reads(&self) -> &[String] {
        &self.reads
    }

    #[allow(dead_code)]
    pub fn autonomous_actions(&self) -> &[String] {
        &self.actions
    }

    /// A short, stable fingerprint of the EFFECTIVE policy — already narrowed, sorted and
    /// deduplicated. `CLASSIFIER_VERSION` goes on meaning *the code*; this means *the configuration*.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Whether this shell command runs without asking.
    ///
    /// **The raw command, not a normalized one, and that is load-bearing.** `normalize_command`
    /// lowercases, and `-L` is `gh`'s short `--limit` while `-l` is its short `--label`: folded to
    /// one case the list would either miss the first or refuse the second. The same reason
    /// `lands_inside_the_workspace` reads raw tokens.
    ///
    /// Tokenized with `classifier::shell_words` — the tokenizer already in the house, rather than a
    /// second one that would drift from it. Quotes are stripped by it, so
    /// `--search 'in:body x'` is two tokens and the flag is seen.
    ///
    /// A refused flag is matched by `word_is_flag`, which is that rule and lives beside the map that
    /// also needs it — see its own doc for why one rule may not have two spellings here.
    ///
    /// **This decides the lists and the flags and nothing else.** Whether the line is a single
    /// command at all, whether it redirects, whether it hides a second command behind a separator —
    /// those are `classifier.rs`'s guards, applied before this is ever consulted, and this function
    /// would be wrong to be read as covering them.
    ///
    /// **Two doors, one flag check, and the ORDER is the whole safety argument.** The machine's list
    /// is asked by prefix and the project's by name, but `REFUSED_READ_FLAGS` runs before either and
    /// binds both. `op_kind_of_gh_command` has a refused-flag guard of its own and it is not this
    /// one: it covers the flags that change WHICH operation a line is, so that `--log-failed` cannot
    /// be named `run_status`. The rest of the constant — `--limit` above all — bounds HOW MUCH a
    /// stranger gets to say, and `ReadOp::effect` leans on it by name: `PrList` is graded `ReadsOwn`
    /// because a stranger reaches it through thirty short fields and no further. A name from the map
    /// is not a grant, and a mapped name allowed to skip this check would take the grading with it.
    pub fn read_is_autonomous(&self, command: &str) -> bool {
        if self.reads.is_empty() && self.read_ops.is_empty() {
            return false;
        }
        let words = crate::classifier::shell_words(command);
        if words.iter().any(|word| {
            REFUSED_READ_FLAGS
                .iter()
                .any(|flag| word_is_flag(word, flag))
        }) {
            return false;
        }
        let normalized = words.join(" ").to_ascii_lowercase();
        if self
            .reads
            .iter()
            .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix} ")))
        {
            return true;
        }
        // The named door. `read_ops` holds no action, so this cannot answer for one however the line
        // is spelled — see the field's own doc.
        //
        // Skipped outright when there is no name to ask about, which is EVERY machine policy — and
        // that is a cost and not only a tidiness. This runs per segment of every shell line a hook
        // sees, and `op_kind_of_gh_command` tokenizes the segment a second time before it can even
        // tell that the program is not `gh`. A daemon whose projects declared nothing goes on paying
        // exactly what it paid before this field existed.
        if self.read_ops.is_empty() {
            return false;
        }
        op_kind_of_gh_command(command).is_some_and(|kind| self.read_ops.iter().any(|op| op == kind))
    }

    /// Whether an operation of this `kind()` is executed without asking. Everything else becomes a
    /// proposal a person approves, and the turn carries on either way.
    ///
    /// One list for both authors: an action the machine granted and an action the project declared
    /// are the same sentence in the same vocabulary, so `for_project` merges rather than adding a
    /// second field. The reading half could not do that, and its field says why.
    pub fn action_is_autonomous(&self, kind: &str) -> bool {
        self.actions.iter().any(|allowed| allowed == kind)
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::empty()
    }
}

/// The pillar as the daemon holds it: whether the owner switched it on, and what runs without
/// asking. Resolved once at startup and read-only afterwards, like `WebRuntime` and
/// `BrowserRuntime` beside it in `AppState`.
///
/// `enabled` is kept even though `Policy` already collapses to empty when it is false, and the
/// reason is `health.rs`: a pillar the owner switched off is `NotConfigured` and a pillar with an
/// empty list is working as intended. Those are different sentences to read at two in the morning,
/// and only one of them is a problem.
///
/// `Default` is what an ABSENT `.ai/github.yaml` produces — on, and autonomous in nothing — so a
/// test that does not care about GitHub gets the shipped state rather than an invented one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRuntime {
    pub enabled: bool,
    /// Whether `.ai/github.yaml` EXISTS. Not whether it says anything useful.
    ///
    /// It is here because `enabled` defaults to true, so `enabled` alone can no longer answer "did
    /// anybody ask for this pillar" — and `health.rs` needs that answer or every machine that has
    /// never heard of GitHub reports a fault. **Writing the file is the opt-in; its contents are the
    /// autonomy**, and those are two different decisions the owner makes at two different times.
    ///
    /// The inverse of the distinction `config.rs` draws about a working capability "as opposed to a
    /// file that merely exists": there a file that exists proves nothing, and here it is the only
    /// thing that proves anything.
    pub configured: bool,
    /// The `gh` executable, resolved once at startup from `NUCLEOS_GH_BIN`.
    ///
    /// Overridable for the reason `NUCLEOS_CLAUDE_BIN` exists, and it is the same reason rather than
    /// a borrowed one: on Windows a CLI installed through npm is a `.cmd` shim that Rust's `Command`
    /// cannot spawn by name. `health.rs::binary_candidate` deliberately refuses `.cmd` candidates
    /// too, so without this a machine where `gh` works in the console would report `Missing` and
    /// nothing would say why.
    ///
    /// It lives HERE and is not read from the environment per call, which matters twice. Startup is
    /// where every other pillar resolves its configuration, so a run cannot change what the daemon
    /// spawns halfway through itself. And a test can hand this a stub program without mutating
    /// process-global state, which in Rust 2024 is `unsafe` and races every other test in the
    /// binary.
    pub binary: String,
    pub policy: Policy,
}

impl Default for GithubRuntime {
    fn default() -> Self {
        Self {
            enabled: true,
            configured: false,
            binary: "gh".to_owned(),
            policy: Policy::empty(),
        }
    }
}

impl GithubRuntime {
    /// `configured` is passed in rather than derived, because whether the file exists is a fact
    /// about the disk and this module does no I/O to find out. `main.rs` knows the path it just
    /// read and is the one place that should.
    pub fn from_config(
        config: &crate::config::GithubConfig,
        configured: bool,
        binary: String,
    ) -> Self {
        Self {
            enabled: config.enabled,
            configured,
            binary,
            policy: Policy::from_config(config),
        }
    }

    /// The policy a decision about THIS project is taken under: the machine default with the
    /// project's declared operations laid over it.
    ///
    /// **The door the daemon uses, and the one place the switch is read.** `Policy::for_project`
    /// does the layering and knows nothing about `enabled`, because by the time a `Policy` exists a
    /// switched-off pillar and an empty pair of lists are the same value — `from_config` collapses
    /// one into the other on purpose. Layering onto that collapsed value would let a project's row
    /// turn back on a pillar the owner switched off, which is the one thing `enabled: false` is for.
    /// Kept here rather than at the call sites so that neither `hooks.rs` nor `runs.rs` has to
    /// remember it.
    pub async fn policy_for_project(&self, pool: &sqlx::SqlitePool, project_id: &str) -> Policy {
        if !self.enabled {
            return Policy::empty();
        }
        self.policy.for_project(pool, project_id).await
    }
}

/// PURE: one list intersected with its ceiling, sorted and deduplicated, warning about each entry it
/// had to drop.
///
/// Written for the owner's file and used unchanged for a project's rows, which is why the warning
/// says "config" rather than "file": a row in `project_github_ops` is configuration too, and it
/// reaches this by the same route and for the same reason.
fn narrow(asked: &[String], ceiling: &[&str], what: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for entry in asked {
        let entry = entry.trim();
        if ceiling.contains(&entry) {
            kept.push(entry.to_owned());
        } else {
            tracing::warn!(
                entry = %entry,
                kind = %what,
                "github config: outside the compiled ceiling; dropped, and it will keep asking"
            );
        }
    }
    kept.sort();
    kept.dedup();
    kept
}

/// PURE: FNV-1a over the effective policy, rendered as sixteen hex characters.
///
/// **Not a cryptographic hash, and it does not need to be.** What this labels is a scoreboard row,
/// so the property required is that two different effective policies get different labels and one
/// policy gets the same label everywhere. Nothing here defends against a chosen collision: the text
/// being hashed is the owner's own file, already narrowed by the ceilings.
///
/// Hand-written rather than `DefaultHasher`, and that is the reason it exists as ten lines instead
/// of two: `DefaultHasher`'s algorithm is explicitly allowed to change between Rust releases, and a
/// toolchain upgrade that silently renumbered every digest would fragment the very scoreboard this
/// is for. FNV-1a is fixed forever and costs no dependency.
///
/// The input is the effective policy and NOT the file, so a comment edit or a reordering does not
/// break the scoreboard — which is the whole reason `§7` asked for it normalized before hashed. A
/// switched-off pillar and an empty pair of lists hash alike, and they should: they are the same
/// effective policy.
///
/// **`read_ops` is written only when there IS one, and that conditional is the same sentence again
/// rather than a special case.** A project that declared nothing has the machine's effective policy,
/// so it must carry the machine's label; appending an empty field instead would renumber every
/// digest ever recorded, and `shadow::shadow_readiness` would then warn that one action class spans
/// two policies on machines where nothing changed. A project's declared ACTIONS need no such care —
/// they merge into `actions`, so they move the digest through a field that was always written.
fn digest_of(reads: &[String], read_ops: &[String], actions: &[String]) -> String {
    let mut text = format!(
        "v1\nreads={}\nactions={}",
        reads.join(","),
        actions.join(",")
    );
    if !read_ops.is_empty() {
        text.push_str(&format!("\nread_ops={}", read_ops.join(",")));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// How long one `gh` invocation may take before its whole process tree goes down.
///
/// Shorter than `git_exec::OPERATION_TIMEOUT`'s five minutes, and the difference is what the two
/// wait for: a git operation can be pushing objects over a slow link, and every operation here is
/// one HTTP request to GitHub with a CLI wrapped around it. Two minutes is generous for that and
/// short enough that a wedged call does not hold a caller for the length of a coffee break.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

/// The Credential Manager key the token lives under (`secrets.rs`).
///
/// **`gh auth login` is not an alternative and the two are not equivalent.** That login writes the
/// credential into a keyring belonging to the interactive session of whoever ran it, and this daemon
/// runs as a scheduled task. Worse, a green `gh auth status` would then read "healthy" while the
/// module could not act — so the health probe asks about the token the module will USE, never about
/// the CLI's own opinion of itself.
pub const TOKEN_KEY: &str = "github-token";

/// What comes back with the answer, before any of it reaches a caller's context.
///
/// A workflow log is unbounded and a run's context is not, so `stdout` is capped and says when the
/// cap bit. Truncation is at the START rather than the end: the tail of a failing log is where the
/// error is, and clipping that to keep the setup lines would keep the half nobody needs.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// The result of one `gh` invocation.
///
/// There is no `github_requests` table for this to be written to, and that is deliberate: `vcs.rs`
/// has one because its queue is asynchronous and somebody has to be able to read the ticket later,
/// while here the answer is synchronous and the caller already holds it. A `github-action` proposal
/// records its own outcome on its own row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub kind: &'static str,
    pub exit_code: Option<i32>,
    /// Standard output, capped and already through `redact.rs`.
    pub stdout: String,
    /// stdout then stderr, for the caller that wants to see why a non-zero exit happened.
    pub output_tail: String,
}

/// Why an invocation did not happen, in the vocabulary `health.rs` already has.
///
/// The three that look alike are kept apart on purpose. `MissingCli` is a thing this computer cannot
/// do; `MissingToken` is a thing it could do if somebody pasted a credential; `NotConfigured` is a
/// pillar nobody asked for. Collapsing them costs an hour of looking in the wrong place, which is
/// exactly what `health.rs`'s own doc says about `NotRunning` versus `Missing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// `enabled: false`. Not a fault.
    NotConfigured,
    /// No `gh` on PATH.
    MissingCli,
    /// No `github-token` in the Credential Manager, or it could not be read.
    MissingToken,
    /// The command outlived `OPERATION_TIMEOUT` and its tree was killed.
    TimedOut,
    Unknown(String),
}

impl Failure {
    pub fn category(&self) -> crate::health::FailureCategory {
        match self {
            Failure::NotConfigured => crate::health::FailureCategory::NotConfigured,
            Failure::MissingCli => crate::health::FailureCategory::Missing,
            Failure::MissingToken => crate::health::FailureCategory::PermissionDenied,
            Failure::TimedOut => crate::health::FailureCategory::Timeout,
            Failure::Unknown(_) => crate::health::FailureCategory::Unknown,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NotConfigured => write!(formatter, "the github pillar is switched off"),
            Failure::MissingCli => write!(formatter, "gh is not on this machine's PATH"),
            Failure::MissingToken => write!(
                formatter,
                "no github token is stored; run the daemon with --set-github-token"
            ),
            Failure::TimedOut => {
                write!(formatter, "gh did not finish within {OPERATION_TIMEOUT:?}")
            }
            Failure::Unknown(reason) => write!(formatter, "{reason}"),
        }
    }
}

/// Reads the token out of the Credential Manager.
///
/// `spawn_blocking` because `keyring` is a blocking OS call and this runs on the async runtime.
/// An error reading the store and an empty store are the same answer here — neither yields a token,
/// and the distinction would only ever be repeated back as a message.
pub async fn load_token() -> Option<String> {
    tokio::task::spawn_blocking(|| crate::secrets::load_secret(TOKEN_KEY))
        .await
        .ok()?
        .ok()?
        .filter(|token| !token.trim().is_empty())
}

/// Runs one operation and returns what `gh` said.
///
/// **It decides HOW, never WHETHER.** `Policy` answers whether it runs without asking and `auth.rs`
/// answers whether the actor could ask at all; nothing in here consults either. That separation is
/// `vcs.rs`'s between when an operation runs and whether the actor was allowed to request it, and
/// keeping it means a second caller cannot acquire permission by calling this instead.
///
/// Four things it does that are each somebody's past bug:
///
/// 1. **The argv is built and never interpreted.** `op.argv()` produces a list, `Command` takes a
///    list, and no shell is anywhere on the path — so a `;` inside a PR body is one more character
///    in an argument.
/// 2. **The token goes in by ENVIRONMENT and never as an argument**, because a process's argv is
///    readable by any process on the machine. `GITHUB_TOKEN` is removed rather than left alone: `gh`
///    prefers `GH_TOKEN` so it would not win, and an inherited variable that cannot win is one
///    somebody will later assume is being used.
/// 3. **The whole process tree goes down on a deadline.** It is the obligation `vcs.rs` records,
///    born of `run_git` saying "nothing here hands git a shell" and that ceasing to be true without
///    anybody editing the comment. `gh` spawns a credential helper and an editor given the chance,
///    so the direct child is not the whole of what was started.
/// 4. **A non-zero exit is returned and never retried.** Retrying a `PrComment` that failed halfway
///    posts it twice, and this module cannot tell halfway from not-at-all.
pub async fn execute(runtime: &GithubRuntime, op: &Op) -> Result<Outcome, Failure> {
    if !runtime.enabled {
        return Err(Failure::NotConfigured);
    }
    let token = load_token().await.ok_or(Failure::MissingToken)?;
    spawn_gh(runtime, op, &token, OPERATION_TIMEOUT).await
}

/// Everything `execute` does once it holds a credential and a deadline.
///
/// Split out for one reason, and it is worth stating rather than leaving as a shape: **the keyring
/// and the production deadline are the two things a test cannot have.** The Credential Manager is a
/// real OS store with no fake behind it, and 120 seconds is not a wait a suite can take. With both
/// passed in, everything below — the argv built and never interpreted, the token by environment
/// rather than by argument, the terminator, the tree going down on the deadline, the output capped
/// and redacted, a non-zero exit returned and never retried — is exercised against a stub program.
///
/// The four obligations named in `execute`'s own doc live down here, so that is where they are read.
async fn spawn_gh(
    runtime: &GithubRuntime,
    op: &Op,
    token: &str,
    deadline: Duration,
) -> Result<Outcome, Failure> {
    let argv = op.argv();

    let mut command = tokio::process::Command::new(&runtime.binary);
    command
        .args(&argv)
        .env("GH_TOKEN", token)
        .env_remove("GITHUB_TOKEN")
        // `gh` opens a prompt or a pager given half a chance, and there is nobody here to answer one.
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        // Otherwise every answer arrives wrapped in escape sequences, and `Body::new` would refuse
        // its own module's output if it ever came back round.
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::process_tree::spawn_in_own_group(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        // The one spawn error worth its own variant. Everything else is a real failure to start a
        // program that exists; this is a machine that does not have `gh` on it, which is not a fault.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Failure::MissingCli);
        }
        Err(error) => return Err(Failure::Unknown(format!("could not run gh: {error}"))),
    };

    // Declared AFTER the child so it drops FIRST, while tokio's handle still pins the pid — the
    // invariant `TreeKiller` documents, and the only thing keeping the pid it names ours.
    let mut killer = child.id().map(crate::process_tree::TreeKiller::new);

    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout_task = tokio::spawn(drain(
        child.stdout.take().expect("stdout was piped"),
        Arc::clone(&stdout_bytes),
    ));
    let stderr_task = tokio::spawn(drain(
        child.stderr.take().expect("stderr was piped"),
        Arc::clone(&stderr_bytes),
    ));

    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            return Err(Failure::Unknown(format!("could not wait for gh: {error}")));
        }
        Err(_) => {
            // The tree goes down HERE and not at drop, because `child` is still alive at this point
            // and its handle is what stops the pid being reused under the killer.
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            tracing::warn!(
                kind = op.kind(),
                "gh timed out; its process tree was killed"
            );
            return Err(Failure::TimedOut);
        }
    };

    let drained = async {
        for task in [stdout_task, stderr_task] {
            let _ = task.await;
        }
    };
    if tokio::time::timeout(DRAIN_GRACE, drained).await.is_ok() {
        if let Some(killer) = killer.as_mut() {
            killer.disarm();
        }
    } else {
        // The exit code survives a stuck drain: `gh` ran and said how it ended, and only the tail is
        // short. The killer stays ARMED, so dropping it takes whatever still holds the pipe down.
        tracing::warn!(
            kind = op.kind(),
            "gh's output did not finish draining; reporting the result with a truncated tail"
        );
    }

    let stdout = std::mem::take(&mut *stdout_bytes.lock().expect("the buffer is not poisoned"));
    let stderr = std::mem::take(&mut *stderr_bytes.lock().expect("the buffer is not poisoned"));
    let stdout = clip(&String::from_utf8_lossy(&stdout));
    let stderr = clip(&String::from_utf8_lossy(&stderr));

    let exit_code = status.code();
    if exit_code != Some(0) {
        // A line, and never a row: see `Outcome`. The kind and the code, and deliberately not the
        // output — the caller has that, and a log file is a worse place for a stranger's prose than
        // a turn's context is.
        tracing::warn!(kind = op.kind(), ?exit_code, "a gh operation failed");
    }

    Ok(Outcome {
        kind: op.kind(),
        exit_code,
        // Through `redact.rs`, which already knows the five `gh*_` prefixes and `github_pat_`. A
        // token can come back out of `gh`'s own diagnostics, and the shortest path from there to a
        // third party is a caller pasting the answer somewhere.
        stdout: crate::redact::redact_secrets(&stdout),
        output_tail: crate::redact::redact_secrets(&format!("{stdout}{stderr}")),
    })
}

/// How long the pipes get to finish after the process has exited.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Reads one pipe to EOF into a buffer the caller keeps, so a drain that has to be abandoned still
/// leaves behind what it managed to read.
///
/// A `std::sync::Mutex` and never held across an `await`: the lock is taken per chunk and released
/// before the next read, which is what keeps a blocking lock correct inside an async task.
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut reader: R, into: Arc<Mutex<Vec<u8>>>) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut buffer = into.lock().expect("the buffer is not poisoned");
                buffer.extend_from_slice(&chunk[..read]);
                // A sliding TAIL, because `clip` promises one and this is the half that has to
                // deliver it.
                //
                // This used to STOP appending once the buffer reached the cap, which keeps the
                // head. `clip` then kept the last `MAX_OUTPUT_BYTES` of the first
                // `MAX_OUTPUT_BYTES` and stamped it with a marker announcing the tail — so the one
                // case the cap exists for, a workflow log that failed, came back holding the setup
                // lines with the errors thrown away, while saying the opposite in writing.
                //
                // Measured on a real red run before it was fixed: the `failures:` block cargo
                // prints after every test is exactly what went missing, which is the only part
                // anybody reads a failing log for.
                //
                // Trimmed in one step at twice the cap rather than on every chunk, because trimming
                // per read would memmove a quarter of a megabyte for every 8 KiB that arrives. What
                // is left is never smaller than `MAX_OUTPUT_BYTES + 1`, so `clip` still sees that
                // something was cut and still says so.
                if buffer.len() > MAX_OUTPUT_BYTES * 2 {
                    let excess = buffer.len() - (MAX_OUTPUT_BYTES + 1);
                    buffer.drain(..excess);
                }
            }
        }
    }
}

/// PURE: keeps the LAST `MAX_OUTPUT_BYTES` and says so when that clipped anything.
///
/// The tail rather than the head, because the tail of a failing workflow log is where the error is.
/// Reading is done on characters and not bytes so the cut never lands mid-codepoint.
fn clip(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_owned();
    }
    let kept: String = text
        .chars()
        .rev()
        .take(MAX_OUTPUT_BYTES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("[... clipped to the last {MAX_OUTPUT_BYTES} characters ...]\n{kept}")
}

/// What a door gets back: the operation ran, or it is waiting for a person.
///
/// **Neither outcome blocks the caller**, and that is the decision rather than a convenience. An
/// item waiting on a human would be a run sitting in `working` for days, holding a concurrency slot
/// and counting against the ceiling — so filing answers immediately and the turn carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    Ran(Outcome),
    Filed {
        proposal_id: i64,
        kind: &'static str,
    },
}

/// Something went wrong deciding what to do, as opposed to doing it.
#[derive(Debug)]
pub enum DecisionError {
    NotFound,
    NotPending,
    /// The proposal carries nothing this module can read as an operation.
    Malformed,
    /// It was claimed and then `gh` would not run it.
    Failed(Failure),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// The single place that answers "does this run now, or does it wait for a person".
///
/// Shared by the HTTP route and by whatever internal trigger comes later, because two copies of this
/// decision is how the tool and the trigger would come to disagree about the same `.ai/github.yaml`.
///
/// **A READ never files a proposal, and that is not an omission.** What limits reads is the EFFECT
/// (`ReadOp::effect`) and not the autonomy list: a read that returns a stranger's prose marks the
/// turn and burns its right to act, which is a stronger and more precise brake than an approval
/// prompt would be. `autonomous_actions` governs the acting half alone, and the acting half is the
/// only half whose refusal has anywhere to go.
pub async fn submit(
    pool: &sqlx::SqlitePool,
    runtime: &GithubRuntime,
    op: Op,
) -> Result<Submitted, Failure> {
    if !runtime.enabled {
        return Err(Failure::NotConfigured);
    }
    match &op {
        Op::Read(_) => execute(runtime, &op).await.map(Submitted::Ran),
        Op::Act(act) if runtime.policy.action_is_autonomous(act.kind()) => {
            execute(runtime, &op).await.map(Submitted::Ran)
        }
        Op::Act(act) => {
            let kind = act.kind();
            let why = act.describe();
            let payload = serde_json::to_string(&op).map_err(|error| {
                Failure::Unknown(format!("the operation could not be recorded: {error}"))
            })?;
            let proposal_id = crate::proposals::create_github_action(pool, kind, &why, &payload)
                .await
                .map_err(|error| {
                    tracing::warn!(kind, %error, "filing a github action for approval failed");
                    Failure::Unknown("the operation could not be filed for approval".to_owned())
                })?;
            Ok(Submitted::Filed { proposal_id, kind })
        }
    }
}

/// Runs a `github-action` a person has just approved.
///
/// **The claim comes BEFORE the run, and the order is the safety argument.** Marking first means a
/// second approval racing this one loses at the compare-and-set and cannot post the same comment
/// twice; the cost is that a `gh` failure leaves a row reading `approved` with nothing published,
/// which the note beneath it says in words. The other order trades a readable row for a double post,
/// and `execute` deliberately never retries for the same reason.
///
/// The outcome goes on the proposal's own row rather than into a table of its own — see `Outcome`.
pub async fn approve_proposed_operation(
    pool: &sqlx::SqlitePool,
    runtime: &GithubRuntime,
    proposal_id: i64,
) -> Result<Outcome, DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "github-action" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }
    let op: Op = proposal
        .tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or(DecisionError::Malformed)?;
    // Read back through the same validating `Deserialize` every other route in, so a row edited by
    // hand in the database cannot put a dashed string on a command line months later.

    if !crate::proposals::transition(pool, proposal_id, "approved", "approved by user").await? {
        return Err(DecisionError::NotPending);
    }

    match execute(runtime, &op).await {
        Ok(outcome) => {
            let note = match outcome.exit_code {
                Some(0) => format!("gh {} ran and succeeded", outcome.kind),
                Some(code) => format!("gh {} ran and exited {code}", outcome.kind),
                None => format!("gh {} was terminated by a signal", outcome.kind),
            };
            crate::proposals::note(pool, proposal_id, &note).await?;
            Ok(outcome)
        }
        Err(failure) => {
            crate::proposals::note(
                pool,
                proposal_id,
                &format!("gh {} did not run: {failure}", op.kind()),
            )
            .await?;
            Err(DecisionError::Failed(failure))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubConfig, load_github_config};

    /// Goes through the real loader rather than around it, so "absent" is genuinely an absent file
    /// and not a hand-built default that happens to look like one.
    fn policy_from(text: Option<&str>) -> Policy {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("github.yaml");
        if let Some(text) = text {
            std::fs::write(&path, text).expect("the sample config should be writable");
        }
        Policy::from_config(&load_github_config(&path))
    }

    fn repo() -> Repo {
        Repo::new("owner/name").expect("owner/name is a repository")
    }

    /// A pillar the owner switched off, which is what a test uses when it wants `execute` to refuse
    /// before spawning anything or touching the Credential Manager.
    fn switched_off() -> GithubRuntime {
        GithubRuntime {
            enabled: false,
            ..GithubRuntime::default()
        }
    }

    /// A runtime pointed at a stub program instead of `gh`.
    ///
    /// **These spawn `echo`, `false` and `yes` as PROGRAMS**, so on Windows they need Git's
    /// `usr/bin` on PATH — the same requirement the nine tests in `gate::` and `transcribe::`
    /// already carry, and the same failure without it: `program not found`, which reads like a
    /// broken repository and is not.
    ///
    /// The stub is what makes the spawn testable at all. `gh` is not on every machine, and the two
    /// things `execute` needs that a suite cannot have — a real Credential Manager entry and a
    /// 120-second deadline — are `spawn_gh`'s arguments rather than its constants.
    fn pointed_at(program: &str) -> GithubRuntime {
        GithubRuntime {
            enabled: true,
            configured: true,
            binary: program.to_owned(),
            policy: Policy::empty(),
        }
    }

    fn a_comment(body: &str) -> Op {
        Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new(body).expect("the sample body is valid"),
        })
    }

    /// The law, measured instead of argued. `echo` prints the argv it was handed, so what comes back
    /// says exactly what the operating system was asked to run.
    ///
    /// The body carries a `;`, a `$(...)` and an `&&`. If any shell were anywhere on this path they
    /// would split the command, run `whoami`, or append a second one; what comes back is one
    /// argument with all three characters still in it.
    #[tokio::test]
    async fn the_argv_is_built_and_never_interpreted() {
        let dangerous = "a; rm -rf / $(whoami) && echo pwned";
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment(dangerous),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert_eq!(outcome.exit_code, Some(0));
        // The whole argv, exactly, on ONE line. `echo` writes its arguments separated by spaces and
        // nothing else, so this is the operating system reporting back what it was asked to run.
        //
        // Written first as `!stdout.contains("pwned")`, which was the wrong assertion and failed:
        // the body CONTAINS that word, so `echo` prints it and always would. What distinguishes an
        // interpreted line from a literal one is not the presence of the word — it is that a shell
        // would have put it on a SECOND line, from a second command. Comparing the whole output
        // against the whole argv says that and says it exactly.
        assert_eq!(
            outcome.stdout.trim_end(),
            format!("pr comment --repo=owner/name --body={dangerous} -- 42"),
        );
    }

    /// **The credential never touches the command line.** A process's argv is readable by any
    /// process on the machine, so this is the difference between a secret and a broadcast.
    ///
    /// The token is deliberately NOT token-shaped: a `ghp_...` would be caught by `redact.rs` on the
    /// way out, and the test would then pass whether or not it had ever been in the argv. This one
    /// nothing redacts, so its absence is the argument.
    #[tokio::test]
    async fn the_token_goes_by_environment_and_never_as_an_argument() {
        let token = "plainly-not-token-shaped-9876543210";
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment("an ordinary comment"),
            token,
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert!(
            !outcome.stdout.contains(token),
            "the token reached the argv"
        );
        assert!(!outcome.output_tail.contains(token));
    }

    /// And a credential that DOES come back out of the CLI's own diagnostics is redacted, which is
    /// the other half and a different claim: the first is about what goes in, this is about what
    /// comes out.
    #[tokio::test]
    async fn a_credential_in_the_output_comes_back_redacted() {
        let leaked = format!("ghp_{}", "a1B2c3D4e5".repeat(4)); // 40 chars after the prefix
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment(&leaked),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert!(!outcome.stdout.contains(&leaked), "{}", outcome.stdout);
        assert!(
            outcome.stdout.contains("[SECRET:github]"),
            "{}",
            outcome.stdout
        );
    }

    /// A non-zero exit is an ANSWER and not an error: it comes back with its code for the caller to
    /// read. Nothing here retries, and `PrComment` is why — a retry of one that failed halfway posts
    /// it twice, and this module cannot tell halfway from not-at-all.
    #[tokio::test]
    async fn a_non_zero_exit_comes_back_rather_than_failing() {
        let outcome = spawn_gh(
            &pointed_at("false"),
            &a_comment("a comment"),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("a failing command still produced a result");

        assert_eq!(outcome.exit_code, Some(1));
        assert_eq!(outcome.kind, "pr_comment");
    }

    /// The deadline, against a program that never ends. `yes` repeats its arguments forever, so
    /// this also drives the output cap: the buffer stops growing and the call still returns.
    ///
    /// It is `spawn_gh`'s argument rather than `OPERATION_TIMEOUT` for exactly this reason — the
    /// production two minutes is not a wait a suite can take, and a deadline that cannot be
    /// exercised is a deadline nobody has seen fire.
    ///
    /// **`ApiRead` and not `PrList`, and the first attempt is the reason.** `yes pr list --repo=o/r`
    /// exits 1 with "unknown option" — `yes` runs getopt over its arguments like any coreutil, so a
    /// stub cannot be handed an argv full of flags and be expected to ignore them. `ApiRead` puts every
    /// caller value after `--`, which is the one argv shape in this module that carries no option at
    /// all, so it is the shape a stub can actually receive.
    #[tokio::test]
    async fn a_command_that_never_ends_dies_on_the_deadline() {
        let refused = spawn_gh(
            &pointed_at("yes"),
            &Op::Act(ActOp::ApiRead {
                args: vec!["forever".to_owned()],
            }),
            "unused",
            Duration::from_millis(300),
        )
        .await;

        assert_eq!(refused, Err(Failure::TimedOut));
    }

    /// A machine without `gh` is a machine that cannot do this, and that is not a fault. Told
    /// anything else, somebody goes looking for a broken repository.
    #[tokio::test]
    async fn a_binary_that_is_not_there_is_missing_and_not_an_error() {
        let refused = spawn_gh(
            &pointed_at("nucleos-gh-that-is-not-installed"),
            &Op::Read(ReadOp::PrList { repo: repo() }),
            "unused",
            Duration::from_secs(30),
        )
        .await;

        assert_eq!(refused, Err(Failure::MissingCli));
    }

    /// The cap keeps the END of a long stream, which is the half a failing log is read for.
    ///
    /// `drain` and not `spawn_gh`, because a stub that emits a quarter-megabyte of DISTINGUISHABLE
    /// output is not something `echo` or `yes` can be asked for — `yes` repeats one line, and a
    /// buffer of identical lines cannot tell a kept head from a kept tail. Numbered lines can.
    ///
    /// Written after the bug shipped and was found in use: a red CI run came back with its setup
    /// lines and without its `failures:` block, under a marker claiming the opposite.
    #[tokio::test]
    async fn a_stream_past_the_cap_keeps_its_end_and_not_its_beginning() {
        let mut source: Vec<u8> = Vec::new();
        let mut n = 0_u64;
        while source.len() < MAX_OUTPUT_BYTES * 3 {
            source.extend_from_slice(format!("line {n}\n").as_bytes());
            n += 1;
        }
        let ultima = format!("line {}\n", n - 1);

        let buffer = Arc::new(Mutex::new(Vec::new()));
        drain(&source[..], buffer.clone()).await;
        let guardado = buffer.lock().expect("the buffer is not poisoned");

        let texto = String::from_utf8_lossy(&guardado);
        assert!(
            texto.ends_with(&ultima),
            "the end of the stream is missing; it ends with {:?}",
            &texto[texto.len().saturating_sub(40)..]
        );
        assert!(
            !texto.contains("line 0\n"),
            "the beginning was kept instead of the end"
        );
        assert!(
            guardado.len() > MAX_OUTPUT_BYTES,
            "nothing was kept beyond the cap, so `clip` would not report a cut"
        );
        assert!(
            guardado.len() <= MAX_OUTPUT_BYTES * 2,
            "the sliding window did not bound memory: {} bytes",
            guardado.len()
        );
    }

    /// The override exists for the reason `NUCLEOS_CLAUDE_BIN` exists, and a default that drifted
    /// from `gh` would be a pillar looking for a program nobody installs.
    #[test]
    fn the_default_binary_is_gh() {
        assert_eq!(GithubRuntime::default().binary, "gh");
    }

    /// The partition. `ReadOp` and `ActOp` cover `Op` with no overlap and no hole: an operation in
    /// both halves would be the partition undone, and one in neither would be an operation
    /// `execute` knows how to run and no tool knows how to ask for.
    #[test]
    fn read_ops_and_act_ops_partition_op() {
        let reads: Vec<&str> = ReadOp::all().iter().map(|op| op.kind()).collect();
        let acts: Vec<&str> = ActOp::all().iter().map(|op| op.kind()).collect();

        for kind in &reads {
            assert!(
                !acts.contains(kind),
                "{kind} is on both sides of the partition"
            );
        }
        let mut all: Vec<&str> = reads.iter().chain(acts.iter()).copied().collect();
        all.sort_unstable();
        let mut every: Vec<&str> = Op::all().iter().map(|op| op.kind()).collect();
        every.sort_unstable();
        assert_eq!(all, every, "an operation of `Op` belongs to neither half");
    }

    /// No read acts and no act fails to. It is the effects table, and this is what makes it
    /// checkable rather than decorative.
    #[test]
    fn every_operation_declares_an_effect() {
        for op in ReadOp::all() {
            assert!(
                matches!(
                    op.effect(),
                    ToolEffect::ReadsOwn | ToolEffect::ReadsUntrusted
                ),
                "{} is a read and may not act",
                op.kind()
            );
        }
        for op in ActOp::all() {
            assert_eq!(op.effect(), ToolEffect::Acts, "{} acts", op.kind());
        }
    }

    /// The six reads that carry a stranger's words are marked, and the four that carry structure are
    /// not. Getting this backwards is the whole failure the per-operation effect exists to stop.
    ///
    /// The list is written out here rather than derived, and the duplication is the point: `effect`
    /// is one `match` and a test that read it back would agree with whatever that `match` said. A
    /// new variant defaults to `ReadsOwn` in the `_` arm below, so an author who grades a stranger's
    /// text as structure fails here and reads the grading again.
    #[test]
    fn prose_is_untrusted_and_structure_is_not() {
        for op in ReadOp::all() {
            let expected = match op.kind() {
                "pr_view" | "issue_view" | "run_logs" | "pr_diff" | "pr_thread"
                | "checks_for_ref" => ToolEffect::ReadsUntrusted,
                _ => ToolEffect::ReadsOwn,
            };
            assert_eq!(op.effect(), expected, "{}", op.kind());
        }
    }

    /// The three reads added for a stranger's words, each in the shape its grading claims.
    ///
    /// It asserts the argv as well as the effect, because the grading is a claim ABOUT the argv: a
    /// `pr_diff` that had quietly become `gh pr view` would still say `ReadsUntrusted` and would no
    /// longer be reading a diff. And it asserts the effect through `effect_of_kind`, which is the
    /// route `mcp_tools::effect_of_call` actually takes — the one that latches the turn.
    #[test]
    fn the_three_reads_that_carry_a_strangers_words_say_so() {
        let number = PrNumber::new("42").expect("42 is a pull request number");
        let expected: [(ReadOp, Vec<&str>); 3] = [
            (
                ReadOp::PrDiff {
                    repo: repo(),
                    number: number.clone(),
                },
                vec!["pr", "diff", "--repo=owner/name", "--", "42"],
            ),
            (
                ReadOp::PrThread {
                    repo: repo(),
                    number,
                },
                vec!["pr", "view", "--repo=owner/name", "--comments", "--", "42"],
            ),
            (
                ReadOp::ChecksForRef {
                    repo: repo(),
                    r#ref: Branch::new("main").expect("main is a ref"),
                },
                vec!["pr", "checks", "--repo=owner/name", "--", "main"],
            ),
        ];

        for (op, argv) in expected {
            assert_eq!(op.argv(), argv, "{}", op.kind());
            assert_eq!(op.effect(), ToolEffect::ReadsUntrusted, "{}", op.kind());
            assert_eq!(
                ReadOp::effect_of_kind(op.kind()),
                Some(ToolEffect::ReadsUntrusted),
                "{} has to be untrusted on the route effect_of_call takes",
                op.kind()
            );
            // A read is a read: the partition does not bend for the grading.
            assert!(!ActOp::all().iter().any(|act| act.kind() == op.kind()));
        }

        // Reading the thread and posting to it are different operations on different sides of the
        // partition, and each tool sends the other's kind to the right place rather than answering
        // "unknown operation". This is what the names `pr_thread` and `pr_comment` are for; it was
        // `pr_comments` and `pr_comment` for one commit, which is one letter to carry a boundary on.
        assert!(
            ReadOp::from_request(ReadRequest {
                operation: "pr_comment".to_owned(),
                repo: "owner/name".to_owned(),
                id: Some("42".to_owned()),
            })
            .expect_err("pr_comment acts")
            .contains("github_act"),
            "posting a comment is sent to the acting tool"
        );
        assert!(
            ActOp::from_request(ActRequest {
                operation: "pr_thread".to_owned(),
                repo: Some("owner/name".to_owned()),
                id: Some("42".to_owned()),
                ..ActRequest::default()
            })
            .expect_err("pr_thread only reads")
            .contains("github_read"),
            "reading the thread is sent to the reading tool"
        );

        // And the two withdrawn names are gone rather than aliased. An operation that answered to
        // its old string would be the rename undone in the one place a caller can reach.
        for withdrawn in ["pr_files", "pr_comments"] {
            assert!(
                ReadOp::from_request(ReadRequest {
                    operation: withdrawn.to_owned(),
                    repo: "owner/name".to_owned(),
                    id: Some("42".to_owned()),
                })
                .expect_err("a withdrawn name is not an operation")
                .contains("unknown read operation"),
                "{withdrawn} may not still resolve"
            );
        }
    }

    /// None of the three is declarable, and that is the decision rather than an oversight.
    ///
    /// `READ_CEILING` is what a project may be granted, and it holds only reads of structural shape
    /// — the constant's own doc refuses `gh pr view` in those words. These three return a stranger's
    /// text by definition, so admitting them would contradict the sentence that admits anything at
    /// all. They are reachable the way `pr_view` and `issue_view` are: a run asks for one, gets it,
    /// and pays the turn's right to act for it.
    #[test]
    fn a_stranger_carrying_read_is_reachable_and_not_declarable() {
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: Vec::new(),
        });
        for op in ReadOp::all()
            .into_iter()
            .filter(|op| op.effect() == ToolEffect::ReadsUntrusted)
        {
            assert!(
                !widest.read_is_autonomous(&format!("gh {}", op.argv().join(" "))),
                "{} returns a stranger's words and may not be autonomous",
                op.kind()
            );
        }
    }

    /// The law, in the variant where it costs most and matters most. A `ApiRead` carrying a line would
    /// be "never a command string" undone by the one variant that exists for the cases nobody
    /// foresaw.
    #[test]
    fn api_read_carries_separate_arguments_and_never_a_line() {
        let op = ActOp::ApiRead {
            args: vec!["repos/o/r".into(), "-X".into(), "GET".into()],
        };
        assert_eq!(op.argv(), vec!["api", "--", "repos/o/r", "-X", "GET"]);
    }

    /// Every caller value is either behind `--` or inside a `--flag=value`. Nothing this module
    /// builds hands `gh` a bare `--flag value` pair carrying a caller's string.
    ///
    /// The exceptions are the module's own BOOLEANS, and that is the whole rule they satisfy: a flag
    /// that takes no value cannot be followed by a caller's string, so it consumes nothing and can
    /// stand bare. Named one by one rather than waved through by a prefix check — the day a flag
    /// that DOES take a value is added, the failure should be here and not in a shell.
    #[test]
    fn no_caller_value_reaches_argv_where_it_could_act_as_a_flag() {
        const OUR_OWN_BOOLEANS: &[&str] = &["--log", "--comments"];
        for op in Op::all() {
            let argv = op.argv();
            let terminator = argv.iter().position(|part| part == "--");
            for (index, part) in argv.iter().enumerate() {
                if terminator.is_some_and(|at| index > at) {
                    continue;
                }
                assert!(
                    !part.starts_with("--")
                        || part.contains('=')
                        || OUR_OWN_BOOLEANS.contains(&part.as_str())
                        || part == "--",
                    "{} puts {part} on the command line as a bare flag",
                    op.kind()
                );
            }
        }
    }

    /// The terminator goes in when there is a positional to protect and stays out when there is
    /// not. A stray `--` on `gh pr create` would be an empty positional argument, not a no-op.
    #[test]
    fn the_terminator_appears_exactly_where_a_positional_does() {
        let with = ActOp::RunRerun {
            repo: repo(),
            id: RunId::new("7").expect("7 is a run id"),
        };
        assert_eq!(
            with.argv(),
            vec!["run", "rerun", "--repo=owner/name", "--", "7"]
        );

        let without = ReadOp::PrList { repo: repo() };
        assert_eq!(without.argv(), vec!["pr", "list", "--repo=owner/name"]);
    }

    /// `--ref` is built as `--ref=value` and sits BEFORE the terminator, because the workflow name
    /// is positional. A `["--ref", value]` pair would be read as two positionals the moment
    /// anything moved.
    #[test]
    fn a_flag_that_precedes_a_positional_is_written_as_flag_equals_value() {
        let op = ActOp::WorkflowRun {
            repo: repo(),
            workflow: WorkflowName::new("CI Build").expect("CI Build is a workflow name"),
            r#ref: Branch::new("main").expect("main is a branch"),
        };
        assert_eq!(
            op.argv(),
            vec![
                "workflow",
                "run",
                "--repo=owner/name",
                "--ref=main",
                "--",
                "CI Build",
            ]
        );
    }

    /// The node types refuse what could act as an option, and a `Deserialize` that only called
    /// `String::deserialize` would let every one of these through.
    #[test]
    fn a_node_type_refuses_a_value_that_could_act_as_an_option() {
        assert!(Repo::new("--upload-pack=x").is_err());
        assert!(Repo::new("name").is_err(), "a repository is owner/name");
        assert!(Repo::new("a/b/c").is_err());
        assert!(Branch::new("--delete").is_err());
        assert!(Branch::new("").is_err());
        assert!(WorkflowName::new("-x").is_err());
        assert!(RunId::new("12; rm -rf /").is_err());
        assert!(PrNumber::new("-1").is_err());
        assert!(IssueNumber::new("").is_err());

        assert!(Repo::new("owner/name").is_ok());
        assert!(WorkflowName::new("CI Build").is_ok());
        assert!(Body::new("a body\nwith lines").is_ok());
        assert!(Body::new("a body with a \u{1b} in it").is_err());
    }

    /// The validating `Deserialize` is what covers the raw route, and this is the case that proves
    /// it is not the constructor doing the work.
    #[test]
    fn deserializing_an_op_validates_every_node_it_carries() {
        let good: ReadOp = serde_json::from_value(serde_json::json!({
            "op": "pr_view", "repo": "owner/name", "number": "42"
        }))
        .expect("a well formed read deserializes");
        assert_eq!(good.kind(), "pr_view");

        let bad = serde_json::from_value::<ReadOp>(serde_json::json!({
            "op": "pr_view", "repo": "--upload-pack=x", "number": "42"
        }));
        assert!(bad.is_err(), "a dashed repository may not deserialize");
    }

    /// Every other field's default is a convenience; these two are a refusal.
    #[test]
    fn a_github_yaml_that_is_absent_unreadable_or_malformed_is_autonomous_in_nothing() {
        for text in [
            None,
            Some("{{{ this is not yaml"),
            Some("autonomous_reads: this is not a list\n"),
            Some(""),
        ] {
            let policy = policy_from(text);
            assert!(policy.autonomous_reads().is_empty(), "{text:?}");
            assert!(policy.autonomous_actions().is_empty(), "{text:?}");
            assert!(!policy.read_is_autonomous("gh run list"), "{text:?}");
            assert!(!policy.action_is_autonomous("pr_comment"), "{text:?}");
        }
    }

    /// A missing file leaves the pillar CAPABLE and not switched off, which is the asymmetry with
    /// the web and browser pillars that `GithubConfig`'s doc argues for. Nothing is autonomous;
    /// everything can still be asked for.
    #[test]
    fn a_missing_file_withholds_autonomy_and_not_capability() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let config = load_github_config(&directory.path().join("nothing-here.yaml"));
        assert!(
            config.enabled,
            "an absent file may not switch the pillar off"
        );
        assert!(config.autonomous_reads.is_empty());
        assert!(config.autonomous_actions.is_empty());
    }

    /// The file may narrow the ceiling and may never widen it. It asks for two things the ceiling
    /// does not have and one it does, and keeps the one it does.
    #[test]
    fn the_file_may_narrow_the_ceiling_and_never_widens_it() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n  - gh auth token\n\
             autonomous_actions:\n  - pr_comment\n  - api_read\n",
        ));
        assert_eq!(policy.autonomous_reads(), ["gh run list"]);
        assert_eq!(policy.autonomous_actions(), ["pr_comment"]);
        assert!(policy.read_is_autonomous("gh run list"));
        assert!(!policy.read_is_autonomous("gh auth token"));
        assert!(policy.action_is_autonomous("pr_comment"));
        assert!(!policy.action_is_autonomous("api_read"));
    }

    /// The two that never pass, with the file explicitly asking for the opposite.
    #[test]
    fn gh_api_and_gh_auth_token_are_never_autonomous() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh auth token\n  - gh auth status\n  - gh secret list\n  \
             - gh variable list\nautonomous_actions:\n  - api_read\n",
        ));
        assert!(policy.autonomous_reads().is_empty());
        assert!(policy.autonomous_actions().is_empty());
    }

    /// A read that returns prose is not in the ceiling, so the file cannot turn it on.
    #[test]
    fn a_read_that_returns_prose_is_not_in_the_read_ceiling() {
        for prose in ["gh pr view", "gh issue view", "gh run download"] {
            assert!(
                !READ_CEILING.contains(&prose),
                "{prose} returns a stranger's words and may not be eligible for autonomy"
            );
        }
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh pr view\n  - gh issue view\n",
        ));
        assert!(policy.autonomous_reads().is_empty());
    }

    /// **The central test.** The prefix matches and autonomy has to fall anyway: a structural read
    /// turns into prose — or into a thousand lines of it — without changing subcommand.
    #[test]
    fn a_refused_flag_takes_autonomy_from_a_prefix_that_had_it() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh run view\n  - gh pr list\n"));

        assert!(policy.read_is_autonomous("gh run view 123"));
        assert!(policy.read_is_autonomous("gh pr list --state open"));
        assert!(policy.read_is_autonomous("gh pr list --author octocat"));

        for command in [
            "gh run view 123 --log",
            "gh run view 123 --log-failed",
            "gh pr list --json body",
            // pflag accepts `=`, and equality alone would let this one through.
            "gh pr list --json=body",
            // The short form of `--jq`.
            "gh pr list -q .[].body",
            "gh pr list --template {{.body}}",
            "gh pr list -t {{.body}}",
            "gh pr list --search 'in:body secret'",
            // For an injection channel, how many IS the payload.
            "gh pr list --limit 1000",
            "gh pr list --limit=1000",
            // The alias that a review forgot, on the very entry created to stop aliases being
            // forgotten.
            "gh pr list -L 1000",
        ] {
            assert!(
                !policy.read_is_autonomous(command),
                "{command} should have lost its autonomy"
            );
        }
    }

    /// `-L` is `--limit` and `-l` is `--label`. Folding the comparison to one case would either
    /// miss the first or refuse the second, which is why the raw command is what gets read.
    #[test]
    fn the_flag_comparison_is_case_sensitive_because_gh_is() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh pr list\n"));
        assert!(!policy.read_is_autonomous("gh pr list -L 1000"));
        assert!(policy.read_is_autonomous("gh pr list -l bug"));
    }

    /// A prefix is a prefix of WORDS. `gh run listen` is not `gh run list`, and a comparison by
    /// `starts_with` alone would have said it was.
    #[test]
    fn a_prefix_matches_whole_words_and_not_a_string() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh run list\n"));
        assert!(policy.read_is_autonomous("gh run list"));
        assert!(policy.read_is_autonomous("gh run list --branch main"));
        assert!(!policy.read_is_autonomous("gh run listen"));
        assert!(!policy.read_is_autonomous("gh runlist"));
    }

    /// A pillar the owner switched off is autonomous in nothing, which is the same answer an empty
    /// list gives — and the reason `enabled` is not carried as a third state.
    #[test]
    fn enabled_false_collapses_both_lists() {
        let config = GithubConfig {
            enabled: false,
            autonomous_reads: vec!["gh run list".to_owned()],
            autonomous_actions: vec!["pr_comment".to_owned()],
        };
        let policy = Policy::from_config(&config);
        assert!(policy.autonomous_reads().is_empty());
        assert!(policy.autonomous_actions().is_empty());
        assert_eq!(
            policy.digest(),
            Policy::empty().digest(),
            "a switched-off pillar and an empty pair of lists are the same effective policy"
        );
    }

    /// The digest tracks the effective policy and nothing else: a comment, the order of the entries,
    /// and a line the ceiling drops all leave it alone.
    #[test]
    fn the_digest_changes_with_the_effective_policy_and_not_with_a_comment() {
        let base = policy_from(Some("autonomous_reads:\n  - gh run list\n  - gh pr list\n"));
        let commented = policy_from(Some(
            "# the owner's notes\nautonomous_reads:\n  - gh pr list\n  - gh run list\n",
        ));
        let dropped = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n  - gh pr list\n  - gh api\n",
        ));
        let different = policy_from(Some("autonomous_reads:\n  - gh run list\n"));

        assert_eq!(base.digest(), commented.digest());
        assert_eq!(base.digest(), dropped.digest());
        assert_ne!(base.digest(), different.digest());
        assert_ne!(base.digest(), Policy::empty().digest());
        assert_eq!(base.digest().len(), 16);
    }

    /// **The digest's OUTPUT, pinned to literals, because what it labels is already on disk.**
    ///
    /// Every other assertion about this function is RELATIVE — two policies hash alike, or they do
    /// not — and every one of them survives a change to `digest_of` itself, because both sides move
    /// together. `a_project_that_declared_nothing_is_the_machine_default_exactly` cannot help either,
    /// for exactly that reason: it compares `for_project`'s answer against the machine's, and a
    /// rewrite moves the pair. So the property that actually matters here had nothing holding it.
    ///
    /// That property is not internal consistency. `policy_digest` is a column in
    /// `shadow_decisions`, written on every governed tool call, and rows carrying these strings
    /// exist in live databases. A change that renumbers them orphans the history: `shadow_readiness`
    /// COUNTS distinct digests per action class, so the same class would suddenly be reported as
    /// spanning two policies on a machine where nobody changed anything, and
    /// `READINESS_MIN_REVIEWED` would restart ten reviews of progress toward promotion.
    ///
    /// **The third assertion is the one that guards the conditional in `digest_of`.** Drop the
    /// `if !read_ops.is_empty()` and an empty overlay starts hashing `\nread_ops=` — the machine
    /// default's own label changes, and so does every row already recorded under it. The fourth
    /// pins the other half: when there IS a name, the suffix is written, so two genuinely different
    /// effective policies still get different labels.
    ///
    /// Literals rather than a recomputation, deliberately. A test that recomputed FNV-1a here would
    /// agree with whatever `digest_of` did, which is the failure mode this exists to close.
    #[tokio::test]
    async fn the_digest_of_a_known_policy_is_a_known_string() {
        let pool = test_pool().await;

        // `v1\nreads=\nactions=` — the label a machine with no autonomy has always carried.
        assert_eq!(Policy::empty().digest(), "ef64af62014226ae");

        let machine = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        // `v1\nreads=gh run list\nactions=pr_comment`
        assert_eq!(machine.digest(), "9cef55bb486b8280");

        // A project that declared nothing carries the machine's own byte, and that is the claim
        // about rows already written rather than a claim about two values in this process.
        assert_eq!(
            machine.for_project(&pool, "alpha").await.digest(),
            "9cef55bb486b8280"
        );
        assert_eq!(
            Policy::empty().for_project(&pool, "alpha").await.digest(),
            "ef64af62014226ae"
        );

        // And a project that declared something gets a label of its own, suffix and all:
        // `v1\nreads=gh run list\nactions=pr_comment\nread_ops=pr_list`
        crate::project_policy::declare_github_op(&pool, "alpha", "pr_list")
            .await
            .unwrap();
        assert_eq!(
            machine.for_project(&pool, "alpha").await.digest(),
            "edcfd3d2f1212aab"
        );
    }

    /// Four failures, four different places to go looking. Collapsing any two of them costs
    /// somebody an hour in the wrong one, which is the argument `health.rs` already makes about
    /// `NotRunning` versus `Missing`.
    #[test]
    fn every_failure_has_a_category_of_its_own() {
        use crate::health::FailureCategory;
        assert_eq!(
            Failure::NotConfigured.category(),
            FailureCategory::NotConfigured
        );
        assert_eq!(Failure::MissingCli.category(), FailureCategory::Missing);
        assert_eq!(
            Failure::MissingToken.category(),
            FailureCategory::PermissionDenied,
            "a missing credential may not read like a broken repository"
        );
        assert_eq!(Failure::TimedOut.category(), FailureCategory::Timeout);
        for failure in [
            Failure::NotConfigured,
            Failure::MissingCli,
            Failure::MissingToken,
            Failure::TimedOut,
        ] {
            assert!(!failure.to_string().is_empty());
        }
    }

    /// A pillar the owner switched off is refused before anything is spawned and before the
    /// Credential Manager is touched.
    #[tokio::test]
    async fn a_switched_off_pillar_executes_nothing() {
        let op = Op::Read(ReadOp::PrList { repo: repo() });
        let runtime = switched_off();
        assert_eq!(execute(&runtime, &op).await, Err(Failure::NotConfigured));
    }

    /// The tail survives and the head is what goes, because the tail of a failing workflow log is
    /// where the error is. And the cut lands on a character boundary, which a byte-wise slice of a
    /// log full of accented commit messages would not.
    #[test]
    fn clipping_keeps_the_tail_and_never_splits_a_character() {
        let short = "a short answer";
        assert_eq!(clip(short), short);

        let long = "\u{e1}".repeat(MAX_OUTPUT_BYTES);
        let clipped = clip(&long);
        assert!(clipped.starts_with("[... clipped"));
        assert!(clipped.ends_with('\u{e1}'));
        assert!(!clipped.contains('\u{fffd}'));
    }

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn runtime_with(actions: &[&str]) -> GithubRuntime {
        GithubRuntime::from_config(
            &GithubConfig {
                enabled: true,
                autonomous_reads: Vec::new(),
                autonomous_actions: actions.iter().map(|entry| (*entry).to_owned()).collect(),
            },
            true,
            // Never spawned by the tests that use this: they file a proposal, or run against a
            // pillar that is switched off. A name nothing installs is the honest value — if one of
            // them ever does start spawning, it fails as `MissingCli` instead of reaching a real
            // `gh` and doing something on GitHub from a test run.
            "nucleos-gh-never-spawned-here".to_owned(),
        )
    }

    /// A caller that asks the wrong tool is told SO, rather than being told it invented a word.
    /// Told "unknown operation: pr_comment" it would go looking for a typo in its own request; told
    /// where the operation lives it sends it there.
    #[test]
    fn from_request_says_which_tool_an_operation_belongs_to() {
        let asked_read_for_an_action = ReadOp::from_request(ReadRequest {
            operation: "pr_comment".to_owned(),
            repo: "owner/name".to_owned(),
            id: Some("1".to_owned()),
        });
        assert!(
            asked_read_for_an_action
                .unwrap_err()
                .contains("belongs to github_act")
        );

        let asked_act_for_a_read = ActOp::from_request(ActRequest {
            operation: "pr_view".to_owned(),
            repo: Some("owner/name".to_owned()),
            id: Some("1".to_owned()),
            ..ActRequest::default()
        });
        assert!(
            asked_act_for_a_read
                .unwrap_err()
                .contains("belongs to github_read")
        );

        let nonsense = ReadOp::from_request(ReadRequest {
            operation: "frobnicate".to_owned(),
            repo: "owner/name".to_owned(),
            id: None,
        });
        assert!(nonsense.unwrap_err().contains("unknown read operation"));
    }

    /// A missing field names itself, and `api_read` is settled before the repository is — otherwise the
    /// one operation that names no repository would be refused for not having one.
    #[test]
    fn from_request_names_the_field_it_is_missing() {
        let missing = ActOp::from_request(ActRequest {
            operation: "pr_comment".to_owned(),
            repo: Some("owner/name".to_owned()),
            id: Some("1".to_owned()),
            ..ActRequest::default()
        });
        assert!(missing.unwrap_err().contains("body"));

        let api_read = ActOp::from_request(ActRequest {
            operation: "api_read".to_owned(),
            args: Some(vec!["repos/o/r".to_owned()]),
            ..ActRequest::default()
        });
        assert_eq!(
            api_read.expect("api_read needs no repository").kind(),
            "api_read"
        );
    }

    /// `effect_of_kind` and `effect()` are one answer, because the first is derived from the same
    /// list the second is exhaustive over.
    #[test]
    fn the_effect_of_a_kind_is_the_effect_of_the_operation() {
        for op in ReadOp::all() {
            assert_eq!(ReadOp::effect_of_kind(op.kind()), Some(op.effect()));
        }
        assert_eq!(ReadOp::effect_of_kind("pr_comment"), None);
        assert_eq!(ReadOp::effect_of_kind(""), None);
    }

    /// **Off the list is not refused — it is filed, and the turn carries on.** That is the whole
    /// reading of this pillar, and a version that answered "denied" would be a different product.
    #[tokio::test]
    async fn an_action_off_the_list_is_filed_and_never_refused() {
        let pool = test_pool().await;
        let runtime = runtime_with(&["run_rerun"]);
        let op = Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new("a comment").expect("a body"),
        });

        let submitted = submit(&pool, &runtime, op).await.expect("filing succeeds");
        let Submitted::Filed { proposal_id, kind } = submitted else {
            panic!("an operation off the list must be filed, never run");
        };
        assert_eq!(kind, "pr_comment");

        let proposal = crate::proposals::get(&pool, proposal_id)
            .await
            .expect("the proposal is readable")
            .expect("the proposal exists");
        assert_eq!(proposal.kind, "github-action");
        assert_eq!(proposal.status, "pending");
        assert_eq!(
            proposal.tool_name.as_deref(),
            Some("pr_comment"),
            "the column the approvals list renders has to say what is being agreed to"
        );
    }

    /// A read never files, whatever the action list says. What limits reads is the effect and not
    /// the autonomy, and a read that queued for approval would be the two mixed up.
    #[tokio::test]
    async fn a_read_never_becomes_a_proposal() {
        let pool = test_pool().await;
        // Switched off, so `execute` refuses before spawning anything and the test needs no `gh`.
        // What is under test is which BRANCH a read takes, and the refusal proves it took the one
        // that executes rather than the one that files.
        let runtime = switched_off();
        let op = Op::Read(ReadOp::PrView {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
        });
        assert_eq!(
            submit(&pool, &runtime, op).await,
            Err(Failure::NotConfigured)
        );
        // Counted straight out of the table and not through `list_pending`, which filters
        // `kind = 'action-approval'` — asking it would have passed whether or not a row was written.
        let filed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposals WHERE kind = 'github-action'")
                .fetch_one(&pool)
                .await
                .expect("the table is readable");
        assert_eq!(filed, 0, "a read may never become a proposal");
    }

    /// **The claim comes before the run.** A second approval racing the first loses at the
    /// compare-and-set, which is what stops one comment being posted twice — and `execute` never
    /// retrying is the same argument from the other end.
    ///
    /// The pillar is switched off so `gh` is never spawned: what is under test is the ORDER of the
    /// two writes, and a failure to run is the sharpest way to see it. The row is `approved` with
    /// nothing published, and the note beneath it says so in words.
    #[tokio::test]
    async fn approving_claims_the_proposal_before_running_it() {
        let pool = test_pool().await;
        let op = Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new("a comment").expect("a body"),
        });
        let Submitted::Filed { proposal_id, .. } = submit(&pool, &runtime_with(&[]), op)
            .await
            .expect("filing succeeds")
        else {
            panic!("it should have been filed");
        };

        let off = switched_off();
        let ran = approve_proposed_operation(&pool, &off, proposal_id).await;
        assert!(
            matches!(ran, Err(DecisionError::Failed(Failure::NotConfigured))),
            "the operation could not run, and that is what the caller is told"
        );

        let proposal = crate::proposals::get(&pool, proposal_id)
            .await
            .expect("the proposal is readable")
            .expect("the proposal exists");
        assert_eq!(
            proposal.status, "approved",
            "the claim is what stops a second approval posting the same comment again"
        );

        assert!(
            matches!(
                approve_proposed_operation(&pool, &off, proposal_id).await,
                Err(DecisionError::NotPending)
            ),
            "a second approval must lose at the status check"
        );
    }

    /// A proposal whose payload is not an operation is refused rather than guessed at, and it is
    /// refused by the same validating `Deserialize` every other road in uses — so a row edited by
    /// hand in the database months later cannot put a dashed string on a command line.
    #[tokio::test]
    async fn a_proposal_carrying_no_usable_operation_is_refused() {
        let pool = test_pool().await;
        for payload in [
            "not json at all",
            r#"{"op":"pr_comment","repo":"--upload-pack=x","number":"1","body":"hi"}"#,
            r#"{"op":"no_such_operation"}"#,
        ] {
            let proposal_id =
                crate::proposals::create_github_action(&pool, "pr_comment", "why", payload)
                    .await
                    .expect("the proposal is written");
            assert!(
                matches!(
                    approve_proposed_operation(&pool, &runtime_with(&[]), proposal_id).await,
                    Err(DecisionError::Malformed)
                ),
                "{payload}"
            );
        }
    }

    /// The ceilings hold to the operations. An `ACTION_CEILING` entry naming a kind no `ActOp` has
    /// would be a line in the owner's file that grants nothing and says it grants something.
    #[test]
    fn every_action_ceiling_entry_names_a_real_operation() {
        let kinds: Vec<&str> = ActOp::all().iter().map(|op| op.kind()).collect();
        for entry in ACTION_CEILING {
            assert!(kinds.contains(entry), "{entry} is not an ActOp kind");
        }
        assert!(
            !ACTION_CEILING.contains(&"api_read"),
            "`api_read` is never eligible for autonomy"
        );
    }

    /// Decision #7 as behaviour, and not as a constant that happens to lack a word.
    ///
    /// The assertion above says `ACTION_CEILING` does not contain the string `api_read`. That is
    /// worth having and it is not the claim: the claim is that a `Policy` TOLD to allow `api_read`
    /// by every route that can build one still refuses. So this asks each route in turn, and asks
    /// the reading side too, because an operation refused as an action and admitted as a command
    /// would be the same capability through the other door.
    ///
    /// `Policy` has three constructors — `empty`, `from_config` and `for_project` — and only the
    /// last two can be told anything. That is why this is a complete enumeration rather than a
    /// sample, and why adding a fourth constructor is a change that has to come back here.
    ///
    /// **Route three is the one this chunk added, and it is the sharpest of the three.** A row in
    /// `project_github_ops` reaches `read_is_autonomous` through `op_kind_of_gh_command`, and that
    /// map DOES name `gh api …` — `ApiRead`'s argv is `api -- …`, so the form matches and the answer
    /// is `Some("api_read")`. Nothing about the map refuses it. What refuses it is `for_project`
    /// narrowing the stored rows against `declarable_ops` on the way OUT, which is the whole reason
    /// the read side narrows at all when the write route already validated: this row is written
    /// straight into the table below, exactly as an out-of-band write or a pre-ceiling row would
    /// arrive, and without that narrowing `gh api -X DELETE` would run in Bash unasked.
    #[tokio::test]
    async fn api_read_is_never_autonomous_by_any_route_that_can_ask_for_it() {
        // Route one: the owner's `.ai/github.yaml`, asking for it in both lists and asking beside
        // entries the ceilings DO admit — so a narrowing that dropped the whole file would satisfy
        // this by accident and the kept entries prove it did not.
        let owner = policy_from(Some(
            "autonomous_reads:\n  - gh api\n  - gh run list\n\
             autonomous_actions:\n  - api_read\n  - pr_create\n",
        ));
        assert_eq!(owner.autonomous_actions(), ["pr_create"]);
        assert_eq!(owner.autonomous_reads(), ["gh run list"]);

        // Route two: a `GithubConfig` built in code from the ceilings themselves — the widest policy
        // this codebase can construct, and the one `http::declarable_github_ops` asks in order to
        // decide what a project may put in `project_github_ops`. A project cannot declare what this
        // policy refuses, so refusing here is what keeps the per-project table from becoming the
        // ceiling with a different door on it.
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: ACTION_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
        });
        assert!(
            widest
                .autonomous_actions()
                .contains(&"pr_create".to_owned())
        );

        // Route three: a project's own table, written straight past the route that validates it, and
        // laid over the two widest machine policies there are.
        let pool = test_pool().await;
        for kind in ["api_read", "run_list", "pr_create"] {
            sqlx::query(
                "INSERT INTO project_github_ops (project_id, op_kind, created_at)
                 VALUES ('alpha', ?, datetime('now'))",
            )
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        let declared = widest.for_project(&pool, "alpha").await;
        let declared_over_nothing = Policy::empty().for_project(&pool, "alpha").await;
        // The kept rows prove the narrowing dropped one entry and not the whole table, the way the
        // owner's route above proves it.
        assert!(declared_over_nothing.read_is_autonomous("gh run list -R owner/name"));
        assert!(declared_over_nothing.action_is_autonomous("pr_create"));

        for policy in [
            Policy::empty(),
            Policy::default(),
            owner,
            widest,
            declared,
            declared_over_nothing,
        ] {
            assert!(
                !policy.action_is_autonomous("api_read"),
                "api_read is never an autonomous action"
            );
            // And not through the reading side either: the argv `ApiRead` actually builds matches no
            // `READ_CEILING` prefix, so `gh api` is refused as a command exactly as it is refused as
            // an operation.
            let argv = ActOp::ApiRead {
                args: vec!["repos/owner/name".to_owned()],
            }
            .argv();
            assert!(!policy.read_is_autonomous(&format!("gh {}", argv.join(" "))));
            assert!(!policy.read_is_autonomous("gh api repos/owner/name"));
            // The line the map WOULD name `api_read`, spelled the way it would cost the most.
            assert!(!policy.read_is_autonomous("gh api -X DELETE repos/owner/name"));
        }
        // Said once, outside the loop, because it is a fact about the map rather than about any
        // policy: the map names this line, and the name is not a grant.
        assert_eq!(
            op_kind_of_gh_command("gh api repos/owner/name"),
            Some("api_read"),
            "the map names it; the ceiling is what refuses it"
        );
    }

    /// The non-regression, proved rather than argued: a project that declared nothing IS the machine
    /// default, value for value.
    ///
    /// Equality over the whole `Policy` and not over its answers, which is the stronger claim and
    /// the cheaper one to keep: it covers the two lists, the third list this chunk added, and the
    /// `digest` — the last of which labels rows in a live scoreboard, so an overlay that renumbered
    /// it would tell `shadow::shadow_readiness` that one action class spans two policies on a
    /// machine where nothing changed.
    ///
    /// Asked of a machine default with entries as well as of an empty one, because the interesting
    /// direction is the one where there is something to lose.
    #[tokio::test]
    async fn a_project_that_declared_nothing_is_the_machine_default_exactly() {
        let pool = test_pool().await;
        let owner = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        for machine in [Policy::empty(), Policy::default(), owner] {
            let for_project = machine.for_project(&pool, "alpha").await;
            assert_eq!(
                for_project, machine,
                "a project with no rows is the machine default and not a copy of it"
            );
            assert_eq!(for_project.digest(), machine.digest());
        }
    }

    /// The widening half, and the reason any of this exists: an operation this project declared runs
    /// without asking HERE and nowhere else.
    ///
    /// The machine default is empty throughout, so nothing but the project's own row can be
    /// producing the `true` — which is what makes this the test a `for_project` that ignored its
    /// rows would fail.
    #[tokio::test]
    async fn a_declared_operation_runs_without_asking_in_this_project_and_in_no_other() {
        let pool = test_pool().await;
        crate::project_policy::declare_github_op(&pool, "alpha", "run_list")
            .await
            .unwrap();
        crate::project_policy::declare_github_op(&pool, "alpha", "pr_comment")
            .await
            .unwrap();

        let machine = Policy::empty();
        let alpha = machine.for_project(&pool, "alpha").await;
        let beta = machine.for_project(&pool, "beta").await;

        assert!(alpha.read_is_autonomous("gh run list -R owner/name"));
        assert!(alpha.action_is_autonomous("pr_comment"));

        assert!(!beta.read_is_autonomous("gh run list -R owner/name"));
        assert!(!beta.action_is_autonomous("pr_comment"));
        assert!(!machine.read_is_autonomous("gh run list -R owner/name"));
        assert!(!machine.action_is_autonomous("pr_comment"));

        // A different effective policy gets a different label, which is the one property `digest`
        // has to have.
        assert_ne!(alpha.digest(), machine.digest());
        assert_eq!(beta.digest(), machine.digest());
    }

    /// It ADDS to the machine default; it does not replace it. A project that declares one operation
    /// keeps every grant the owner's file already gave the machine.
    #[tokio::test]
    async fn a_projects_declaration_adds_to_the_machine_default_and_never_replaces_it() {
        let pool = test_pool().await;
        crate::project_policy::declare_github_op(&pool, "alpha", "pr_list")
            .await
            .unwrap();

        let machine = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        let alpha = machine.for_project(&pool, "alpha").await;

        // The project's own row, granted.
        assert!(alpha.read_is_autonomous("gh pr list -R owner/name"));
        // And the machine's two, still standing. Under a replacement reading both of these would be
        // false, and the owner would have had no way to write down that they wanted them kept.
        assert!(alpha.read_is_autonomous("gh run list -R owner/name"));
        assert!(alpha.action_is_autonomous("pr_comment"));
    }

    /// The ceiling is a ceiling on the way OUT of the table too.
    ///
    /// Every row here is one `POST /projects/{id}/github-ops` would refuse, written straight into
    /// the table the way a row stored before a ceiling narrowed — or written out of band — would
    /// arrive. `run_logs` and `pr_view` are the interesting pair: both are real `ReadOp`s the map
    /// names, so nothing about the naming refuses them, and `pr_view` shares no flag with anything —
    /// it is outside `READ_CEILING` and that alone is what stops it.
    #[tokio::test]
    async fn a_row_outside_the_ceiling_grants_nothing_however_it_got_there() {
        let pool = test_pool().await;
        for kind in ["run_logs", "pr_view", "issue_view", "pr_diff", "api_read"] {
            sqlx::query(
                "INSERT INTO project_github_ops (project_id, op_kind, created_at)
                 VALUES ('alpha', ?, datetime('now'))",
            )
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        for command in [
            "gh run view --log 1 -R owner/name",
            "gh pr view 7 -R owner/name",
            "gh issue view 7 -R owner/name",
            "gh pr diff 7 -R owner/name",
            "gh api repos/owner/name",
        ] {
            assert!(
                !alpha.read_is_autonomous(command),
                "{command:?} is outside the ceiling whatever the table says"
            );
        }
        // And the whole overlay is inert, digest included: five refused rows are no rows.
        assert_eq!(alpha, Policy::empty());
    }

    /// A declared ACTION does not open the reading door in Bash.
    ///
    /// Decision #5 keeps the partition in the types, and this is what it buys once one list of names
    /// governs both doors: `op_kind_of_gh_command` names `gh pr comment` perfectly well, and the
    /// project declared `pr_comment`, so the only thing between that line and running unattended is
    /// `read_ops` holding no action.
    ///
    /// It is the twin of a sentence the machine's own square already measures — "no action is ever
    /// autonomous in Bash", in `the_map_names_an_operation_and_does_not_yet_make_the_two_doors_agree`
    /// — asked of the door that did not exist when that was written. There it holds by ARITHMETIC,
    /// because no `ACTION_CEILING` kind has a `READ_CEILING` prefix; here the prefixes are gone and
    /// only the partition is left holding it up.
    #[tokio::test]
    async fn a_declared_action_never_becomes_an_autonomous_bash_line() {
        let pool = test_pool().await;
        for kind in declarable_act_ops() {
            crate::project_policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        for op in ActOp::all() {
            let line = format!("gh {}", op.argv().join(" "));
            assert!(
                !alpha.read_is_autonomous(&line),
                "{line:?} acts, and no declaration makes an action an autonomous read"
            );
        }
        // The one that would hurt most, spelled by hand so the assertion survives a change to
        // `PrComment`'s argv.
        assert!(!alpha.read_is_autonomous("gh pr comment 7 --body hello -R owner/name"));
        // ...while the typed door answers yes, which is what makes the line above a partition and
        // not an accident of spelling.
        assert!(alpha.action_is_autonomous("pr_comment"));
    }

    /// **A name from the map is not a grant.** The refused flags bind the named door exactly as they
    /// bind the prefix one, and they run BEFORE it.
    ///
    /// `--log` and `--log-failed` are the flags that change WHICH operation a line is, and the map
    /// has its own guard for those. The rest of `REFUSED_READ_FLAGS` is a different job: `--json`
    /// and `--jq` change the SHAPE of what comes back, and `--limit` bounds HOW MUCH — `ReadOp::
    /// effect` grades `PrList` as `ReadsOwn` precisely because a stranger reaches it through thirty
    /// short fields and no further. A mapped name allowed past this check would take that grading
    /// with it, and `gh pr list --limit 1000` would be a thousand stranger-chosen titles in a call
    /// that marks nothing.
    #[tokio::test]
    async fn a_declared_name_does_not_survive_a_flag_that_changes_what_comes_back() {
        let pool = test_pool().await;
        for kind in ["run_status", "pr_list", "run_list"] {
            crate::project_policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        // The plain forms run, or there would be nothing to take away below.
        assert!(alpha.read_is_autonomous("gh run view 1 -R owner/name"));
        assert!(alpha.read_is_autonomous("gh pr list -R owner/name"));

        for refused in [
            "gh run view --log 1 -R owner/name",
            "gh run view --log-failed 1 -R owner/name",
            "gh pr list --limit 1000 -R owner/name",
            "gh pr list -L 1000 -R owner/name",
            "gh pr list --json body -R owner/name",
            "gh pr list --jq .[] -R owner/name",
            "gh run list --search in:body -R owner/name",
        ] {
            assert!(
                !alpha.read_is_autonomous(refused),
                "{refused:?} carries a flag that changes what comes back"
            );
        }
    }

    /// A pillar the owner switched off stays off, whatever a project declared.
    ///
    /// `Policy::from_config` collapses a disabled pillar to `empty()`, so by the time a `Policy`
    /// exists "switched off" and "granted nothing" are the same value and the layering cannot tell
    /// them apart. `GithubRuntime::policy_for_project` is where the switch is read, and this is the
    /// test that says so — layering onto the collapsed value directly, as the second half shows,
    /// would hand a row the pillar's own off switch.
    #[tokio::test]
    async fn a_switched_off_pillar_stays_off_whatever_the_project_declared() {
        let pool = test_pool().await;
        crate::project_policy::declare_github_op(&pool, "alpha", "run_list")
            .await
            .unwrap();

        let off = GithubRuntime {
            enabled: false,
            ..GithubRuntime::default()
        };
        let policy = off.policy_for_project(&pool, "alpha").await;
        assert_eq!(policy, Policy::empty());
        assert!(!policy.read_is_autonomous("gh run list -R owner/name"));

        // The same project, the same row, through a pillar that is on.
        let on = GithubRuntime::default();
        assert!(
            on.policy_for_project(&pool, "alpha")
                .await
                .read_is_autonomous("gh run list -R owner/name"),
            "the row is a real grant; the switch is what withheld it above"
        );
    }

    /// `pr_create` is inside the ceiling; every other route to it is unchanged.
    ///
    /// A pull request is a proposal a human still has to act on, which is what puts it beside
    /// `pr_comment` rather than beside `api_read`. The file may still narrow it away, and the test
    /// says so: the ceiling decides what MAY be autonomous and the owner decides what is.
    #[test]
    fn opening_a_pull_request_is_inside_the_ceiling_and_still_the_owners_choice() {
        assert!(ACTION_CEILING.contains(&"pr_create"));

        let asked = policy_from(Some("autonomous_actions:\n  - pr_create\n"));
        assert!(asked.action_is_autonomous("pr_create"));

        let did_not_ask = policy_from(Some("autonomous_actions:\n  - pr_comment\n"));
        assert!(
            !did_not_ask.action_is_autonomous("pr_create"),
            "a ceiling entry the file does not name grants nothing"
        );
    }

    /// The reading ceiling holds to the operations too, and it is the half that had drifted.
    ///
    /// `gh workflow list` sat in `READ_CEILING` with no `ReadOp` building it, which is the mirror of
    /// the failure the test above guards against: a prefix that grants a capability the typed path
    /// cannot ask for is a line the owner reads as a permission and that permits nothing. The
    /// direction is ceiling to operation and not the reverse — a `ReadOp` outside the ceiling is
    /// ordinary (`pr_view` is one, deliberately), a ceiling entry outside the operations is not.
    #[test]
    fn every_read_ceiling_prefix_is_built_by_a_real_operation() {
        let commands: Vec<String> = ReadOp::all()
            .iter()
            .map(|op| format!("gh {}", op.argv().join(" ")))
            .collect();
        for prefix in READ_CEILING {
            assert!(
                commands
                    .iter()
                    .any(|command| command.starts_with(&format!("{prefix} "))),
                "{prefix} is in the ceiling and no ReadOp builds it"
            );
        }
    }

    /// The `gh` line an operation builds, as somebody would have typed it into Bash.
    fn gh_line(op: &Op) -> String {
        format!("gh {}", op.argv().join(" "))
    }

    /// **Decision #4's test, and the only one that would catch the map drifting from the
    /// operations.**
    ///
    /// One list of NAMES can govern both doors only if a name picks out the SAME operation in each
    /// vocabulary. So: enable exactly one kind, then ask about every operation twice — once the way
    /// the typed door asks (is this the kind that is enabled?) and once the way the Bash door would
    /// have to ask it (does the map name this command line as the kind that is enabled?). The two
    /// answers have to be the same, for all sixteen operations, sixteen times over.
    ///
    /// **What makes this more than a tautology is the pair `gh run view` serves.** A map that named
    /// both halves `run_status` would answer yes to `run_logs`'s command line while the typed door
    /// said no, and one cell of this square is where that shows up; `gh pr view` is the same trap a
    /// second time. It is written as a loop and not as a list of cases on purpose — a case per
    /// operation is a second place to forget the eleventh operation, which is the drift the whole
    /// decision exists to stop.
    ///
    /// **What it does NOT prove is that the two doors behave alike today.** They do not, and the
    /// test below measures by how much. This proves the map, which is the piece that was missing.
    #[test]
    fn one_name_picks_out_the_same_operation_at_both_doors() {
        let operations = Op::all();
        for enabled in operations.iter().map(Op::kind) {
            for op in &operations {
                let line = gh_line(op);
                let typed_door = op.kind() == enabled;
                let bash_door = op_kind_of_gh_command(&line) == Some(enabled);
                assert_eq!(
                    typed_door,
                    bash_door,
                    "with {enabled} enabled, the typed door says {typed_door} about {} and the map \
                     reads {line:?} as {:?}",
                    op.kind(),
                    op_kind_of_gh_command(&line)
                );
            }
        }
    }

    /// What the map does not do, measured rather than promised.
    ///
    /// The map names an operation; it does not change who runs it. Under the WIDEST policy a
    /// `.ai/github.yaml` can express — both ceilings, whole — the two doors still answer differently
    /// about eleven of the sixteen operations, and this pins which eleven. Reads run typed whatever
    /// the list says (`submit`: *«A READ never files a proposal, and that is not an omission»*), and
    /// no action is ever autonomous in Bash because no `ACTION_CEILING` kind has a `READ_CEILING`
    /// prefix.
    ///
    /// **`Policy::for_project` has since landed and this test did not fail, which is the fact worth
    /// recording rather than the promise it replaces.** The paragraph here said the single list
    /// would be what closed the gap. It is not, and the second half below measures why: the project
    /// list widens the BASH door by name, and `submit` — the typed door — reads `runtime.policy`,
    /// the machine default, because `POST /github/requests` carries no project. So a project that
    /// declares every operation it may declare disagrees with the typed door about exactly the same
    /// eleven. Decision #6 remains the only thing that closes it.
    ///
    /// **And the second arm is a REMINDER, not a tripwire — which this paragraph, of all paragraphs,
    /// must not get wrong twice.** It was written closing with "what will fail, correctly and loudly,
    /// on the day the typed door learns which project it is acting for", which is the same species of
    /// promise the paragraph exists to retract. It will not fail then: the closure below hardcodes
    /// `widest.action_is_autonomous(...)` and never calls `submit`, so wiring `submit` to a project
    /// changes nothing here until somebody edits that closure. What the arm does is state today's
    /// measurement where whoever does that wiring will be standing, with a comment inside the closure
    /// telling them which line to change. That is worth having and it is not a guard.
    #[tokio::test]
    async fn the_map_names_an_operation_and_does_not_yet_make_the_two_doors_agree() {
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: ACTION_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
        });
        let operations = Op::all();

        let in_bash: Vec<&str> = operations
            .iter()
            .filter(|op| widest.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            in_bash,
            ["run_list", "run_status", "pr_list", "workflow_list"],
            "the Bash door is the four structural reads and nothing else"
        );

        // The typed door, for the same sixteen, as `submit` decides it.
        let typed = |op: &Op| match op {
            Op::Read(_) => true,
            Op::Act(act) => widest.action_is_autonomous(act.kind()),
        };
        let disagreeing: Vec<&str> = operations
            .iter()
            .filter(|op| typed(op) != widest.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            disagreeing,
            [
                // Reads the ceiling refuses because they carry a stranger's words, and `run_logs`,
                // which the ceiling would admit by prefix and `--log` takes back.
                "pr_view",
                "issue_view",
                "run_logs",
                "pr_diff",
                "pr_thread",
                "checks_for_ref",
                // Every action inside `ACTION_CEILING`: autonomous typed, never autonomous in Bash.
                "workflow_run",
                "run_rerun",
                "pr_create",
                "pr_comment",
                "issue_close",
            ],
            "eleven of sixteen, and `api_read` agrees only because both doors refuse it"
        );

        // And the same square again, for a project that declared everything it MAY declare — the
        // widest per-project policy there is, laid over the widest machine one. Same eleven, because
        // `for_project` reaches only the Bash door.
        let pool = test_pool().await;
        for kind in declarable_ops() {
            crate::project_policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let declared = widest.for_project(&pool, "alpha").await;
        let typed_for_project = |op: &Op| match op {
            // **THIS is the line to change when `submit` learns which project it is acting for**,
            // and nothing will fail to tell you so — the header says why that is a reminder and not
            // a guard. `widest` is written deliberately, because `submit` reads `runtime.policy` and
            // never a project's: swap it for `declared` on that day and the assertion below is the
            // measurement of how much the gap actually closed.
            Op::Read(_) => true,
            Op::Act(act) => widest.action_is_autonomous(act.kind()),
        };
        let still_disagreeing: Vec<&str> = operations
            .iter()
            .filter(|op| typed_for_project(op) != declared.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            still_disagreeing, disagreeing,
            "declaring everything a project may declare moves the Bash door and not the typed one"
        );
    }

    /// The pair that makes the map a map and not a prefix table, asked the way a person types it.
    #[test]
    fn one_subcommand_two_operations_and_a_flag_between_them() {
        assert_eq!(op_kind_of_gh_command("gh run view 123"), Some("run_status"));
        assert_eq!(
            op_kind_of_gh_command("gh run view --log 123"),
            Some("run_logs")
        );
        // The flag after the positional, which is where a person actually puts it.
        assert_eq!(
            op_kind_of_gh_command("gh run view 123 --log"),
            Some("run_logs")
        );
        // pflag's other spelling for a boolean — the one `REFUSED_READ_FLAGS` had to learn too.
        assert_eq!(
            op_kind_of_gh_command("gh run view 123 --log=true"),
            Some("run_logs")
        );
        assert_eq!(op_kind_of_gh_command("gh pr view 7"), Some("pr_view"));
        assert_eq!(
            op_kind_of_gh_command("gh pr view --comments 7"),
            Some("pr_thread")
        );

        // A flag carrying a caller's value names nothing and takes nothing away, in either spelling.
        assert_eq!(
            op_kind_of_gh_command("gh run view -R owner/name 123"),
            Some("run_status")
        );
        assert_eq!(
            op_kind_of_gh_command("gh run view --repo=owner/name 123"),
            Some("run_status")
        );
        // Case-sensitive about the flag, exactly as `read_is_autonomous` is, and for its reason:
        // `gh` would not read `--LOG` as `--log` either.
        assert_eq!(
            op_kind_of_gh_command("gh run view --LOG 123"),
            Some("run_status")
        );
        // Past the terminator it is a positional and not a flag.
        assert_eq!(
            op_kind_of_gh_command("gh run view -- 123"),
            Some("run_status")
        );
    }

    /// The map is an allowlist because the operations are a closed set, so everything else is
    /// nameless — including the four reads `READ_CEILING`'s doc refuses by name.
    #[test]
    fn a_command_no_operation_builds_has_no_name() {
        for command in [
            "gh auth token",
            "gh auth status",
            "gh secret list",
            "gh variable list",
            "gh run download 1",
            // The whole-word compare `read_is_autonomous` needs a prefix rule for.
            "gh run listen",
            "gh runlist",
            "gh",
            "git status",
            "",
        ] {
            assert_eq!(op_kind_of_gh_command(command), None, "{command:?}");
        }
    }

    /// **A flag that changes what comes back changes which operation it is.**
    ///
    /// `--log-failed` is the case the derivation cannot reach: no `ReadOp` builds it, so no form
    /// names it, and without the guard `gh run view --log-failed 1` is named `run_status` — a
    /// `ReadsOwn` operation, inside `READ_CEILING`, and one a project may declare — while returning
    /// the log of a failed step, which is a stranger's words. The map would be a prefix table with
    /// respect to exactly the spelling its own doc says a prefix table gets wrong.
    ///
    /// `REFUSED_READ_FLAGS` already holds that knowledge, under its own heading: *«The first seven
    /// change the KIND of thing that comes back»*. So the map reads that list rather than growing a
    /// second one, and answers `None` — the same rule the tie-break applies, one place further. A
    /// guess is a worse answer than no answer when the caller is deciding autonomy with it.
    ///
    /// The test above covers `gh auth token` and `gh secret list` and not one flag-bearing line,
    /// which is why this gap was untested as well as unhandled.
    #[test]
    fn a_flag_that_changes_what_comes_back_takes_the_name_away() {
        for command in [
            // The one that motivated the guard, in all three spellings a person reaches for.
            "gh run view --log-failed 1",
            "gh run view 1 --log-failed",
            "gh run view --log-failed=true 1",
            // The rest of the constant, on the prefixes the ceiling does admit.
            "gh run view --jq .jobs 1",
            "gh pr list --json body",
            "gh pr list --limit 1000",
            "gh pr list -L 1000",
            "gh pr list --search in:body secret",
            "gh pr list -t {{.body}}",
        ] {
            assert_eq!(op_kind_of_gh_command(command), None, "{command:?}");
        }

        // The flag an operation NAMES is the flag that names it. `--log` is on that constant too,
        // and `run_logs` is the row that exists to carry it — the exemption is what keeps a refusal
        // written for prefixes from eating the operation a name can spell.
        assert_eq!(
            op_kind_of_gh_command("gh run view --log 1"),
            Some("run_logs")
        );
        // `--comments` is not on the constant at all, so the other pair is untouched.
        assert_eq!(
            op_kind_of_gh_command("gh pr view --comments 7"),
            Some("pr_thread")
        );
        // A flag that only chooses WHICH takes nothing away, and `-l` is `--label` while `-L` is
        // `--limit`: the case sensitivity `read_is_autonomous` keeps, kept here for the same reason.
        for (command, kind) in [
            ("gh pr list --state open", "pr_list"),
            ("gh pr list --author octocat", "pr_list"),
            ("gh pr list -l bug", "pr_list"),
            ("gh run list --branch main", "run_list"),
        ] {
            assert_eq!(op_kind_of_gh_command(command), Some(kind), "{command:?}");
        }
    }

    /// What makes "the most switches accounted for" a safe way to break a tie: inside one
    /// subcommand no two operations carry the same number of switches, so the winner is never a coin
    /// toss.
    ///
    /// **It rules out one tie and not every tie, and the difference is worth stating rather than
    /// implied.** `matches` needs the subcommand to be a PREFIX of the line, so two forms whose
    /// subcommands are of different lengths — a hypothetical `gh api` beside a `gh api graphql` —
    /// can both match, and this loop walks past that pair on the `continue`. That case is safe
    /// without being asserted here: the sort's first key is the subcommand's length, so the more
    /// specific form wins, which is the answer anybody would want. What would NOT be safe is two
    /// forms of the same shape, and that is the tie this rules out.
    #[test]
    fn no_two_operations_wear_the_same_gh_form() {
        let forms = gh_forms();
        assert_eq!(forms.len(), Op::all().len(), "one form per operation");
        for (index, one) in forms.iter().enumerate() {
            for other in &forms[index + 1..] {
                if one.subcommand != other.subcommand {
                    continue;
                }
                assert_ne!(
                    one.switches.len(),
                    other.switches.len(),
                    "{} and {} share `gh {}` and nothing tells them apart",
                    one.kind,
                    other.kind,
                    one.subcommand.join(" ")
                );
            }
        }
    }

    /// The operation the ceiling had been granting to nobody: structural, and matching its prefix.
    #[test]
    fn workflow_list_is_a_structural_read_the_ceiling_already_admitted() {
        let op = ReadOp::WorkflowList { repo: repo() };
        assert_eq!(op.kind(), "workflow_list");
        assert_eq!(op.effect(), ToolEffect::ReadsOwn);
        assert_eq!(op.argv(), vec!["workflow", "list", "--repo=owner/name"]);

        // The prefix was always there; what is new is that a typed operation reaches it.
        assert!(READ_CEILING.contains(&"gh workflow list"));
        let policy = policy_from(Some("autonomous_reads:\n  - gh workflow list\n"));
        assert!(policy.read_is_autonomous(&format!("gh {}", op.argv().join(" "))));

        // And it is buildable from the flat parameters, or the tool could not ask for it.
        assert_eq!(
            ReadOp::from_request(ReadRequest {
                operation: "workflow_list".to_owned(),
                repo: "owner/name".to_owned(),
                id: None,
            }),
            Ok(op)
        );
    }
}
