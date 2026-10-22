//! Which tools each box serves, the admission rule, and the worktree, ticket and client a box's calls are held to.

use super::*;

/// The tools a turn answered by a model on this machine may be offered.
///
/// Every `ReadsOwn` tool, the two mail reads, plus `create_run` and `create_job`.
///
/// The mail reads are here because being asked what arrived is half of what a chat on a phone is
/// for, and they are the reason `local_agent.rs` carries a barrier rather than a fixed list. A turn
/// that reads mail has a stranger's words in its context, and from that moment `run_turn` refuses
/// every `Acts` tool for the rest of the turn — the rule `ToolEffect::ReadsUntrusted` already
/// states and that `hooks.rs` already enforces for a cloud run. Without it, "read this mail" and
/// "start a job" in one turn would let a sender write the job.
///
/// `list_files` is `ReadsUntrusted` too and is deliberately NOT here: a filename is a poor thing to
/// answer a chat with, and every untrusted tool added widens the surface for no gain.
///
/// The write half is the judgement, and it stops short of two things. `approve_proposal`,
/// `reject_proposal`, `cancel_run` and `set_kill` are absent because a local turn is a chat window
/// on a phone and those are the controls somebody reaches for when something is going wrong; they
/// stay where the person can see what they are agreeing to. `vcs_request` is absent for the reason
/// stated below it in `TOOL_EFFECTS`: it is the only effect on this server that outlives the daemon
/// and that its owner cannot take back from here.
///
/// `list_teams` is here because `create_job` is, and only because of that. `create_job` now takes a
/// `team_id`, and a turn that may start a job but cannot learn which teams exist can only name one
/// by guessing — which the daemon refuses. Offering the act without the read that makes it nameable
/// is offering a parameter nobody on a phone can fill in.
pub const LOCAL_TOOLS: &[&str] = &[
    "create_job",
    "create_run",
    // Here and on no other list, for now. A chat is one turn the owner is watching, and the owner
    // saying "remember that the daemon holds nucleos-core.exe" is the shortest path there is from a
    // lesson to the layer. A team or a council declaring is a wider decision — several agents
    // launched by one sentence, each able to queue a change to every later run's brief — and it
    // belongs to whoever takes it, in writing, here.
    "declare_refinement",
    "get_budget",
    "get_email",
    "get_email_queue",
    "get_job",
    "get_kill",
    "get_run",
    "latest_models",
    "list_jobs",
    "list_projects",
    "list_proposals",
    "list_teams",
    // A chat that can teach the layer can also ask what people approved into it.
    "recall",
    "vcs_ticket",
];

/// The tools a council seat may be offered, whichever machine answers it.
///
/// Named rather than computed, and the difference is what the list is for. "Everything that is not
/// `Acts`" would be shorter and would hand a council every tool added to this server from now on,
/// decided by whoever added it. A seat is a agent answering somebody's question with N siblings
/// running beside it, so the surface it gets is a decision taken here, once, in writing.
///
/// It is what this machine already knows, and NOTHING that acts. A seat reads runs, proposals, the
/// budget, the kill switch, the VCS queue, the mail and the files folder; it starts no work, lifts
/// no approval and touches no switch. `create_run` and `create_job` are on `LOCAL_TOOLS` and
/// deliberately absent here: a chat is one turn a person is watching, and a council is up to eight
/// agents launched by one sentence.
///
/// `list_files` IS here where it is absent from `LOCAL_TOOLS`, and the asymmetry is deliberate: a
/// filename is a poor thing to answer a chat with, and a good thing to answer "what has arrived
/// about X" with when the seat can then read the mail it came from.
///
/// **`web_search` and `web_read` are absent, and they are the interesting absence** — both are
/// `ReadsUntrusted` rather than `Acts`, so no rule below excludes them and this list is the only
/// thing that does. Two reasons, and the second is the one that settles it. A council fans one
/// question into up to eight agents, so a network door on each multiplies the egress of asking a
/// question by eight. And an all-local roster is meant to be the way a question stays on this
/// machine — a `kind: local` seat holding `web_search` would put the question on the network anyway,
/// which makes the roster stop being the statement of where the question goes. If a seat ever needs
/// the web, that is a decision to take once, here, with the owner having asked for it.
///
/// The taint rule (`ReadsUntrusted` then no `Acts`) still applies on top and is redundant here by
/// construction — there is no `Acts` on this list for it to refuse. Two independent reasons for the
/// same refusal is what one wants at a boundary like this.
/// `every_council_tool_only_reads` holds this list to `TOOL_EFFECTS`, so reclassifying a tool as
/// `Acts` without removing it from here fails the gate.
///
/// `vcs_ticket` is absent for a different reason from either: it is the read-back half of
/// `vcs_request`, and a seat that cannot queue an operation has nothing of its own to read back.
pub const COUNCIL_TOOLS: &[&str] = &[
    "get_budget",
    "get_email",
    "get_email_queue",
    "get_kill",
    "get_run",
    "latest_models",
    "list_files",
    "list_projects",
    "list_proposals",
];

/// The tools a team agent may be offered — a director or a specialist, cloud or local.
///
/// **This list is economy; `auth::TEAM_ROUTES` is the boundary.** Narrowing `--allowedTools` to it
/// stops the model from ever seeing a tool it would only be refused: `runner.rs` otherwise grants
/// `mcp__nucleos__*` wholesale, a specialist calls `create_run`, takes a 403 and burns its turns —
/// the failure mode `runner.rs` already documents, arriving by a different road.
///
/// **The two lists must not diverge, and `every_team_tool_has_a_route` is what holds them
/// together.** A tool offered to the model and refused by the token is a rain of 403s nobody traces
/// back to its cause; a tool refused to the model and permitted by the token is a boundary nobody
/// is testing.
///
/// It is written out and not computed for the reason `COUNCIL_TOOLS` gives: "everything that is not
/// `Acts`" would hand every future tool on this server to a department, decided by whoever added it.
///
/// Beside the council's list it gains three and loses four. `read_team_file` is new and is the one
/// tool of the teams design. `web_search` and `web_read` are the deliberate divergence — a council
/// answers from the state of this machine, while a department investigates the world, and one that
/// cannot open a page answers from what it half-remembers. `get_budget` and `get_kill` are gone
/// because a department is not convened to answer about the machine, and `list_projects` and
/// `list_proposals` with them: those are the state of the house, a council's subject and not a
/// marketing department's.
/// The seventh and eighth entries are the alçada, and they are the only `Acts` a department will
/// ever hold. (Seventh AND eighth: this paragraph said "the seventh entry" and named one for as long
/// as there was one, and `propose_teammate` arrived beside it without the sentence moving.)
/// `propose_action` performs nothing — it records an intention the core carries out later, if a
/// human agrees — which is what lets one name cover every action a department may ever be granted
/// instead of one name per action. It is graded `Acts` all the same, and that grading is the
/// point: a specialist that has read a web page or a colleague's file loses it for the rest of the
/// turn, which is exactly the door that must close.
/// `send_team_note` is the ninth, and it is the one entry here that is neither a read nor an ask.
/// It writes into the department's own state — a row that another node of the SAME run will be
/// handed — which is what makes it `WritesOwn` and not `Acts`. That grading is load-bearing rather
/// than cosmetic: six of the eight names beside it are `ReadsUntrusted`, so as an action it would be
/// shut by the caller's own first `web_read`, and a researching specialist would lose the ability to
/// tell a colleague the one thing it was convened to find out. What pays for it is at the other end — reading a note taints the
/// receiving node, so the stranger's words travel WITH the message instead of being refused at the
/// source, and the blast radius stays inside this list.
pub const TEAM_TOOLS: &[&str] = &[
    "get_email",
    "get_email_queue",
    "latest_models",
    "list_files",
    "propose_action",
    // Offered to every team agent and answered only for the director. The narrowing happens in the
    // handler, against `team_runs.director_run_id`, because a team's key names the RUN and both
    // nodes present the identical one. A specialist that calls it is told so in a sentence it can
    // act on — which is better than hiding the tool from a list the two nodes share.
    "propose_teammate",
    "read_team_file",
    "report_to_owner",
    "send_team_note",
    // Director-only like `propose_teammate`, and narrowed the same way, in the handler.
    "suggest_model",
    "web_read",
    "web_search",
];

/// The tools only a job node can answer, kept out of the chats' door.
///
/// A finding belongs to one running job node. Offering it through `LOCAL_TOOLS` would give a chat
/// that authority.
///
/// `verify` and `verify_status` are listed too, so a job node can verify its own worktree (they are
/// in `--allowedTools` through this list) — but, unlike `note_finding`, they are also offered to
/// every other box: see `EVERY_BOX_TOOLS`. `record` rides with them: a job node files its workflow
/// ledger records against the same worktree it verifies.
pub const JOB_NODE_TOOLS: &[&str] = &["note_finding", "record", "verify", "verify_status"];

/// The members of `JOB_NODE_TOOLS` that `McpBox::All` serves as well. A chat or a plain run has a
/// worktree of its own to verify and the daemon is the only thing that may run verification, so
/// these are not a job node's privilege the way a finding is. `record` is served wherever `verify`
/// is, because the workflow that verifies is the one that keeps the ledger, in whichever box it
/// runs.
pub const EVERY_BOX_TOOLS: &[&str] = &["record", "verify", "verify_status"];

/// What a team agent's box serves by default: today's `TEAM_TOOLS` plus the door that asks for more
/// and `read_context`, the door onto the context files the agent was given.
pub const TEAM_BASE: &[&str] = &[
    "get_email",
    "get_email_queue",
    "latest_models",
    "list_files",
    "propose_action",
    "propose_teammate",
    "read_context",
    "read_team_file",
    "report_to_owner",
    "request_tool",
    "send_team_note",
    "suggest_model",
    "web_read",
    "web_search",
];

/// What a team agent's box may be granted on top of `TEAM_BASE`. Empty this phase, tool by tool:
/// - every `Acts` tool is out (decisions.md 2026-08-16: the team surface stays read-only);
/// - project reads, shadow reads, job tools, `get_council`/`list_*`, budget and kill are out by the
///   teams design and pinned by tests;
/// - `github_read` rides the owner's Control door (decisions 2026-08-20);
/// - `browser_*` need a `project_id` a team lacks;
/// - `verify`/`verify_status`/`record` need a worktree;
/// - `vcs_ticket` needs a `vcs_request`;
/// - `recall` is the first candidate once knowledge scoping is agent/team-aware.
pub const TEAM_EXTRAS: &[&str] = &[];

/// What a job node's box serves by default: today's `JOB_NODE_TOOLS` plus the request door and
/// `read_context`.
pub const JOB_NODE_BASE: &[&str] = &[
    "note_finding",
    "read_context",
    "record",
    "request_tool",
    "verify",
    "verify_status",
];

/// What a job node's box may be granted on top of `JOB_NODE_BASE`. `web_read` is `ReadsUntrusted`
/// and needs no human-approval path.
pub const JOB_NODE_EXTRAS: &[&str] = &["web_read"];

/// Names only the narrowed boxes serve; the unboxed server never announces them.
pub const BOX_ONLY_TOOLS: &[&str] = &["read_context", "request_tool"];

/// Where a tool stands against one box's loadout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Base,
    Extra,
    Outside,
}

/// Sorts one tool into the base, the extras, or outside the loadout. Pure.
pub fn box_admission(base: &[&str], extras: &[&str], tool: &str) -> Admission {
    if base.contains(&tool) {
        Admission::Base
    } else if extras.contains(&tool) {
        Admission::Extra
    } else {
        Admission::Outside
    }
}

/// The base and extras of a box that has a loadout; `None` for the boxes that have none.
pub fn box_lists(served: &McpBox) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match served {
        McpBox::Team(_) => Some((TEAM_BASE, TEAM_EXTRAS)),
        McpBox::JobNode(_) => Some((JOB_NODE_BASE, JOB_NODE_EXTRAS)),
        McpBox::All | McpBox::Worktree(_) => None,
    }
}

/// Whether a name is one of NucleOS's own tools (as opposed to a CLI builtin or a foreign one).
pub fn is_nucleos_tool(name: &str) -> bool {
    TOOL_EFFECTS.iter().any(|(tool, _)| *tool == name)
}

/// Whether a box announces and dispatches one name. `McpBox::All` is the whole server except the
/// named job-node tools that `EVERY_BOX_TOOLS` does not give back, and the box-only tools.
///
/// Lifted out of `NucleosTools::serves` when a second caller appeared that has no instance to ask:
/// `advertised_schema_chars` prices what a box WOULD announce, from a launch site that never builds
/// a server. Two copies of this three-line match is how the price and the surface would come to
/// disagree — and the disagreement would be silent in both directions, because neither side has any
/// way to observe the other.
pub(super) fn served_in_box(served: &McpBox, tool: &str) -> bool {
    match served {
        McpBox::All => {
            (!JOB_NODE_TOOLS.contains(&tool) || EVERY_BOX_TOOLS.contains(&tool))
                && !BOX_ONLY_TOOLS.contains(&tool)
        }
        // Every job-node spawn writes its `run_loadout` row; the hook and the routes decide the
        // extras from that row, so the box announces the base and the extras alike.
        McpBox::JobNode(_) => JOB_NODE_BASE.contains(&tool) || JOB_NODE_EXTRAS.contains(&tool),
        McpBox::Team(_) => TEAM_BASE.contains(&tool) || TEAM_EXTRAS.contains(&tool),
        McpBox::Worktree(_) => EVERY_BOX_TOOLS.contains(&tool),
    }
}

/// A path compared as the `worktree` fence compares it: a backslash read as `/`, trailing `/`
/// dropped, and case folded on Windows only (a Unix filesystem tells `A` from `a`).
pub(super) fn fence_key(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// The `worktree` argument a `verify` call goes out with, given the box and what the model asked.
///
/// The worktree box fills in its own path when none was asked and refuses any other before the
/// call reaches the daemon (the daemon validates again as the owner, so this is the first fence
/// and not the only one). The other boxes hand the argument through untouched.
pub fn worktree_for_call(served: &McpBox, asked: Option<&str>) -> Result<Option<String>, String> {
    let McpBox::Worktree(own) = served else {
        return Ok(asked.map(str::to_owned));
    };
    match asked {
        None => Ok(Some(own.clone())),
        Some(asked) if !asked.trim().is_empty() && fence_key(asked) == fence_key(own) => {
            Ok(Some(own.clone()))
        }
        Some(asked) => Err(format!(
            "this session may only verify its own worktree, {own}, not {asked:?}"
        )),
    }
}

/// What a `verify_status` answer becomes in this box. The worktree box reads only tickets of its
/// own worktree: a ticket of another worktree, or one that names none, is refused with the same
/// words as a ticket that does not exist, so the refusal does not reveal that it does. The other
/// boxes hand the answer through untouched.
pub fn ticket_for_box(
    served: &McpBox,
    ticket: i64,
    answer: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let McpBox::Worktree(own) = served else {
        return Ok(answer);
    };
    match answer["worktree"].as_str() {
        Some(theirs) if !theirs.is_empty() && fence_key(theirs) == fence_key(own) => Ok(answer),
        _ => Err(format!("no ticket {ticket}")),
    }
}

/// The daemon client a box starts with. The environment's token wins; only the worktree box, which
/// an IDE session starts by hand with no `NUCLEOS_DAEMON_TOKEN`, may fall back to the stored one.
/// The stored token is held in memory and never written into a launch file. Nothing stored, an
/// empty value or a read error leaves the original error standing.
pub fn client_for_box(
    served: &McpBox,
    from_env: Result<crate::daemon_client::DaemonClient, String>,
    stored: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<crate::daemon_client::DaemonClient, String> {
    let error = match from_env {
        Ok(client) => return Ok(client),
        Err(error) => error,
    };
    if !matches!(served, McpBox::Worktree(_)) {
        return Err(error);
    }
    match stored() {
        Ok(Some(token)) if !token.is_empty() => Ok(crate::daemon_client::DaemonClient::new(
            crate::daemon_client::daemon_url(),
            token,
        )),
        _ => Err(error),
    }
}
