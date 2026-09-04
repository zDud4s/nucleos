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
        }
    }

    /// What reading this does to the turn.
    ///
    /// Structure — a status, a conclusion, a list of workflow names — is `ReadsOwn`. Prose somebody
    /// wrote is `ReadsUntrusted`, and it burns the turn's right to act, which is the trade the
    /// design accepts on purpose.
    pub fn effect(&self) -> ToolEffect {
        match self {
            // `WorkflowList` sits with the structural three and not with the prose three, and the
            // sentence above already decided it: "a list of workflow names" is the example this doc
            // gives for `ReadsOwn`. What comes back is the repository's own workflow files by name,
            // id and state — the same shape `RunList` and `PrList` return, chosen by whoever may
            // commit to the repository rather than by whoever may open a pull request against it.
            ReadOp::RunList { .. }
            | ReadOp::RunStatus { .. }
            | ReadOp::PrList { .. }
            | ReadOp::WorkflowList { .. } => ToolEffect::ReadsOwn,
            ReadOp::PrView { .. } | ReadOp::IssueView { .. } | ReadOp::RunLogs { .. } => {
                ToolEffect::ReadsUntrusted
            }
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
                repo,
                id: RunId::new("1").expect("the sample run id is valid"),
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
                | ReadOp::RunLogs { .. } => {}
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

    /// Every operation, for the partition test. Test-only, and said so rather than left to be
    /// discovered: `clippy --all-targets` computes dead code per target, so an item the tests alone
    /// use is dead in the bin build.
    #[allow(dead_code)]
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
    /// A run id for `run_status` and `run_logs`, a number for `pr_view` and `issue_view`, and
    /// nothing at all for the three listings. One field rather than three, because the model reading
    /// this has to fill in one thing and choosing which name it is called by is not that thing.
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
/// `pr_create` is outside too, and for its own reason rather than `api_read`'s: opening a pull request
/// publishes a title and a body under the owner's name to people who will read them as the owner's
/// words. That is not undoable by closing it.
pub const ACTION_CEILING: &[&str] = &["workflow_run", "run_rerun", "pr_comment", "issue_close"];

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
///   VALUE, and this comparison reads tokens.
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

/// What runs without asking.
///
/// Built once at startup from `.ai/github.yaml` and then immutable: it does no I/O after
/// construction, which is what lets `classifier::classify` take it by reference and stay pure. There
/// is deliberately no hot reload — a policy a run could reload is a policy a run could change in the
/// middle of itself.
///
/// **Both lists are intersections with a compiled ceiling, and never unions with one.** The file
/// chooses inside what the code fixes. `.ai/` is gitignored and travels with nobody, so it is
/// per-developer configuration no review ever sees; one line in it may not be the only thing between
/// an autonomous run and `gh api -X DELETE`.
///
/// It answers WHETHER, never HOW: `execute` builds the argv and this type never sees one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    reads: Vec<String>,
    actions: Vec<String>,
    digest: String,
}

impl Policy {
    /// Autonomous in nothing — what an absent, unreadable or malformed file produces, and what every
    /// test that is not about the policy itself should be given.
    pub fn empty() -> Self {
        Self {
            reads: Vec::new(),
            actions: Vec::new(),
            digest: digest_of(&[], &[]),
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
    /// to that in exactly the way an empty list does.
    pub fn from_config(config: &crate::config::GithubConfig) -> Self {
        if !config.enabled {
            return Self::empty();
        }
        let reads = narrow(&config.autonomous_reads, READ_CEILING, "read");
        let actions = narrow(&config.autonomous_actions, ACTION_CEILING, "action");
        let digest = digest_of(&reads, &actions);
        Self {
            reads,
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
    /// A refused flag is matched as `tok == flag || tok.starts_with("{flag}=")`. Equality alone
    /// would let `--json=body` through, and that is one character of difference between an
    /// implementation that works and one that looks like it does.
    ///
    /// **This decides the list and the flags and nothing else.** Whether the line is a single
    /// command at all, whether it redirects, whether it hides a second command behind a separator —
    /// those are `classifier.rs`'s guards, applied before this is ever consulted, and this function
    /// would be wrong to be read as covering them.
    pub fn read_is_autonomous(&self, command: &str) -> bool {
        if self.reads.is_empty() {
            return false;
        }
        let words = crate::classifier::shell_words(command);
        if words.iter().any(|word| {
            REFUSED_READ_FLAGS
                .iter()
                .any(|flag| word == flag || word.starts_with(&format!("{flag}=")))
        }) {
            return false;
        }
        let normalized = words.join(" ").to_ascii_lowercase();
        self.reads
            .iter()
            .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix} ")))
    }

    /// Whether an operation of this `kind()` is executed without asking. Everything else becomes a
    /// proposal a person approves, and the turn carries on either way.
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
}

/// PURE: one list intersected with its ceiling, sorted and deduplicated, warning about each entry it
/// had to drop.
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
fn digest_of(reads: &[String], actions: &[String]) -> String {
    let text = format!(
        "v1\nreads={}\nactions={}",
        reads.join(","),
        actions.join(",")
    );
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

    /// The three reads that carry a stranger's prose are marked, and the three that carry structure
    /// are not. Getting this backwards is the whole failure the per-operation effect exists to stop.
    #[test]
    fn prose_is_untrusted_and_structure_is_not() {
        for op in ReadOp::all() {
            let expected = match op.kind() {
                "pr_view" | "issue_view" | "run_logs" => ToolEffect::ReadsUntrusted,
                _ => ToolEffect::ReadsOwn,
            };
            assert_eq!(op.effect(), expected, "{}", op.kind());
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
    #[test]
    fn no_caller_value_reaches_argv_where_it_could_act_as_a_flag() {
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
                        || part == "--log"
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
