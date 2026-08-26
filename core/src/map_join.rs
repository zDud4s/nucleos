//! The junction between what a spec decided and what the code implements, both halves of it.
//!
//! The lexical half reads `§` references out of a source file. The joining half — [`join`] —
//! checks them against the approved decisions and says, one decision at a time, how firmly that
//! decision is tied to code.
//!
//! **Sound where it matters, visibly approximate where it does not, and the payload says which is
//! which.** That asymmetry is the design and not a concession inside it. Not one citation in this
//! repository names its document today (§8), so every positive join is a guess and comes back
//! [`Anchor::Ambiguous`] rather than as a match. The negatives are untouched by that: a section no
//! file names anywhere is claimed under no document, whichever document each `§` meant — so
//! [`Anchor::Silent`] is sound while every positive is a guess. Decision 3 of the spec says the
//! value is in the nodes that do not match, which makes the answers this module is certain about
//! exactly the ones it exists to give. A map that is confidently wrong is worse than no map: it is
//! the disease this feature treats, with better pixels.
//!
//! **It names sections. It cannot name documents, and does not pretend to.** A `§7` in a Rust
//! file is a number and nothing else; which of this repository's forty specs it points at is
//! written down nowhere in the file. §8 mapa-do-projeto is the fix — a citation that carries a
//! short document slug, the way `§6.4 workspace-de-projeto` does — and until the code is edited
//! to carry one, the anchor is missing and no amount of reading recovers it.
//!
//! **[`Citation::named`] is a candidate, never a verdict.** The word after a section number has
//! the same shape whether it is a slug or an English word, and nothing lexical tells them apart:
//! `§4.4 rule` yields `Some("rule")` exactly the way `§6.4 workspace-de-projeto` yields the slug.
//! Today this repository is entirely the former. Run over every `.rs`, `.ts`, `.tsx` and `.go`
//! file outside this module, `citations` finds 729 citations in 174 files across 83 sections, and
//! 308 of them carry a candidate — every one an English word. The only hyphenated candidate in
//! shipping code is `§8.4 approval-pause` in `runs.rs`, and `approval-pause` is a phrase, not a
//! document; the others the scan reports sit inside test fixtures in `project_map.rs` that quote
//! the form §8 prescribes. **Zero real slug citations exist here** — §8 is unfixed until the
//! edit that puts a slug on all of them lands. Checking a candidate against the project's actual
//! spec slugs is [`names_document`]'s job, below, and refusing to guess at this layer is what
//! keeps that rejection worth anything.
//!
//! **Approximate where the error is visible, silent nowhere.** A `§` inside a string literal
//! counts, and the scan above proves the cost is exactly that: a fixture quoting a citation is
//! reported as one. That error is legible in the answer. The one place the approximation is
//! *not* self-announcing is the ordering, so [`Citation`] states it outright rather than leaving
//! it to be found. What this module refuses is the silent error — a citation confidently tied to
//! the wrong document — which is why nothing here decides anything.

// This is a bin-only crate, so dead-code reachability starts at `main`, and nothing reaches here
// yet — which is now true one step further out than it was. `join` gives `section_number` and the
// `Citation` fields a caller inside this module, and `join` itself has none: the HTTP route that
// serves the map is a later task, and until it lands everything here is reachable only from the
// tests. The instruction, not a description: DELETE THIS LINE with the change that gives `join` a
// production caller.
//
// Still one line rather than an attribute per item. When this module held five items, three
// scattered attributes would have covered them — measured, not assumed: an `#[allow]` seeds its
// item as a liveness root, so covering `citations` and `section_number` also covered
// `leading_number` and `candidate`, which nothing but those two call, and only `Citation` needed
// its own because its fields are written and never read outside the tests. The join added four
// more public types of exactly that last shape, which argues for the module-wide form rather than
// against it: a count of attributes that has to be recomputed every time the module grows is one
// somebody eventually gets wrong, and getting it wrong here is silent.
//
// Scoped to the non-test build so it silences only the absence of that caller. Under `cfg(test)`
// the lint stays live — every item below is exercised by this module's tests, and one that stops
// being exercised has to say so. `errands.rs:28` is the precedent for the module-wide form;
// `contacts.rs` scopes to `not(test)` the same way but hangs its attributes on individual fields,
// which is right there and wrong here, where the whole module is waiting on one caller.
#![cfg_attr(not(test), allow(dead_code))]

use crate::map_intent::Kind;
use crate::map_store::Decision;
use crate::project_map::{Foreign, Module};
use serde::Serialize;
use std::collections::BTreeSet;

/// One `§` reference found in a source file.
///
/// Ordering is lexical rather than numeric, because the derived `Ord` compares `section` as the
/// string it is. `§10` before `§2` is the visible half. **The half that hides is the
/// interleaving**: real sections from this repository sort
/// `6.10, 6.19, 6.2, 6.20, 6.4` — `§6.2` lands *between* `§6.19` and `§6.20` rather than at the
/// head of a misplaced block, and a reader scanning the list has no cue that anything is wrong.
/// Both shapes occur here (`§6.10`, `§6.13`–`§6.16`, `§6.19`, `§6.20`, `§10`–`§14` all exist), so
/// this is stated rather than left to be discovered: everything else in this module is
/// approximate where the error is *visible*, and this one is not.
///
/// It stays lexical anyway. Determinism is what the `BTreeSet` is for, and a numeric comparison
/// would first have to answer what `§5.3a` is worth as a number. A list a caller can re-sort for
/// display beats an order that quietly disagrees with the document; the test below pins it so the
/// determinism is asserted rather than assumed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Citation {
    /// The section label, normalized: `7`, `6.4`, `5.3a`. Never the `§`, and never the
    /// punctuation that happened to follow it.
    pub section: String,
    /// The word that followed it, when there was one.
    ///
    /// A **candidate** for a document slug and not a document — see the module doc. The join
    /// checks it against the project's real spec slugs and drops it when it is not one. Nothing
    /// at this layer can make that check, so nothing at this layer makes the claim.
    pub named: Option<String>,
}

/// Every `§` reference in a source file, deduplicated and ordered.
///
/// **Deliberately not a parser**, the same way [`crate::project_map::rust_imports`] is not: a
/// `§` inside a string literal, a comment, or a doc-comment example counts. The error that
/// produces is one extra row, in a file that was already naming that section out loud — visible
/// to whoever opens it, and cheaper than the parser that would avoid it.
///
/// Dedup is by the whole citation and not by section alone, so a file writing `§7` in one place
/// and `§7 rule` in another yields two rows. Collapsing them would mean picking which tail
/// survives and throwing a candidate away; two rows the join resolves separately throw nothing
/// away.
pub fn citations(source: &str) -> BTreeSet<Citation> {
    let mut found = BTreeSet::new();
    for (index, _) in source.match_indices('§') {
        let rest = &source[index + '§'.len_utf8()..];
        // `§§7` needs no special case: the first sign is followed by a sign, reads no number,
        // and is skipped, while the second reads `7`. One citation, without a rule for it.
        let Some((section, taken)) = leading_number(rest) else {
            continue;
        };
        found.insert(Citation {
            section,
            named: candidate(&rest[taken..]),
        });
    }
    found
}

/// The section number a piece of text begins with, and how many bytes it took.
///
/// Digits, then any number of `.digits` groups, then a single lowercase letter — one that a
/// digit precedes and no second letter follows, which is what keeps `§4.a` from reading as `4.a`
/// and `§7ab` from reading as `7a`.
///
/// **A `.` only continues the number when a digit follows it**, and that single rule is what
/// separates the number from the sentence it sits in. `§4.` ends the sentence, `§5.2).` closes
/// a parenthesis, and both are far more common here than a deeper subsection would be. Reading
/// the dot greedily would invent sections `4.` and `5.2)` that no document has.
fn leading_number(text: &str) -> Option<(String, usize)> {
    let mut number = String::new();
    let mut end = 0;
    let mut after_digit = false;

    for (at, character) in text.char_indices() {
        if character.is_ascii_digit() {
            number.push(character);
            end = at + 1;
            after_digit = true;
            continue;
        }
        if character == '.'
            && after_digit
            && text[at + 1..].starts_with(|next: char| next.is_ascii_digit())
        {
            number.push(character);
            end = at + 1;
            after_digit = false;
            continue;
        }
        // `§5.3a`, `§6c`, `§4.4a` — and the letter is taken only when it is the LAST one. Two
        // letters mean this was never a suffix, so `§7ab` is section `7` with `ab` as prose, not
        // section `7a` with the `b` quietly dropped. Everywhere else here an unexpected shape
        // ends the number and the tail becomes prose; truncating would be the one place this
        // module answers wrongly instead of answering less, which is the asymmetry it exists to
        // refuse.
        if character.is_ascii_lowercase()
            && after_digit
            && !text[at + 1..]
                .chars()
                .next()
                .is_some_and(|following| following.is_ascii_lowercase())
        {
            number.push(character);
            end = at + 1;
        }
        break;
    }

    if number.is_empty() {
        None
    } else {
        Some((number, end))
    }
}

/// The document slug a citation might be carrying, from whatever followed its number.
///
/// One space, then a run of `[a-z0-9-]`. A second space, an uppercase letter, or an apostrophe
/// all leave the run empty, and an empty run is not a candidate — one rule rather than a rule per
/// punctuation mark, each of which would be a place for prose to be mistaken for a slug.
///
/// The run stops where the shape stops, so `§7 rule.` offers `rule` and keeps the full stop out
/// of it. Whether `rule` is a document is a question for the join, which has the list.
///
/// **Three shapes are refused because no slug has them**, and refusing them is not the
/// "sounds like a slug" vocabulary guess this module declines to make. A hyphen at either end
/// (`§7 - a regra` gives `-`, a dash used as punctuation; `§7 -rule`; `§7 rule-`) and an
/// all-digit run (`§7 2 vezes`) are ruled out by *shape*, the way an uppercase initial already
/// is — no dictionary is consulted and no plausibility is judged. `rule` still comes back a
/// candidate, because only the join can know it is not a document.
fn candidate(rest: &str) -> Option<String> {
    let after = rest.strip_prefix(' ')?;
    let word: String = after
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if word.is_empty()
        || word.starts_with('-')
        || word.ends_with('-')
        || word.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(word)
}

/// The section number a spec heading carries, or `None` when it carries none.
///
/// The input is a heading copied verbatim out of a document by a model, so it arrives wearing
/// its `#` markers, with or without a `§`, and in whatever language the spec was written in:
/// `## 4.1 Três tipos…`, `### 5.1 Estado derivado`, `4.1 Três tipos…`, `## 0. Decisões fixadas`.
///
/// What follows the number has to be whitespace, `.`, `)`, `:` or the end of the string, which
/// is what keeps `## 2026-08-24 algo` from being section 2026. A heading that names no section
/// answers `None`, and that is a real answer this layer reports rather than a failure: an
/// approved decision that cannot be placed under a number still exists — it just anchors
/// nothing, and saying so is the honest half of a junction that admits what it does not know.
pub fn section_number(heading: &str) -> Option<String> {
    // **The marker run is taken once and never returned to.** Stripping `#` and whitespace
    // together — one `trim_start_matches` over both — eats the `#` of an item label as well, and
    // `### #1 — A alçada vive numa tabela` comes back as section 1. That collides with the real
    // `## 1. Contexto e problema` in the same document, and there are 39 such headings across
    // five specs here. They sit under `## 2. Decisões fechadas`, which makes them precisely the
    // decisions the intent layer harvests and copies verbatim — this function's likeliest input,
    // not a fringe one. A `#` that survives the run is left where it is, for `leading_number` to
    // refuse.
    let text = heading.trim_start().trim_start_matches('#').trim_start();
    let text = text.strip_prefix('§').unwrap_or(text);
    let (number, taken) = leading_number(text)?;
    match text[taken..].chars().next() {
        None => Some(number),
        Some(c) if c.is_whitespace() || c == '.' || c == ')' || c == ':' => Some(number),
        Some(_) => None,
    }
}

/// How firmly one approved decision is tied to code.
///
/// **Four states, because collapsing any two would make the map claim something nobody
/// measured.** *Nothing names this*, *something names it and cannot say which document it meant*,
/// and *there was no number to look for* are three different facts, and the fourth — a citation
/// that does say which document — is the only one that earns the word confirmed. This feature
/// exists to cure a false sense of confidence; a state that averaged a certainty with a guess
/// would manufacture one, which is the disease with better pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// A **readable module** names this section and names a document this project has — the shape
    /// §8 prescribes, `§6.4 workspace-de-projeto`. Certain, and the only state that may be
    /// presented as confirmed.
    ///
    /// **Readable is part of the definition and not an accident of what this map happens to
    /// parse.** A Go file carrying a slug would be exactly as certain about *which* section it
    /// names, and still does not belong here, because of what this state is for downstream: a
    /// `Declared` decision is the one that later carries a stamp and loses it when the anchor code
    /// changes (§7). Watching an anchor means knowing what the anchor is, and a file nobody here
    /// can read has no anchor to watch — so certifying one would be promising an expiry that the
    /// stamp slice cannot deliver, and an *está como quero* that silently never expires is the
    /// worst row this map could produce. The conservative rule is therefore the correct one and
    /// not merely the safe one. Such a file lands in [`Anchor::Ambiguous`] instead.
    ///
    /// **Zero of these exist in this repository today.** That is §8 unfixed and not a bug in this
    /// code: 729 citations across 174 files, 308 carrying a candidate, every one an English word,
    /// and slice 6 is the edit that puts a slug on them. The variant ships anyway, because a map
    /// that could not express certainty even once it is earned would have to be rewritten by the
    /// slice that earns it, and the count being zero is itself the measurement that says the edit
    /// has not landed.
    Declared,
    /// Something names this section, and this map cannot confirm it means this decision. **Shown,
    /// never counted as confirmed.**
    ///
    /// **This variant carries two different uncertainties, and the doc says so because the code
    /// cannot.** One is *which document is this?* — `§7` appears in 21 files here and `§6.4` in
    /// 10, and not one of them says of what, so any of them may be another spec's §7. The other is
    /// *this is code I cannot read* — a Go sidecar names the section, and nothing here knows what
    /// Go does with it.
    ///
    /// **The anchor does not tell them apart; [`Anchored::modules`] and [`Anchored::foreign`]
    /// do.** A consumer that wants the distinction has to look at the two lists, and one that
    /// renders this state without looking is rendering the weaker of the two claims for both. That
    /// is the cost of the cut, stated rather than hidden.
    ///
    /// **A fifth variant was considered and refused, and the reason is that the two are not
    /// disjoint.** A Go file naming `§6.4` with no slug is *both* at once — unreadable *and*
    /// unattributed — so a fifth state would have had to invent a precedence between them, and
    /// nothing in the spec says which of the two a reader should be told about first. Inventing
    /// that ordering is the kind of judgement §6 reserves for the owner and this layer has no
    /// standing to make. Two honest lists beat a fifth word that quietly picks a winner.
    Ambiguous,
    /// **Nothing anywhere** names this section — no module, no Go file, no migration, nothing in
    /// any language scanned. §5.1's *declared, with no code*.
    ///
    /// **Sound despite §8, and the only state here that is.** Every positive join this repository
    /// can make today is a guess about which document a bare `§` meant; this one is untouched by
    /// that, because if nothing names §4.1 at all then nothing claims it under *any* document and
    /// there is no ambiguity left to resolve. Decision 3 of the spec says the value is in the
    /// nodes that do not match — so the answers this map is certain about are exactly the ones it
    /// exists to give.
    Silent,
    /// The heading the decision was copied from carries no number, so nothing could be looked
    /// for. **Not a claim about the code.**
    ///
    /// Collapsing this into [`Anchor::Silent`] is the tempting move and the wrong one: *nothing
    /// claims this* is the report of a search, and here no search ran. A decision extracted from
    /// `## Contrato` is approved and real; it simply anchors nothing. Saying so in its own word is
    /// what keeps the *declared, with no code* pile — the pile §5.1 exists to make somebody look
    /// at — from filling up with rows nobody ever went looking for.
    Unnumbered,
}

/// One approved decision, with everything the structure layer can say about it and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Anchored {
    pub decision_id: i64,
    /// The line's number within its spec's extraction, carried through untouched.
    ///
    /// The owner approved that spec as a **numbered list**, and *line 3 of that document* is how
    /// they will refer to a decision afterwards — so the number they read belongs in the payload
    /// and not only in the sort. Without it a client can only reproduce this order, never a
    /// different one, which makes the ordering caveat on [`join`] a promise it cannot keep: §10
    /// wants a different order eventually, and a payload that cannot be re-sorted would have to be
    /// widened by the slice that changes it.
    pub ordinal: i64,
    pub spec_slug: String,
    /// The heading as the model copied it — `## 4.1 Três tipos de decisão` — and not the number
    /// [`section_number`] read off it. The owner approved this string, so this is the string that
    /// goes back to them; the number is an internal step and shows up only as [`Anchored::anchor`].
    pub section: String,
    pub text: String,
    pub kind: Kind,
    pub anchor: Anchor,
    /// Readable modules naming this section, sorted by path. Empty for [`Anchor::Silent`] and
    /// [`Anchor::Unnumbered`] — in the first case because nothing was found, in the second because
    /// nothing was sought.
    pub modules: Vec<String>,
    /// Files naming it in a language this map cannot read — the Go sidecars, mostly.
    ///
    /// **Separate from [`Anchored::modules`] and never merged into it.** *We know something is
    /// there* and *we can see what it is* are different facts, and one list would let the second
    /// borrow the first's confidence: a caller counting `modules` would be told a Go file has
    /// imports, tests and a reader, none of which this map ever established. The separation is
    /// also what keeps `Silent` honest — 77 Go files here name a `§`, and reading *declared, with
    /// no code* off the module list alone would report a whole language as absent.
    pub foreign: Vec<String>,
}

/// The junction: the intention layer read against the structure layer.
///
/// Two of the three lists are §5.1's rows and the third is the day-one reality. `decisions` holds
/// *declared, with no code* under [`Anchor::Silent`]; `unclaimed` is *code nobody asked for*; and
/// `unmatched` is neither, which on day one is nearly every module because approval has barely
/// started (§10). Folding `unmatched` into `unclaimed` would inflate the one number §5.1 exists to
/// put in front of somebody.
///
/// **Only modules are placed in a pile.** A Go file that names a section nobody approved has
/// nowhere to fall, and that is not an oversight: *code nobody asked for* is a claim about a file
/// this map has read, and it has not read that one. An extra pile of foreign paths would be a
/// count of files whose only measured property is that they contain a `§`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Junction {
    /// Ordered by `spec_slug`, then `ordinal`, then id — see [`join`] for why that order is
    /// deterministic rather than final.
    pub decisions: Vec<Anchored>,
    /// Modules naming no section at all. §5.1's *code nobody asked for*.
    pub unclaimed: Vec<String>,
    /// Modules naming a section no approved decision names. Neither orphan nor matched.
    pub unmatched: Vec<String>,
    pub counts: Counts,
}

/// The numbers behind §5.3's header.
///
/// **They are required to add up, and a test asserts it rather than a comment promising it.**
/// `declared + ambiguous + silent + unnumbered == decisions`, and every module counted here is in
/// exactly one of `unclaimed`, `unmatched`, or some [`Anchored::modules`]. A total that does not
/// reconcile is the false confidence this whole feature exists to cure, reproduced inside the
/// cure — and it would be invisible, because a header is exactly where a reader stops checking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub decisions: usize,
    pub declared: usize,
    pub ambiguous: usize,
    pub silent: usize,
    pub unnumbered: usize,
    pub unclaimed: usize,
    pub unmatched: usize,
}

/// Whether a citation's candidate names this document.
///
/// **The one piece of judgement in this module, and it is deliberately not a vocabulary guess.**
/// Two conditions, both required:
///
/// 1. The candidate has **at least two hyphen-joined segments**. A one-word candidate is an
///    English word until proven otherwise — `§4.4 rule` occurs twelve times here — and the rule
///    also has to survive Portuguese, where it is doing real work rather than being cautious: of
///    the 40 spec slugs in this repository **34 carry a `design` segment and 14 carry a `de`**, so
///    without this condition a single `§7 de` would declare against a third of the intention layer
///    at once, and `§7 design` against nearly all of it.
/// 2. Its segments appear as a **contiguous run** inside the slug's segments. `workspace-projeto`
///    names two segments the slug really has, in order, with one missing between them, and a rule
///    that accepted that would accept any two words a document happens to contain.
///
/// This is why [`citations`] hands the join *every* candidate rather than pre-guessing which look
/// slug-shaped. A parser tightened to "sounds like a document" would move the guess to the layer
/// that has no list of documents to check it against, and the rejection here would stop meaning
/// anything — it would only ever be re-confirming a decision already taken upstream, in the dark.
/// A heuristic in this function would be exactly the silent wrongness the module refuses.
///
/// The residual is stated rather than left to be found: a run shorter than the whole slug matches,
/// so a two-segment candidate that happens to sit inside a slug reads as that document. Demanding
/// the whole slug would refuse `mapa-do-projeto`, which is the abbreviation §8 itself writes, and
/// the error a short run can make is bounded — it can only ever name a document this project
/// really has.
///
/// The other residual is the price of condition 1, and it is not hypothetical: a document is named
/// by its filename ([`crate::map_intent::spec_slug`]), so a project keeping its specs as `beta.md`
/// has a one-segment slug that no citation can ever match, and every decision of that document is
/// capped at [`Anchor::Ambiguous`] no matter how carefully somebody cites it. That is accepted
/// rather than fixed. Letting a single word declare is what would let `rule` declare, twelve times
/// over, against whichever spec happened to contain that segment — a wrong answer far more often
/// than this is an under-reported one, and an under-report is the direction this module errs in on
/// purpose.
fn names_document(candidate: &str, spec_slug: &str) -> bool {
    let wanted: Vec<&str> = candidate.split('-').collect();
    if wanted.len() < 2 {
        return false;
    }
    let slug: Vec<&str> = spec_slug.split('-').collect();
    slug.windows(wanted.len())
        .any(|run| run == wanted.as_slice())
}

/// What one file's citations say about one decision.
///
/// Ordered so that the strongest wins when a file says several things at once, which is common:
/// `shell/src/pages/Fleet.tsx` names §9.2 twice, and the day slice 6 lands a module will routinely
/// carry the same section bare in one place and with its slug in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    /// This file says nothing about that section. Not the same as saying nothing at all.
    Nothing,
    /// It names the section without saying which document, or names it after a word that is no
    /// document of this project.
    Ambiguous,
    /// It names the section and names this decision's document.
    Declared,
}

/// What one file's citations say about one section of one document.
///
/// **Grouped by section, never counted by row.** [`citations`] deduplicates by the whole citation
/// and not by section — deliberately, because collapsing by section would mean choosing which
/// trailing candidate survives and the discarded one could be the real slug. So `cites.len()` is
/// not a section count: `shell/src/pages/Fleet.tsx` really does come back with two rows for §9.2,
/// from `§9.2 risk` and `§9.2 spike`. Iterating rows into a list instead of folding them into one
/// verdict lists that file twice under one decision.
///
/// **A candidate that names another of this project's documents is not weak evidence — it is
/// evidence against**, and the citation is skipped rather than downgraded. Counting `§6.4
/// mapa-do-projeto` towards a decision of the workspace spec would be the §8 ambiguity inverted:
/// not a guess about which document was meant, but a known mismatch read as a match. A candidate
/// that names *no* document — `rule`, `approval-pause` — is different and must not be treated the
/// same way: it leaves a citation that still names a bare section, which is ordinary ambiguous
/// evidence and not a disqualification.
fn evidence(cites: &[Citation], section: &str, spec_slug: &str, spec_slugs: &[String]) -> Evidence {
    let mut found = Evidence::Nothing;
    for cite in cites.iter().filter(|cite| cite.section == section) {
        let says = match &cite.named {
            Some(candidate) if names_document(candidate, spec_slug) => Evidence::Declared,
            Some(candidate)
                if spec_slugs
                    .iter()
                    .any(|slug| names_document(candidate, slug)) =>
            {
                continue;
            }
            _ => Evidence::Ambiguous,
        };
        found = found.max(says);
    }
    found
}

/// Read the approved decisions against the structure layer, and say what does not match.
///
/// **Sound where it matters, visibly approximate where it does not**, and the payload says which
/// is which. Today not one citation in this repository names its document (§8), so every positive
/// join is a guess and is reported as [`Anchor::Ambiguous`] rather than as a match. The negatives
/// are unaffected: a section no file names anywhere is claimed under no document, which is a
/// correct conclusion even while every positive is a guess — and decision 3 of the spec says the
/// negatives are the product.
///
/// **`decisions` are the approved ones, and this function does not check that.** The filter lives
/// in the query that reads the table, where the project id already lives, for the reason
/// [`crate::map_store::decide`] gives about checks that live in handlers. It is worth naming
/// because the check cannot simply be added here as a belt: `map_store::from_row` sets
/// `approved_at: None` on every row it builds, so a guard reading that field would empty the map
/// for every caller the store can serve today — a map that silently shows nothing, which is worse
/// than one that shows too much.
///
/// **`unclaimed` is computed from `cites`, never from `declares`**, and the two are near-synonyms
/// that a later reader will swap by accident. `declares` is `source.contains('§')` — the file's
/// own gesture, including a bare `§` with no number. `cites` is the parsed citations, and for
/// TypeScript it folds in the sibling test's, which is what makes the two languages answer the
/// same question rather than answering differently about where each keeps its tests. Four modules
/// today (`Fleet.tsx`, `Home.tsx`, `Workspace.tsx`, `priority.ts`) are `declares: false` with a
/// non-empty `cites`. §5.1's *code nobody asked for* means nothing claims this module, and a
/// module whose test names what it proves is claimed; reading the pile off `declares` would put
/// those four in it, which is a wrong answer wearing the right word.
///
/// **The ordering is deterministic and is not the one §10 asks for.** §10 wants recency of the
/// anchor code's last change — *what moved since I last looked?* — which needs git, and nothing in
/// this crate reads git yet. It arrives with the slice that adds `map_stamps` (§9.2), which
/// already has to know when the anchor code moved in order to expire a stamp (§7.1). Until then
/// the order is `spec_slug`, `ordinal`, id, and the id is there because ordinal alone is not a
/// total order: `map_decisions` is unique on `(project_id, spec_slug, ordinal, extracted_at)`, so
/// two extractions of one spec can both hold ordinal 1 and both be approved. Determinism here is
/// the absence of a wobble, not the intended answer, and a test pins it so the day §10's order
/// lands it is a visible change rather than a silent one.
pub fn join(
    decisions: &[Decision],
    modules: &[Module],
    foreign: &[Foreign],
    spec_slugs: &[String],
) -> Junction {
    let mut order: Vec<&Decision> = decisions.iter().collect();
    order.sort_by(|left, right| {
        left.spec_slug
            .cmp(&right.spec_slug)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut anchored: Vec<Anchored> = Vec::with_capacity(order.len());
    let mut matched: BTreeSet<&str> = BTreeSet::new();

    for decision in order {
        let (anchor, named_by, abroad) = match section_number(&decision.section) {
            None => (Anchor::Unnumbered, Vec::new(), Vec::new()),
            Some(section) => {
                let mut named_by: Vec<String> = Vec::new();
                let mut declared = false;
                for module in modules {
                    let says = evidence(&module.cites, &section, &decision.spec_slug, spec_slugs);
                    if says == Evidence::Nothing {
                        continue;
                    }
                    declared |= says == Evidence::Declared;
                    named_by.push(module.path.clone());
                    matched.insert(module.path.as_str());
                }
                // A foreign file's verdict is read for presence only, and `Evidence::Declared`
                // from one is deliberately not promoted — see [`Anchor::Ambiguous`]. The call is
                // still the same one, so a slug in a Go file is skipped or kept by exactly the
                // rule that governs a module, and the two cannot drift apart.
                let mut abroad: Vec<String> = foreign
                    .iter()
                    .filter(|file| {
                        evidence(&file.cites, &section, &decision.spec_slug, spec_slugs)
                            != Evidence::Nothing
                    })
                    .map(|file| file.path.clone())
                    .collect();
                named_by.sort();
                abroad.sort();

                let anchor = if declared {
                    Anchor::Declared
                } else if named_by.is_empty() && abroad.is_empty() {
                    Anchor::Silent
                } else {
                    Anchor::Ambiguous
                };
                (anchor, named_by, abroad)
            }
        };

        anchored.push(Anchored {
            decision_id: decision.id,
            ordinal: decision.ordinal,
            spec_slug: decision.spec_slug.clone(),
            section: decision.section.clone(),
            text: decision.text.clone(),
            kind: decision.kind,
            anchor,
            modules: named_by,
            foreign: abroad,
        });
    }

    let mut unclaimed: Vec<String> = Vec::new();
    let mut unmatched: Vec<String> = Vec::new();
    for module in modules {
        if module.cites.is_empty() {
            unclaimed.push(module.path.clone());
        } else if !matched.contains(module.path.as_str()) {
            unmatched.push(module.path.clone());
        }
    }
    unclaimed.sort();
    unmatched.sort();

    let counts = Counts {
        decisions: anchored.len(),
        declared: tally(&anchored, &Anchor::Declared),
        ambiguous: tally(&anchored, &Anchor::Ambiguous),
        silent: tally(&anchored, &Anchor::Silent),
        unnumbered: tally(&anchored, &Anchor::Unnumbered),
        unclaimed: unclaimed.len(),
        unmatched: unmatched.len(),
    };

    Junction {
        decisions: anchored,
        unclaimed,
        unmatched,
        counts,
    }
}

/// How many of these landed in one state.
///
/// Counted off the built list rather than incremented while building it. An accumulator would be a
/// second place the four states are enumerated, and the failure mode of the second place is a
/// header that disagrees with the rows under it — which is unfalsifiable by eye, since a header is
/// exactly where a reader stops checking.
fn tally(anchored: &[Anchored], anchor: &Anchor) -> usize {
    anchored.iter().filter(|one| &one.anchor == anchor).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_map::Reader;

    /// The single citation in `source`, or a failure naming what was found instead.
    fn only(source: &str) -> Citation {
        let found = citations(source);
        assert_eq!(
            found.len(),
            1,
            "expected one citation in {source:?}: {found:?}"
        );
        found.into_iter().next().expect("one citation")
    }

    /// Two real spec slugs of this repository, because the whole point of the join is that a
    /// candidate is checked against documents that actually exist.
    const SLUGS: [&str; 2] = [
        "2026-08-22-workspace-de-projeto-design",
        "2026-08-24-mapa-do-projeto-design",
    ];

    fn slugs() -> Vec<String> {
        SLUGS.iter().map(|slug| (*slug).to_owned()).collect()
    }

    /// An approved decision. `section` arrives exactly as a model copied it out of the document.
    fn decided(id: i64, spec_slug: &str, section: &str, ordinal: i64) -> Decision {
        Decision {
            id,
            spec_slug: spec_slug.to_owned(),
            section: section.to_owned(),
            ordinal,
            text: format!("decision {id}"),
            kind: Kind::Character,
            brain: "local".to_owned(),
            extracted_at: "2026-08-24T00:00:00Z".to_owned(),
            approved_at: Some("2026-08-24T01:00:00Z".to_owned()),
        }
    }

    fn cited(cites: &[(&str, Option<&str>)]) -> Vec<Citation> {
        cites
            .iter()
            .map(|(section, named)| Citation {
                section: (*section).to_owned(),
                named: named.map(str::to_owned),
            })
            .collect()
    }

    fn module_at(path: &str, cites: &[(&str, Option<&str>)]) -> Module {
        Module {
            path: path.to_owned(),
            reader: Reader::Rust,
            declares: !cites.is_empty(),
            cites: cited(cites),
            tested: false,
        }
    }

    fn foreign_at(path: &str, cites: &[(&str, Option<&str>)]) -> Foreign {
        Foreign {
            path: path.to_owned(),
            cites: cited(cites),
        }
    }

    /// The one decision of a junction built from one decision.
    fn single(junction: &Junction) -> &Anchored {
        assert_eq!(junction.decisions.len(), 1, "{:?}", junction.decisions);
        &junction.decisions[0]
    }

    #[test]
    fn a_bare_section_number_is_the_whole_citation() {
        let citation = only("//! §7 — and nothing else is claimed");
        assert_eq!(citation.section, "7");
    }

    #[test]
    fn a_subsection_keeps_the_dot_that_joins_its_digits() {
        assert_eq!(only("/// drawn by §6.4").section, "6.4");
    }

    #[test]
    fn a_closing_bracket_ends_the_number_and_is_not_part_of_it() {
        // `§5.2).` appears seven times in this repository, always closing a parenthesis the
        // sentence opened. The `)` and the `.` belong to the prose.
        let citation = only("// (the rule lives in §5.2).");
        assert_eq!(citation.section, "5.2");
        assert_eq!(citation.named, None);
    }

    #[test]
    fn a_dot_with_no_digit_after_it_is_the_sentence_and_not_the_number() {
        let citation = only("// this is what is asked by §4.");
        assert_eq!(citation.section, "4");
        assert_eq!(citation.named, None);
    }

    #[test]
    fn an_apostrophe_ends_the_number_and_names_nothing() {
        let citation = only("// §9's second paragraph");
        assert_eq!(citation.section, "9");
        assert_eq!(citation.named, None);
    }

    #[test]
    fn a_lowercase_letter_after_a_digit_belongs_to_the_section() {
        assert_eq!(only("// guarded by §5.3a").section, "5.3a");
    }

    #[test]
    fn a_letter_suffix_is_read_at_every_depth_of_the_number() {
        // `§6.0b`, `§4.4a` and `§6c` are all real shapes here. Dropping the letter would merge
        // `§6.0b` into `§6.0`, which is a different decision.
        assert_eq!(only("// §6.0b").section, "6.0b");
        assert_eq!(only("// §6c").section, "6c");
        assert_eq!(only("// §4.4a").section, "4.4a");
    }

    #[test]
    fn a_section_sign_with_no_digit_after_it_is_not_a_citation() {
        // A `§` is not a citation by itself, and a number is not one either when a space or a
        // letter stands between them. Reporting one anyway would put a row on the map that
        // points at nothing, which is exactly the silent wrongness this module refuses.
        assert!(citations("// the § symbol").is_empty());
        assert!(citations("// §x").is_empty());
        assert!(citations("// § 7").is_empty());
    }

    #[test]
    fn a_doubled_section_sign_still_yields_one_citation() {
        let citation = only("// see §§7 for both");
        assert_eq!(citation.section, "7");
    }

    #[test]
    fn two_mentions_of_the_same_section_in_one_file_are_one_citation() {
        let source = "//! §6.4 workspace-de-projeto — four kinds\n\
                      /// and §6.4 workspace-de-projeto is why this enum has four variants\n";
        let found = citations(source);
        assert_eq!(found.len(), 1);
        // Asserted, not merely counted: a length of 1 survives a candidate truncated at the
        // first hyphen, and `workspace-de-projeto` is a real spec slug in this repository — the
        // exact input the whole feature exists for.
        let citation = found.iter().next().expect("one citation");
        assert_eq!(citation.section, "6.4");
        assert_eq!(citation.named, Some("workspace-de-projeto".to_string()));

        // Identical is the whole citation and not the section alone. `§7` bare and `§7 rule`
        // are two rows on purpose: collapsing them means picking which tail survives, and the
        // one thrown away might have been the slug. The join resolves each row separately, so
        // keeping both costs a row and loses nothing.
        let mixed = "// §7, and later §7 rule";
        assert_eq!(citations(mixed).len(), 2);
    }

    #[test]
    fn a_citation_inside_a_string_literal_counts() {
        // **Deliberately not a parser.** A `§` inside a string literal is indistinguishable
        // from one in a doc-comment without reading Rust, and this module does not read Rust.
        // The error is one extra row on a map, in a file that was already naming that section
        // out loud — visible to whoever opens it. The error a real parser would avoid does not
        // justify pulling `syn` into this slice, exactly as `project_map::rust_imports` argues
        // next door.
        let source = r#"let message = "the approval pause is §8.4";"#;
        assert_eq!(only(source).section, "8.4");
    }

    #[test]
    fn the_candidate_is_only_taken_when_it_is_a_single_lowercase_word() {
        // One space, then a run of `[a-z0-9-]`, and nothing else. A slug is written that way
        // and prose is not, so everything else is left as `None` rather than guessed at.
        assert_eq!(only("// §7 The rule").named, None);
        assert_eq!(only("// §7  two spaces").named, None);
        assert_eq!(only("// §7's tail").named, None);
        assert_eq!(only("// §7 rule.").named, Some("rule".to_string()));
    }

    #[test]
    fn an_english_word_is_still_a_candidate_here_because_this_module_cannot_know() {
        // `§4.4 rule` occurs twelve times in this repository and `rule` is not a document. This
        // module answers `Some("rule")` anyway, and that is not a defect to be fixed here: the
        // join rejects the candidate when it matches no spec slug, and it can only do that
        // because this layer hands it every candidate rather than the ones it liked the look of.
        // Whoever tightens this parser to "sound like a slug" moves the guess to the layer that
        // has no document list to check it against.
        assert_eq!(only("/// §4.4 rule").named, Some("rule".to_string()));
    }

    #[test]
    fn a_slug_survives_whole_with_its_hyphens_and_its_digits() {
        // The shape `§8 mapa-do-projeto` prescribes is the one input this module must not
        // mangle. Truncating at the hyphen would hand the join `mapa` — a *wrong* candidate
        // rather than a missing one, and the join has no way to tell it was cut.
        assert_eq!(
            only("//! §8.4 approval-pause").named,
            Some("approval-pause".to_string())
        );
        assert_eq!(
            only("//! §7 v2-do-plano").named,
            Some("v2-do-plano".to_string())
        );
    }

    #[test]
    fn a_candidate_is_never_a_dash_and_never_a_bare_number() {
        // Refused by shape, not by vocabulary: no slug opens or closes with a hyphen, and none
        // is all digits. A lone `-` is a dash the sentence used as punctuation, and `2` in
        // `§7 2 vezes` is a count. Judging whether a *word* sounds like a document stays the
        // join's business — `rule` is still a candidate, and that is tested above.
        assert_eq!(only("// §7 - a regra").named, None);
        assert_eq!(only("// §7 -rule").named, None);
        assert_eq!(only("// §7 rule-").named, None);
        assert_eq!(only("// §7 2 vezes").named, None);
    }

    #[test]
    fn a_second_lowercase_letter_means_the_first_was_never_a_suffix() {
        // `§7ab` is section 7 with `ab` as prose. Reporting `7a` would invent a section no
        // document has — answering wrongly where every other unexpected shape here answers
        // less. No such citation exists in the tree today; the point is the asymmetry.
        let citation = only("// §7ab");
        assert_eq!(citation.section, "7");
        assert_eq!(citation.named, None);
        // One letter is still a suffix, and a letter followed by a space still ends the number.
        assert_eq!(only("// §7a").section, "7a");
        assert_eq!(only("// §7a rule").section, "7a");
    }

    #[test]
    fn an_item_label_is_not_a_section_number() {
        // `### #1 — …` is decision one of a list, not section one of the document. Reading the
        // `#` markers and the whitespace in a single pass eats the label's own `#` and answers
        // `Some("1")` — a confident, silent, wrong anchor on the input class this function most
        // often sees, since these headings sit under `## 2. Decisões fechadas` and are exactly
        // what the intent layer harvests. 39 headings across five specs have this shape.
        assert_eq!(
            section_number("### #1 — A alçada vive numa **tabela**, não num ficheiro"),
            None
        );
        assert_eq!(section_number("## #11 — O tecto"), None);

        // The collision is live in one document: `.ai/specs/2026-08-16-alcada-por-equipa-design.md`
        // carries both of these, eleven lines apart. They must not answer the same thing.
        let real = section_number("## 1. Contexto e problema");
        let item = section_number("### #1 — A alçada vive numa **tabela**, não num ficheiro");
        assert_eq!(real, Some("1".to_string()));
        assert_eq!(item, None);
        assert_ne!(real, item);
    }

    #[test]
    fn the_order_is_lexical_and_that_interleaves_the_deep_sections() {
        // Pins the determinism the `BTreeSet` exists for, and pins the wart with it: `§6.2`
        // sorts BETWEEN `§6.19` and `§6.20`, which is not a misplaced block a reader would
        // notice but an interleaving they would read straight past. Every section here is real.
        let source = "// §6.2 §6.19 §6.20 §10 §2";
        let order: Vec<String> = citations(source)
            .into_iter()
            .map(|citation| citation.section)
            .collect();
        assert_eq!(order, ["10", "2", "6.19", "6.2", "6.20"]);
    }

    #[test]
    fn a_spec_heading_gives_up_the_number_it_carries() {
        assert_eq!(
            section_number("## 4.1 Três tipos de decisão"),
            Some("4.1".to_string())
        );
        assert_eq!(
            section_number("### 5.1 Estado derivado"),
            Some("5.1".to_string())
        );
        assert_eq!(
            section_number("4.1 Três tipos de decisão"),
            Some("4.1".to_string())
        );
        assert_eq!(
            section_number("## 0. Decisões fixadas"),
            Some("0".to_string())
        );
        assert_eq!(section_number("## §8.1 A regra"), Some("8.1".to_string()));
        // Each of the four followers, including the one that is nothing at all. A heading that
        // is only its number is still a heading, and `)` and `:` are both in use here.
        assert_eq!(section_number("## 7"), Some("7".to_string()));
        assert_eq!(section_number("## 7) A regra"), Some("7".to_string()));
        assert_eq!(section_number("## 7: A regra"), Some("7".to_string()));
        assert_eq!(section_number("### 6.0a Estado"), Some("6.0a".to_string()));
    }

    #[test]
    fn a_heading_with_no_number_has_no_section_number() {
        // Not a failure and not a heading to skip: a decision extracted from prose that carries
        // no number is still approved and still real, it simply anchors nothing.
        assert_eq!(section_number("## Contrato"), None);
        assert_eq!(
            section_number("### A âncora, e a ambiguidade que tem de morrer"),
            None
        );
        // A date is not a section, and the follower rule is what says so.
        assert_eq!(section_number("## 2026-08-24 mapa do projeto"), None);
    }
    #[test]
    fn a_decision_no_file_cites_is_declared_without_code() {
        // The sound half. Every positive join in this repository is a guess today (§8: not one
        // citation names its document), and this answer is untouched by that: if nothing anywhere
        // names §4.1, then nothing claims it under ANY document, and the ambiguity that ruins the
        // positives has nothing left to be ambiguous about.
        let decisions = [decided(1, SLUGS[0], "## 4.1 Três tipos de decisão", 1)];
        let modules = [module_at("core/src/runs.rs", &[("7", None)])];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Silent);
        assert!(single(&junction).modules.is_empty());
        assert!(single(&junction).foreign.is_empty());
        assert_eq!(junction.counts.silent, 1);
    }

    #[test]
    fn a_decision_only_a_sidecar_names_is_not_declared_without_code() {
        // 77 Go files name a `§`. Reading *declared, with no code* off a module list that cannot
        // contain Go would report a whole language as absent — a confident wrong answer, which is
        // the one thing this module exists to refuse. The file is named, and named apart from the
        // modules: *something claims this* and *here is what claims it* are different facts, and
        // one list would let the second borrow the first's confidence.
        let decisions = [decided(1, SLUGS[0], "## 6.4 Vocabulário de nó", 4)];
        let foreign = [foreign_at("sidecars/telegram/main.go", &[("6.4", None)])];

        let junction = join(&decisions, &[], &foreign, &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Ambiguous);
        assert!(
            single(&junction).modules.is_empty(),
            "nothing readable claims it"
        );
        assert_eq!(single(&junction).foreign, ["sidecars/telegram/main.go"]);
        assert_eq!(junction.counts.silent, 0, "not a decision with no code");
    }

    #[test]
    fn a_decision_cited_without_its_document_is_ambiguous_and_not_confirmed() {
        // `§7` appears in 21 files here and none of them says of what. This is the shape of every
        // positive join the repository can make today, and it is shown rather than counted.
        let decisions = [decided(1, SLUGS[1], "## 7. O carimbo", 7)];
        let modules = [module_at("core/src/runs.rs", &[("7", None)])];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Ambiguous);
        assert_ne!(single(&junction).anchor, Anchor::Declared);
        assert_eq!(single(&junction).modules, ["core/src/runs.rs"]);
        assert_eq!(junction.counts.declared, 0);
        assert_eq!(junction.counts.ambiguous, 1);
    }

    #[test]
    fn a_decision_cited_with_its_document_is_declared() {
        // The shape §8 prescribes, and the only one that earns certainty. Zero of these exist in
        // this tree — the test is what says what the fix is supposed to produce.
        let decisions = [decided(1, SLUGS[0], "### 6.4 Quatro tipos", 4)];
        let modules = [module_at(
            "core/src/workflow_graph.rs",
            &[("6.4", Some("workspace-de-projeto"))],
        )];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Declared);
        assert_eq!(single(&junction).modules, ["core/src/workflow_graph.rs"]);
        assert_eq!(junction.counts.declared, 1);
    }

    #[test]
    fn a_heading_with_no_number_is_unnumbered_and_not_a_claim_about_code() {
        // `Silent` would be a report on a search, and no search ran: there was no number to look
        // for. The decision is approved and real; it simply anchors nothing, and the map says so
        // in its own word rather than borrowing one that means something nobody measured.
        let decisions = [decided(1, SLUGS[1], "## Contrato", 9)];
        let modules = [module_at("core/src/http.rs", &[("9.1", None)])];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Unnumbered);
        assert_ne!(single(&junction).anchor, Anchor::Silent);
        assert!(single(&junction).modules.is_empty());
        assert!(single(&junction).foreign.is_empty());
        assert_eq!(junction.counts.unnumbered, 1);
        assert_eq!(junction.counts.silent, 0);
        // And the module it could not be matched against is not lost with it.
        assert_eq!(junction.unmatched, ["core/src/http.rs"]);
    }

    #[test]
    fn a_module_citing_nothing_is_code_nobody_asked_for() {
        // §5.1's last row, and the one that attacks the failure this whole mode exists for: what
        // arrived inside a thousand-line plan nobody read.
        let modules = [module_at("shell/src/ui/Meter.tsx", &[])];

        let junction = join(&[], &modules, &[], &slugs());

        assert_eq!(junction.unclaimed, ["shell/src/ui/Meter.tsx"]);
        assert!(junction.unmatched.is_empty());
        assert_eq!(junction.counts.unclaimed, 1);
    }

    #[test]
    fn a_module_whose_only_citation_comes_from_its_test_is_not_unclaimed() {
        // `declares` is `source.contains('§')` — the file's OWN gesture. `cites` folds in the test
        // sibling's citations, so four real modules (`Fleet.tsx`, `Home.tsx`, `Workspace.tsx`,
        // `priority.ts`) are `declares: false` with a non-empty `cites`. *Code nobody asked for*
        // means nothing claims this module, and a module whose test names what it proves is
        // claimed. Computing this pile from `declares` would put those four in it — a wrong answer
        // wearing the right word.
        let module = Module {
            path: "shell/src/pages/Fleet.tsx".to_owned(),
            reader: Reader::Typescript,
            declares: false,
            cites: cited(&[("9.2", None)]),
            tested: true,
        };

        let junction = join(&[], &[module], &[], &slugs());

        assert!(junction.unclaimed.is_empty());
        assert_eq!(junction.unmatched, ["shell/src/pages/Fleet.tsx"]);
    }

    #[test]
    fn a_module_citing_a_section_nobody_approved_is_neither_orphan_nor_matched() {
        // On day one this is nearly every module, because approval has barely started (§10). It is
        // not *code nobody asked for* — somebody wrote down a purpose — and it is not matched
        // either. Folding it into the orphan pile would inflate the one number §5.1 counts.
        let decisions = [decided(1, SLUGS[0], "## 4.1 Três tipos", 1)];
        let modules = [module_at("core/src/gate.rs", &[("12", None)])];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert!(junction.unclaimed.is_empty());
        assert_eq!(junction.unmatched, ["core/src/gate.rs"]);
        assert!(single(&junction).modules.is_empty());
        assert_eq!(single(&junction).anchor, Anchor::Silent);
    }

    #[test]
    fn an_english_word_after_the_number_is_not_a_document() {
        // `§4.4 rule` occurs twelve times here and `§8.4 approval-pause` sits in `runs.rs` — the
        // only hyphenated candidate in shipping code, and an English phrase rather than a
        // document. The lexical half hands the join every candidate precisely so that this
        // rejection means something; a candidate refused here is not evidence thrown away, it is a
        // citation that falls back to naming a bare section.
        assert!(!names_document("rule", SLUGS[0]));
        assert!(!names_document("approval-pause", SLUGS[0]));
        assert!(!names_document("approval-pause", SLUGS[1]));

        let decisions = [decided(1, SLUGS[1], "### 8.4 A pausa", 8)];
        let modules = [module_at(
            "core/src/runs.rs",
            &[("8.4", Some("approval-pause"))],
        )];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(
            single(&junction).anchor,
            Anchor::Ambiguous,
            "a candidate that is no document leaves a bare section citation, not nothing"
        );
        assert_eq!(single(&junction).modules, ["core/src/runs.rs"]);
    }

    #[test]
    fn a_one_segment_candidate_is_never_a_document() {
        // The first of the two conditions, and it is what keeps English out. `design` is a segment
        // of nearly every spec slug in this repository and `de` of a third of them; without this
        // rule a `§7 de` would declare against half the intention layer.
        assert!(!names_document("design", SLUGS[0]));
        assert!(!names_document("de", SLUGS[0]));
        assert!(!names_document("workspace", SLUGS[0]));
        assert!(!names_document("projeto", SLUGS[1]));
    }

    #[test]
    fn the_document_match_must_be_contiguous() {
        // The second condition. A run is contiguous or it is nothing: `workspace-projeto` names
        // segments the slug has, in order, with one missing between them — and a rule that
        // accepted it would accept any two words the document happens to contain.
        assert!(names_document(
            "workspace-de-projeto",
            "2026-08-22-workspace-de-projeto-design"
        ));
        assert!(!names_document(
            "workspace-projeto",
            "2026-08-22-workspace-de-projeto-design"
        ));
        assert!(!names_document(
            "projeto-de-workspace",
            "2026-08-22-workspace-de-projeto-design"
        ));
        // A run shorter than the whole slug still matches, and that is the accepted looseness:
        // demanding the full slug would refuse `mapa-do-projeto`, which is the abbreviation §8
        // itself writes.
        assert!(names_document("mapa-do-projeto", SLUGS[1]));
        assert!(!names_document("mapa-do-projeto", SLUGS[0]));
    }

    #[test]
    fn a_section_named_twice_in_one_file_lists_that_file_once() {
        // `citations` dedups by the whole citation and not by section, deliberately: collapsing by
        // section would mean choosing which trailing candidate survives, and the discarded one
        // could have been the slug. `shell/src/pages/Fleet.tsx` really does come back with two
        // rows for §9.2 (`§9.2 risk` and `§9.2 spike`). Counting rows instead of sections would
        // list the file twice under one decision.
        let decisions = [decided(1, SLUGS[1], "### 9.2 Persistência", 9)];
        let modules = [module_at(
            "shell/src/pages/Fleet.tsx",
            &[("9.2", Some("risk")), ("9.2", Some("spike"))],
        )];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).modules, ["shell/src/pages/Fleet.tsx"]);
        assert_eq!(single(&junction).anchor, Anchor::Ambiguous);
        assert!(junction.unmatched.is_empty());
    }

    #[test]
    fn two_decisions_naming_the_same_section_both_get_the_same_modules() {
        // A section holds more than one decision routinely — §9.2 of the map spec fixes the
        // migration numbers and also decides that stamps are append-only. Each is anchored on its
        // own and neither consumes the evidence.
        let decisions = [
            decided(1, SLUGS[1], "### 9.2 Persistência", 9),
            decided(2, SLUGS[1], "### 9.2 Persistência", 10),
        ];
        let modules = [module_at("core/src/map_store.rs", &[("9.2", None)])];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(junction.decisions.len(), 2);
        assert_eq!(junction.decisions[0].modules, ["core/src/map_store.rs"]);
        assert_eq!(junction.decisions[0].modules, junction.decisions[1].modules);
        assert!(junction.unmatched.is_empty(), "matched once is matched");
        assert_eq!(junction.counts.ambiguous, 2);
    }

    #[test]
    fn a_citation_naming_another_spec_is_not_evidence_for_this_one() {
        // The candidate names a document this project really has, and it is not this decision's.
        // Counting it would be the §8 ambiguity inverted — not a guess, but a known mismatch read
        // as a match. Nothing else in the fixture claims §6.4 of the workspace spec, so the honest
        // answer is also the sound one: nothing claims it.
        let decisions = [decided(1, SLUGS[0], "### 6.4 Quatro tipos", 4)];
        let modules = [module_at(
            "core/src/x.rs",
            &[("6.4", Some("mapa-do-projeto"))],
        )];

        let junction = join(&decisions, &modules, &[], &slugs());

        assert_eq!(single(&junction).anchor, Anchor::Silent);
        assert!(single(&junction).modules.is_empty());
        assert_eq!(junction.unmatched, ["core/src/x.rs"]);

        // The contrast that makes the rule worth having: a candidate that is no document at all
        // does not disqualify its citation, it just leaves it bare.
        let prose = [module_at("core/src/x.rs", &[("6.4", Some("rule"))])];
        let bare = join(&decisions, &prose, &[], &slugs());
        assert_eq!(single(&bare).anchor, Anchor::Ambiguous);
    }

    #[test]
    fn the_order_is_by_spec_then_ordinal_and_every_path_list_is_sorted() {
        // Deterministic, and deterministic is not the same as right: §10 wants the order to be
        // recency of the anchor code's last change, which needs git and is not in this slice.
        // Pinned so that when that lands it is a visible change rather than a silent one.
        let decisions = [
            decided(3, SLUGS[1], "## 7. O carimbo", 2),
            decided(1, SLUGS[1], "## 7. O carimbo", 1),
            decided(2, SLUGS[0], "## 7. O carimbo", 9),
        ];
        let modules = [
            module_at("shell/src/ui/Meter.tsx", &[]),
            module_at("core/src/gate.rs", &[]),
            module_at("shell/src/pages/Fleet.tsx", &[("7", None)]),
            module_at("core/src/runs.rs", &[("7", None)]),
            module_at("shell/src/zz.ts", &[("99", None)]),
            module_at("core/src/aa.rs", &[("99", None)]),
        ];

        let junction = join(&decisions, &modules, &[], &slugs());

        let order: Vec<i64> = junction.decisions.iter().map(|d| d.decision_id).collect();
        assert_eq!(order, [2, 1, 3], "spec_slug, then ordinal");
        let numbers: Vec<i64> = junction.decisions.iter().map(|d| d.ordinal).collect();
        assert_eq!(
            numbers,
            [9, 1, 2],
            "the number the owner read travels with the line"
        );
        assert_eq!(
            junction.decisions[0].modules,
            ["core/src/runs.rs", "shell/src/pages/Fleet.tsx"]
        );
        assert_eq!(
            junction.unclaimed,
            ["core/src/gate.rs", "shell/src/ui/Meter.tsx"]
        );
        assert_eq!(junction.unmatched, ["core/src/aa.rs", "shell/src/zz.ts"]);

        // The id is the third key and not decoration. `map_decisions` is unique on
        // `(project_id, spec_slug, ordinal, extracted_at)`, so two extractions of one spec can
        // both hold ordinal 1 and both be approved; without the tiebreak their order would be
        // whatever the caller's query happened to hand over. Given in reverse, they come back in
        // order.
        let tied = [
            decided(8, SLUGS[0], "## 7. O carimbo", 1),
            decided(5, SLUGS[0], "## 7. O carimbo", 1),
        ];
        let broken: Vec<i64> = join(&tied, &[], &[], &slugs())
            .decisions
            .iter()
            .map(|anchored| anchored.decision_id)
            .collect();
        assert_eq!(broken, [5, 8]);
    }

    #[test]
    fn the_counts_account_for_every_decision_and_every_module() {
        // The load-bearing one. A header whose numbers do not add up is exactly the false
        // confidence this feature exists to cure, reproduced inside the cure: every decision is in
        // one of four states and every module in one of three places, or the map is quietly
        // dropping rows.
        let decisions = [
            decided(1, SLUGS[0], "### 6.4 Quatro tipos", 1),
            decided(2, SLUGS[0], "## 4.1 Três tipos", 2),
            decided(3, SLUGS[1], "### 9.2 Persistência", 1),
            decided(4, SLUGS[1], "## Contrato", 2),
            decided(5, SLUGS[1], "### 9.3 Módulos", 3),
        ];
        let modules = [
            module_at(
                "core/src/workflow_graph.rs",
                &[("6.4", Some("workspace-de-projeto"))],
            ),
            module_at("core/src/map_store.rs", &[("9.2", None)]),
            module_at("shell/src/ui/Meter.tsx", &[]),
            module_at("core/src/gate.rs", &[("12", None)]),
            module_at("core/src/http.rs", &[("9.1", None)]),
        ];
        let foreign = [foreign_at("sidecars/echo/main.go", &[("9.3", None)])];

        let junction = join(&decisions, &modules, &foreign, &slugs());
        let counts = &junction.counts;

        assert_eq!(counts.decisions, decisions.len());
        assert_eq!(
            counts.declared + counts.ambiguous + counts.silent + counts.unnumbered,
            junction.decisions.len(),
            "four states and no fifth place to fall through: {:?}",
            junction.decisions
        );
        assert_eq!(counts.declared, 1);
        assert_eq!(counts.ambiguous, 2, "§9.2 by a module, §9.3 by a sidecar");
        assert_eq!(counts.silent, 1);
        assert_eq!(counts.unnumbered, 1);

        assert_eq!(counts.unclaimed, junction.unclaimed.len());
        assert_eq!(counts.unmatched, junction.unmatched.len());

        for module in &modules {
            let orphan = junction.unclaimed.contains(&module.path);
            let unmatched = junction.unmatched.contains(&module.path);
            let matched = junction
                .decisions
                .iter()
                .any(|anchored| anchored.modules.contains(&module.path));
            let places = usize::from(orphan) + usize::from(unmatched) + usize::from(matched);
            assert_eq!(
                places, 1,
                "{} is in {places} places, not one (orphan {orphan}, unmatched {unmatched}, matched {matched})",
                module.path
            );
        }

        // The sidecar is accounted for too, in the one list that can hold it.
        assert!(junction.decisions.iter().any(|anchored| {
            anchored
                .foreign
                .contains(&"sidecars/echo/main.go".to_owned())
        }));
    }
}
