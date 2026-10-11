//! GitHub operation types, requests, validation and argv construction.

use super::*;

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
pub(super) fn required(
    value: Option<String>,
    operation: &str,
    field: &str,
) -> Result<String, String> {
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
pub(super) fn argv<const F: usize>(
    subcommand: &[&str],
    flags: [String; F],
    trailing: &[&str],
) -> Vec<String> {
    let mut built: Vec<String> = subcommand.iter().map(|part| (*part).to_owned()).collect();
    built.extend(flags);
    if !trailing.is_empty() {
        built.push("--".to_owned());
        built.extend(trailing.iter().map(|part| (*part).to_owned()));
    }
    built
}

pub(super) fn repo_flag(repo: &Repo) -> String {
    format!("--repo={}", repo.as_str())
}

/// PURE: the whole argv rule for the names that reach a command line as positional words.
///
/// Shared as a function rather than by making one type serve two roles, for the reason `vcs.rs`
/// gives about the same split: a repository and a branch answer to different authorities and
/// nothing says they must stay checked for the same three properties.
pub(super) fn argv_safe(value: &str, what: &str) -> Result<String, String> {
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
pub(super) fn numeric(value: &str, what: &str) -> Result<String, String> {
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
pub(super) fn single_line(value: &str, what: &str, ceiling: usize) -> Result<String, String> {
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
