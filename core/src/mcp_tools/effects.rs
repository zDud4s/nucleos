//! What each tool does to the world: `ToolEffect`, the effects table and the effect of one call.

#[cfg(any(test, feature = "testkit"))]
use super::*;

/// The tools a hosted turn may be offered — a third-party model reached over OpenRouter, not a
/// process this machine runs.
///
/// **An explicit allowlist, and not a filter over `ToolEffect`, and that distinction is the whole
/// point of this list existing at all.** The obvious design is "every `ReadsOwn` tool" — shorter,
/// self-maintaining, and wrong: `TOOL_EFFECTS` above records that `get_run` is `ReadsOwn` "only
/// lexically", because a triage run's stdout is a model's answer over somebody's mail. A rule keyed
/// on the table waves that straight through to a stranger's server, and waves through the next tool
/// whose output quietly carries somebody else's words on the day it is added — the exact laundering
/// `filter_outgoing`'s doc argues against for redaction, except here there is no second pass behind
/// it to catch what the classification missed. So this list is written out by hand, once, and
/// `every_hosted_tool_only_reads_its_own_state` holds it to the table from the OTHER direction —
/// catching a tool reclassified out from under it — while naming the exclusions below is what
/// catches the direction that test cannot: a tool that stayed `ReadsOwn` and simply should not have
/// been added here.
///
/// The six are the daemon's own bookkeeping and nothing that carries a word another person or
/// process wrote: the budget, the kill switch, and the job, project, proposal and team lists.
///
/// Named one by one, everything else is out, and why:
/// - `get_run` and `get_job` carry a run's stdout, which for a triage run is a local model's
///   answer over somebody's mail — see the paragraph above.
/// - `project_cat`, `project_grep`, `project_diff`, `project_ls` carry the owner's own source, which
///   can itself hold a vendored dependency, a saved page, or an issue body committed to a file.
/// - `github_read` and `web_*` carry other people's prose outright.
/// - the browser six carry a stranger's page, rendered.
/// - `shadow_queue` and `shadow_scoreboard` are `ReadsOwn` by the table but excluded anyway:
///   `LocalToolBox::call` has no dispatch arm for either, and a tool offered here that can only ever
///   answer "no local dispatch" is worse than a tool never offered at all.
/// - every one of the twelve `Acts` tools is out on a single, unconditional rule: a third-party
///   model must never be able to start a run, spend a budget, approve or reject a proposal, send
///   mail, move a branch, or touch the kill switch. There is no argument for any one of them that
///   would not equally argue for all twelve, so none is considered on its own merits.
pub const HOSTED_TOOLS: &[&str] = &[
    "get_budget",
    "get_kill",
    "list_jobs",
    "list_projects",
    "list_proposals",
    "list_teams",
];

/// What calling one NucleOS tool does to the turn that called it.
///
/// This partition exists because an orchestrator turn is the only agent that both reads a
/// stranger's words and holds the daemon's controls, and `hooks.rs` allows every tool on this
/// server unconditionally. The tool description on `get_email` tells the model the body is data —
/// which is worth saying and is not a boundary, because the thing being instructed is the thing
/// under attack. Knowing which tool brought third-party text into the turn and which tool would act
/// on it is what lets the daemon refuse the second after the first, whatever the model concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Returns text a third party chose. Always permitted — reading mail is what the turn is for —
    /// but it marks the run, and every `Acts` tool called afterwards is refused.
    ReadsUntrusted,
    /// Changes something outside the turn: starts or stops a run, lifts an approval the classifier
    /// withheld, works the kill switch. Refused once the turn has read third-party text.
    Acts,
    /// Reads only what NucleOS recorded about the owner's own work. Neither marks the turn nor is
    /// refused: it changes nothing, and the answer travels to the owner's own chat.
    ReadsOwn,
    /// Writes, and only into the state of the run this turn already belongs to. Reaches no
    /// network, starts no work, spends nothing and lifts no approval — so there is no third party
    /// it can aim at, and `permitted_after_untrusted` lets it through the barrier.
    ///
    /// It is not folded into `Acts` because a turn that reads the web FIRST and writes down what it
    /// found afterwards would have the write refused by its own first `web_read`, every time.
    ///
    /// The write is not laundering: what lands is read back by another node of the same run, and
    /// reading it taints that node, so the stranger's words travel with the message.
    WritesOwn,
}

/// Every tool this server exposes, and what calling it does to the turn. Ordered as the router
/// lists them, so the two can be read side by side.
///
/// The three mail entries are the whole reason the table exists. A body is the obvious carrier of
/// a stranger's words; the other two are less obvious and no less sender-chosen. `get_email_queue`
/// carries subjects, which arrive exactly as written and are capped nowhere on this path, next to
/// the triage summaries — a model's words about a stranger's. `list_files` returns filenames, and
/// for anything filed out of the mail the sender picked the filename. That the user's own uploads
/// now sit in the same folder does not make the listing trustworthy: one sender-chosen name in it
/// is enough.
///
/// `triage_email` is an action despite reading nothing back: it spends the budget, and a gate that
/// let a mail body choose when to spend money would be missing the point narrowly. `get_run` is
/// `ReadsOwn` only lexically — a triage run's stdout is a model's answer over mail — so `hooks.rs`
/// looks at WHICH run is named before it settles that one.
///
/// The two web entries are the same argument the mail ones make, arriving from a wider door. A page
/// is the obvious carrier; `web_search` is the less obvious one and belongs here for the same reason
/// `get_email_queue` does. A search result's title and snippet are written by whoever owns the page,
/// they arrive exactly as written, and ranking for a query somebody expects an agent to run is a
/// thing people already do on purpose. Classifying search as `ReadsOwn` would leave the cheapest
/// path — one poisoned result, never fetched — able to reach the kill switch.
///
/// `vcs_request` is the sharpest `Acts` on the list. Every other action here changes something
/// inside NucleOS — a run, a proposal, the kill switch — and the owner can undo all of them from
/// this same server. This one moves a branch in a repository other people build on: it is the only
/// effect on this list that outlives the daemon, and the only one its owner cannot take back from
/// here. `vcs_ticket` reads back what the owner's own queue did, and acts on nothing.
pub(super) const TOOL_EFFECTS: &[(&str, ToolEffect)] = &[
    ("approve_proposal", ToolEffect::Acts),
    // Convening spends money — up to nine model invocations on one question — and `council::start`
    // reads the budget before it writes a row, which is the same shape as `create_run`'s. Nothing
    // it starts touches the world outside this daemon, and it is still an act: what it spends is
    // the owner's, and `permitted_after_untrusted` reads this table BY NAME to keep a turn that has
    // just read a stranger's words from spending it.
    ("ask_council", ToolEffect::Acts),
    // The browser's six, all `ReadsUntrusted`, and the classification is an ASSERTION ABOUT THE
    // FENCE rather than an observation about the verbs (spec §6.1a). `browser_act` clicks and types;
    // under the fence of §6.2 nothing it does leaves the machine with a consequence — no non-GET
    // request, no download, no WebSocket, no new window — so what it produces is more of a
    // stranger's prose and no effect on the world. If the fence stops holding, this line becomes a
    // lie, which is why the gate group against a real Chrome is a gate on this registration and not
    // a nice-to-have.
    //
    // "More of a stranger's prose" went literally false for one of the six, and the correction is
    // worth making rather than reading past: `browser_look` produces a stranger's PICTURE. Same
    // classification for the same reason — it reads, and there is no verb on it that acts — but the
    // carrier is the one thing nothing downstream can inspect, where prose meets a redactor.
    // `filter_outgoing`'s image arm is where that price is argued and bounded.
    //
    // "No form submission" was on that list and was taken off, and a loosening gets spelled out
    // rather than quietly edited: a GET form submits now. It IS a document GET to a host the profile
    // admits, so the two rules that have always bounded a link — the method and the allowlist —
    // bound it unchanged, and it can carry nothing a link with a query string could not. What this
    // line never claimed is that no bytes travel: clicking a link has always sent a GET.
    //
    // # The second loosening, which is a real one, and the weakest line on this page
    //
    // A POST can now leave. Five things have to hold at once — it produces a document, it goes back
    // to the origin the page is on, a PERSON granted that origin permission to be written to, an act
    // on something the reading showed caused it, and it is written down — but the sentence above has
    // changed. "Nothing it does leaves the machine with a consequence" is no longer true; what is
    // true is that a consequence is bounded to an origin a person chose and is recorded when it
    // happens. Those are not the same claim, and this classification now rests on the second.
    //
    // Splitting the tool was considered and does not work. A `browser_submit` classified `Acts`
    // would be shut off by the rule that closes acting tools in a turn that has read a stranger's
    // words — and reading the page is how an agent knows where to press. The result would be a verb
    // that can never be used, which is not a safer arrangement but a broken one.
    //
    // So the honest statement is: this line is the weakest thing on this page, it is held up by the
    // grant being per-origin and human-given, by the act having to cause the submission, and by
    // `browser_writes` recording every one that leaves. Whoever attacks this design should attack
    // here. See `.ai/specs` for the argument in full and `fence/policy.go` for the rule.
    //
    // `browser_handoff` is here rather than `ReadsOwn`, and that is a correction worth keeping: it
    // spends a person's attention and proposes a host chosen by an agent whose context is full of
    // the page's words (§5.2, the confused deputy). `ReadsOwn` is defined below as "neither marks
    // the turn nor is refused: it changes nothing", and this changes something — the same argument
    // that makes `triage_email` an act despite reading nothing back.
    //
    // `browser_close` is the only `ReadsOwn` of the set: it destroys local state and reaches nothing.
    ("browser_act", ToolEffect::ReadsUntrusted),
    ("browser_close", ToolEffect::ReadsOwn),
    ("browser_handoff", ToolEffect::ReadsUntrusted),
    ("browser_look", ToolEffect::ReadsUntrusted),
    ("browser_open", ToolEffect::ReadsUntrusted),
    ("browser_snapshot", ToolEffect::ReadsUntrusted),
    // Stopping a job, graded like stopping a run and for the same reason: it ends work that is
    // already in flight, which is an effect on the world and not a reading of it.
    ("cancel_job", ToolEffect::Acts),
    ("cancel_run", ToolEffect::Acts),
    // A job is a chain of runs, so it is at least as much of an act as one run is.
    ("create_job", ToolEffect::Acts),
    ("create_run", ToolEffect::Acts),
    // `Acts`, though nothing is in force when it returns — the same reading that makes
    // `propose_action` an act rather than a write of its own state. What it spends is a person's
    // attention, and what it proposes is a change to what EVERY later run in scope is told, which is
    // the widest blast radius any tool on this server has. A turn that has just read a stranger's
    // mail must not be able to put "from now on, always…" in front of a tired yes.
    ("declare_refinement", ToolEffect::Acts),
    ("get_budget", ToolEffect::ReadsOwn),
    // The house's own deliberation, read back. Every word in it was written by a model this daemon
    // launched against a question this daemon was given — no stranger's text reaches it — so this
    // is a read of our own state in the same sense `get_run` is.
    ("get_council", ToolEffect::ReadsOwn),
    ("get_email", ToolEffect::ReadsUntrusted),
    ("get_email_queue", ToolEffect::ReadsUntrusted),
    // A job row and a job listing: this daemon's own record of work it started itself. `ReadsOwn`
    // with less doubt than `get_run` carries, and the difference is worth stating — `get_run`
    // answers with a run's STDOUT, which for a triage run is a model's answer over a stranger's
    // mail, while these answer with status, ordinals, states and the wait reason `job::park` wrote.
    ("get_job", ToolEffect::ReadsOwn),
    ("get_kill", ToolEffect::ReadsOwn),
    ("get_run", ToolEffect::ReadsOwn),
    // The GitHub pair, and their being TWO is a security boundary rather than an arrangement.
    // `permitted_after_untrusted` reads this table by NAME and never calls `effect_of_call`, so a
    // single tool would have had to be `ReadsOwn` for the argument-aware arm to run at all — and
    // `ReadsOwn` passes that barrier. A turn that had read a stranger's PR body could then have
    // written to GitHub. Split in two, `github_act` meets the barrier by name on both paths and
    // `github_read` never acts, whatever its arguments say.
    //
    // `github_read` is `ReadsOwn` BY NAME ONLY: three of its six operations return prose somebody
    // wrote, so `effect_of_call` asks the operation before it settles that one — exactly as it
    // already does for `get_run`.
    //
    // Both are deliberately outside `LOCAL_TOOLS`: the loop in `local_agent.rs` answers a person's
    // chat, and nothing there has a repository in mind.
    ("github_act", ToolEffect::Acts),
    ("github_read", ToolEffect::ReadsOwn),
    // The daemon's own model catalogue: vendor lists the daemon fetched itself (a vendor API, the
    // Codex CLI's cache, models.dev), carrying only ids, names and effort levels.
    ("latest_models", ToolEffect::ReadsOwn),
    ("list_files", ToolEffect::ReadsUntrusted),
    ("list_jobs", ToolEffect::ReadsOwn),
    ("list_projects", ToolEffect::ReadsOwn),
    ("list_proposals", ToolEffect::ReadsOwn),
    // The catalogue of departments. `ReadsOwn` without the doubt `get_run` and `github_read` carry:
    // every word of a team row — its mission, its members, its ceilings — was written by the owner
    // in the Teams tab, so there is no path by which a stranger's text arrives in this answer and
    // therefore nothing for `effect_of_call` to second-guess by argument.
    ("list_teams", ToolEffect::ReadsOwn),
    // Work nodes live in this machine own database and describe the caller own work, so the
    // reads are `ReadsOwn` and the writes `WritesOwn`.
    ("node_active", ToolEffect::ReadsOwn),
    ("node_close", ToolEffect::WritesOwn),
    ("node_open", ToolEffect::WritesOwn),
    ("node_tree", ToolEffect::ReadsOwn),
    ("node_update", ToolEffect::WritesOwn),
    // A finding writes into this job's own state, and only later nodes of the SAME job read it,
    // like `send_team_note`. As `Acts`, the node's own first read of third-party text would shut it
    // off and the feature would work only for a node that read nothing. What pays for the write is
    // at the receiving end: the label, the floor of one evidence item, the same-job fence, and death
    // with the job. This grade does not validate the content of the fact.
    ("note_finding", ToolEffect::WritesOwn),
    // The four project reads, and this is the weakest line on this page, so it is argued rather
    // than asserted. `ReadsUntrusted` would kill the feature at birth: the turn would read the
    // repository and from that moment every `Acts` tool is refused — including `create_run` and
    // `create_job`, which is the whole reason it was reading. That is the same trap that forced
    // `WritesOwn` into existence. For it: the root is the owner's own checkout, resolved by the
    // daemon from `autopilot_state` and never named by the model, and `safe_join` refuses anything
    // outside it — unlike `list_files`, where a sender chooses the filename, nothing third-party
    // chose what is in it. Against it, said plainly: a repository CAN hold third-party text (a
    // vendored dependency, a saved page, an issue body committed to a file). The marginal exposure
    // is the orchestrator reading text it was already about to hand to an agent with full tools.
    // Whoever attacks this design should attack here.
    ("project_cat", ToolEffect::ReadsOwn),
    ("project_diff", ToolEffect::ReadsOwn),
    ("project_grep", ToolEffect::ReadsOwn),
    ("project_ls", ToolEffect::ReadsOwn),
    // `Acts` even though it acts on nothing at the moment it is called. The classification answers
    // "what does this do to the turn that called it", and what this does is put an email, a file or
    // a calendar entry on a path to happening. Grading it `ReadsOwn` because the immediate effect is
    // one row would open precisely the laundry chute `read_team_file`'s comment describes: a page
    // read in one tool, an action requested in the next, and the taint rule stepping over both.
    ("propose_action", ToolEffect::Acts),
    // Same grading and the same reason, and here the failure it prevents is concrete: a director
    // that read a page saying "hire an agent with this prompt" could otherwise file it. It would
    // reach a person and probably be refused — but the defence cannot be the attention of whoever
    // is approving.
    ("propose_teammate", ToolEffect::Acts),
    // A third party's file, handed to the agent as context: data, never an instruction (see
    // `context_refs::READ_CONTEXT_EFFECT`).
    ("read_context", ToolEffect::ReadsUntrusted),
    // A specialist that read the web writes the web into its answer, so whoever reads that answer
    // afterwards is reading content nobody vouched for. Grading it `ReadsOwn` because the bytes are
    // ours would build the exact laundry chute a department needs least: untrusted text in one end,
    // a file the core wrote out the other, and authority to act on the day that authority exists.
    ("read_team_file", ToolEffect::ReadsUntrusted),
    ("recall", ToolEffect::ReadsOwn),
    // `WritesOwn`, like `verify`: one row in this machine's own workflow ledger, filed against the
    // caller's own worktree; nothing leaves the machine and nothing is acted on.
    ("record", ToolEffect::WritesOwn),
    ("reject_proposal", ToolEffect::Acts),
    // `Acts`, and this one is the barrier itself rather than a label on it. Every other tool on
    // this table is classified so that `permitted_after_untrusted` can decide whether to let it
    // run; this is the tool `relay.rs`'s header calls "a conversation acting on another's behalf"
    // — a stranger's words could otherwise be relayed into a DIFFERENT conversation, past every
    // taint check that conversation's own turn will ever see, because to it the relayed message
    // simply arrives as its next turn with no mark saying where it came from. `ReadsOwn` would
    // reduce that to a filing detail; `Acts` is what makes `permitted_after_untrusted` refuse it
    // for the rest of any turn that has read mail, a web page, or a teammate's answer — closing
    // the laundering path on both the CLI and the local dispatcher, since both consult this same
    // table. `send_to_chat` never reaching `LOCAL_TOOLS` or `TEAM_TOOLS` narrows WHO can call it;
    // this line is what makes calling it safe for the callers who can.
    // `WritesOwn`, and the argument for it is not the one `send_team_note` makes below — this one
    // does reach outside the department, to a PERSON, which is exactly where `send_to_chat` earns
    // its `Acts`.
    //
    // What separates them is that a department ALREADY speaks to its owner, without any barrier at
    // all: the delivery is a document the owner opens and reads, written by a director that has been
    // reading the folder, the web and its colleagues' answers all along. Grading this `Acts` would
    // put a lock on a side door standing beside an open main one — and it would bite constantly
    // rather than rarely, because the delivery node is told in its own prompt to read files with
    // `read_team_file`, which taints it. The tool would be offered to a director and refused to it
    // in the same breath, on nearly every run.
    //
    // What actually protects the owner here is not a grading but ATTRIBUTION: the words are drawn
    // quoted, named, and marked as a department's, never as the conversation's own. A person weighing
    // a message they can see the source of is the mechanism; a taint flag on a channel whose audience
    // is human would be theatre.
    //
    // The audience is the whole of it. Nothing runs, nothing is spent, and no model reads this — a
    // notice is deliberately absent from `recent_exchanges` and from the CLI's resumed session, so
    // there is no context for a stranger's words to be laundered INTO. That is what makes this a
    // report rather than a relay, and it is why `send_to_chat` one line down keeps its `Acts`.
    ("report_to_owner", ToolEffect::WritesOwn),
    // It writes a proposal row the owner decides; it grants nothing and reaches nothing outside.
    ("request_tool", ToolEffect::WritesOwn),
    // `WritesOwn` and NOT `Acts`, and the line below it is the reason the two differ. Both put words
    // in front of a model that did not write them; what separates them is what that model can then
    // do. `send_to_chat` lands in a conversation a PERSON reads, whose next turn holds the whole
    // surface of this machine — so laundering into it is an escalation, and it is barred at the
    // source. A team note lands on another node of the same department, holding the same eight
    // narrow tools, whose only power is to ask. The blast radius is bounded by `TEAM_TOOLS` itself.
    //
    // And the cost of getting this wrong is not symmetric. `Acts` here would be shut by the caller's
    // own first `web_read` — six of the eight tools a department holds are `ReadsUntrusted` — so the
    // tool would fire only for a specialist that read nothing, which is nearly never.
    //
    // What pays for it sits at the receiving end rather than here: a node handed a note is born
    // marked `read_untrusted` (`team::launch_specialist`), so the taint travels WITH the words. The
    // barrier is not skipped, it is moved one hop.
    ("send_team_note", ToolEffect::WritesOwn),
    ("send_to_chat", ToolEffect::Acts),
    ("set_kill", ToolEffect::Acts),
    // The two shadow reads. `ReadsOwn`, on the argument the project reads above set out, and the
    // honest half of that argument applies here too. A scoreboard row is the daemon's own tally of
    // its own classifier and carries nobody's words. A queued decision carries `tool_input` — the
    // argument a local agent of this machine proposed — which CAN quote text that agent read
    // somewhere else, exactly the exposure `get_run` is graded `ReadsOwn` "only lexically" for.
    // What keeps that acceptable is that neither of them is a step toward acting: the verdict is
    // not on this server, so a turn that reads them has nothing to reach for next.
    ("shadow_queue", ToolEffect::ReadsOwn),
    ("shadow_scoreboard", ToolEffect::ReadsOwn),
    // `ReadsOwn`: it asks this machine's own model adviser, on loopback, and files nothing. What it
    // sends is the director's own description of a candidate; what it answers is a model name.
    ("suggest_model", ToolEffect::ReadsOwn),
    // `WritesOwn`: a row in this machine's own claim table, keyed to the caller; nothing leaves the
    // machine and nothing is acted on. A claim is advisory until the land gate reads it.
    ("task_claim", ToolEffect::WritesOwn),
    ("task_done", ToolEffect::WritesOwn),
    ("task_list", ToolEffect::ReadsOwn),
    ("task_release", ToolEffect::WritesOwn),
    ("triage_email", ToolEffect::Acts),
    ("vcs_request", ToolEffect::Acts),
    ("vcs_ticket", ToolEffect::ReadsOwn),
    // `WritesOwn`: it enqueues work on this machine's own verification queue and writes its own
    // ticket and cache rows; nothing leaves the machine. `verify_status` only reads that ticket.
    ("verify", ToolEffect::WritesOwn),
    ("verify_status", ToolEffect::ReadsOwn),
    ("web_read", ToolEffect::ReadsUntrusted),
    ("web_search", ToolEffect::ReadsUntrusted),
];

/// Every tool name this server registers, in the router's own order.
///
/// `cfg(test)` because only tests ask, and they ask from more than one module: enumerating the
/// router is how an assertion covers a tool added tomorrow instead of one added by the person who
/// remembered to edit the test.
#[cfg(any(test, feature = "testkit"))]
pub fn every_tool_name() -> Vec<String> {
    NucleosTools::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect()
}

/// PURE: what one tool name does, by name alone.
///
/// A name absent from the table resolves to `Acts`, which is the fail-closed direction for a tool
/// that does not exist: refused after untrusted text rather than waved through. That default is a
/// backstop and not the guarantee — a tool ADDED to this server and left out of the table would
/// become `Acts` too, and if it happened to read mail it would bring a stranger's words into the
/// turn without marking it, which is the one failure that looks like nothing.
/// `every_registered_tool_is_classified` is what actually holds the table to the server, by failing
/// the moment the two disagree.
pub fn tool_effect(tool: &str) -> ToolEffect {
    match TOOL_EFFECTS.iter().find(|(name, _)| *name == tool) {
        Some((_, effect)) => *effect,
        None => ToolEffect::Acts,
    }
}

/// What one CALL does — the name, plus the one case where the arguments change the answer.
///
/// `tool_effect` above is a table lookup and cannot see that `get_run` is `ReadsOwn` by name and
/// not always by content: a triage run's stdout is a model's answer over mail a stranger wrote, and
/// the parse that bounds a verdict to a class and 200 stripped characters runs AFTER the raw stream
/// is stored. So what comes back through that tool was never put through it.
///
/// This exists because there were two dispatchers and one of them knew that. `hooks.rs` had this
/// rule inline and `LocalToolBox` answered from the table alone, so a local chat turn could pull a
/// triage run's stdout — a stranger's words — without the turn being marked, and then start work.
/// One function, both callers, and the drift is not expressible.
///
/// Unreadable arguments resolve to `ReadsUntrusted` and never to an error: "I could not tell" is
/// not "no" when the question is whether a stranger's words are about to enter the turn.
pub async fn effect_of_call(
    pool: &sqlx::SqlitePool,
    tool: &str,
    arguments: &serde_json::Value,
) -> ToolEffect {
    let effect = tool_effect(tool);
    if effect != ToolEffect::ReadsOwn {
        return effect;
    }

    match tool {
        "get_run" => run_read_effect(pool, arguments).await,
        "github_read" => github_read_effect(arguments),
        _ => effect,
    }
}

/// `github_read`, resolved by which operation was named.
///
/// The only arm here that needs no database: which GitHub reads carry a stranger's prose is a
/// property of the operation and not of any row, so `github::ReadOp::effect_of_kind` answers it
/// outright.
///
/// **`github_act` deliberately has no arm.** It is `Acts` in the table, so `effect_of_call`
/// short-circuits before reaching this match and its arguments are never read — which is correct,
/// because every operation it accepts acts. An arm here would be unreachable code implying a
/// question that has already been settled.
///
/// Unreadable arguments resolve to `ReadsUntrusted` and never to an error: this runs before the `parsed!` macro that refuses malformed
/// arguments, it returns a `ToolEffect` rather than a `Result`, and the question being asked is
/// whether a stranger's words are about to enter the turn — where "I could not tell" is not "no".
/// The malformed call is refused a moment later by `parsed!`, like any other.
pub(super) fn github_read_effect(arguments: &serde_json::Value) -> ToolEffect {
    arguments
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::integrations::github::ReadOp::effect_of_kind)
        .unwrap_or(ToolEffect::ReadsUntrusted)
}

/// `get_run`, resolved by which run was named.
pub(super) async fn run_read_effect(
    pool: &sqlx::SqlitePool,
    arguments: &serde_json::Value,
) -> ToolEffect {
    let Some(id) = arguments.get("id").and_then(serde_json::Value::as_i64) else {
        return ToolEffect::ReadsUntrusted;
    };
    match sqlx::query_scalar::<_, String>("SELECT mode FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(mode)) if mode == crate::integrations::email::TRIAGE_MODE => {
            ToolEffect::ReadsUntrusted
        }
        // `Ok(None)` is the one gap left, and it is left knowingly. A run that does not exist when
        // this asks, but exists as a triage run by the time the tool reads it, is classified own.
        // Closing it means holding a transaction across the loopback call, which trades a race the
        // model would have to win by naming an id that has not been issued yet for a lock held
        // across HTTP. The race it replaced was the real one — classification used to run twice,
        // after the content was already in the conversation, so a row deleted in between made a
        // stranger's words a clean turn — and that one is gone.
        Ok(Some(_)) | Ok(None) => ToolEffect::ReadsOwn,
        Err(error) => {
            tracing::warn!(
                run_id = id,
                %error,
                "could not resolve the mode of the run being read — treating it as third-party content"
            );
            ToolEffect::ReadsUntrusted
        }
    }
}
