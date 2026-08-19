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
//! daemon builds every argv itself. **`ActOp::Raw` carries `Vec<String>` rather than a line**, and
//! that is the variant where the law costs most and matters most: the one escape hatch for cases
//! nobody foresaw is exactly where a command string would undo it.

use crate::mcp_tools::ToolEffect;
use serde::{Deserialize, Serialize};

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
    /// **What `--` costs here, said out loud rather than discovered.** Every caller argument lands
    /// after the terminator, so `Raw` reaches `gh api` with a path and positional arguments and no
    /// flags — `-X DELETE` arrives as two more positionals and does not select a method. That is a
    /// real narrowing of "total capability" down to reads of the REST surface, and it is the
    /// deliberate reading of the rule that no caller value may ever act as a flag. Whoever wants the
    /// verb wants a typed variant for it, which is the same answer the rest of this enum gives.
    Raw {
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
            ReadOp::RunList { .. } | ReadOp::RunStatus { .. } | ReadOp::PrList { .. } => {
                ToolEffect::ReadsOwn
            }
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
            ActOp::Raw { .. } => "raw",
        }
    }

    /// Always `Acts`, and the constant answer is the point: this half is defined by acting, so a
    /// variant here can never be graded down into something the untrusted barrier lets through.
    pub fn effect(&self) -> ToolEffect {
        ToolEffect::Acts
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
            ActOp::Raw { args } => {
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
            ActOp::Raw { args: Vec::new() },
        ];
        for operation in &every {
            match operation {
                ActOp::WorkflowRun { .. }
                | ActOp::RunRerun { .. }
                | ActOp::PrCreate { .. }
                | ActOp::PrComment { .. }
                | ActOp::IssueClose { .. }
                | ActOp::Raw { .. } => {}
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

    pub fn effect(&self) -> ToolEffect {
        match self {
            Op::Read(operation) => operation.effect(),
            Op::Act(operation) => operation.effect(),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            Op::Read(operation) => operation.argv(),
            Op::Act(operation) => operation.argv(),
        }
    }

    pub fn all() -> Vec<Self> {
        ReadOp::all()
            .into_iter()
            .map(Op::Read)
            .chain(ActOp::all().into_iter().map(Op::Act))
            .collect()
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

/// The `ActOp` kinds eligible for autonomy. `raw` is outside it and stays outside.
///
/// `pr_create` is outside too, and for its own reason rather than `raw`'s: opening a pull request
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

    pub fn autonomous_reads(&self) -> &[String] {
        &self.reads
    }

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
        self.reads.iter().any(|prefix| {
            normalized == *prefix || normalized.starts_with(&format!("{prefix} "))
        })
    }

    /// Whether an operation of this `kind()` is executed without asking. Everything else becomes a
    /// proposal a person approves, and the turn carries on either way.
    pub fn action_is_autonomous(&self, kind: &str) -> bool {
        self.actions.iter().any(|allowed| allowed == kind)
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

    /// The law, in the variant where it costs most and matters most. A `Raw` carrying a line would
    /// be "never a command string" undone by the one variant that exists for the cases nobody
    /// foresaw.
    #[test]
    fn raw_carries_separate_arguments_and_never_a_line() {
        let op = ActOp::Raw {
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
                    !part.starts_with("--") || part.contains('=') || part == "--log" || part == "--",
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
        assert!(config.enabled, "an absent file may not switch the pillar off");
        assert!(config.autonomous_reads.is_empty());
        assert!(config.autonomous_actions.is_empty());
    }

    /// The file may narrow the ceiling and may never widen it. It asks for two things the ceiling
    /// does not have and one it does, and keeps the one it does.
    #[test]
    fn the_file_may_narrow_the_ceiling_and_never_widens_it() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n  - gh auth token\n\
             autonomous_actions:\n  - pr_comment\n  - raw\n",
        ));
        assert_eq!(policy.autonomous_reads(), ["gh run list"]);
        assert_eq!(policy.autonomous_actions(), ["pr_comment"]);
        assert!(policy.read_is_autonomous("gh run list"));
        assert!(!policy.read_is_autonomous("gh auth token"));
        assert!(policy.action_is_autonomous("pr_comment"));
        assert!(!policy.action_is_autonomous("raw"));
    }

    /// The two that never pass, with the file explicitly asking for the opposite.
    #[test]
    fn gh_api_and_gh_auth_token_are_never_autonomous() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh auth token\n  - gh auth status\n  - gh secret list\n  \
             - gh variable list\nautonomous_actions:\n  - raw\n",
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
        let policy = policy_from(Some("autonomous_reads:\n  - gh pr view\n  - gh issue view\n"));
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

    /// The ceilings hold to the operations. An `ACTION_CEILING` entry naming a kind no `ActOp` has
    /// would be a line in the owner's file that grants nothing and says it grants something.
    #[test]
    fn every_action_ceiling_entry_names_a_real_operation() {
        let kinds: Vec<&str> = ActOp::all().iter().map(|op| op.kind()).collect();
        for entry in ACTION_CEILING {
            assert!(kinds.contains(entry), "{entry} is not an ActOp kind");
        }
        assert!(
            !ACTION_CEILING.contains(&"raw"),
            "`raw` is never eligible for autonomy"
        );
    }
}
