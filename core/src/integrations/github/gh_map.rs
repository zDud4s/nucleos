//! The allowlisted gh command forms and their operation mapping.

use super::*;

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
    "-S",
];

/// One row of the map below: what a `gh` command line has to say in order to BE one operation.
///
/// Neither field is written here. Both are read off the operation's own `argv()` — see `gh_forms`.
#[derive(Debug)]
pub(super) struct GhForm {
    pub(super) kind: &'static str,
    /// The literal subcommand words, in order: `["run", "view"]`. This module's, never a caller's.
    pub(super) subcommand: Vec<String>,
    /// The valueless flags this module writes for THIS operation and not for the one it shares a
    /// subcommand with — the `--log` that makes `gh run view` mean `run_logs`. Empty for most rows.
    pub(super) switches: Vec<String>,
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
    pub(super) fn matches(&self, arguments: &[String]) -> bool {
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
pub(super) fn gh_forms() -> &'static [GhForm] {
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
/// differently, and the design measured what that costs an owner: a `~/.nucleos/github.yaml` without `gh
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
    let words = crate::governance::classifier::shell_words(command);
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
pub(super) fn word_is_flag(word: &str, flag: &str) -> bool {
    // A short flag takes its value glued on (`-L1000`, `-qEXPR`, `-S"x"`): pflag reads everything
    // after the letter as the value, so the word is still that flag.
    let short = flag.len() == 2 && flag.starts_with('-') && !flag.starts_with("--");
    word == flag || word.starts_with(&format!("{flag}=")) || (short && word.starts_with(flag))
}
