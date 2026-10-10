//! The prompts each node kind is given: spec, plan, replan, implement and review.

use super::*;

/// What every planning node has to be told about who owns the history.
///
/// Measured, not guessed. Job 12 on 2026-08-08 asked for eight modules and said "I want to review
/// each one on its own" — a sentence about how to SPLIT the work. The plan node read it as a
/// sentence about git and wrote "commit X and Y as two separate commits" into all four items. Every
/// implement node then reached for `git commit`, which is a write and must ask, and under the
/// zero-approval policy the night runs on, all four items were skipped. Nothing was built.
///
/// The prompt is the right place for the fix rather than the classifier, because the command was not
/// misjudged: `git commit` genuinely writes and genuinely must ask. What was wrong is that the item
/// asked for it at all. `worktree::checkpoint` already commits the whole tree after every green
/// gate — an item that commits by hand is redoing the job's own work through the one door that
/// stops.
///
/// Stated as what happens rather than as a prohibition, deliberately. A node told only "do not
/// commit" invents a way around it; a node told the commit already happens has no reason to.
/// What the nodes that only LOOK have to be told about how to look.
///
/// Measured across four jobs on 2026-08-08, and always the same shape: a node whose whole task is to
/// inspect the tree reaches for a shell loop or a pipeline to do it — `for f in …; do cat "$f"; done`
/// three times, `git reflog`, `ls -la && … && find … | sort`. The classifier reads a LINE, not a
/// program, so none of those is recognised, and each one stopped its node dead.
///
/// It has `Read`, `Grep` and `Glob`, all of which run without asking and do the job better. Nothing
/// was missing except being told. Said as the consequence rather than as a rule, because the
/// consequence is what makes the choice obvious: this node is what the job spends to get an answer,
/// and giving it up is not free.
pub(super) const LOOK_WITH_THE_READING_TOOLS: &str = "You are running unattended, so anything that needs a person is not answered — it ends this \
     node. Look with Read, Grep and Glob rather than with shell loops or pipelines: they need \
     nobody, and they are what this node has.";

pub(super) const HISTORY_IS_THE_JOBS: &str = "The job commits the tree itself once an item's gate agrees, so no \
     item should ask anyone to commit, stage or branch — that work is already done for you, and an \
     item that asks for it is skipped rather than done.";

/// The prompt the spec node is given.
///
/// The node exists because of what the plan node produces. A plan is a QUEUE — descriptions handed
/// to implement nodes one at a time, each in its own context window — so anything the planner
/// misread about a vague request is not recoverable afterwards: every item inherits it, and the
/// gate then measures the wrong work correctly. Writing the reading down first makes it a document
/// somebody can disagree with while disagreeing is still cheap.
///
/// `Not this` is the section that earns the node. A request grows in the gap between what was asked
/// and what a reader assumes, and that gap is invisible until somebody writes down which side of it
/// they landed on.
///
/// It is told to keep a precise request short, deliberately. A node that must fill five headings
/// will fill them, and a spec that restates a clear sentence at length is a document the planner
/// then has to read past.
pub fn spec_prompt(task: &str, artifacts: &str) -> String {
    format!(
        "You are the SPEC node of an autonomous job. Nothing has been planned yet and no work has \
         started.\n\n\
         The job was asked for in one sentence, at the bottom, and that sentence is everything \
         anybody wrote down. Read the project first — the working tree is the project — and then \
         write down what you understood, so that every node after you works from a considered \
         document instead of from a sentence each of them interprets alone.\n\n\
         {LOOK_WITH_THE_READING_TOOLS}\n\n\
         Write it to {artifacts}/{SPEC_FILE} and change nothing else. Cover, in this order:\n\n\
         - What is being asked, in your own words, including what was implied rather than said.\n\
         - What it touches: the files, modules or surfaces you expect to be involved, and how you \
         know — you have just read the project, so say what you saw rather than what you assume.\n\
         - Done means: what has to be true for this to be finished, written so somebody else could \
         check it without asking you.\n\
         - Not this: what a reasonable reader might think is included and is not. This section is \
         worth more than the other four, because it is the only one that stops the work growing.\n\
         - Decided for you: every question the request did not answer, the answer you chose, and \
         why. Somebody will read this to disagree with you, and that is what it is for.\n\n\
         Do not begin any of the work, and do not write a plan or a list of tasks — the node after \
         you does that, and it reads this file. If the request is already precise, say so and keep \
         this short: a spec that pads a clear request is worse than no spec at all.\n\n\
         The task:\n\n{task}"
    )
}

/// The prompt the plan node is given.
///
/// It says the file is the only thing read, because it is: §5.2 of the design takes the queue from
/// `plan.json` and never from stdout, so that a stream truncated mid-write cannot be parsed into a
/// plausible short queue that reads as "there was less work than expected".
///
/// `spec` is a boolean and not a path, and it is a boolean rather than "the prompt always names the
/// file" on purpose: a sentence telling the planner to read a document that is not there is worse
/// than saying nothing, because a node sent to a missing file spends a turn finding that out.
pub fn plan_prompt(
    task: &str,
    max_items: usize,
    artifacts: &str,
    graph: Option<&Graph>,
    spec: bool,
) -> String {
    let brief = if spec {
        format!(
            "A spec node has already read the project and written down what it understood, in \
             {artifacts}/{SPEC_FILE}. Read that first and plan from it. The task at the bottom is \
             the sentence it was written from, kept here so you can see what was actually asked; \
             where the two differ, the spec is the considered reading — but its `Decided for you` \
             section is decisions it took on somebody's behalf, not instructions from them.\n\n"
        )
    } else {
        String::new()
    };
    let Some(graph) = graph else {
        return format!(
            "{brief}You are the PLAN node of an autonomous job. Break the task below into at most \
             {max_items} items that can be done one after another, in order, in the same working \
             tree. Prefer fewer, larger items to more, smaller ones.\n\n\
             {HISTORY_IS_THE_JOBS}\n\n\
             Write them to {artifacts}/plan.json and change nothing else:\n\n\
             {{\"items\": [{{\"description\": \"...\", \"files\": [\"path\", \"...\"]}}]}}\n\n\
             \"files\" is optional and best-effort — name the files you expect the item to touch \
             if you know them, and omit the field if you do not. A wrong guess costs more than no \
             guess.\n\n\
             That file is the only thing that is read; anything you print is discarded. If there is \
             no work to do, write {{\"items\": []}} — an empty queue is a legitimate answer and \
             is not a failure. Do not begin any of the work yourself.\n\n\
             The task:\n\n{task}"
        );
    };
    format!(
        "{brief}You are the PLAN node of an autonomous job that a TEAM will carry out. Break the task \
         below into at most {max_items} items. Items that do not depend on each other are worked on \
         AT THE SAME TIME, each in a working tree of its own, so how you split the work decides how \
         much of it can happen at once.\n\n\
         {HISTORY_IS_THE_JOBS}\n\n\
         Write them to {artifacts}/plan.json and change nothing else:\n\n\
         {{\"items\": [{{\"description\": \"...\", \"files\": [\"path\"], \"depends_on\": [], \
         \"agent_id\": \"...\"}}]}}\n\n\
         ALL THREE keys are required on every item, and leaving one out refuses the whole plan.\n\n\
         \"agent_id\": who does the item. {roster} Choose by what each one is for; one of them may \
         take several items, and an item may go to whoever fits it best.\n\n\
         \"files\": every file you expect the item to touch. This is what tells two items apart \
         before either has run — two items naming the same file are never started together. Name \
         too few and their edits collide; name the whole repository and nothing ever runs beside \
         anything.\n\n\
         \"depends_on\": the ordinals of the items this one may not start before, or [] if there \
         are none. Ordinals are counted across the whole job, and THIS ROUND'S ITEMS ARE NUMBERED \
         FROM {first}: the first item you list is ordinal {first}, the second is {second}, and so \
         on. A dependency must point at an EARLIER ordinal than the item naming it — list each \
         item after everything it needs. A plan that points forwards, or in a circle, is refused.\n\n\
         Registry files that many items would touch — a lock file, a module list, a migrations \
         directory — belong in the \"files\" of every item that will edit one. Two items that \
         both add a migration do not conflict in git and break the build together.\n\n\
         That file is the only thing that is read; anything you print is discarded. If there is no \
         work to do, write {{\"items\": []}} — an empty queue is a legitimate answer and is not a \
         failure. Do not begin any of the work yourself.\n\n\
         The task:\n\n{task}",
        roster = graph.introduce(),
        first = graph.first_ordinal,
        second = graph.first_ordinal + 1,
    )
}

/// The prompt the replan node is given at the end of a round.
///
/// The node's whole job is to answer one question — is there more to do? — and the two answers go to
/// the same file for the same reason the plan node's queue does: a stream truncated mid-write must
/// not be readable as a plausible short answer.
///
/// It is given the archives and told what they are, because without them the pattern this feature
/// rests on cannot work. A replan that cannot see what round 1 tried reproposes round 1, no round
/// ever comes back empty, and the "until it dries up" brake never fires — leaving `max_rounds` as
/// the only thing between the job and its budget.
///
/// Told to prefer `done` explicitly, and that is not politeness. The failure this feature has to
/// avoid is a job that will not admit it is finished: ending #4 (out of rounds) costs a full round of
/// runs to discover, where ending #1 costs one node.
/// An item the last round did not finish, and what is known about why.
///
/// Read by `replan_prompt` and by nothing else. Both shapes of not-finishing are here and are kept
/// apart, because they call for different next moves: a gate that spoke gives the node something
/// concrete to answer, and a node that died before any gate looked leaves the item unmeasured —
/// which is worth saying plainly rather than dressing up as a verdict nobody reached.
pub struct Unfinished {
    pub ordinal: usize,
    pub description: String,
    /// The tail of what the gate printed rejecting this item, or `None` when nothing gated it.
    pub gate_output: Option<String>,
}

pub fn replan_prompt(
    task: &str,
    round: i64,
    archives: &[String],
    artifacts: &str,
    graph: Option<&Graph>,
    unfinished: &[Unfinished],
) -> String {
    // **"Do not repropose any of it" is qualified when something broke, and the qualification is
    // the point.** Unqualified it is right about work that LANDED — reproposing a green item wastes
    // a round redoing it. Aimed at an item the gate rejected it is exactly backwards: that item is
    // the one thing the round did not achieve, and the sentence tells the node to leave it alone.
    // Measured 2026-08-30 on job 23, whose first item went red and whose next round would have read
    // this and gone looking for something else to do.
    let leave_alone = if unfinished.is_empty() {
        " Do not repropose any of it."
    } else {
        " Do not repropose the parts of it that LANDED. What did not is listed below, and \
         reproposing that — differently — is the point of this round."
    };
    let history = if archives.is_empty() {
        // Reachable when the archive copy failed, and the honest thing to say is that it is missing.
        // Claiming a file that is not there sends the node looking, and what it finds is nothing.
        "The earlier rounds' plans could not be recovered, so judge from the working tree and its \
         git history alone."
            .to_owned()
    } else {
        format!(
            "What the earlier rounds already tried is in {}.{leave_alone}",
            archives.join(", ")
        )
    };
    // Named, with the gate's own words where a gate spoke. `implement_prompt` already carries the
    // gate output into a RETRY of the same item, for the reason §5.4 makes structural: no node ever
    // sees another's session, so a node that cannot see what it broke spends a whole run
    // rediscovering what the gate already printed. A replan is the same node in the same position,
    // one round later, and it was the one place that argument had not reached.
    //
    // The tree is NOT the answer to this. A red item was reverted to its footing before the queue
    // moved on, so what the gate objected to is not there to read — the branch shows the work as
    // never having been done, which is true and is not why.
    let broke = if unfinished.is_empty() {
        String::new()
    } else {
        // Which items get to spend [`REPLAN_GATE_BUDGET`], decided before anything is written.
        //
        // Spent NEWEST-FIRST and rendered oldest-first, which is the whole of the design. `ordinal`
        // is job-wide and increasing, so the list arrives chronological and the freshest failure —
        // the one this round is actually answering — is at the END. A budget spent in list order
        // would hand every byte to the oldest failures on the job and leave the current one
        // wordless, which is the opposite of useful and would look like it was working.
        let mut budget = REPLAN_GATE_BUDGET;
        let mut shown = vec![false; unfinished.len()];
        for (index, item) in unfinished.iter().enumerate().rev() {
            let Some(output) = &item.gate_output else {
                continue;
            };
            // All or nothing per item: half a gate's tail is half a stack trace, and the half that
            // survives a budget cut is the earlier half — the summary line a node needs is the part
            // that would be dropped. An item that does not fit keeps its place in the list and
            // says what it is missing.
            //
            // The walk CONTINUES past an item that does not fit rather than stopping, so a short
            // older failure can still use room a long newer one could not. That is deliberate: the
            // guarantee worth having is that recency decides PRIORITY, and stopping at the first
            // miss would throw away room already paid for to buy a tidier rule.
            if output.len() <= budget {
                budget -= output.len();
                shown[index] = true;
            }
        }
        let mut lines = String::from(
            "\n\nWhat the last round could not finish, and what is known about why:\n\n",
        );
        for (index, item) in unfinished.iter().enumerate() {
            lines.push_str(&format!(
                "- item {}: {}\n",
                item.ordinal + 1,
                item.description
            ));
            match &item.gate_output {
                Some(output) if shown[index] => {
                    lines.push_str(&format!("  the gate said:\n{output}\n"))
                }
                Some(output) => lines.push_str(&format!(
                    "  the gate said {} bytes, not shown: this prompt carries at most {} bytes of \
                     gate output and newer failures took it. The item is listed because it still \
                     decides how the job ends; read the gate's words from this item's own run if \
                     you take it over.\n",
                    output.len(),
                    REPLAN_GATE_BUDGET
                )),
                None => lines.push_str(
                    "  its node ended before any gate looked at it, so nothing measured this one.\n",
                ),
            }
        }
        lines.push_str(
            "\nAn item of yours that takes one of these over says so with \"replaces\": [N], N \
             being the number it has above. The job then judges it by that item instead of by the \
             old failure. Leave \"replaces\" out for new work; a failure nothing takes over still \
             decides how the job ends.\n",
        );
        lines
    };
    let replaces_key = if unfinished.is_empty() {
        ""
    } else {
        ", \"replaces\": []"
    };
    // The graph rules are repeated here word for word rather than referred to, and the reason is
    // that the node reading them is not the node that read them before. A replan writes items into
    // the SAME queue, numbered from wherever that queue left off, so a `depends_on` written to mean
    // "the first item I just listed" names a finished item of round 1 — the plan is refused, and
    // two dry rounds later the job reports `completed` having done only its first round.
    let (shape, rules) = match graph {
        Some(graph) => (
            ", \"files\": [\"path\"], \"depends_on\": [], \"agent_id\": \"...\"",
            format!(
                "All three extra keys are required on every item you list, and leaving one out \
                 refuses the whole plan. \"files\" is every file you expect the item to touch, and \
                 it is what tells two items apart before either has run. \"depends_on\" is the \
                 ordinals this item may not start before, or [] if there are none — counted across \
                 the WHOLE job, so the items you list now are numbered from {}, and a dependency \
                 must point at an EARLIER ordinal than the item naming it. \"agent_id\" is who \
                 does the item.\n\n\
                 {}\n",
                graph.first_ordinal,
                graph.introduce()
            ),
        ),
        None => ("", String::new()),
    };
    format!(
        "You are the REPLAN node of an autonomous job, at the end of round {round}. The working tree \
         holds everything the job has done so far.\n\n\
         {history}{broke}\n\n\
         Decide whether the task below is finished. Write ONE of these to {artifacts}/plan.json and \
         change nothing else:\n\n\
         {{\"done\": true, \"why\": \"...\"}}\n\
         {{\"items\": [{{\"description\": \"...\"{shape}{replaces_key}}}]}}\n\n\
         {rules}\
         That file is the only thing that is read; anything you print is discarded. Prefer \
         {{\"done\": true}} when the task is met — saying so ends the job in one node, where \
         leaving it to run out of rounds costs a full round of work to discover the same thing. Do \
         not begin any of the work yourself.\n\n\
         {HISTORY_IS_THE_JOBS}\n\n\
         {LOOK_WITH_THE_READING_TOOLS}\n\n\
         The task:\n\n{task}"
    )
}

/// The prompt one implement node is given.
///
/// It gets the item and the queue, and no account of how the previous node reasoned — §5.4 of the
/// design makes each node's independence structural rather than requested, by never keeping a
/// session another node could resume.
///
/// `files` is the plan node's guess, appended only when it made one: a node with no hint is given
/// the brief it has always been given, word for word, rather than a paragraph about a mechanism it
/// has nothing to put in.
///
/// `gate_output` is appended on the same terms and for the same reason: the tail of what the gate
/// printed when it rejected a PREVIOUS attempt at this item, or `None` on a first attempt — which is
/// every attempt of every job that never retries anything, and which therefore has to produce the
/// prompt this function has always produced, word for word. It is what makes a retry a retry rather
/// than a second independent guess at the same item, because §5.4 keeps the second node from ever
/// seeing the first one's session: without the output travelling in the prompt, a whole run is spent
/// rediscovering what the gate already printed.
///
/// `persona` is who the director gave the item to, and it is `None` for every item of every job
/// without a team — which is what keeps the brief below byte-for-byte what it has always been. It is
/// PREPENDED rather than mixed in, for the reason `speciality` exists at all: the agent's own prompt
/// is a standing instruction about how this one works, and the item is a piece of work. Interleaving
/// them would make each read like a qualification of the other.
pub fn implement_prompt(
    description: &str,
    ordinal: usize,
    total: usize,
    artifacts: &str,
    files: &[String],
    gate_output: Option<&str>,
    persona: Option<&str>,
) -> String {
    // Prepended and not woven in, so a job with no team is handed the brief it has always been
    // handed, byte for byte. It goes FIRST because it is who is reading, and everything after it is
    // what they are reading about — the order `team.rs`'s `specialist_prompt` already uses.
    let mut prompt = persona.unwrap_or_default().to_owned();
    prompt.push_str(&format!(
        "You are item {} of {total} in an autonomous job. The working tree already holds the work \
         of the earlier items; this is the only one you do.\n\n\
         {description}\n\n\
         The full queue is in {artifacts}/plan.json for context. Do not start another item and do \
         not edit that file. Your work is verified by the daemon after you finish, so leave the \
         tree building, and do not run the whole suite yourself. While you work, check your change \
         with the verify tool: kind test (or check for a quick compile), scope own, and files set \
         to the paths you changed. It runs only the tests those files select, and answers within \
         about 45 seconds with the result or a ticket and its progress. While a ticket is not \
         done, call verify_status with it instead of starting the same check again. If the project \
         has no test map, own runs nothing and says so; then run the narrowest test that tells you \
         your change works. If you hand part of this item to subagents, tell each one to call \
         verify with scope own and the files it changed. A command of yours that prints nothing \
         for long is taken for a hang, and it ends this item. \
         Leave it UNCOMMITTED: the job commits for you once the gate agrees, and committing by hand \
         stops this item to ask permission for something already arranged.",
        ordinal + 1
    ));
    if !files.is_empty() {
        // Told where to begin, and told in the same breath that beginning is all it is. A list
        // written before any of the earlier items ran cannot know what they moved, so a node that
        // reads it as the edge of its work stops halfway and leaves the tree half-changed.
        prompt.push_str(&format!(
            "\n\nThe plan expected this item to touch {}. Start there, and treat that list as \
             possibly incomplete or wrong — it was guessed before any of the work was done. Edit \
             whatever the item actually needs.",
            files.join(", ")
        ));
    }
    if let Some(output) = gate_output {
        // Told as what happened rather than as a rule, because it IS what happened: this item was
        // attempted, the tree was measured, and the measurement is below. A node given a policy
        // about gates would argue with it; a node given the verdict on its own work fixes it.
        prompt.push_str(&format!(
            "\n\nThis item has been attempted before. That attempt finished, the project's gate ran \
             over the tree, and the gate said no — this is the tail of what it printed:\n\n{output}\n\n\
             The work that attempt left is still in the tree: nothing was undone, so you are \
             continuing it rather than starting again. Make the gate agree: the tail says which \
             step went red, so re-run that step, not the whole gate."
        ));
    }
    prompt
}

/// The prompt the review node is given.
///
/// Advisory by design: §5.5 gives ship/no-ship to the deterministic gate, and this opinion travels
/// with the proposal as information.
///
/// `council_file` says whether a council deliberated first and left [`COUNCIL_FILE`] beside the
/// queue. It is a parameter and not a sentence written unconditionally, because a prompt that names
/// a file which is not there is an instruction that fails by omission: the node reads nothing, gets
/// an error it was not warned about, and either spends turns hunting for the file or quietly
/// decides the whole paragraph was wrong. The consumer is off by default and a council can fail, so
/// "absent" is the common case rather than the edge one.
///
/// And it is named as ADVICE, in the same breath as the sentence telling the node to judge the diff
/// rather than the intent. A synthesis carries no authority here — the deterministic gate already
/// holds ship/no-ship — so a node told to read it must not read it as a verdict it should agree
/// with. The council never saw this tree; it deliberated on the task.
///
/// `task` is the job's own prompt, carried here for the reason `plan_prompt` carries it beside the
/// spec: so the node can see what was actually asked. Without it the queue was the only yardstick
/// the review had, and a flaw that comes from the queue itself can never be found by checking the
/// diff against the queue. Measured on job 27, 2026-09-14: the task said, in so many words, that
/// widening a test's timing margin until it stops failing "loses that property and is not a fix";
/// the spec restated it under `Done means` and `Not this`; the plan's item told the implementer to
/// widen `a_finished_request_returns_its_outcome_without_waiting`'s bound from 25ms to 1s anyway,
/// and the review read the diff against that item and answered "nothing wrong, nothing missing".
///
/// `spec` is a boolean for the reason it is one on `plan_prompt`: the node is sent to
/// [`SPEC_FILE`] only when the file is there, because a sentence naming a missing file costs the
/// node a turn to find out.
pub fn review_prompt(
    task: &str,
    base: Option<&str>,
    artifacts: &str,
    spec: bool,
    council_file: bool,
) -> String {
    let diff = match base {
        Some(sha) => {
            format!("Run `git diff {sha}..HEAD` — that is the whole of what this job changed.")
        }
        // No recorded base: it could not be read when the job started. Asking for the branch's own
        // commits is worse than naming a sha and better than reviewing a guess.
        None => "Run `git log --oneline` to find the commits this job made on the current branch, \
                 and review their combined diff."
            .to_owned(),
    };
    let council = if council_file {
        format!(
            "\n\nA panel of models was asked about this task before you started, and its synthesis \
             is in {artifacts}/{COUNCIL_FILE}. Read it as ADVICE and nothing more: the panel never \
             saw this branch, it deliberated on the task, and the verdict on the tree is the \
             project's gate rather than anyone's opinion. Where it disagrees with what you find in \
             the diff, the diff is what is real."
        )
    } else {
        String::new()
    };
    // Named with the sections that decide, and not only as a file: `Done means` and `Not this` are
    // the two a diff can break while satisfying every item of the queue, and `Not this` is the one
    // the job-27 plan walked straight through.
    let brief = if spec {
        format!(
            " A spec node read the project before anything was planned and wrote down what the \
             task means, in {artifacts}/{SPEC_FILE}. Read its `Done means` and `Not this` \
             sections: they say what finished looks like and what the work must not become, and \
             a diff can satisfy every item of the queue and still break either."
        )
    } else {
        String::new()
    };
    let against = if spec {
        "the task, the spec"
    } else {
        "the task"
    };
    format!(
        "You are the REVIEW node of an autonomous job. Every change on this branch was written by \
         other sessions whose reasoning you cannot see, and you are not going to be shown it. \
         Judge the diff, not the intent.\n\n\
         {diff}\n\n\
         The queue those changes were meant to satisfy is in {artifacts}/plan.json. That queue was \
         written by a node too, and it can be wrong.{brief}\n\n\
         Report what is wrong and what is missing, judged against {against} and the queue. Where \
         the diff — or the queue item it follows — departs from what the task asks for or rules \
         out, say so, and say whether the fault is the queue's: a diff that does exactly what a \
         wrong item said is not a diff with nothing wrong in it. Nothing else. Change no files.\
         {council}\n\n\
         {LOOK_WITH_THE_READING_TOOLS}\n\n\
         The task, exactly as it was asked:\n\n{task}"
    )
}
