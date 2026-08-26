//! What the triager is allowed to answer.
//!
//! The model enters this feature twice and is a compressor both times (§6). Before, it turns a
//! thousand lines of spec into a dozen lines of decision, and `map_intent.rs` is that half. After —
//! here — it looks at one node with the mechanical proof beside it and answers a single question:
//! *isto merece o olhar dele?*
//!
//! Split from `map_store.rs` for the reason `map_intent.rs` is: the prompt and the parse are the
//! part worth exercising without a database, and the SQL is the part worth exercising without a
//! model. Nothing here knows what SQL is, what HTTP is, or which model answered — it takes a
//! decision and a reading of the repository and returns text, data, and a hash.
//!
//! **[`reconcile`] takes `map_store`'s row struct, and that is not the seam being crossed.** It is
//! the same borrow `map_stamp::standing` already takes of `map_store::Stamp`: a plain struct of six
//! fields, with no pool, no query and nothing to await. What this module still does not know is
//! where a `Judged` came from or how to fetch another one — and §5.3's header is here rather than
//! next door because the rule it enforces is §10's scope for the TRIAGER, which is this file's
//! subject and not the stamp's.
//!
//! **The one rule this module exists to make unsayable: the triager may never approve.** §6's table
//! names exactly one forbidden column for it — *Aprovar* — and §6.1 says why the prohibition has to
//! survive rendering as well as storage: *"se colapsassem, a autoridade que foi retirada ao modelo
//! era-lhe devolvida pela porta da renderização — e o mapa passava a ser a falsa confiança de novo,
//! agora com autoridade de semáforo."* A silenced node is *sem sinal de problema*, which is a claim
//! about **the triager** and about nothing else.
//!
//! **And the rule that keeps the first one from being got round by accident: an answer that is
//! neither of the two words is an ERROR and is never defaulted to either.** Both defaults are worse
//! than the absence, in opposite directions and by different amounts — see [`parse_answer`], where
//! the argument is spelled out. What a failure leaves behind is a decision nobody has looked at,
//! which is exactly what it is.

use serde::{Deserialize, Serialize};

/// The triager's answer about one decision (§6).
///
/// **Two, and the missing third is the entire design.** There is no `Approved`, no `Ok`, no
/// `Settled`: turning something green is the owner's act and the owner's alone (§5.2), and a variant
/// here would hand back the authority §6 took away — with the compiler's blessing, which is worse
/// than a handler doing it, because nobody reviews an enum arm twice. The prohibition is written in
/// four places on purpose and this is only one of them: `0119`'s `CHECK (verdict IN ('flagged',
/// 'silenced'))` is the one that survives a caller who never reaches for this type at all.
///
/// **Neither value is a statement about the code**, and that is the sentence a reader is most likely
/// to lose. [`Self::Silenced`] does not say the decision is fine; it says the triager saw nothing
/// worth the owner's time. §6.1 refuses to let that share a colour with a stamp, and §13 rates *o
/// triador silencia o que devia mostrar* a **real** residual risk whose only mitigation is that the
/// silenced pile stays visible with the reason for each silence and the model that produced it —
/// which is what `map_triage.reason` and `map_triage.model` are, and why both are NOT NULL.
///
/// **This axis is not [`crate::map_stamp::Standing`] and not [`crate::map_join::Anchor`].** §5
/// forbids flattening them — *"achatá-las numa só punha o triador e o dono a falar pela mesma
/// boca"* — so a decision carries all three independently, and [`Self::Flagged`] feeds §5.3's `J à
/// tua espera` beside the lapsed stamps without ever becoming one.
///
/// Two wire forms, read as two, exactly as [`crate::map_stamp::Verdict`] and
/// [`crate::map_intent::Kind`] are. [`Self::as_str`] and [`Self::from_wire`] are the STORAGE form —
/// what `map_triage.verdict` holds and precisely what its CHECK admits — and the derived
/// `Serialize`/`Deserialize` are the JSON the window speaks. They spell the words alike today
/// because English happens to serve in both places, which is a convenience and not a guarantee:
/// `Kind` had to choose `b`/`c` for its column and `countable`/`character` for the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgement {
    /// *À espera.* The triager thinks this deserves the owner's eyes, and said why.
    ///
    /// The reason is required and it is not decoration: a flag with no reason is a nag the owner
    /// cannot answer, and a nag nobody can answer is one they stop reading — which costs the same
    /// trust the silent green costs, from the other side.
    ///
    /// **The cheap failure of the two, and that asymmetry is what the parse leans on.** A wrong flag
    /// costs one unnecessary look; a wrong silence costs the very thing §1 says the map is for. So
    /// nothing here may ever default to [`Self::Silenced`].
    Flagged,
    /// *Silenciado.* The triager saw no sign of a problem. **Nobody looked.** Not green.
    ///
    /// §5.1 spells the meaning out — *sem sinal de problema* — and §6.1 spends a paragraph on why it
    /// may not share a colour with a stamp. A decision silenced here is still *nunca vista* by the
    /// owner; it has simply stopped being at the front of the queue.
    ///
    /// **Reachable only with a reason recorded, and by design it is reversible by nobody but the
    /// evidence.** §6.2 keeps the pile accessible *com a razão de cada silenciamento e o modelo que
    /// o produziu*, because *"um triador que silencia o que não devia é um bug do triador, e um bug
    /// só é corrigível se for visível."*
    Silenced,
}

impl Judgement {
    /// The storage form: what `map_triage.verdict` holds, and exactly what its CHECK admits.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flagged => "flagged",
            Self::Silenced => "silenced",
        }
    }

    /// The two, and nothing else.
    ///
    /// `Option` rather than a fallback, following [`crate::map_stamp::Verdict::from_wire`] and
    /// [`crate::map_intent::Kind::from_wire`]. Neither of the two is a safe default: guessing
    /// [`Self::Silenced`] would let a word nobody can read clear a decision out of the owner's
    /// queue, which is §13's residual risk arriving through a parser instead of through a model;
    /// guessing [`Self::Flagged`] would invent an alarm the reason column cannot explain. A
    /// judgement that cannot be read is therefore no judgement, which leaves the decision *not
    /// looked at* — visible debt, and the one answer that claims nothing.
    ///
    /// Named after the pair in `map_intent` and `map_stamp` rather than `FromStr`, because the
    /// codebase already has two of these and a third spelling would make the reader check which is
    /// which.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "flagged" => Some(Self::Flagged),
            "silenced" => Some(Self::Silenced),
            _ => None,
        }
    }
}

/// Everything one triage question is asked about: the decision as the junction hands it over, and
/// the reading of its anchor code that stands beside it.
///
/// **Two borrows and not a struct of copies**, because whoever calls already holds both — the
/// junction's row and the `git ls-files` answer sliced out per decision — and because the same
/// bundle feeds [`triage_prompt`] and [`inputs_digest`], which is the property that matters: the
/// prompt and the staleness hash must be about the same thing or a judgement can be current about
/// evidence that was never shown. [`crate::map_stamp::Anchoring`] is the same shape for the same
/// reason.
///
/// **The two fields are not shown to the model alike, and that asymmetry is deliberate.** Everything
/// in `decision` reaches the prompt; `anchors` reaches only the hash. §6 has the model answer *"com
/// a prova mecânica ao lado"*, and the proof a model can read is which files name this section and
/// how firmly — not forty lines of `blob sha path`, which it can do nothing with and which would
/// crowd out the question. The blobs still belong in the digest, because §10's first trigger is *o
/// código âncora de uma decisão mexeu-se*: the judgement is about whether the code behind those
/// paths deserves the owner's eyes, and that code moving is precisely the event the answer must not
/// outlive. `0119`'s header argues the same inclusion from *the model looked at it*, which this
/// module makes untrue; the conclusion survives its reason being corrected, and the cost of the
/// stricter rule is stated at [`inputs_digest`].
#[derive(Debug, Clone, Copy)]
pub struct Evidence<'a> {
    pub decision: &'a crate::map_join::Anchored,
    pub anchors: &'a crate::map_stamp::Anchors,
}

/// How many anchor paths of each kind the prompt lists before it says how many it left out.
///
/// A ceiling rather than a budget: `§7` is named by 21 files in this repository and `§6.4` by 10,
/// so nothing this repository contains comes near it. What it exists for is the one decision whose
/// section number is named by half the tree — a prompt of two hundred paths is a question buried in
/// a file listing, and the model answers about the listing.
///
/// **The cap is on the prompt and never on [`inputs_digest`]**, which covers the whole set. That is
/// the conservative direction: a path appearing beyond the cap lapses a judgement the model could
/// not have taken it into account in, which costs one model call, where the other way round would
/// keep a judgement current across a change nobody weighed.
const MAX_LISTED_PATHS: usize = 40;

/// How long a reason may be before it is cut.
///
/// **Cut, and not kept whole — the choice this slice had to make either way, so here is the
/// argument.** The column would take an essay: `0119` constrains `reason` to be non-blank and to
/// nothing else, on purpose. What cannot take one is §6.2's pile, which is the mitigation §13 names
/// and is read as one line per decision across a backlog §10 expects to be in the hundreds. A model
/// that answers the question asked — *one sentence saying what you saw* — is never near a thousand
/// bytes; one that returns four paragraphs of reasoning has answered a different question, and
/// storing all of it makes the pile unreadable, which costs the same mitigation an unreadable reason
/// costs. So the essay is cut.
///
/// **What is forbidden is the silent cut**, which is why [`CUT_MARK`] exists: a bug report with its
/// ending removed reads as a complete thought that stops making sense, and the reader blames the
/// triager for a sentence the daemon truncated. The mark says which of the two happened.
const MAX_REASON_BYTES: usize = 1_000;

/// What a cut leaves behind so nobody mistakes it for the end of the thought.
const CUT_MARK: &str = " […cut]";

/// How much of a verdict that was neither word is quoted back in [`Unreadable::ThirdVerdict`].
///
/// One word was asked for, so a hundred and twenty bytes is generous for the thing this quotes and
/// mean enough for the thing it defends against — a model that puts a paragraph of reasoning where
/// the verdict goes, and a log line nobody scrolls past.
const MAX_QUOTED_VERDICT: usize = 120;

/// `text`, or as much of it as fits, with [`CUT_MARK`] where the rest was.
///
/// Backs off to a character boundary before slicing, and that is a correctness rule rather than
/// politeness. These decisions are Portuguese and the reasons come back in the document's own
/// language, so byte 1000 lands inside a `ç` or an `ã` often enough — and `&text[..1000]` panics
/// there rather than truncating. `map_intent::bounded` learned this going the other way, over the
/// document rather than over the answer; a daemon that died because a reason happened to be the
/// wrong length would be invisible until the one decision that triggered it, and then fatal.
fn clipped(text: &str, ceiling: usize) -> String {
    if text.len() <= ceiling {
        return text.to_owned();
    }
    let mut cut = ceiling;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{CUT_MARK}", &text[..cut])
}

/// One list of anchor paths as the prompt prints it, capped, with the count of what it left out.
fn listed(paths: &[String]) -> String {
    if paths.is_empty() {
        // Said in a word rather than left blank. An empty line under a heading reads as a prompt
        // this daemon failed to fill in; "none" is a measurement, and for `Anchor::Silent` it is
        // the whole of the finding.
        return "  (none)".to_owned();
    }
    let mut out = String::new();
    for path in paths.iter().take(MAX_LISTED_PATHS) {
        out.push_str("  ");
        out.push_str(path);
        out.push('\n');
    }
    let left = paths.len().saturating_sub(MAX_LISTED_PATHS);
    if left > 0 {
        out.push_str(&format!("  ... and {left} more not listed here\n"));
    }
    out.pop();
    out
}

/// How firmly this decision is tied to code, in words the model can act on (§8).
///
/// **Four sentences and never three**, because [`crate::map_join::Anchor`] is four states and each
/// collapse costs a specific lie. The one that matters most is the last: *nothing was found* and
/// *nothing was looked for* are different facts, and a model told the second as though it were the
/// first flags a decision for having no code on the strength of a search that never ran. That is a
/// fabricated contradiction in a pile whose entire value is that its contradictions are real.
///
/// `Ambiguous` says out loud that the lists below it are a guess. Every positive join this
/// repository can make today is one — `§7` appears in 21 files and not one of them says of what —
/// and a model handed those paths as proof would silence a decision on evidence nobody established,
/// which is §13's residual risk manufactured by the prompt rather than by the model.
fn anchor_says(anchor: &crate::map_join::Anchor) -> &'static str {
    use crate::map_join::Anchor;
    match anchor {
        Anchor::Declared => {
            "declared — a readable module names this section AND names this document. The only \
             state this map calls certain."
        }
        Anchor::Ambiguous => {
            "ambiguous — something names this section NUMBER, and the map cannot confirm it meant \
             this document rather than another one with a section of the same number. Treat the \
             lists below as a guess and never as proof."
        }
        Anchor::Silent => {
            "silent — the map searched and found nothing anywhere: no readable module, no file in \
             any other language. The decision was made and no code claims it."
        }
        Anchor::Unnumbered => {
            "unnumbered — the heading this decision was copied from carries no number, so there \
             was nothing to look for and no search ran. This is NOT a finding that no code \
             implements it; flagging it for having no code would be reporting a search that never \
             happened."
        }
    }
}

/// What kind of decision this is, and therefore what a contradiction would even look like (§4.1).
///
/// In the prompt and deliberately not in [`inputs_digest`]. §10's second trigger is a *tipo B que
/// deixou de bater* — a countable claim the code stopped matching — so the kind is part of what the
/// triager is looking for and belongs beside the question. It is out of the digest by the rule
/// `0119`'s header sets for that hash: it covers everything whose CHANGE would make the answer
/// wrong, and a decision's kind is fixed when the line is extracted and never updated afterwards,
/// so a digest covering it could not move.
fn kind_says(kind: crate::map_intent::Kind) -> &'static str {
    use crate::map_intent::Kind;
    match kind {
        Kind::Countable => {
            "countable — it names a number, a set, or a coverage claim, so code can be counted \
             against it and agree or disagree."
        }
        Kind::Character => {
            "character — it says what something IS or IS NOT. No count settles it, so complete \
             coverage and the wrong thing are perfectly compatible."
        }
    }
}

/// Which question the triager was asked, bumped by hand when the question changes.
///
/// **A hand-bumped integer and never a hash of the prompt text**, following
/// `classifier::CLASSIFIER_VERSION`, which is this codebase's existing answer to the same problem.
/// Hashing [`triage_prompt`]'s output would re-triage a whole backlog for a typo fix, a reflowed
/// line or a comma — real money spent to re-learn what the model already said. The question this
/// number answers is whether the question *materially* changed, and only a person knows that.
///
/// **What obliges a bump**, so that nobody has to guess:
///
/// - a change to **what is asked** — the two answers, what `silenced` is defined to mean, the
///   instruction to flag when unsure, the shape of the reason;
/// - a change to **what evidence is included** — a field added to or removed from the decision
///   block, the two anchor lists, [`anchor_says`] or [`kind_says`] saying something different about
///   a state;
/// - a change to the **answer vocabulary** — anything [`parse_answer`] would read differently.
///
/// Reformatting does not. Rewrapping a sentence, renaming a heading, fixing a typo: the model is
/// being asked the same thing, and a judgement it already gave is still an answer to it.
///
/// **What a bump costs, in the number rather than in the phrase "not free".** Every judgement in
/// every project goes stale at once, and recovery is bounded by the batch cap on
/// `http::MAX_TRIAGE_BATCH`: at this repository's ~350 approved decisions and 20 a press, that is
/// **eighteen presses of a button whose every run is minutes long**, spread over as many sittings
/// as the owner has patience for — and until the last of them the map is missing judgements it had
/// before the edit. Weigh a wording tweak against that, not against the word "free".
///
/// **In the digest and therefore in the staleness rule**, which is the half slice 4 does not need:
/// a stamp is the owner's and nothing about how it was asked for can make it wrong, while this pile
/// is a model's opinions and the question is half of what produced them. Without this field, editing
/// [`triage_prompt`] leaves every stored judgement claiming to be current about a question that no
/// longer exists — an approximate answer wearing an exact one's clothes, which is the one trade this
/// feature never makes.
const TRIAGE_PROMPT_VERSION: u32 = 1;

/// What to ask a model about one decision.
///
/// **One question, and the whole design of this module is in this string** (§6: *"Por nó, com a
/// prova mecânica ao lado, responde a uma pergunta só: isto merece o olhar dele?"*). What each rule
/// buys:
///
/// - **Approving is refused in words, not merely left off the list.** A model told only "answer
///   flagged or silenced" still explains, at least sometimes, that everything looks correct — and
///   that sentence must not become a silence. So the prompt says the owner's verdict is not on
///   offer, says that *I saw nothing wrong* has a word and the word claims nothing further, and says
///   what happens to a third answer: it is thrown away and nobody has looked at the decision.
/// - **`silenced` is defined as a statement about the model.** §6.1 spends a paragraph on why it may
///   not share a colour with a stamp; a model that thinks it is signing something off will silence
///   differently from one that knows it is only saying *nothing caught my eye*.
/// - **When in doubt, flag.** The asymmetry is real and it is worth telling the model: one
///   unnecessary look costs a minute, one wrong silence costs §1's whole failure mode.
/// - **The two file lists are two lists.** See [`crate::map_join::Anchored::foreign`] — *we know
///   something is there* and *we can see what it is* are different facts, and merging them would let
///   77 Go files this map cannot read lend their confidence to a silence.
///
/// Says nothing about the language of the answer beyond *the decision's own*, for `map_intent`'s
/// reason: these decisions are Portuguese, and a reason translated into English is a paraphrase the
/// owner cannot check against the document at a glance.
pub fn triage_prompt(evidence: Evidence<'_>) -> String {
    let decision = evidence.decision;
    let slug = &decision.spec_slug;
    let section = &decision.section;
    let text = &decision.text;
    let kind = kind_says(decision.kind);
    let anchor = anchor_says(&decision.anchor);
    let module_count = decision.modules.len();
    let foreign_count = decision.foreign.len();
    let modules = listed(&decision.modules);
    let foreign = listed(&decision.foreign);

    format!(
        "You are looking at ONE decision from a design document, with everything this project's map \
         could mechanically find out about it beside you, and you answer ONE question: does this \
         deserve the owner's eyes?\n\
         \n\
         There are two answers and there is no third.\n\
         \n\
         - \"flagged\" — this deserves his eyes. Say why, in one sentence.\n\
         - \"silenced\" — you saw no sign of a problem. That is a statement about YOU and not about \
         the code: it does not mean the decision is done, implemented, correct, or agreed with. \
         Nobody has looked at this decision, and after you answer \"silenced\" nobody still has.\n\
         \n\
         Approving is not one of your options. No answer here turns anything green — that is the \
         owner's act and his alone, and this map exists because a thousand-line plan once looked \
         right and was not. If you find nothing wrong, the word for that is \"silenced\", and it \
         claims nothing further. Do NOT answer in prose that everything looks correct: an answer \
         that is neither of the two words is thrown away, no record is written, and the decision \
         goes back to being one nobody has looked at.\n\
         \n\
         If you cannot tell, answer \"flagged\". One unnecessary look costs a minute; one wrong \
         silence costs the thing this map is for.\n\
         \n\
         ----- BEGIN DECISION -----\n\
         Document: {slug}\n\
         Section: {section}\n\
         Kind: {kind}\n\
         Decision: {text}\n\
         ----- END DECISION -----\n\
         \n\
         What the map found by reading the code. This is the only proof you have — you cannot open \
         a file, and nothing else was measured.\n\
         \n\
         Anchor: {anchor}\n\
         \n\
         Readable code naming this section ({module_count}):\n\
         {modules}\n\
         \n\
         Files naming it in a language this map CANNOT read ({foreign_count}):\n\
         {foreign}\n\
         \n\
         Those two lists are two different facts and must never be read as one. The first is code \
         this map has parsed: it knows what those files import and what they declare. The second is \
         code it has only found a section mark inside; it knows nothing whatever about what those \
         files do, so they can neither confirm nor deny that anything implements this decision.\n\
         \n\
         Your reason is ONE sentence, in the decision's own language, saying what you actually saw. \
         Never empty, and never a restatement of the decision.\n\
         \n\
         Answer with JSON only, shaped exactly like this and nothing else:\n\
         {{\"verdict\":\"flagged\",\"reason\":\"...\"}}"
    )
}

/// The shape a local model is sampled into.
///
/// A grammar the sampler enforces, which the CLI path has no equivalent of — there the shape is
/// asked for in the prompt and checked afterwards by [`parse_answer`]. Both arms end at that same
/// parse, so a local answer is not trusted more for having been constrained; it is only likelier to
/// arrive as JSON at all. `map_intent::extraction_format` takes the same posture, and `web::summarise`
/// took it first.
///
/// **`verdict` is a bare string and NOT `enum: ["flagged", "silenced"]`, and that omission is the
/// whole of this function.** The enum looks like the way to make [`Unreadable::ThirdVerdict`]
/// unreachable. It does the opposite, and `extraction_format` already argues the identical case for
/// §4.1's type A: a grammar with nowhere to put a third answer does not stop the model having one,
/// it makes the model spell its confusion as one of the two — so a confused *approved* arrives as
/// `silenced` carrying a reason that reads like an approval, and the parse can no longer tell it
/// from a real silence. That is §13's residual risk manufactured by the sampler rather than by the
/// model, and it would defeat
/// `a_model_that_answers_approved_is_a_parse_failure_and_not_a_silence` before it ran. A grammar
/// that admits the third value which this parse then refuses is what keeps the failure visible.
///
/// **No `minLength` on `reason` either**, for the same shape of reason. Forcing a non-empty string
/// out of a model that had nothing to say produces a filled column and an empty thought, which is
/// worse than the row that never lands — and §6.2 makes that column §13's only mitigation.
fn triage_format() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string"},
            "reason": {"type": "string"}
        },
        "required": ["verdict", "reason"]
    })
}

/// Ask one brain about one decision, and hand back exactly what it said.
///
/// **The raw text and not an [`Answer`], which is the seam this signature exists to keep open.**
/// *Nobody answered* and *somebody answered something that is not a judgement* are two different
/// facts about a decision, they send whoever is debugging to two different places — the machine and
/// the prompt — and a function that parsed here could only ever report one of them. The caller gets
/// an `Err` for the first and an [`Unreadable`] from [`parse_answer`] for the second, and counts
/// them apart.
///
/// **Which brain is the parameter and there is no fallback**, exactly as `map_intent::extract` has
/// it: the owner picked, and a `local` that quietly became a `cloud` would break the two promises
/// that word carries — the machine and the bill — at once. The type is `map_intent`'s own rather
/// than a second copy of it, because a second enum is a second place to add a brain, and the reason
/// it has two arms is behavioural and holds here unchanged: `OllamaRunner` wears the
/// [`crate::runner::CommandRunner`] trait while imposing the mail-triage `{id, class, summary}`
/// grammar on every prompt it is handed, so a judgement sent through it comes back as a triage
/// array and parses to nothing.
///
/// The local window is `triage::LOCAL_NUM_CTX`, and unlike the extraction next door that is not a
/// window this machine has failed to prove: the startup probe establishes exactly this size, and a
/// triage prompt is one decision plus at most [`MAX_LISTED_PATHS`] paths twice over. The extraction
/// asks for four times it because a spec runs to 60 000 bytes; this does not, so it does not.
pub async fn ask(
    asked: crate::map_intent::Extractor<'_>,
    evidence: Evidence<'_>,
) -> std::io::Result<String> {
    let prompt = triage_prompt(evidence);
    match asked {
        crate::map_intent::Extractor::Cli(runner) => {
            crate::map_intent::ask_once(runner, prompt, "triage").await
        }
        crate::map_intent::Extractor::Loopback {
            client,
            base_url,
            model,
        } => {
            crate::runner::ollama_chat(
                client,
                base_url,
                model,
                &prompt,
                serde_json::json!({
                    "num_ctx": crate::triage::LOCAL_NUM_CTX,
                    // Zero, because the question has one right answer about one decision and a
                    // sampled one would make a re-run disagree with itself over evidence that had
                    // not moved — and `inputs_digest` is built on the premise that unchanged inputs
                    // deserve no second call.
                    "temperature": 0
                }),
                Some(triage_format()),
                false,
            )
            .await
        }
    }
}

/// One judgement, read off a model's answer and ready for the table.
///
/// Deliberately not [`crate::map_store::Judged`]: that one carries `computed_at`, the model's name
/// and the digest, which are facts about the RUN and not about the answer. A parse that returned
/// them would have to invent two of the three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub judgement: Judgement,
    pub reason: String,
}

/// Why a model's answer produced no judgement at all.
///
/// **Three named failures rather than one, and the names are for the log and not for the code.**
/// Nothing branches on which of these happened — every one of them ends the same way, with no row
/// written and the decision still *not looked at*. What differs is what somebody debugging the
/// triager needs to read: *it answered a third thing, and the thing was `approved`* and *it never
/// emitted JSON at all* send you to two different halves of the prompt, and a single
/// `Err(())` would send you to neither. §6.2's argument, applied to the answers that never became
/// rows: a bug is only fixable if it is visible, and a failed judgement's only trace is a log line.
///
/// **A note for whoever gives this a runner (Task 3): do not constrain the sampler to two values.**
/// The temptation is a JSON schema with `enum: ["flagged", "silenced"]`, which looks like it makes
/// [`Self::ThirdVerdict`] unreachable. `map_intent::extraction_format` argues the opposite case and
/// it holds here too: a grammar with nowhere to put a third answer does not stop the model having
/// one, it makes the model spell it as one of the two — a confused *approved* arrives as `silenced`
/// with a reason that reads like an approval, and the parse can no longer tell. A grammar that
/// admits a third value which this parse then refuses keeps the failure visible, which is the whole
/// point of the enum below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreadable {
    /// Nothing shaped like an answer came back — prose, an apology, an empty string.
    NotAnAnswer,
    /// A verdict that is neither of the two, quoted so the log can say what it actually was.
    ///
    /// **Bounded when it is built and not when it is printed**, because the string came from a model
    /// and a model can put a page in a field that was asked for one word. Clipping in `Display`
    /// alone would leave the whole page reachable through `Debug`, which is the formatter a `warn!`
    /// is most likely to reach for.
    ThirdVerdict(String),
    /// The verdict read and the reason said nothing. Carries the verdict, because *it silenced with
    /// no reason* and *it flagged with no reason* are different bugs in the prompt.
    NoReason(Judgement),
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAnswer => {
                write!(formatter, "the answer carried no JSON object at all")
            }
            Self::ThirdVerdict(said) => write!(
                formatter,
                "the verdict was `{said}`, which is neither `flagged` nor `silenced`"
            ),
            Self::NoReason(judgement) => write!(
                formatter,
                "the verdict `{}` arrived with no reason to read",
                judgement.as_str()
            ),
        }
    }
}

#[derive(Deserialize)]
struct RawAnswer {
    /// `Option<String>` and not `String`, for the reason `map_intent::RawDecision` gives:
    /// `#[serde(default)]` fills in for an ABSENT key and does nothing at all for a key present as
    /// `null`, and a model constrained to emit JSON answers with an explicit `null` at least as
    /// readily as by omitting the field. Typed as `String`, that one null would arrive here as
    /// [`Unreadable::NotAnAnswer`] — *it emitted no JSON* — about an answer that emitted plenty,
    /// and the log would send somebody to fix the wrong half of the prompt.
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// The judgement in a model's answer, or the reason there is none.
///
/// **`Result` and not `Option`, and never a fallback — this is the function the slice is built
/// around.** An answer that is neither `flagged` nor `silenced` is an error, and both of the
/// tempting defaults are worse than the error, in opposite directions:
///
/// - **Defaulting to [`Judgement::Silenced`]** lets a confused model quietly clear the pile. That is
///   the authority §6 removes from the model, handed straight back to it through a parser — and
///   §6.1's door is the rendering one, so nobody is watching this one. §13 rates *o triador silencia
///   o que devia mostrar* a real residual risk; this would be that risk without a triager ever
///   having had an opinion.
/// - **Defaulting to [`Judgement::Flagged`]** is the cheap failure — one unnecessary look — and it
///   is still a fabricated judgement, carrying a fabricated reason into the one column §6.2 makes
///   §13's mitigation. A pile whose reasons were written by a parser is a pile the owner stops
///   believing, which is the same trust spent from the other side.
///
/// The honest outcome is the third one: no row, and the decision stays *nunca vista*, which is what
/// it is. That is also the answer `map_store::judged_from_row` and both `from_wire`s already give.
///
/// **A blank reason is refused here and not left to the table.** `0119`'s CHECK would reject it, so
/// a parse that produced one would be assembling a request designed to fail — and the failure would
/// reach the route as an sqlite error about a decision, which is the least readable place for *the
/// model said nothing* to turn up. Rust's `trim` strips every Unicode space, which is strictly more
/// than the CHECK's `' ' || char(9) || char(10) || char(13)`; being the stricter of the two is the
/// right way round, because the row this refuses and that one would admit is a reason made of one
/// non-breaking space.
///
/// **Case is normalised and that is not leniency.** `SILENCED` is a capitalisation, not a third
/// answer. It is done here and deliberately NOT in [`Judgement::from_wire`], which reads the COLUMN:
/// there a case difference means somebody wrote a value no writer of ours produces, and quietly
/// accepting it would hide exactly that.
///
/// Prose around the JSON is tolerated, through the same `json_object` `map_intent` uses — models
/// wrap answers in fences and apologies, and refusing those would spend a model call on a habit.
pub fn parse_answer(answer: &str) -> Result<Answer, Unreadable> {
    let Some(raw) = crate::map_intent::json_object(answer)
        .and_then(|slice| serde_json::from_str::<RawAnswer>(slice).ok())
    else {
        return Err(Unreadable::NotAnAnswer);
    };

    let said = raw.verdict.unwrap_or_default();
    let said = said.trim();
    let Some(judgement) = Judgement::from_wire(&said.to_ascii_lowercase()) else {
        return Err(Unreadable::ThirdVerdict(clipped(said, MAX_QUOTED_VERDICT)));
    };

    let reason = raw.reason.unwrap_or_default();
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(Unreadable::NoReason(judgement));
    }

    Ok(Answer {
        judgement,
        reason: clipped(reason, MAX_REASON_BYTES),
    })
}

/// What the daemon writes when the triager answered and nobody could read the answer.
///
/// **A row, and not the absence a strict reading of this module would give.** [`parse_answer`]
/// refuses to invent a verdict and that rule is untouched — what it forbids is FABRICATING one,
/// specifically defaulting to [`Judgement::Silenced`], which would let a confused model quietly
/// clear the pile: §6's authority handed back through a parser, at the one door §6.1 does not think
/// to watch. This is not that, and the difference is the whole justification:
///
/// - **[`Judgement::Flagged`] is the safe direction and it is always available to a machine.** §6
///   forbids the triager to *approve* and forbids it nothing else; asking for the owner's eyes takes
///   no authority away from anybody. A decision whose triage answer could not be read genuinely
///   *does* deserve them — that is not a guess about the code, it is a fact about the run.
/// - **The reason says a machine wrote it, in words a reader cannot mistake for the model's.** §6.2
///   makes the reason column §13's only mitigation, and a mitigation that reads as *the triager
///   thinks this is worth your time* when the triager thought nothing at all is worse than none. So
///   the sentence names the failure first and quotes what actually came back second.
///
/// **What it buys, which is the reason it exists at all.** Writing nothing leaves the decision *not
/// looked at* — honest, and starving: with no record that it was ever attempted, it is stale again
/// on the next run and consumes a slot of the batch cap for ever. Twenty decisions that reliably
/// confuse the model would mean nothing behind them is ever triaged again, and the report would go
/// on saying so accurately while the map quietly stopped working. The row carries an
/// `inputs_digest`, so the decision reads current and is not asked about again until its inputs
/// actually change.
///
/// **What it costs, named rather than left to be discovered.** A transient glitch — one malformed
/// answer from a model that would have answered properly a second later — pins that decision as
/// flagged until its anchor code or its text moves, or until [`TRIAGE_PROMPT_VERSION`] is bumped.
/// That is the trade, and it is the right way round: a visible wrong flag costs one unnecessary
/// look and can be argued with, while an invisible dead queue costs the feature.
///
/// Clipped through the same [`MAX_REASON_BYTES`] every other reason is, and in that order — the
/// sentence naming the failure comes first, so a cut takes the model's babble and never the
/// explanation of why the row is there.
pub fn unreadable_flag(why: &Unreadable, said: &str) -> Answer {
    Answer {
        judgement: Judgement::Flagged,
        reason: clipped(
            &format!(
                "{DAEMON_MARK} the triager's answer could not be read, so nobody has looked at \
                 this decision — {why}. It said: {said}"
            ),
            MAX_REASON_BYTES,
        ),
    }
}

/// What a reason this daemon wrote itself opens with.
///
/// **A convention, and therefore something a reader must be able to test rather than remember.**
/// This daemon already marks its own words this way in the one other place a machine writes into a
/// field a person reads — see the runner's stderr — and here it carries a meaning §6.2 depends on:
/// the pile is a list of sentences attributed to a model, and an unattributed one in the middle of
/// it would be read as the model's. `map_triage.model` names the brain that ANSWERED, which is true
/// even of the answer nobody could read, so the column alone cannot say who wrote the sentence.
///
/// Named as a constant rather than repeated as a literal because [`unreadable_flag`] writes it and
/// `GET …/map/silenced` reads it, and a convention with two spellings is one that has already
/// stopped working somewhere.
pub const DAEMON_MARK: &str = "nucleos:";

/// Did this daemon write this sentence, or did a model?
///
/// **Always `false` on the silenced pile today, and that is not a reason to leave it out.** The one
/// producer of a daemon-written reason is [`unreadable_flag`], which may only ever reach
/// [`Judgement::Flagged`] — §6 forbids the triager to approve and forbids a machine to silence for
/// exactly the same reason — so a `silenced` row carrying this mark cannot be produced by anything
/// in this crate. What the field buys is that a client never has to know that: a payload that let
/// the two be told apart only by the reader's memory of a convention is one where the first
/// machine-written silence, from wherever it comes, arrives on screen as a model's opinion.
///
/// A prefix test and not a parse. The rest of the sentence is prose meant for a person, and
/// anything that tried to read structure out of it would be inventing a format the writer does not
/// keep.
pub fn written_by_the_daemon(reason: &str) -> bool {
    reason.trim_start().starts_with(DAEMON_MARK)
}

/// One field into the hash, with its length in front of it.
///
/// **The length prefix is what keeps two different sets of inputs from hashing alike.** Without it,
/// a section called `ab` beside a text called `c` folds to the same bytes as a section `a` beside a
/// text `bc` — a collision cheap enough to hit by accident, and one whose whole cost is a judgement
/// silently presented as current about a decision that has been rewritten. `workflows::digest_of`
/// makes the same argument about bundle paths and does the same thing.
fn feed(buffer: &mut Vec<u8>, field: &str) {
    buffer.extend_from_slice(&(field.len() as u64).to_le_bytes());
    buffer.extend_from_slice(field.as_bytes());
}

/// One list of anchor paths into the hash: sorted, de-duplicated, and counted.
///
/// **Sorted here rather than trusted from the caller.** `map_join` happens to answer in path order
/// today, and a judgement that went stale because a directory walk came back differently would send
/// a model over a decision nothing had happened to — which spends money to learn nothing, and is the
/// same false alarm `map_stamp::canonical` sorts to avoid.
///
/// **The count goes in as well as the entries**, so that a path in `modules` and the same path in
/// `foreign` cannot swap places unnoticed. They are two different facts (§9.3) and the digest has to
/// be able to tell the sets apart, not merely their union.
fn feed_paths(buffer: &mut Vec<u8>, paths: &[String]) {
    let sorted: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    buffer.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
    for path in sorted {
        feed(buffer, path);
    }
}

/// The [`crate::map_join::Anchor`] variant as a word this hash owns.
///
/// **Not `Debug`, and not the serde name.** `format!("{:?}")` is not a stable form — renaming a
/// variant would re-hash every judgement in every project and send a model back over all of them
/// for an edit that changed no behaviour — and the serde name is the window's wire form, which is
/// free to be renamed for the window's own reasons. A tag this function owns can only change when
/// somebody changes it here, on purpose.
fn anchor_tag(anchor: &crate::map_join::Anchor) -> &'static str {
    use crate::map_join::Anchor;
    match anchor {
        Anchor::Declared => "declared",
        Anchor::Ambiguous => "ambiguous",
        Anchor::Silent => "silent",
        Anchor::Unnumbered => "unnumbered",
    }
}

/// What the triager looked at, hashed — the one thing `map_triage.inputs_digest` holds.
///
/// **A scalar, and that is the OPPOSITE of what slice 4 chose one file back.**
/// `map_stamps.code_digest` is readable text because §7 requires a lapsed decision to show **what
/// moved** —
/// paths added, changed and gone — which a hash can never say. Nothing shows a diff of these inputs
/// to anybody. The only question ever asked of this value is [`is_current`]'s, that question is an
/// equality test, and a hash is the cheapest honest way to answer it.
///
/// **What it covers is everything whose change would make the answer wrong, and nothing else:** the
/// decision's `text` and `section`, because the model read them; the anchor set — `modules` and
/// `foreign`, apart — because those are the proof it was shown; the [`crate::map_join::Anchor`]
/// variant, because *declared* and *guessed* are different evidence about the very same paths; the
/// anchor blobs, because §10's first trigger is the anchor code moving; and
/// [`TRIAGE_PROMPT_VERSION`], because the evidence is only half of what produced an answer and the
/// question is the other half.
///
/// **[`crate::map_stamp::Standing`] is deliberately NOT covered.** §10 gives the triager one scope —
/// a decision *"se nunca foi vista"* — so a decision that leaves `Never` leaves triage altogether.
/// Its judgement does not go stale; it goes irrelevant, and the reader drops it on the standing. A
/// digest that moved when somebody stamped something would send a model back over rows the owner has
/// already answered.
///
/// **All four states of [`crate::map_stamp::Anchors`] go in distinguishably, and that is the trap
/// this function exists to avoid.** An `unwrap_or_default()` on the way in makes *git would not
/// answer* — transient, a fact about this daemon — hash identically to *computed, and there is
/// nothing to watch* — permanent, a fact about the decision. A judgement made in the minute git was
/// broken would then stay current for ever over anchors it never saw, which is the collapse `0118`
/// was amended to prevent, reappearing one table over. A tag before the payload keeps the four
/// apart; `the_four_anchor_states_hash_to_four_different_digests` asserts all six pairs.
///
/// **The cost of covering the blobs, measured rather than left to be discovered.** A decision
/// anchored to a file that changes weekly goes stale weekly, so the triager's bill scales with the
/// **commit rate** and not with the size of the backlog. Counted on this repository on 2026-08-26:
/// 109 files under `core/src` and `shell/src` name a `§`, and **17 of them were touched in the last
/// 20 commits** — 26 in the last 50. An anchor set is several files, so a decision's chance of
/// lapsing is higher than that per-file rate, not lower. This is §10's rule working and not an
/// accident — *o código âncora de uma decisão mexeu-se* is exactly when the owner is supposed to be
/// asked again — but whoever caps the batch in Task 3 should size it knowing that a working day in
/// this repository re-opens a sixth of its own anchors, so the cap will sit saturated rather than
/// draining a backlog once.
///
/// `sha256:` in front for the reason `workflows::digest_of` puts it there: `0119` refuses to
/// constrain the column to an alphabet or a length, precisely so tomorrow's hash can land in it, and
/// a stored value that names the function that produced it is the only way anybody tells the two
/// apart afterwards. The hex itself comes from `workflows::hash_of`, which is this codebase's one
/// spelling of *sha256 these bytes*; a second one would be a second answer to *did this change*.
pub fn inputs_digest(evidence: Evidence<'_>) -> String {
    digest_against(evidence, TRIAGE_PROMPT_VERSION)
}

/// [`inputs_digest`] with the prompt version handed in rather than read off the constant.
///
/// **It exists so the version can be tested at all.** A constant folded in at the only call site is
/// a field no test can vary, so a later edit could drop it and every test would stay green — which
/// is the shape of false green this repository has already been bitten by once, in its own module-map
/// gate. Private, and `inputs_digest` is the only door in production: nothing outside gets to choose
/// which question a judgement is recorded against.
fn digest_against(evidence: Evidence<'_>, prompt_version: u32) -> String {
    use crate::map_stamp::Anchors;

    let decision = evidence.decision;
    let mut buffer = Vec::new();
    // The question first, and in its own labelled fixed-width field rather than folded in with the
    // evidence. The tag is length-prefixed like every other field, so nothing that follows can land
    // at this offset and imitate a different version; the four bytes after it are the whole of the
    // number, so 1 and 11 cannot be confused the way `"1"` and `"11"` running together would be.
    feed(&mut buffer, "triage-prompt-version");
    buffer.extend_from_slice(&prompt_version.to_le_bytes());
    feed(&mut buffer, &decision.text);
    feed(&mut buffer, &decision.section);
    feed(&mut buffer, anchor_tag(&decision.anchor));
    feed_paths(&mut buffer, &decision.modules);
    feed_paths(&mut buffer, &decision.foreign);

    // The tag is fed separately from the payload, so `Computed("")` differs from `NoRepository` by
    // the tag and from `Computed(text)` by the payload. Folding them into one string — `""` for the
    // failures — is the `unwrap_or_default()` this doc spends a paragraph on, wearing a match.
    let (tag, payload) = match evidence.anchors {
        Anchors::Computed(digest) => ("computed", digest.as_str()),
        Anchors::NoRepository => ("no-repository", ""),
        Anchors::Failed => ("git-failed", ""),
    };
    feed(&mut buffer, tag);
    feed(&mut buffer, payload);

    format!("sha256:{}", crate::workflows::hash_of(&buffer))
}

/// Is a stored judgement still about the same thing?
///
/// **Kept here rather than in `map_store.rs`, and that split is settled.** Deciding this needs the
/// digest of the inputs as they stand NOW, which means reading the repository — exactly what
/// `map_store` is kept away from so its SQL stays exercisable with no git anywhere near it. So
/// `judgements()` returns the latest row whatever its digest says and this compares, which is the
/// same seam [`crate::map_stamp::standing`] sits on. It also keeps §6.2's silenced pile whole: a
/// reader that filtered would hide the judgements most worth reading — the ones whose reason was
/// written about code that has since moved.
///
/// **A blank on either side is never current**, and that is the one direction this function is not
/// allowed to fail in. `0119` refuses a blank `inputs_digest` at the table for the same reason it
/// gives in its own header — an empty hash compares EQUAL to the next empty one, and so presents a
/// stale judgement as current. The CHECK guards the write; this guards the read, and the read is
/// what decides whether a model is asked again. Unreachable while both hold, and written anyway,
/// because the cost of being wrong here is a judgement that never expires.
///
/// Trimmed on both sides before comparing: whitespace that crossed a text boundary is not a
/// different answer, and this working tree is CRLF.
pub fn is_current(recorded: &str, current: &str) -> bool {
    let recorded = recorded.trim();
    !recorded.is_empty() && recorded == current.trim()
}

/// §5.3's two remaining numbers, and the piles they are made of.
///
/// **A sibling of [`crate::map_stamp::StampCounts`] and deliberately not a widening of it.** The
/// tempting move is a few more fields over there, where the rest of the header already lives. It
/// would cost the one property this type is for: `StampCounts` is the DENOMINATOR these numbers are
/// checked against — [`Self::flagged`] plus [`Self::silenced`] plus [`Self::untriaged`] must equal
/// `StampCounts::never` — and a single function producing both sides of that equation could not be
/// checked by anything, because both sides would be wrong together. `map_stamp::counts` must stay
/// callable with no judgement anywhere near it.
///
/// The second reason is whose rule is being applied. Everything below rests on *triage describes
/// only [`crate::map_stamp::Standing::Never`]*, which is §10's scope for the triager and this
/// module's subject — the same sentence [`inputs_digest`] leaves `Standing` out of the hash for.
/// `map_stamp.rs` is the stamp and its expiry; a filter about what the MODEL is allowed to have an
/// opinion on does not belong in it.
///
/// **Required to add up, and by construction rather than by subtraction — see [`reconcile`].**
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageCounts {
    /// Decisions still in [`crate::map_stamp::Standing::Never`] whose current judgement is
    /// [`Judgement::Flagged`]. **The only pile that leaves §5.3's `K`**, and it leaves because it
    /// has arrived somewhere: it is in `J`, in front of the owner, and counting it in both would
    /// break the sum.
    pub flagged: usize,
    /// Decisions still in [`crate::map_stamp::Standing::Never`] whose current judgement is
    /// [`Judgement::Silenced`].
    ///
    /// **Counted here AND left inside [`Self::unseen`], which is the correction this type needed
    /// most.** A silence is *"sem sinal de problema. **Ninguém olhou.** Não é verde"* (§5.1) — a
    /// claim about the triager and about nothing else — so it is debt the owner has still never
    /// given a verdict on, and §5.3 says `K` *"é dívida, e é suposto incomodar"*. Subtracting it
    /// would let a triager that silences three hundred decisions drive `K` to zero over a backlog
    /// nobody has read: §1's false confidence, manufactured by the cure's own arithmetic, on the
    /// one line that exists to be uncomfortable. §5.3 authorises exactly one departure from `K` —
    /// *"o triador acrescenta-lhe as decisões que assinalar, **que saem de `K`**"* — and that
    /// sentence is about `J` and about the flagged. It says nothing whatever about a silence.
    ///
    /// **What a silence buys, since it looks like it buys nothing.** It buys **not being in `J`.**
    /// That is the triager's whole job: deciding what deserves the owner's eyes, never deciding
    /// what has already been seen. `K` stays large and honest, `J` stays short and actionable, and
    /// §6.2's pile carries the reason for every silence so a triager that silenced what it should
    /// have shown is findable. A number that grows here is also far easier to notice than a pile
    /// quietly absorbing a backlog, which is what §13 rates a **real** residual risk.
    pub silenced: usize,
    /// Decisions still in [`crate::map_stamp::Standing::Never`] that no CURRENT judgement describes
    /// at all — never triaged, or triaged and the answer has since gone stale.
    ///
    /// Inside [`Self::unseen`] with [`Self::silenced`], and apart from it because the cures differ:
    /// this pile is what a press of the triage button consumes, and that one is what a reading of
    /// §6.2's pile is for. One number would leave the owner unable to tell which of the two they
    /// were being offered.
    pub untriaged: usize,
    /// §5.3's `K nunca vistas` — `silenced + untriaged`, which is `StampCounts::never − flagged`.
    ///
    /// **Derived, and derived HERE rather than by whoever draws the panel**, exactly as
    /// `StampCounts::unwatched` is and for its reason: §9.3 makes `core/` the single owner of this
    /// map's derivations, this is the number the panel actually prints, and a shell adding the two
    /// itself would be a second implementation free to drift. A redundant field is a field that can
    /// disagree with its parts, so it is incremented beside each part in one pass and a test pins
    /// the sum — the discipline `unwatched` already keeps.
    ///
    /// **Counted rather than subtracted, which is the whole reason it is built this way.** See
    /// [`reconcile`]: every `Never` decision falls into exactly one of [`Self::flagged`],
    /// [`Self::silenced`] and [`Self::untriaged`] in one pass, so `K` is two of those three buckets
    /// and never the result of an arithmetic that could go negative and be clamped. A debt figure
    /// repaired by a `saturating_sub` is the one number in this header nobody would ever catch
    /// being wrong.
    pub unseen: usize,
    /// §5.3's `J à tua espera` — `StampCounts::lapsed + flagged`.
    ///
    /// The union slice 4 wrote down and could not yet compute: *"`J` são hoje os carimbos que
    /// caducaram. Cresce na fatia 5: o triador acrescenta-lhe as decisões que assinalar, que saem de
    /// `K`."* The two halves reach the owner for opposite reasons — a lapse is the owner's own green
    /// gone stale, a flag is a model asking — and they are one number because what the header
    /// promises is *how many things want you today*, which is a count and not a taxonomy.
    ///
    /// **The way out of this number is a stamp, and §5.2 already has the right one.** A flag the
    /// owner has read and decided is noise leaves through *a meio, e eu sei* — whose stated purpose
    /// is *"converte um não sabia num sabia, que é metade da cura"* — with a note saying so. There
    /// is no dismiss button and there must not be a fourth verdict for one: dismissing without
    /// saying anything is precisely the *não sabia* this map exists to convert.
    pub waiting: usize,
    /// How many of `flagged + silenced` this reading could **not** re-check, because git would not
    /// say what their anchor code is.
    ///
    /// **A subset of the two piles above and never a fourth bucket**, the way `StampCounts::guessed`
    /// is a subset of `settled`: these judgements still count, in whichever pile they were already
    /// in. What this number says is how much of the header rests on an answer that could not be
    /// verified against the code as it stands right now.
    ///
    /// **It exists because the alternative was a lie the size of the project.** Dropping an
    /// unverifiable judgement would empty `triage` during a git outage and report the whole backlog
    /// as *nunca vista* — a fact about this daemon presented as a fact about the project. See
    /// [`Held::checked`]: a flag we could not re-verify is still a flag; a flag we discarded becomes
    /// a claim that nobody ever looked.
    pub unchecked: usize,
}

/// One judgement the map is still holding, and whether this reading could re-check it.
///
/// **Three-valued staleness, wearing two fields.** [`is_current`] answers *yes* or *no* about a
/// digest, and there is a third case it cannot express: the digest could not be computed at all,
/// because the decision's anchors came back [`crate::map_stamp::Anchors::Failed`]. That is neither
/// *this answer still stands* nor *this answer has expired*, and guessing either is the error —
/// slice 4 took exactly this posture one table over with `map_stamp::Lapse::Unreadable`, and
/// `POST /map/triage` takes it again when it skips a decision rather than triaging it under a
/// `git-failed` digest. A judgement that survived the caller's staleness test arrives here
/// `checked: true`; one that could not be tested arrives `checked: false`; one that was tested and
/// failed never arrives at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    /// **`false` means the digest could not be COMPUTED, and never that it failed to match.**
    ///
    /// The panel may not present such a row as verified against the code as it stands — but it must
    /// still draw it, and [`TriageCounts`] must still count it. Dropping it is what produced the
    /// false *nunca vista* over an entire project every time git hiccuped.
    pub checked: bool,
    /// The row itself, flattened onto the wire so a client reads `judgement` and `reason` where it
    /// would read them anyway — the arrangement `RosterEntry` already uses for a summary with one
    /// fact added.
    #[serde(flatten)]
    pub judged: crate::map_store::Judged,
}

/// The judgements that still describe this map, and §5.3's numbers over exactly those.
///
/// **One value carrying both, because the header disagreeing with the rows beneath it is the
/// disease this whole feature is the cure for.** Two functions — one filtering, one tallying —
/// would be two chances for a caller to ship one set and count another, and the screen would look
/// entirely correct while doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// What the panel may draw, by `decision_id`.
    ///
    /// **A map and never a parallel array**, the rule `MapAnswer::standings` and
    /// [`crate::map_recency::Recency::ages`] already keep: `junction.decisions` is re-sorted into
    /// §10's order after the join, and an index-aligned array would silently start drawing one
    /// decision's verdict against another's text.
    pub judgements: std::collections::BTreeMap<i64, Held>,
    /// The numbers, tallied in the same pass that built the map above.
    pub counts: TriageCounts,
}

/// Which judgements are answers about the map as it stands, and what they add up to.
///
/// **The two filters, and they are the whole of this function.** §5.3's arithmetic — `J = lapsed +
/// flagged`, `K = never − flagged` — does not hold on a raw reading of the table, and it fails in
/// two independent ways:
///
/// 1. **Standing.** Nothing deletes a judgement when a decision is later stamped, and nothing
///    should: §6.2 keeps the silenced pile *sempre acessível*, with no exception for decisions that
///    moved on, and the row proving a silence happened IS §13's mitigation. So
///    [`crate::map_store::judgements`] returns a `Flagged` row for a decision the owner settled last
///    week, and a naive tally counts that decision in `settled` **and** in `flagged` — two header
///    lines on one screen disagreeing, with nobody noticing, which is precisely the failure
///    `StampCounts` already has a property test for. Triage only ever describes
///    [`crate::map_stamp::Standing::Never`] (§10), so the loop below walks the STANDINGS and lets
///    the judgements answer, rather than walking the judgements and hoping.
/// 2. **Staleness, and it is three-valued rather than two.** A judgement whose inputs have moved is
///    not an answer about the code as it stands; counting it reports a model's opinion of a file it
///    never saw. That filter is [`is_current`] against a digest computed now, and it cannot happen
///    here — computing that digest means reading the repository, which is what keeps `is_current`
///    and `map_store` on opposite sides of the same seam [`crate::map_stamp::standing`] sits on.
///    **The caller does it, and hands over what survived as [`Held`]** — including the judgements it
///    could not test at all, marked [`Held::checked`] `false`, which are counted here in whichever
///    pile they belong to. Dropping those is what turned a git outage into a whole project
///    reporting *nunca vista*.
///
/// **This is the opposite of what `GET …/map/silenced` does, and the two are not an
/// inconsistency.** This function answers *what is true now*, so a judgement about a decision that
/// has moved on, or about code that has changed, is not an answer to it — and a decision that has
/// been **retired** is not in this map at all, so it has no standing here to be counted against.
/// That pile is the audit trail of *what the triager did*, and it drops none of the three: a
/// silencing that was wrong is exactly as worth reading after the decision was stamped as before,
/// after its code moved as before, and after somebody retired the decision as before. §6.2's *um
/// bug só é corrigível se for visível* is a rule about the record, and every one of those three
/// filters would delete part of it. Both doors are needed and neither may be made to look like the
/// other.
///
/// **The reconciliation is by construction and not by arithmetic.** Every `Never` standing lands in
/// exactly one of `flagged`, `silenced` and `untriaged`, in one pass, so `flagged + silenced <=
/// never` is not a promise a comment makes — there is no path through this loop that could break
/// it. The alternative shape, `unseen = never - flagged - silenced`, is one `saturating_sub` away
/// from reporting a clean-looking header over a population that does not reconcile, which is the
/// same silent wrongness one level down inside the cure — and it was also the wrong formula: see
/// [`TriageCounts::silenced`] for why a silence stays in `K`.
///
/// **`stamps` is read for `lapsed` alone**, and the `debug_assert` below is what pins it to the
/// same reading as `standings`: the two arguments are meant to be one answer about one project at
/// one instant, and a caller that mixed a fresh tally with a stale map would produce a `J` that was
/// never true at any single moment. The `never` count is deliberately NOT taken from it — recounting
/// it here is what makes the assertion say something.
pub fn reconcile(
    standings: &std::collections::BTreeMap<i64, crate::map_stamp::Standing>,
    stamps: &crate::map_stamp::StampCounts,
    current: Vec<Held>,
) -> Reconciled {
    let mut unfiltered: std::collections::BTreeMap<i64, Held> = current
        .into_iter()
        .map(|held| (held.judged.decision_id, held))
        .collect();

    let mut judgements = std::collections::BTreeMap::new();
    let mut counts = TriageCounts {
        flagged: 0,
        silenced: 0,
        untriaged: 0,
        unseen: 0,
        waiting: 0,
        unchecked: 0,
    };

    for (decision_id, standing) in standings {
        if !matches!(standing, crate::map_stamp::Standing::Never) {
            continue;
        }
        // `remove` and not `get`, so a judgement can be counted at most once even if the caller
        // handed over two rows for one decision — which `map_store::judgements` cannot produce and
        // which a reader of the whole append-only history, like `map_store::silencings`, does.
        match unfiltered.remove(decision_id) {
            Some(held) => {
                match held.judged.judgement {
                    // `unseen` is incremented beside each of its two parts rather than summed at
                    // the bottom, so the field and the parts are written by one pass over one match
                    // and cannot fall out of step — the arrangement `map_stamp::counts` uses for
                    // `unwatched`. A test pins the sum regardless.
                    Judgement::Flagged => counts.flagged += 1,
                    Judgement::Silenced => {
                        counts.silenced += 1;
                        counts.unseen += 1;
                    }
                }
                if !held.checked {
                    counts.unchecked += 1;
                }
                judgements.insert(*decision_id, held);
            }
            None => {
                counts.untriaged += 1;
                counts.unseen += 1;
            }
        }
    }

    counts.waiting = stamps.lapsed + counts.flagged;

    debug_assert_eq!(
        counts.flagged + counts.silenced + counts.untriaged,
        stamps.never,
        "the standings this tallied and the standings the header was tallied from are one reading, \
         or `K` is a number about two different moments"
    );

    Reconciled { judgements, counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_intent::Kind;
    use crate::map_join::{Anchor, Anchored};
    use crate::map_stamp::Anchors;

    /// One approved decision, as the junction hands it over.
    ///
    /// Built by hand rather than by running `map_join`, for the reason `map_stamp`'s tests build
    /// their digests by hand: a test that asked the junction for its input would be asserting about
    /// two modules at once and would go red for a change in neither of the things it names.
    fn decision(anchor: Anchor, modules: &[&str], foreign: &[&str]) -> Anchored {
        Anchored {
            decision_id: 7,
            ordinal: 3,
            spec_slug: "2026-08-24-mapa-do-projeto-design".to_owned(),
            section: "## 6. O papel do modelo".to_owned(),
            text: "O triador pode silenciar com razão registada; nunca pode pôr verde.".to_owned(),
            kind: Kind::Character,
            anchor,
            modules: modules.iter().map(|path| (*path).to_owned()).collect(),
            foreign: foreign.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    /// The digest of one decision read against one state of the anchors.
    fn digest_of(decision: &Anchored, anchors: &Anchors) -> String {
        inputs_digest(Evidence { decision, anchors })
    }

    #[test]
    fn a_model_that_answers_approved_is_a_parse_failure_and_not_a_silence() {
        // **The most important assertion in this slice**, and the two tempting defaults are wrong in
        // opposite directions and by different amounts.
        //
        // Defaulting to `silenced` would let a confused model clear a decision out of the owner's
        // queue on the strength of a word nobody could read — the authority §6 takes away from the
        // model, handed back by a parser, which is the one door §6.1 does not think to watch. §13
        // rates *o triador silencia o que devia mostrar* a real residual risk; this would be that
        // risk arriving without a triager ever having had an opinion.
        //
        // Defaulting to `flagged` is the cheap failure — one unnecessary look — and it is still a
        // fabricated judgement, and it would carry a fabricated reason into the very column §6.2
        // makes §13's only mitigation. A pile whose reasons were written by a parser is a pile the
        // owner stops believing, and that is the same trust, spent from the other side.
        //
        // Neither. No row is written and the decision stays *not looked at*, which is what it is.
        let said = r#"{"verdict":"approved","reason":"This matches the code."}"#;

        let answer = parse_answer(said);

        assert_eq!(
            answer,
            Err(Unreadable::ThirdVerdict("approved".to_owned())),
            "the word the model actually used comes back with the failure, because a triager that \
             answers a third thing is a bug and a bug is only fixable if it is visible (§6.2)"
        );
        // Said again the other way round, because this is the assertion that would rot silently: no
        // failure path here may produce a judgement of any kind, and `Silenced` least of all.
        assert!(!matches!(&answer, Ok(Answer { .. })));
    }

    #[test]
    fn a_model_that_declares_everything_correct_in_prose_is_not_a_silence() {
        // The realistic version of the test above. A model rarely answers `approved`; it explains,
        // in a paragraph, that everything looks right — which is the sentence §1 says this whole
        // feature exists because somebody once believed. It parses to nothing, so nobody looked.
        let said = "I have reviewed this decision against the files listed and everything looks \
                    correct — the code matches what the document says, so no action is needed.";

        assert_eq!(parse_answer(said), Err(Unreadable::NotAnAnswer));

        // And the same prose with a JSON-shaped afterthought that still names no verdict: the
        // outermost braces parse, the fields do not, and the answer is still no answer.
        let hedged = "Everything checks out. {\"note\":\"all good\"}";
        assert_eq!(
            parse_answer(hedged),
            Err(Unreadable::ThirdVerdict(String::new())),
            "an absent verdict is a third answer, not an empty one"
        );
    }

    #[test]
    fn an_empty_reason_is_a_parse_failure_even_when_the_verdict_parses() {
        // `0119` refuses a blank reason at the table, so a parse that produced one would be
        // assembling a request designed to fail — and the failure would arrive at the route as a
        // database error about a decision, which is the least readable place for it. Refused here,
        // where what happened is still known: a verdict with nothing to read behind it.
        let said = r#"{"verdict":"silenced","reason":""}"#;

        assert_eq!(
            parse_answer(said),
            Err(Unreadable::NoReason(Judgement::Silenced))
        );

        // A flag with no reason is refused by the same rule and for its own reason: a nag the owner
        // cannot answer is one they stop reading.
        let flagged = r#"{"verdict":"flagged","reason":null}"#;
        assert_eq!(
            parse_answer(flagged),
            Err(Unreadable::NoReason(Judgement::Flagged))
        );
    }

    #[test]
    fn a_reason_that_is_only_whitespace_is_the_same_failure() {
        // `0119`'s CHECK strips space, tab, CR and LF — not space alone, which is all a bare `trim`
        // does in SQLite — precisely so a reason of one tab cannot satisfy a constraint whose whole
        // purpose is that the text says something. This side has to strip at least as much, or a
        // row that got through here would be refused there, and the route would report a database
        // error about a decision when what happened was a model saying nothing.
        for blank in ["   ", "\\t", "\\n", "\\r\\n\\t ", " \u{a0} "] {
            let said = format!(r#"{{"verdict":"silenced","reason":"{blank}"}}"#);
            assert_eq!(
                parse_answer(&said),
                Err(Unreadable::NoReason(Judgement::Silenced)),
                "a reason of {blank:?} says nothing and must not reach the table"
            );
        }
    }

    #[test]
    fn a_well_formed_answer_becomes_the_two_things_the_table_needs() {
        let said = "Here you go:\n```json\n{\"verdict\":\"flagged\",\"reason\":\"Nada no repo \
                    nomeia esta secção.\"}\n```";

        let answer = parse_answer(said).expect("a flag with a reason is an answer");

        assert_eq!(answer.judgement, Judgement::Flagged);
        assert_eq!(answer.reason, "Nada no repo nomeia esta secção.");

        // A capitalised verdict is a capitalisation and not a third answer. Normalised here and
        // deliberately NOT in `Judgement::from_wire`, which reads the COLUMN: there a case
        // difference means somebody wrote a value no writer of ours produces, and quietly accepting
        // it would hide that.
        let shouted = r#"{"verdict":"SILENCED","reason":"Nada estranho."}"#;
        assert_eq!(
            parse_answer(shouted)
                .expect("case is not a third answer")
                .judgement,
            Judgement::Silenced
        );
    }

    #[test]
    fn a_reason_long_enough_to_be_an_essay_is_kept_whole_or_cut_visibly() {
        // **Cut, and the cut says so** — the choice defended in [`MAX_REASON_BYTES`]. A reason that
        // arrives as an essay is a model that answered a different question at length, and §6.2's
        // pile is read one line per decision; three hundred essays is a pile nobody opens, which
        // costs the same mitigation an unreadable reason costs. What is forbidden is the SILENT
        // cut: a bug report with its ending removed reads as a complete thought that stops making
        // sense.
        let essay = format!("Isto merece atenção porque {}", "x".repeat(4_000));
        let said = format!(r#"{{"verdict":"flagged","reason":"{essay}"}}"#);

        let answer = parse_answer(&said).expect("an essay is still an answer");

        assert!(answer.reason.starts_with("Isto merece atenção porque"));
        assert!(
            answer.reason.ends_with(CUT_MARK),
            "the cut is on the page, or it is a silent truncation"
        );
        assert!(answer.reason.len() <= MAX_REASON_BYTES + CUT_MARK.len());
    }

    #[test]
    fn a_reason_cut_in_the_language_these_reasons_are_written_in_does_not_panic() {
        // The test above is ASCII, where every byte index is a character boundary. These decisions
        // are Portuguese and the reason comes back in the document's own language, so the cut lands
        // inside a `ç` or an `ã` often enough — and `&reason[..MAX_REASON_BYTES]` panics there
        // rather than truncating. A daemon that died because a reason happened to be the wrong
        // length would be the worst shape this failure could take: invisible until the one decision
        // that triggers it, and then fatal. `map_intent::bounded` learned this going the other way.
        let essay = "não é a mesma secção ".repeat(MAX_REASON_BYTES);
        let said = format!(r#"{{"verdict":"silenced","reason":"{essay}"}}"#);

        let answer = parse_answer(&said).expect("an answer in Portuguese is an answer");

        assert!(answer.reason.ends_with(CUT_MARK));
        assert!(answer.reason.len() <= MAX_REASON_BYTES + CUT_MARK.len());
    }

    #[test]
    fn the_prompt_names_the_readable_files_and_the_unreadable_ones_apart() {
        // §9.3: *we know something is there* and *we can see what it is* are two different facts,
        // and `map_join` keeps them in two lists so the second cannot borrow the first's confidence.
        // A prompt that concatenated them would hand the model a file list it would read as code —
        // and 77 Go files in this repository name a `§` that nothing here can read. The model would
        // then silence a decision on the strength of evidence nobody has looked at, which is §13's
        // residual risk manufactured by the prompt rather than by the model.
        let decision = decision(
            Anchor::Ambiguous,
            &["core/src/map_triage.rs"],
            &["sidecars/telegram/main.go"],
        );
        let prompt = triage_prompt(Evidence {
            decision: &decision,
            anchors: &Anchors::Computed("aaa core/src/map_triage.rs".to_owned()),
        });

        let readable = prompt
            .find("core/src/map_triage.rs")
            .expect("the readable module is named");
        let unreadable = prompt
            .find("sidecars/telegram/main.go")
            .expect("the foreign file is named");
        let heading = prompt
            .find("CANNOT read")
            .expect("the second list says what it is");

        assert!(
            readable < heading && heading < unreadable,
            "the two lists are two headings and two lists, never one list with everything in it"
        );
        assert!(prompt.contains("Readable code naming this section (1)"));
    }

    #[test]
    fn the_prompt_says_that_approving_is_not_among_the_answers() {
        // §6's table names exactly one thing forbidden to the triager — *Aprovar* — and a model
        // told only "answer flagged or silenced" still explains, at least sometimes, that
        // everything looks correct. The prompt has to say both halves: approving is not on offer,
        // AND the word for *I saw nothing wrong* is `silenced`, which claims nothing further.
        let decision = decision(Anchor::Silent, &[], &[]);
        let prompt = triage_prompt(Evidence {
            decision: &decision,
            anchors: &Anchors::Computed(String::new()),
        });

        assert!(prompt.contains("Approving is not one of your options"));
        assert!(prompt.contains("\"flagged\"") && prompt.contains("\"silenced\""));
        assert!(
            !prompt.contains("\"approved\""),
            "the forbidden word is never offered as a value, not even to be refused"
        );
        // The decision itself, its heading, and the document it came from, or the model is
        // answering about nothing.
        assert!(prompt.contains("O triador pode silenciar"));
        assert!(prompt.contains("## 6. O papel do modelo"));
        assert!(prompt.contains("2026-08-24-mapa-do-projeto-design"));
    }

    #[test]
    fn an_unnumbered_anchor_is_never_presented_as_code_that_is_missing() {
        // `map_join::Anchor::Unnumbered` is *no search ran*, and `Silent` is *a search ran and found
        // nothing*. Collapsing them in the prompt would have the model flag a decision extracted
        // from `## Contrato` for having no code — a finding reported from a search that never
        // happened, in a pile whose whole value is that its contradictions are real.
        let decision = decision(Anchor::Unnumbered, &[], &[]);
        let prompt = triage_prompt(Evidence {
            decision: &decision,
            anchors: &Anchors::Computed(String::new()),
        });

        assert!(prompt.contains("no search ran"));
        assert!(
            !prompt.contains("no code claims it"),
            "that is `Silent`'s sentence — a search that ran and found nothing — and it must not \
             be said about a decision nothing was ever looked for"
        );
    }

    #[test]
    fn a_file_list_too_long_to_send_is_capped_and_says_how_many_it_left_out() {
        // A silent cap reads as "these are the files", which is the same lie as a silent truncation
        // one field over.
        let paths: Vec<String> = (0..MAX_LISTED_PATHS + 5)
            .map(|n| format!("core/src/module_{n:03}.rs"))
            .collect();
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        let decision = decision(Anchor::Ambiguous, &borrowed, &[]);

        let prompt = triage_prompt(Evidence {
            decision: &decision,
            anchors: &Anchors::Failed,
        });

        assert!(prompt.contains("and 5 more not listed"));
        assert!(prompt.contains(&format!("({})", MAX_LISTED_PATHS + 5)));
    }

    #[test]
    fn the_inputs_digest_changes_when_the_anchor_set_changes_and_not_when_its_order_does() {
        let anchors = Anchors::Computed("aaa core/src/map_triage.rs".to_owned());
        let one_way = decision(Anchor::Ambiguous, &["b.rs", "a.rs"], &[]);
        let other_way = decision(Anchor::Ambiguous, &["a.rs", "b.rs"], &[]);

        assert_eq!(
            digest_of(&one_way, &anchors),
            digest_of(&other_way, &anchors),
            "an order the filesystem chose is not a change to the evidence, and a judgement that \
             lapsed because a directory walk came back differently is a model call bought with \
             nothing"
        );

        let grown = decision(Anchor::Ambiguous, &["a.rs", "b.rs", "c.rs"], &[]);
        assert_ne!(digest_of(&one_way, &anchors), digest_of(&grown, &anchors));

        // Moving a path from the readable list to the foreign one is a change even though the set
        // of paths is identical: the two lists are two different facts (§9.3), and a digest that
        // merged them would keep a judgement current across the one change most worth re-reading —
        // a file this map used to be able to parse and now cannot.
        let moved = decision(Anchor::Ambiguous, &["a.rs"], &["b.rs"]);
        assert_ne!(digest_of(&one_way, &anchors), digest_of(&moved, &anchors));
    }

    #[test]
    fn the_four_anchor_states_hash_to_four_different_digests() {
        // **The trap this test exists to spring.** `Anchors` has four distinguishable states and an
        // `unwrap_or_default()` on the way into the hash collapses two of them: *git would not
        // answer*, which is transient and a fact about this daemon, and *computed, and there is
        // nothing to watch*, which is permanent and a fact about the decision. Collapsed, a
        // judgement made in the minute git was broken stays "current" for ever over anchors it
        // never saw — the exact failure `0118` was amended to prevent, reappearing one table over.
        //
        // All six pairs, and not just the one that is easy to think of.
        let decision = decision(Anchor::Ambiguous, &["core/src/map_triage.rs"], &[]);
        let states = [
            (
                "computed",
                Anchors::Computed("aaa core/src/x.rs".to_owned()),
            ),
            ("computed-empty", Anchors::Computed(String::new())),
            ("no-repository", Anchors::NoRepository),
            ("failed", Anchors::Failed),
        ];

        let digests: Vec<(&str, String)> = states
            .iter()
            .map(|(name, anchors)| (*name, digest_of(&decision, anchors)))
            .collect();

        for (index, (left_name, left)) in digests.iter().enumerate() {
            for (right_name, right) in digests.iter().skip(index + 1) {
                assert_ne!(
                    left, right,
                    "{left_name} and {right_name} are different facts about the anchors and must \
                     not hash alike"
                );
            }
        }
    }

    #[test]
    fn a_judgement_does_not_survive_a_change_to_the_question_that_produced_it() {
        // The evidence is only half of what makes an answer; the question is the other half. Without
        // the prompt version in the hash, editing `triage_prompt` — narrowing what `silenced` means,
        // adding a field to the decision block, changing what `anchor_says` claims about a state —
        // leaves every stored judgement claiming to be current about a question that no longer
        // exists. That is an approximate answer wearing an exact one's clothes, which is the one
        // trade this feature never makes.
        //
        // Slice 4 needs none of this and the difference is worth saying: a stamp is the owner's, and
        // nothing about how it was asked for can make it wrong. This pile is a model's opinions.
        let decision = decision(Anchor::Ambiguous, &["core/src/map_triage.rs"], &[]);
        let anchors = Anchors::Computed("aaa core/src/map_triage.rs".to_owned());
        let evidence = Evidence {
            decision: &decision,
            anchors: &anchors,
        };

        assert_ne!(
            digest_against(evidence, 1),
            digest_against(evidence, 2),
            "a judgement made against one question is not an answer to a different one"
        );
        // And the version the module actually ships is the one `inputs_digest` uses, or the field is
        // in the hash without being in the answer anybody stores.
        assert_eq!(
            inputs_digest(evidence),
            digest_against(evidence, TRIAGE_PROMPT_VERSION)
        );
    }

    #[test]
    fn exposes_current_triage_prompt_version() {
        // Pinned the way `classifier::exposes_current_classifier_version` pins its own, and for the
        // reason that test's doc gives: a constant that moves without anybody narrating what moved
        // is a constant nobody can read back. This is version 1 — the first question this module
        // ever asked — so there is nothing yet to narrate.
        //
        // **Bumping this is not free and is not tidy-up.** Every judgement in every project goes
        // stale at once and the triager re-runs over the whole `Never` pile — which on this
        // repository is ~350 decisions at 20 a press, so eighteen presses of a minutes-long button
        // before the map holds what it held before the edit. Bump it when the question materially
        // changed, and not when a line was rewrapped; see [`TRIAGE_PROMPT_VERSION`] for the three
        // things that oblige it and for what the recovery actually costs.
        assert_eq!(TRIAGE_PROMPT_VERSION, 1);
    }

    #[test]
    fn the_inputs_digest_does_not_move_when_the_decision_is_stamped() {
        // §10 gives the triager exactly one scope — a decision *"se nunca foi vista"* — so a
        // decision that leaves `Standing::Never` leaves triage altogether. Its judgement does not go
        // stale; it goes irrelevant, and the reader drops it on the standing rather than on the
        // digest. A digest that moved when somebody stamped something would send a model back over
        // rows the owner has already answered, which spends real money to learn nothing.
        //
        // The standing is computed here rather than asserted about in the abstract, so the test
        // states the thing that changed: the owner's verdict moved from `Never` to `Settled` while
        // every input this digest covers held still.
        let decision = decision(Anchor::Ambiguous, &["core/src/map_triage.rs"], &[]);
        let anchors = Anchors::Computed("aaa core/src/map_triage.rs".to_owned());
        let before = digest_of(&decision, &anchors);

        let anchoring = crate::map_stamp::Anchoring {
            current: &anchors,
            named: 1,
            declared: false,
        };
        let now = chrono::Utc::now();
        let stamp = crate::map_store::Stamp {
            decision_id: decision.decision_id,
            verdict: crate::map_stamp::Verdict::Settled,
            stamped_at: now.to_rfc3339(),
            code_digest: Some("aaa core/src/map_triage.rs".to_owned()),
            note: None,
        };
        assert!(matches!(
            crate::map_stamp::standing(None, anchoring, now),
            crate::map_stamp::Standing::Never
        ));
        assert!(matches!(
            crate::map_stamp::standing(Some(&stamp), anchoring, now),
            crate::map_stamp::Standing::Settled { .. }
        ));

        assert_eq!(
            before,
            digest_of(&decision, &anchors),
            "`Standing` is deliberately not an input, and this is the assertion that says so"
        );
    }

    #[test]
    fn the_inputs_digest_moves_when_the_words_the_model_read_move() {
        let anchors = Anchors::Computed(String::new());
        let base = decision(Anchor::Silent, &[], &[]);

        let mut reworded = base.clone();
        reworded.text = "O triador nunca pode pôr verde.".to_owned();
        assert_ne!(digest_of(&base, &anchors), digest_of(&reworded, &anchors));

        let mut rehoused = base.clone();
        rehoused.section = "## 6.1 Porque é que silenciado".to_owned();
        assert_ne!(digest_of(&base, &anchors), digest_of(&rehoused, &anchors));

        // And on the anchor variant, because *declared* and *guessed* are different evidence about
        // the very same paths (§8).
        let mut certain = base.clone();
        certain.anchor = Anchor::Declared;
        assert_ne!(digest_of(&base, &anchors), digest_of(&certain, &anchors));
    }

    #[test]
    fn a_judgement_is_current_only_while_the_digest_it_was_made_against_still_holds() {
        let decision = decision(Anchor::Ambiguous, &["a.rs"], &[]);
        let anchors = Anchors::Computed("aaa a.rs".to_owned());
        let recorded = digest_of(&decision, &anchors);

        assert!(is_current(&recorded, &digest_of(&decision, &anchors)));

        let moved = Anchors::Computed("bbb a.rs".to_owned());
        assert!(!is_current(&recorded, &digest_of(&decision, &moved)));
    }

    #[test]
    fn a_blank_digest_on_either_side_is_never_read_as_current() {
        // `0119` refuses a blank `inputs_digest` at the table for exactly this reason: an empty hash
        // compares EQUAL to the next empty one and so presents a stale judgement as current. The
        // CHECK guards the write; this guards the read, and the read is what decides whether a model
        // is asked again. Unreachable while both hold — and the direction this function is never
        // allowed to fail in is the one where it answers *current* about nothing.
        assert!(!is_current("", ""));
        assert!(!is_current("   ", "   "));
        assert!(!is_current("", "sha256:abc"));
        assert!(!is_current("sha256:abc", ""));
        // Whitespace that crossed a text boundary is not a different answer.
        assert!(is_current(" sha256:abc ", "sha256:abc"));
    }

    #[test]
    fn a_failure_becomes_a_flag_that_says_a_machine_wrote_it_and_never_a_silence() {
        // The row `POST /map/triage` writes when nobody could read the answer, and every rule it
        // has to keep. **`Flagged` and never `Silenced`** is the first: §6 forbids the triager to
        // approve and forbids it nothing else, so asking for the owner's eyes is always available
        // to a machine — while a silence would clear a decision out of his queue on the strength of
        // a word nobody could read, which is the authority §6 removes, recovered by a fallback.
        let said = r#"{"verdict":"approved","reason":"Tudo bate certo."}"#;
        let why = parse_answer(said).expect_err("`approved` is not a verdict");

        let flag = unreadable_flag(&why, said);

        assert_eq!(flag.judgement, Judgement::Flagged);
        // **Says a machine wrote it**, because §6.2's pile is read as a list of sentences a model
        // produced, and an unattributed one in the middle of it would be read as one.
        assert!(flag.reason.starts_with("nucleos:"), "{}", flag.reason);
        assert!(flag.reason.contains("could not be read"));
        // **And quotes what actually came back**, because a bug is only fixable if it is visible —
        // both the failure this module named and the text that produced it.
        assert!(flag.reason.contains("approved"), "{}", flag.reason);

        // A model that answers with a page does not get a page into the column: cut like every
        // other reason, and cut from the END, so the sentence explaining why the row exists is the
        // half that survives.
        let babble = format!("{{\"verdict\":\"{}\"}}", "z".repeat(4_000));
        let why = parse_answer(&babble).expect_err("four thousand characters is not a verdict");
        let flag = unreadable_flag(&why, &babble);
        assert!(flag.reason.starts_with("nucleos: the triager"));
        assert!(flag.reason.ends_with(CUT_MARK));
        assert!(flag.reason.len() <= MAX_REASON_BYTES + CUT_MARK.len());
    }

    #[test]
    fn a_failure_says_what_it_saw_because_a_pile_nobody_can_debug_is_the_bug() {
        // §6.2's argument, applied to the answers that never became rows: *um triador que silencia o
        // que não devia é um bug do triador, e um bug só é corrigível se for visível*. A judgement
        // that failed to parse writes nothing, so the log line is the only trace it leaves, and a
        // log line that will not say what the model said sends somebody to read the prompt instead.
        let answered = r#"{"verdict":"looks fine to me","reason":"Tudo bate certo."}"#;
        let said = parse_answer(answered)
            .expect_err("a sentence is not a verdict")
            .to_string();
        assert!(said.contains("looks fine to me"));
        assert!(said.contains("flagged") && said.contains("silenced"));

        // Bounded where it is BUILT and not where it is printed, because the field came from a
        // model and a model can put a page in it — and `Debug` is the formatter a `warn!` is most
        // likely to reach for, so clipping only in `Display` would leave the page reachable.
        let essay = format!(r#"{{"verdict":"{}","reason":"x"}}"#, "z".repeat(4_000));
        let held = parse_answer(&essay).expect_err("four thousand characters is not a verdict");
        assert!(matches!(&held, Unreadable::ThirdVerdict(said) if said.ends_with(CUT_MARK)));
        assert!(format!("{held:?}").len() < 400);
        assert!(held.to_string().len() < 400);
    }

    /// One judgement the caller checked and kept.
    fn judged(decision_id: i64, judgement: Judgement) -> Held {
        Held {
            checked: true,
            judged: crate::map_store::Judged {
                decision_id,
                judgement,
                reason: "Nada estranho à vista.".to_owned(),
                model: "cloud".to_owned(),
                computed_at: "2026-08-26T10:00:00+00:00".to_owned(),
                // Whatever this says, `reconcile` may not look at it: staleness is decided by
                // whoever holds a reading of the repository, and a tally that second-guessed that
                // would be a second answer to *is this still about the same thing*.
                inputs_digest: "sha256:whatever".to_owned(),
            },
        }
    }

    /// The same judgement, held over a reading that could not check it — git would not say what the
    /// anchor code is.
    fn unchecked(decision_id: i64, judgement: Judgement) -> Held {
        Held {
            checked: false,
            ..judged(decision_id, judgement)
        }
    }

    /// Every standing a decision can be in, so nothing below is asserted over the easy one.
    fn every_standing() -> Vec<crate::map_stamp::Standing> {
        use crate::map_stamp::{Lapse, Standing, Watch};
        let when = "2026-08-20T09:00:00+00:00".to_owned();
        vec![
            Standing::Never,
            Standing::Settled {
                stamped_at: when.clone(),
                watch: Watch::Guessed,
            },
            Standing::Settled {
                stamped_at: when.clone(),
                watch: Watch::NoAnchor,
            },
            Standing::Partial {
                stamped_at: when.clone(),
                note: "falta migrar as páginas de pilar".to_owned(),
            },
            Standing::Lapsed {
                stamped_at: when.clone(),
                why: Lapse::Moved {
                    changed: vec!["core/src/map_triage.rs".to_owned()],
                    added: Vec::new(),
                    gone: Vec::new(),
                },
            },
            Standing::Lapsed {
                stamped_at: when.clone(),
                why: Lapse::Unreadable,
            },
            Standing::Withdrawn {
                stamped_at: when,
                note: Some("mudei de ideias".to_owned()),
            },
        ]
    }

    /// The property §5.3's two remaining numbers rest on, over a generated mix rather than a clean
    /// one.
    ///
    /// Every standing crossed with every judgement state a decision can be handed over in —
    /// flagged, silenced, unchecked-and-flagged, and never triaged — which puts a `Flagged` row
    /// against a `Settled` decision and a `Silenced` one against a `Withdrawn` decision in the same
    /// population, because those are exactly the rows a naive tally double-counts. Nothing deletes a
    /// judgement when a decision is later stamped and nothing should (§6.2), so this population is
    /// not contrived: it is what any project looks like a week after its first triage run.
    ///
    /// If any of the three piles ever overlapped, or any `Never` decision fell through all three,
    /// the sum would stop matching `StampCounts::never` and this fails. That is `map_stamp`'s own
    /// discipline for the other half of the header, copied deliberately — a header is exactly where
    /// a reader stops checking.
    #[test]
    fn every_never_decision_lands_in_exactly_one_of_the_three_piles() {
        use crate::map_stamp::Standing;

        let states = [
            Some(judged as fn(i64, Judgement) -> Held),
            Some(unchecked as fn(i64, Judgement) -> Held),
            None,
        ];
        let verdicts = [Judgement::Flagged, Judgement::Silenced];
        let mut standings = std::collections::BTreeMap::new();
        let mut current = Vec::new();
        let mut id = 0i64;
        for standing in every_standing() {
            for state in states {
                for verdict in verdicts {
                    id += 1;
                    standings.insert(id, standing.clone());
                    if let Some(build) = state {
                        current.push(build(id, verdict));
                    }
                }
            }
        }
        let stamps = crate::map_stamp::counts(&standings.values().cloned().collect::<Vec<_>>());

        let reconciled = reconcile(&standings, &stamps, current);
        let counts = &reconciled.counts;

        // **The reconciliation, and it is the reason this type exists.** The three are one pass over
        // the standings, so this line cannot fail without the loop itself being wrong.
        assert_eq!(
            counts.flagged + counts.silenced + counts.untriaged,
            stamps.never,
            "every never-stamped decision is flagged, silenced or untriaged, exactly once: \
             {counts:?}"
        );
        assert!(
            counts.flagged + counts.silenced <= stamps.never,
            "a judgement counted here is a judgement about a decision nobody stamped: {counts:?}"
        );
        assert_eq!(
            counts.waiting,
            stamps.lapsed + counts.flagged,
            "§5.3's `J` is the lapsed stamps plus what the triager assinalou, and nothing else: \
             {counts:?}"
        );
        // **`K` keeps the silenced, and this is the assertion that says so.** A silence is *ninguém
        // olhou* (§5.1), so it is still debt the owner has given no verdict on; subtracting it
        // would let a triager drive `K` to zero over a backlog nobody read. The only pile that
        // leaves `K` is the flagged, and it leaves because it has arrived in `J`.
        assert_eq!(
            counts.unseen,
            counts.silenced + counts.untriaged,
            "`K` is exactly its two parts, and a derived field is only safe with this line: \
             {counts:?}"
        );
        assert_eq!(
            counts.unseen,
            stamps.never - counts.flagged,
            "§5.3's `K` is `never − flagged`, and a silence is never subtracted from it: {counts:?}"
        );
        // A judgement nobody could re-check still counts in whichever pile it was in — dropping it
        // is what reported a whole project as *nunca vista* every time git hiccuped — and it is
        // countable apart so nothing presents it as verified.
        assert!(counts.unchecked > 0, "{counts:?}");
        assert!(
            counts.unchecked <= counts.flagged + counts.silenced,
            "unchecked is a subset of the two piles and never a fourth one: {counts:?}"
        );

        // **The shipped map and the header are the same pass**, which is what makes it impossible
        // for the rows on screen to disagree with the numbers above them.
        assert_eq!(
            reconciled.judgements.len(),
            counts.flagged + counts.silenced,
            "the map the panel draws holds exactly the judgements these numbers counted"
        );
        for decision_id in reconciled.judgements.keys() {
            assert_eq!(
                standings.get(decision_id),
                Some(&Standing::Never),
                "decision {decision_id} is drawn with a triage verdict and is not in scope for one"
            );
        }

        // A property nothing exercises is a property nobody proved: the mix has to actually contain
        // the rows this test is about — a judgement on a stamped decision, and one of each verdict
        // on a decision still in scope.
        assert!(
            counts.flagged > 0 && counts.silenced > 0 && counts.untriaged > 0,
            "{counts:?}"
        );
        assert!(
            stamps.settled + stamps.partial + stamps.lapsed + stamps.withdrawn > 0,
            "the population has to contain decisions that moved on, or the filter is untested"
        );
    }

    #[test]
    fn a_silence_does_not_pay_off_a_debt_nobody_looked_at() {
        // **§5.1 in one number.** *Silenciado* is *"sem sinal de problema. **Ninguém olhou.** Não é
        // verde."* — so a silenced decision is still one the owner has never given a verdict on,
        // and §5.3 says `K` *"é dívida, e é suposto incomodar"*. Subtracting the silenced would let
        // a triager that silences everything drive `K` to zero over a backlog nobody has read: §1's
        // false confidence, manufactured by the cure's own arithmetic, on the one line that exists
        // to be uncomfortable.
        //
        // What a silence buys is NOT being in `J`. That is the triager's whole job — deciding what
        // deserves the owner's eyes, never deciding what has already been seen — and the two
        // assertions below are those two halves.
        use crate::map_stamp::Standing;
        let standings =
            std::collections::BTreeMap::from([(1, Standing::Never), (2, Standing::Never)]);
        let stamps = crate::map_stamp::counts(&standings.values().cloned().collect::<Vec<_>>());

        let all_silent = reconcile(
            &standings,
            &stamps,
            vec![
                judged(1, Judgement::Silenced),
                judged(2, Judgement::Silenced),
            ],
        );

        assert_eq!(
            all_silent.counts.unseen, 2,
            "a silenced decision is one nobody looked at, so it stays in `K`: {:?}",
            all_silent.counts
        );
        assert_eq!(
            all_silent.counts.unseen, stamps.never,
            "silencing the whole backlog moved `K` by nothing at all"
        );
        assert_eq!(
            all_silent.counts.waiting, 0,
            "and it bought the one thing it is supposed to buy: nothing is in `J`"
        );

        // The contrast, so the test cannot pass by `unseen` simply never moving: a FLAG does leave
        // `K`, because it has arrived in `J` and counting it in both would break the sum.
        let one_flagged = reconcile(
            &standings,
            &stamps,
            vec![
                judged(1, Judgement::Flagged),
                judged(2, Judgement::Silenced),
            ],
        );

        assert_eq!(one_flagged.counts.unseen, 1, "{:?}", one_flagged.counts);
        assert_eq!(one_flagged.counts.waiting, 1, "{:?}", one_flagged.counts);
    }

    #[test]
    fn a_judgement_nobody_could_re_check_is_kept_and_counted_and_marked() {
        // **The third value of the staleness test.** `Anchors::Failed` means the digest could not be
        // COMPUTED, which is neither *this answer still stands* nor *this answer expired* — and it
        // is the state every judgement in a project is in for the length of a git outage, because
        // all of them were written against a `Computed(...)` reading. Dropping them empties the map
        // and reports the whole backlog as *nunca vista*: a fact about this daemon presented as a
        // fact about the project, which is the collapse `map_stamp::Lapse::Unreadable` exists one
        // table over to prevent.
        //
        // In one line: a flag we could not re-verify is still a flag; a flag we discarded becomes a
        // claim that nobody ever looked.
        use crate::map_stamp::Standing;
        let standings =
            std::collections::BTreeMap::from([(1, Standing::Never), (2, Standing::Never)]);
        let stamps = crate::map_stamp::counts(&standings.values().cloned().collect::<Vec<_>>());

        let reconciled = reconcile(
            &standings,
            &stamps,
            vec![
                unchecked(1, Judgement::Flagged),
                unchecked(2, Judgement::Silenced),
            ],
        );

        // Counted where they already were, so the header does not move because git hiccuped.
        assert_eq!(reconciled.counts.flagged, 1, "{:?}", reconciled.counts);
        assert_eq!(reconciled.counts.silenced, 1, "{:?}", reconciled.counts);
        assert_eq!(
            reconciled.counts.untriaged, 0,
            "an unverifiable answer is not the same fact as no answer: {:?}",
            reconciled.counts
        );
        assert_eq!(reconciled.counts.waiting, 1, "{:?}", reconciled.counts);

        // And marked, on the row and in a number, so nothing presents them as verified against the
        // code as it stands.
        assert_eq!(reconciled.counts.unchecked, 2, "{:?}", reconciled.counts);
        assert!(
            reconciled.judgements.values().all(|held| !held.checked),
            "the panel has to be able to say WHICH rows could not be re-checked, not merely how \
             many"
        );
    }

    #[test]
    fn a_judgement_about_a_decision_somebody_stamped_is_dropped_and_not_merely_uncounted() {
        // The single-case reading of the property above, because the shape of the failure is worth
        // naming: the map the panel DRAWS has to lose the row too. A tally that dropped the number
        // and shipped the judgement would put *silenciado* on a decision the owner has already
        // settled — the two axes §5 refuses to flatten, flattened by a payload rather than by a
        // colour, which is the door §6.1 does not think to watch.
        use crate::map_stamp::{Standing, Watch};
        let standings = std::collections::BTreeMap::from([
            (
                1,
                Standing::Settled {
                    stamped_at: "2026-08-25T09:00:00+00:00".to_owned(),
                    watch: Watch::Guessed,
                },
            ),
            (2, Standing::Never),
        ]);
        let stamps = crate::map_stamp::counts(&standings.values().cloned().collect::<Vec<_>>());

        let reconciled = reconcile(
            &standings,
            &stamps,
            vec![
                judged(1, Judgement::Flagged),
                judged(2, Judgement::Silenced),
            ],
        );

        assert_eq!(reconciled.counts.flagged, 0, "{:?}", reconciled.counts);
        assert_eq!(reconciled.counts.silenced, 1, "{:?}", reconciled.counts);
        assert_eq!(reconciled.counts.unseen, 1, "{:?}", reconciled.counts);
        assert_eq!(
            reconciled.judgements.keys().copied().collect::<Vec<_>>(),
            vec![2],
            "the stamped decision's judgement is not on the wire either"
        );
        // And `J` does not grow by a flag about a decision that already has the owner's answer.
        assert_eq!(reconciled.counts.waiting, stamps.lapsed);
    }
}
