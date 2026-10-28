//! The parameter structs each tool deserialises its arguments into.

use super::*;

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct RunParams {
    pub(super) project_id: String,
    pub(super) prompt: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct JobParams {
    pub(super) project_id: String,
    pub(super) prompt: String,
    /// Optional in the schema and inert in the daemon until Chunk 3. Kept in the schema now so the
    /// tool description can say what it will mean, rather than the schema changing under a model
    /// that has already learned the tool.
    pub(super) budget_usd: Option<f64>,
    pub(super) max_rounds: Option<i64>,
    /// Which team directs this job. Absent is the job every caller has always got: one checkout,
    /// one item at a time. Named, the director splits the work over its members, each in a worktree
    /// of its own — so this is the only field here that changes what actually runs, where the two
    /// above are still inert.
    ///
    /// A team id that names nothing is REFUSED by the daemon rather than quietly dropped, and that
    /// refusal is why there is no fallback to the sequential job: one that ran sequentially anyway
    /// would report `completed`, leaving "the parallelism I asked for never seems to happen" as the
    /// only symptom.
    pub(super) team_id: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct CouncilAskParams {
    pub(super) question: String,
    /// How many critique rounds to run, 1 to 3: after the blind ranking each seat sees the
    /// anonymised peer answers and revises its own. Omit it to use the owner's configured number;
    /// more rounds cost more model invocations.
    #[serde(default)]
    pub(super) rounds: Option<u32>,
    /// A role per seat, keyed by the seat's index as a string, e.g. {"1": "skeptic"}. Roles:
    /// proposer, skeptic, devils_advocate, fact_checker. Omit it and every seat answers plainly.
    #[serde(default)]
    pub(super) roles: Option<std::collections::BTreeMap<String, String>>,
}

/// No roster field, and its absence is the decision.
///
/// `POST /council` takes a per-question override and the shell offers one, but a turn reaching for
/// a council has just met a question it could not answer alone. Letting it also pick who gets asked
/// would let it assemble a panel that agrees with it, which is the one thing a second opinion is
/// for not doing. The roster stays where a person put it.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct CouncilGetParams {
    pub(super) council_id: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct IdParams {
    pub(super) id: i64,
}

/// Which jobs to list. Both fields optional, and both narrow: absent means "everything".
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct JobsListParams {
    /// Narrow to one project. Absent lists every project's jobs.
    pub(super) project_id: Option<String>,
    /// Only the jobs still going. Absent or false includes the finished ones.
    pub(super) live: Option<bool>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct KillParams {
    pub(super) engaged: bool,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct PathParams {
    /// Relative to the files folder's root. Absent or empty means the root itself.
    pub(super) path: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct SearchParams {
    /// What to search for, in plain words.
    pub(super) query: String,
    /// How many results. Absent means a sensible handful; the daemon caps it either way.
    pub(super) limit: Option<i64>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct UrlParams {
    /// The page to read. Must be http or https; loopback and private addresses are refused.
    pub(super) url: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserOpenParams {
    /// Which project this is for. Omit it to browse in the project this conversation belongs to,
    /// or in no project.
    #[serde(default)]
    pub(super) project_id: Option<String>,
    /// The page to open. https only.
    pub(super) url: String,
    /// True opens a window the person can see, with a chat panel on the right, on the project's
    /// profile; use it when the person will take part. Default headless.
    #[serde(default)]
    pub(super) visible: Option<bool>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserSessionParams {
    /// The session id browser_open gave back.
    pub(super) session_id: i64,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserSnapshotParams {
    /// The session id browser_open gave back.
    pub(super) session_id: i64,
    /// Ask for what changed since your last snapshot of this session instead of the whole page.
    /// Refs stay the same across snapshots, so what you already know stays true.
    #[serde(default)]
    pub(super) changes_only: Option<bool>,
    /// Read the page's words on from here, when the last snapshot came back `truncated`. Pass
    /// the `text_next` it gave you. Scrolling does not help: the cut is a budget on words, not
    /// a viewport.
    #[serde(default)]
    pub(super) text_from: Option<i64>,
    /// Read the page's controls on from here, when the last snapshot came back `truncated` with
    /// a `controls_next`. Prose and controls are bounded separately, so a page can run out of
    /// one and not the other.
    #[serde(default)]
    pub(super) controls_from: Option<i64>,
    /// Keep only the lines that say this, matched without regard to case against a control's role,
    /// name and value and against a paragraph's or a row's text. Use it instead of reading a long
    /// page you only need one thing from: a directory of two thousand links costs a whole turn to
    /// page through and nothing to search. The answer comes back marked `partial`, because a search
    /// that found two things is not a page with two things on it.
    pub(super) find: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserActParams {
    /// The session id browser_open gave back.
    pub(super) session_id: i64,
    /// One of: click, type, scroll, select, press, back, goto, upload.
    pub(super) kind: String,
    /// A ref from the most recent snapshot, such as "e5". Never a CSS selector, and never a ref
    /// you have not seen in a snapshot of THIS page. Required for click, type, select and upload.
    /// Leave it out to scroll the page itself, to send a key wherever the focus already is, or to
    /// go back or goto.
    #[serde(rename = "ref", default)]
    pub(super) element_ref: String,
    /// Only for "upload": what the file is called when the site receives it. A NAME - no folders,
    /// no "..", no drive letters. Give it something a person reading the record would recognise,
    /// because that name is what gets written down.
    #[serde(default)]
    pub(super) filename: Option<String>,
    /// The verb's argument: the characters for "type", the option's visible label for "select",
    /// the key's name for "press" (Enter, Tab, Escape, Backspace, Delete, Home, End, PageUp,
    /// PageDown, ArrowUp/Down/Left/Right - no modifiers), the direction for a page "scroll"
    /// (down, up, top, bottom; down if you say nothing), the url for "goto" - absolute, or
    /// relative to the page you are on - and the file's CONTENTS for "upload". You write the file
    /// here: there is no way to attach one that already exists on this machine, and a path is not
    /// something this accepts. For a file you cannot write out - one already on this machine, a
    /// PDF, a picture - ask for the wheel with browser_handoff instead and say that is what you
    /// need it for: the person's own window can attach it.
    pub(super) text: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserHandoffParams {
    /// The session id browser_open gave back.
    pub(super) session_id: i64,
    /// Why a person is needed, in one sentence. They read this before deciding.
    pub(super) reason: String,
}

/// A read of GitHub, flat.
///
/// Flat rather than the tagged union `github::ReadOp` serialises to, for the reason
/// `vcs::Op::from_request` gives: the caller is a model reading a description, and the union is the
/// right wire shape and the wrong prompt.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct GithubReadParams {
    /// One of: run_list, pr_list, workflow_list, run_status, run_logs, pr_view, issue_view,
    /// pr_diff, pr_thread, checks_for_ref.
    pub(super) operation: String,
    /// The repository, as owner/name.
    pub(super) repo: String,
    /// A run id for run_status and run_logs, a pull request number for pr_view, pr_diff and
    /// pr_thread, an issue number for issue_view, a ref for checks_for_ref. The three listings
    /// take none.
    pub(super) id: Option<String>,
}

/// An action on GitHub, flat. Wider than its reading sibling because the operations are.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct GithubActParams {
    /// One of: workflow_run, run_rerun, pr_create, pr_comment, issue_close, api_read.
    pub(super) operation: String,
    /// The repository, as owner/name. Every operation but api_read needs one.
    pub(super) repo: Option<String>,
    /// A run id for run_rerun, a number for pr_comment and issue_close.
    pub(super) id: Option<String>,
    /// pr_create only.
    pub(super) title: Option<String>,
    /// The text of a comment, or a pull request's description.
    pub(super) body: Option<String>,
    /// pr_create: the branch being merged INTO.
    pub(super) base: Option<String>,
    /// pr_create: the branch being merged FROM.
    pub(super) head: Option<String>,
    /// workflow_run: the workflow's display name, file name or id.
    pub(super) workflow: Option<String>,
    /// workflow_run: the branch or tag to run it on.
    #[serde(rename = "ref")]
    pub(super) git_ref: Option<String>,
    /// api_read only: the arguments to `gh api`, already separated. Never a command line — this module
    /// splits nothing.
    pub(super) args: Option<Vec<String>>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct VcsRequestParams {
    /// Which project's repository. Call list_projects if you do not know it.
    pub(super) project_id: String,
    /// What to do. Today the queue executes "merge" and nothing else.
    pub(super) operation: String,
    /// The branch being merged FROM.
    pub(super) source: Option<String>,
    /// The branch being merged INTO.
    pub(super) target: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct DeclareRefinementParams {
    /// `memory` for a fact about the project a later run would otherwise rediscover, `prompt` for a
    /// standing instruction, `skill` for how a recurring job is done here, `subagent` for a
    /// delegation worth repeating.
    pub(super) kind: String,
    /// One line, in the words somebody scanning a list would recognise it by.
    pub(super) title: String,
    /// The lesson itself, written for a run that has none of your context. Roughly the first 600
    /// characters are what reaches a later brief; the whole of it is kept and read in the app.
    pub(super) body: String,
    /// Why a later run is better off knowing. It is the sentence the person deciding reads, beside
    /// the text, and a declaration that does not explain itself is one that gets refused.
    pub(super) reasoning: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct RecallParams {
    /// Words to find in what the house or this run's project knows.
    pub(super) query: String,
    /// Optionally narrow the answer to `semantic`, `episodic` or `procedural` knowledge.
    pub(super) layer: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct FindingParams {
    /// One fact the next node of this job can check.
    pub(super) fact: String,
    /// Concrete records that support the fact; the daemon validates their shape and existence.
    pub(super) evidence: Vec<EvidenceRef>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct RequestToolParams {
    /// The name of the one NucleOS tool you would like.
    pub(super) tool: String,
    /// Why the work needs it.
    pub(super) reason: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct EvidenceRef {
    /// One of run, job_item, proposal, knowledge, project, command or gate.
    pub(super) t: String,
    /// The referenced record's identifier, whose JSON shape depends on its type.
    pub(super) id: serde_json::Value,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProposeActionParams {
    /// What to do: `send_email`, `file_document` or `calendar_event`.
    pub(super) kind: String,
    /// The action's own fields. `send_email` takes `to`, `subject` and `body`; `file_document`
    /// takes `path` and `content`; `calendar_event` takes `title`, `starts_at_local`
    /// (`2026-08-17T09:30:00`, local time, no offset), `duration_minutes` and `tz`
    /// (`Europe/Lisbon`).
    pub(super) payload: serde_json::Value,
    /// One line saying why, for the person who decides. Required.
    pub(super) why: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ReportParams {
    /// What to tell the owner, in your own words. It travels verbatim.
    pub(super) body: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct TeamNoteParams {
    /// Which colleague, by the id on the left of their line in your department's roster.
    pub(super) to: String,
    /// What to tell them, in your own words. It travels verbatim.
    pub(super) body: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProposeTeammateParams {
    /// What to call them, e.g. `Contracts lawyer`.
    pub(super) name: String,
    /// One line: what they are for. This is what a director reads to hand out work.
    pub(super) speciality: String,
    /// Their standing instructions, written as if addressing them.
    pub(super) prompt: String,
    /// `claude`, `codex` or `local`. Absent means yours.
    pub(super) engine: Option<String>,
    /// Absent means yours.
    pub(super) model: Option<String>,
    /// `mcp_only` or `none`. Absent means `mcp_only`.
    pub(super) tool_policy: Option<String>,
    /// Why this department needed somebody it does not have. Required — it is what the owner reads.
    pub(super) why: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct LatestModelsParams {
    /// `anthropic` or `openai`. Absent means both.
    pub(super) vendor: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct SuggestModelParams {
    /// One line: what the candidate is for.
    pub(super) speciality: String,
    /// Why this department needs them.
    pub(super) why: String,
    /// Their standing instructions, if you have drafted them.
    pub(super) prompt: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct VcsTicketParams {
    /// The id the queue gave back when the operation was submitted.
    pub(super) id: i64,
    /// Block until it finishes, up to about 45 seconds. Absent means answer immediately.
    pub(super) wait: Option<bool>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct VerifyParams {
    /// "check" runs the cheap static checks; "test" runs the tests.
    pub(super) kind: String,
    /// "own" verifies what this worktree changed; "scope" widens to what the changed paths touch;
    /// "full" is the whole suite.
    pub(super) scope: String,
    /// The worktree to verify, as an absolute path. Required outside a job node, where the daemon
    /// knows which worktree you are standing in.
    pub(super) worktree: Option<String>,
    /// Restrict the verification to these paths, relative to the worktree.
    pub(super) files: Option<Vec<String>>,
    /// The commit or branch to diff against. Absent means the project's integration branch.
    pub(super) base: Option<String>,
    /// The workflow task this verification is for, the same id you pass to `record`, so the
    /// task's verifications can be counted. Optional.
    pub(super) task_id: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct RecordParams {
    /// "metric", "rework", "intent" or "event".
    pub(super) kind: String,
    /// The workflow task this record belongs to. Use the same id for every record and every
    /// verify call of one task.
    pub(super) task_id: String,
    /// The workflow phase that produced the record, e.g. "plan", "execute" or "review".
    pub(super) phase: Option<String>,
    /// For a rework: the step that caught it, e.g. "verify", "review" or "red". Required on a
    /// rework, refused as blank.
    pub(super) caught_by: Option<String>,
    /// The record's own fields, as a JSON object of at most 16 KiB.
    pub(super) payload: serde_json::Map<String, serde_json::Value>,
    /// The worktree the task runs in, as an absolute path. Required outside a job node, where the
    /// daemon knows which worktree you are standing in.
    pub(super) worktree: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct VerifyStatusParams {
    /// The ticket `verify` gave back.
    pub(super) ticket: i64,
    /// Block until it finishes, up to about 45 seconds. Absent means block.
    pub(super) wait: Option<bool>,
}

/// Shared shape of three of the four project reads: a listing, a file's contents, and a diff all
/// take just the project and a path inside it. `project_grep` is not this — it also needs a query
/// — and has its own struct below rather than this one with an extra optional field bolted on.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProjectPathParams {
    /// Which project's repository. Call list_projects if you do not know it.
    pub(super) project_id: String,
    /// Relative to the project's own root. Absent or empty means the root itself.
    pub(super) path: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProjectCatParams {
    /// Which project's repository. Call list_projects if you do not know it.
    pub(super) project_id: String,
    /// The file, relative to the project's own root.
    pub(super) path: Option<String>,
    /// The first line to show, counting from 1. Absent means the start of the file.
    pub(super) offset: Option<usize>,
    /// How many lines to show at most. Absent means as many as fit in one answer.
    pub(super) limit: Option<usize>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProjectGrepParams {
    /// Which project's repository. Call list_projects if you do not know it.
    pub(super) project_id: String,
    /// Text to search for.
    pub(super) query: String,
    /// Narrow the search to this path. Absent or empty means the whole project.
    pub(super) path: Option<String>,
}

/// Just the project, for the two shadow reads. Neither takes a path or a query: a scoreboard and a
/// review queue are per-project totals, and narrowing either one would only hide the class that is
/// short of the bar.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct ProjectIdParams {
    /// Which project. Call list_projects if you do not know it.
    pub(super) project_id: String,
}

/// What the model chooses, and nothing more — see `send_to_chat`'s own doc for what it does not.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct SendToChatParams {
    /// The OTHER conversation to hand this message to. Never this one — asking for the
    /// conversation you are already in is refused, not a way to talk to yourself.
    pub(super) chat_id: String,
    /// What to say. Arrives at that conversation as its next turn, exactly as written here — it is
    /// not shown to the person on this end first.
    pub(super) text: String,
}

/// What the model chooses for `browser_agent`; the mother conversation is never a parameter.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct BrowserAgentParams {
    /// What the browser agent is to do, written so it can work without asking you anything.
    pub(super) task: String,
    /// The page to start on, an http or https address. Leave out to let the agent choose.
    #[serde(default)]
    pub(super) url: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct NodeOpenParams {
    /// The node to open this one under. Absent opens a new root.
    pub(super) parent_id: Option<String>,
    /// "root" for a node without a parent; "orchestrator" or "leaf" for a child.
    pub(super) kind: String,
    /// A short title.
    pub(super) title: String,
    /// What this node must achieve, in a sentence or two.
    pub(super) goal: String,
    /// Paths this node owns. They may not overlap a running sibling. Default none.
    #[serde(default)]
    pub(super) owned_paths: Vec<String>,
    /// The token budget for this node. Absent takes the daemon default.
    pub(super) budget_tokens: Option<i64>,
    /// The workflow task this node belongs to.
    pub(super) task_id: Option<String>,
    /// The session this tree belongs to, so `node_active` can find it again.
    pub(super) session_id: Option<String>,
    /// The chat this tree belongs to, so `node_active` can find it again.
    pub(super) chat_id: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct NodeUpdateParams {
    /// The node to update.
    pub(super) id: String,
    /// The new status of the node.
    pub(super) status: Option<String>,
    /// A decision to record on the node.
    pub(super) decision: Option<String>,
    /// The peak context size, in tokens, this node has reached.
    pub(super) context_peak: Option<i64>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct NodeCloseParams {
    /// The node to close.
    pub(super) id: String,
    /// The final status, e.g. "done" or "failed".
    pub(super) status: String,
    /// What happened, at most 1500 characters.
    pub(super) summary: String,
    /// A path to the handoff file. It must exist.
    pub(super) handoff_path: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct NodeTreeParams {
    /// The root of the tree.
    pub(super) root_id: String,
    /// "brief" (default) or "full".
    pub(super) view: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct NodeActiveParams {
    /// Find the open tree of this session.
    pub(super) session_id: Option<String>,
    /// Find the open tree of this chat.
    pub(super) chat_id: Option<String>,
}

// The `session_id` of the task tools is filled in by the NucleOS hook of an interactive session
// before the call runs; a run or a job node leaves it out (the daemon knows them by their key).

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct TaskListParams {
    /// Filled in by the NucleOS hook. Leave empty.
    pub(super) session_id: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct TaskClaimParams {
    /// What is being claimed: `master-red`, `test:<path>::<name>`, `file:<path>`, `spec-item:<spec>#<n>` or `topic:<slug>`.
    pub(super) key: String,
    /// One line saying what is wrong. Defaults to the key.
    pub(super) title: Option<String>,
    /// Paths (relative to the repo root) that only the owner may change when landing.
    pub(super) guard_paths: Option<Vec<String>>,
    /// Paths that merely hint where the problem is; they never block anyone.
    pub(super) hint_paths: Option<Vec<String>>,
    /// Filled in by the NucleOS hook. Leave empty.
    pub(super) session_id: Option<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
pub(super) struct TaskCloseParams {
    /// The task id `task_list` or `task_claim` gave back.
    pub(super) id: i64,
    /// Why: it is the task history.
    pub(super) note: String,
    /// Filled in by the NucleOS hook. Leave empty.
    pub(super) session_id: Option<String>,
}

#[cfg(test)]
mod browser_open_tests {
    use super::*;

    #[test]
    fn browser_open_params_accept_a_missing_project_id() {
        let params: BrowserOpenParams =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" }))
                .expect("project_id is optional");
        assert_eq!(params.url, "https://example.com");
        let schema = serde_json::to_value(rmcp::schemars::schema_for!(BrowserOpenParams)).unwrap();
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        assert!(
            !required.iter().any(|name| name == "project_id"),
            "project_id must not be required in the schema: {required:?}"
        );
    }
}
