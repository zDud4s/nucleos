//! Which document a file's bare `§` numbers name: the question put to a model about one file, the
//! parse of what it answers, and the two facts the arithmetic can honestly add to it.
//!
//! **Pure, for the reason `map_intent.rs` is pure.** The prompt and the parse are where the design
//! of this slice actually lives, and a function that needs a model running to be exercised is a
//! function nobody exercises. Nothing here knows what SQL is, what HTTP is, or which model
//! answered. The one thing it touches beyond a `&str` is the filesystem, exactly as
//! [`crate::map_intent::specs_in`] does and for the same reason: where a project keeps its
//! documents is a convention to be probed, not a setting to be filled in.
//!
//! **The model proposes, and it is the only judgement in the loop.** Matching a file's cited
//! `§N` against each document's headings is the obvious mechanical design and it is dead. Over this
//! repository's citing files, plain overlap gives 48 "unique" answers and the uniques are wrong —
//! `map_join.rs` under `pilar-de-browser`, `http.rs` under `email-pillar`. Weighting rare section
//! numbers by IDF makes it worse rather than better: `project_map.rs` comes out under
//! `pilar-de-browser-design` at score 1.00 with a margin of 0.26, which passes any confidence
//! filter anybody would think to write, and is still wrong. The cause is structural rather than a
//! matter of tuning — `§1`, `§2` and `§7` exist in nearly every document here, so a scorer ranks by
//! *how many headings a document has* and not by which one the file means. **So nothing in this
//! module scores anything**, and the prompt deliberately does not show the model each document's
//! headings, because that is the measured-wrong scorer handed over with a rationale attached.
//!
//! ## The veto this module used to have, why it was withdrawn, and why it must not come back
//!
//! **The obvious replacement for a scorer is a veto** — *a document that does not contain a section
//! the file cites is certainly not that file's document* — and this module shipped one, under the
//! claim that a veto has no false positives. **That claim is false, and it was measured.** Of 30
//! hand-verified true file/document pairs, **nine are refused by it** — counted while this slice's
//! ground truth was being built, and recorded there — and `map_join.rs`, the file carrying §8's own
//! worked example, is one of them. Files legitimately cite numbers their own document lacks:
//! cross-references (`§6.4 workspace-de-projeto`), fixture numbers invented by tests, and other
//! documents quoted in prose.
//!
//! **`map_join.rs` measured by this module's own rules**, which is the figure to reproduce from:
//! it cites **32** distinct sections, **16** of them absent from the map document, and **13 of
//! those 16 exist in some other document** — so the obvious refinement, *ignore numbers no
//! document anywhere has*, rescues three of sixteen and no whole file at all. (A hand count that
//! skips the single-letter and fixture forms — `6c`, `7a`, `4.4a`, `6.44` — gives 27/11/10 instead;
//! the shape is the same either way and nothing in the finding turns on which is used.)
//!
//! **And the deeper error, which is the half worth carrying forward: a missing section is the
//! HARMLESS case.** Walk one through the pipeline. A bare `§6.4` in a file declaring
//! `mapa-do-projeto-design` becomes `Citation { section: "6.4", named: Some(the map document) }`;
//! [`crate::map_join::join`] then looks for an approved decision at §6.4 **of that document**;
//! there is none, because that is what *missing* means; so the citation anchors nothing and no
//! count moves. The harmful case is the exact reverse — a bare citation that belongs to document B
//! inherits document A, and **A has that section too**, which manufactures an
//! [`crate::map_join::Anchor::Declared`]: the one state the map may present as confirmed, wrong.
//!
//! So the veto filtered on precisely the citations that cannot hurt and was silent about the ones
//! that can. Coverage does not discriminate either, in either direction: the IDF spike put
//! `project_map.rs` under `pilar-de-browser-design` at **100% coverage** and it is wrong, while the
//! true `map_recency.rs` pair sits at **53%**.
//! `a_section_the_declared_document_lacks_anchors_nothing_and_a_section_it_has_is_the_danger` pins
//! both halves of that walk-through against the real join, so the argument is executable rather
//! than remembered.
//!
//! **No arithmetic discriminates here, and none may be reintroduced.** What survives is one
//! mechanical check that genuinely has no false positives — **the proposed slug must name a
//! document this project has** — and that is the only thing here that can reject.
//!
//! ## What the arithmetic is for now
//!
//! The unaccounted sections stop being a gate and become the module's product. [`Verdict`] carries
//! [`Verdict::unaccounted`] — the sections a file's header would govern that the named document has
//! no heading for — and [`Verdict::needs_override`], the subset some *other* document does have,
//! which is the subset a person can act on by writing `§N other-slug` on those citations. That
//! matters because of what a declaration costs: once a file declares document A it stops being
//! evidence for any decision of document B, which is what narrows a decision's anchor set and makes
//! §10's ordering work — and a module genuinely implementing two documents moves decisions from
//! `Ambiguous` to `Silent` unless its exceptional citations carry overrides. Under-reporting is the
//! safe direction and it still LOOKS like the map forgot something, so the list is named per file.
//!
//! ## Where the safety actually lives
//!
//! **Not here.** With the veto withdrawn there is no mechanical check on whether a proposal is
//! right, and pretending otherwise would be the false confidence this feature exists to cure. The
//! safety of this slice is, in its entirety, a **ground truth fixed before any model ran**: 21
//! verified file/document pairs the run is scored against before a single header is written. A
//! wrong answer there is worth roughly ten wrong files across the 209 that cite anything, so
//! **21/21 applies and anything less stops and reports**. Whoever changes this module without
//! changing that arrangement has removed the only thing standing between it and §1's failure.
//!
//! ## The four ways a file ends up unannotated, counted apart and never summed
//!
//! *The model did not know* ([`Outcome::Abstained`]), *it named a document that does not exist*
//! ([`Outcome::NoSuchDocument`]) and *nobody could read the answer* ([`Outcome::Unreadable`]) are
//! three different facts about a run, fixed in three different places — the model, the prompt, the
//! runner. The fourth is that a file was never asked about at all ([`Skipped`]). A single
//! *not annotated* total would be a number nobody could act on.

// This is a bin-only crate, so dead-code reachability starts at `main`, and nothing in this module
// is reached yet: it is the proposal half of §8's disambiguation, and the task that gives it a
// caller is the NEXT one — Task 3 of the slice-6 plan, which runs it over this repository and
// writes the proposal down. One line rather than an attribute on each of a dozen public items and
// their fields, which is the argument `errands.rs` makes about the same suppression and the reason
// this is spelled the same way. The instruction, not a description: DELETE THIS LINE with the
// change that gives this module a production caller.
//
// Scoped to the non-test build, so it silences only the absence of that caller. Under `cfg(test)`
// the lint stays live — every item below is exercised by this module's tests, and one that stops
// being exercised has to say so.
#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// One document of the project, reduced to what deciding a file's anchor needs.
///
/// **The title is carried and it is not decoration.** A slug is a filename —
/// `2026-08-15-pilar-de-browser-design` — and a list of forty of them is a list of dates. The title
/// is the line the document opens with, and it is what makes one recognisable to a model that has
/// been shown neither. Handing over slugs alone would be asking which document a file belongs to
/// while withholding what the documents are about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub slug: String,
    pub title: String,
    /// Every numbered heading, normalized the way [`crate::map_join::section_number`] normalizes
    /// one. **What [`Verdict::unaccounted`] is measured against**, and never shown to the model:
    /// see the module comment for why handing a model the headings is handing it the scorer that
    /// was measured wrong.
    pub sections: BTreeSet<String>,
}

/// One document's card, read off its text.
///
/// **Fenced blocks are skipped, and today that changes nothing.** Measured across all 43 documents
/// this project has: a fence-blind reader invents exactly **zero** sections. The rule is here for
/// the direction of the error rather than for its size — a `# 4.1 …` inside a ```` ``` ```` block
/// would put a section in a document that does not have one, and this list is what says whether a
/// citation is accounted for — a phantom heading silently accounts for a citation nothing accounts
/// for. Every other approximation in this feature errs towards under-reporting; this is the one
/// place where the cheap reading errs the other way, so it is not taken.
///
/// A document with no numbered headings gets an empty set, and that is a real answer rather than a
/// missing one: four of this project's documents are in that state, and a `§7` can refer to none of
/// them. Every section of a file proposed under one comes back unaccounted for, which reads
/// correctly: a `§7` cannot mean a document that has no §7.
pub fn spec_card(slug: &str, source: &str) -> Spec {
    let mut title = String::new();
    let mut sections = BTreeSet::new();
    let mut fenced = false;

    for line in source.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || !line.starts_with('#') {
            continue;
        }
        // The first top-level heading and not the first heading of any depth: a document opens
        // with `# NucleOS — Mapa do projeto (design)`, and `## 0. Decisões fixadas` is a section of
        // it rather than a name for it.
        if title.is_empty()
            && let Some(rest) = line.strip_prefix("# ")
        {
            title = rest.trim().to_owned();
        }
        if let Some(number) = crate::map_join::section_number(line) {
            sections.insert(number);
        }
    }

    Spec {
        slug: slug.to_owned(),
        // A document with no `# ` line is named by its file, which is what the owner reads
        // everywhere else in this feature anyway.
        title: if title.is_empty() {
            slug.to_owned()
        } else {
            title
        },
        sections,
    }
}

/// Every document of a project, read once.
///
/// Reads through [`crate::map_intent::specs_in`] rather than walking folders of its own, because
/// *where a project keeps its specs* is a question with one answer in this crate and a second
/// spelling of it would drift. Note that it finds **43** documents in this repository and not the
/// 40 the design says, because it also reads `docs/specs/` and `docs/superpowers/specs/`.
///
/// A document that cannot be read is dropped rather than failing the batch, exactly as
/// `parse_extraction` drops a line rather than the answer: the cost is one document missing from a
/// list the model is offered, and a proposal naming it is then rejected as a document this project
/// does not have — an under-report, in the direction this module errs on purpose.
pub fn catalogue(root: &Path) -> Vec<Spec> {
    crate::map_intent::specs_in(root)
        .into_iter()
        .filter_map(|relative| {
            let source = std::fs::read_to_string(root.join(&relative)).ok()?;
            Some(spec_card(&crate::map_intent::spec_slug(&relative), &source))
        })
        .collect()
}

/// How much of the text around a citation travels with it, in bytes on each side.
///
/// **Measured over this repository rather than guessed, which is what the plan asked for.** The
/// unit of evidence is *the sentence the citation sits in* — a `§7` alone says nothing about which
/// document it means, and the sentence around it usually says everything. Those sentences run to a
/// median of 168 bytes, p90 323, p95 394, p99 652, and the mark sits near their middle (median 72
/// bytes of sentence before it, 83 after), so a symmetric window is the right shape: at 400 bytes
/// total, splitting it 200/200 holds the whole sentence 74.8% of the time and the best asymmetric
/// split of the same budget, 160/240, holds it 74.7%. There is nothing to buy by biasing it.
///
/// The radius is then chosen off the curve rather than off a round number. A symmetric radius holds
/// the citation's whole sentence for 59.9% of citations at 160 bytes, 74.8% at 200, 87.1% at 250,
/// **92.3% at 300**, 95.6% at 350 and 97.0% at 400. Three hundred is where the return falls off:
/// 250 → 300 buys 5.2 points, 300 → 350 buys 3.3 for the same hundred bytes, and every one of those
/// bytes is paid [`MAX_CITED_SECTIONS`] times over in the worst case.
///
/// **What it costs at the ceiling, because that is the number that has to fit.** Twenty windows at
/// 600 bytes is 12 KB, plus at most [`MAX_DOC_BYTES`] of module comment, plus about 3.5 KB of
/// document list — roughly 22 KB for the largest file in this repository and about 6 KB for the
/// median one, which is inside `triage::LOCAL_NUM_CTX` with room to spare. Not the whole file, and
/// this is the number that says why: `http.rs` is some 20 000 lines.
pub const CITATION_RADIUS: usize = 300;

/// How many distinct sections of one file are shown to the model.
///
/// **A cap on distinct sections and not on citations, which is what makes it affordable.** This
/// repository's citing files carry a median of 3 citations and a maximum of 183 (`http.rs`), but
/// they name a median of **2** distinct sections and a maximum of 33. One window per distinct
/// section is therefore both cheaper and better evidence: the second copy of `§9.1` adds bytes and
/// no information about which document `§9.1` came from.
///
/// Twenty binds on exactly **two** of this repository's 209 citing files — `http.rs` at 33 and
/// `map_join.rs` at 28 — against a p90 of 9. Sixteen would bind on three and twelve on eleven,
/// which starts costing evidence on ordinary files to save bytes nothing needed.
///
/// **What is left out is said out loud**, the way `map_triage::listed` says it. A model shown 20 of
/// 33 sections and not told so is a model reasoning about a file it thinks it has seen.
pub const MAX_CITED_SECTIONS: usize = 20;

/// How much of a file's opening comment is sent.
///
/// The single most identifying thing in a file, so it is cut last and cut generously: this
/// repository's module comments run to a p90 of 1 533 bytes and a p99 of 4 323, with the largest at
/// 8 367. Six thousand keeps all but the very largest whole. The median is **0** — most files have
/// no opening comment at all, which is why the citation windows carry the weight and the comment is
/// a bonus rather than the evidence.
pub const MAX_DOC_BYTES: usize = 6_000;

/// How much of the model's one-line reason is kept.
///
/// **Not the thousand bytes `map_triage::MAX_REASON_BYTES` allows, and the difference is what the
/// sentence is for.** There a reason is §13's only mitigation for a silence nobody else checks, and
/// it is stored. Here it is a note beside a proposal that arithmetic checks and a human reads as a
/// diff, so it needs to be long enough to say *the module comment names the browser pillar* and no
/// longer.
pub const MAX_WHY_BYTES: usize = 500;

/// How much of an unreadable answer is quoted back into the log.
///
/// `map_triage::MAX_QUOTED_VERDICT`'s number and its argument: one slug was asked for, so this is
/// generous for the thing it quotes and mean enough for the thing it defends against — a model that
/// puts a paragraph of reasoning where the document goes.
const MAX_QUOTED: usize = 120;

/// One section this file cites, with the text around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    /// The section label, normalized: `7`, `6.4`, `5.3a`.
    pub section: String,
    /// [`CITATION_RADIUS`] bytes each side of the first place the file writes it, with comment
    /// markers and line breaks flattened away.
    pub around: String,
}

/// One file, reduced to what a model needs in order to name its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub path: String,
    /// The opening comment, flattened and cut to [`MAX_DOC_BYTES`]. Empty for the many files that
    /// have none.
    pub doc: String,
    /// One window per distinct inheriting section, in the order the file writes them, capped at
    /// [`MAX_CITED_SECTIONS`].
    pub cited: Vec<Cited>,
    /// **The sections a file-level declaration would govern**, and therefore what
    /// [`Verdict::unaccounted`] is computed over. Sorted, so a report of it is stable.
    ///
    /// **All of them, and not the ones shown in [`Self::cited`].** The model answers on at most
    /// [`MAX_CITED_SECTIONS`] windows; the arithmetic reads every section the header would touch.
    /// The asymmetry is deliberate: a section nobody showed the model is still a section the
    /// header claims, so leaving it out would let the cap quietly shorten the override list for
    /// exactly the two files — `http.rs` and `map_join.rs` — whose overrides matter most.
    pub inheriting: Vec<String>,
    /// Sections every one of whose citations already names a document of this project — §8's
    /// per-citation override, already written.
    ///
    /// **Excluded from [`Self::inheriting`], and the exclusion is what keeps the override list
    /// honest.** A citation carrying its own slug never inherits the file's declaration (see
    /// [`crate::map_join::citations`], where the header is the default and the line is the
    /// override), so counting one as unaccounted for would put a section somebody has to go and
    /// fix onto a list of things to fix, when it is already fixed. Whether it also happens to be
    /// in [`Self::inheriting`] is what decides: a file writing `§6.4 workspace-de-projeto` in one
    /// place and a bare `§6.4` in another still has a bare one for the header to govern.
    ///
    /// This mattered more when it was a gate — without it `map_join.rs` refused itself over the
    /// very citation §8 tells people to write. That gate is gone; the exclusion is not, because a
    /// list naming work already done is a list nobody finishes reading.
    pub overridden: Vec<String>,
}

impl Question {
    /// How many distinct inheriting sections had no window because of [`MAX_CITED_SECTIONS`].
    pub fn elided(&self) -> usize {
        self.inheriting.len().saturating_sub(self.cited.len())
    }
}

/// Why a file is never put to a model at all.
///
/// **Both of these are cheaper than an answer and neither is a refusal**, which is why they are not
/// [`Outcome`] variants: no proposal was made, so there is nothing to read. A run's report
/// counts them apart from the files that were asked about, because *we did not ask* and *we asked
/// and got nothing usable* are different facts about the same run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// Nothing in the file would inherit a declaration — either it cites no numbered section at
    /// all, or every section it cites already names its own document. A header would move no count,
    /// so asking would spend a model call to learn nothing.
    NothingWouldInherit,
    /// The file already declares a document. Carries the slug that won.
    ///
    /// **Asked about nothing, and never re-proposed.** [`crate::map_join::Declaration::Repeated`]
    /// says the applier for these headers is the caller it exists for and that *this file already
    /// declares something* is precisely what must not be overwritten. This is that rule, one step
    /// earlier: a file that has been decided is not a question.
    AlreadyDeclares(String),
}

/// Everything about one file that a model is shown, and nothing else about it.
///
/// Reads the file's citations through [`crate::map_join::citations`] rather than scanning for `§`
/// itself. A second definition of *what a citation is* would be a second answer to the question
/// this whole feature turns on, and the one that drifted would be found by whichever half of the
/// map stopped working.
pub fn question(path: &str, source: &str, specs: &[Spec]) -> Result<Question, Skipped> {
    if let Some(slug) = already_declared(source) {
        return Err(Skipped::AlreadyDeclares(slug));
    }

    let mut inheriting: Vec<String> = Vec::new();
    let mut overridden: Vec<String> = Vec::new();
    for citation in crate::map_join::citations(source) {
        // The file declares nothing — checked above — so `named` here is what the line itself
        // wrote and never an inherited default.
        let has_own_document = citation.named.as_deref().is_some_and(|candidate| {
            specs
                .iter()
                .any(|spec| crate::map_join::names_document(candidate, &spec.slug))
        });
        let bucket = if has_own_document {
            &mut overridden
        } else {
            &mut inheriting
        };
        if !bucket.contains(&citation.section) {
            bucket.push(citation.section);
        }
    }
    // A section written bare in one place and with its document in another is governed by the
    // header, so it belongs to the inheriting set and not to the exempt list. Deciding it here
    // rather than in the loop keeps the answer independent of the order the citations arrive in.
    overridden.retain(|section| !inheriting.contains(section));

    if inheriting.is_empty() {
        return Err(Skipped::NothingWouldInherit);
    }

    let mut cited: Vec<Cited> = Vec::new();
    for (section, at) in marks(source) {
        if cited.len() >= MAX_CITED_SECTIONS {
            break;
        }
        if !inheriting.contains(&section) || cited.iter().any(|shown| shown.section == section) {
            continue;
        }
        cited.push(Cited {
            section,
            around: flattened(window(source, at)),
        });
    }

    Ok(Question {
        path: path.to_owned(),
        doc: crate::map_triage::clipped(&flattened(&opening_comment(source)), MAX_DOC_BYTES),
        cited,
        inheriting,
        overridden,
    })
}

/// The document this file already declared, if it declared one.
///
/// First occurrence wins, which is [`crate::map_join::Declaration`]'s own rule and not a second
/// one: a declaration is a header, a header sits at the top, and everything after it is text the
/// file happens to contain.
fn already_declared(source: &str) -> Option<String> {
    match crate::map_join::declaration(source) {
        crate::map_join::Declaration::Absent => None,
        crate::map_join::Declaration::Named(slug) => Some(slug),
        crate::map_join::Declaration::Repeated(slugs) => slugs.into_iter().next(),
    }
}

/// Every `§`-number in the source, in the order the file writes them, with where it wrote them.
///
/// **Locates; it does not define.** Which citations a file has is
/// [`crate::map_join::citations`]'s answer and this does not second-guess it — this only says where
/// on the page each one was written, so the window can be cut around it and the prompt can present
/// the sections in the order a reader would meet them. It borrows
/// [`crate::map_join::leading_number`] for exactly that reason: `§6.4` and `§6.44` are different
/// citations, and a looser search here would quote the wrong sentence under the right number.
fn marks(source: &str) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    for (index, _) in source.match_indices('§') {
        let rest = &source[index + '§'.len_utf8()..];
        if let Some((section, _)) = crate::map_join::leading_number(rest) {
            found.push((section, index));
        }
    }
    found
}

/// [`CITATION_RADIUS`] bytes each side of a mark, snapped outward to character boundaries.
///
/// Snapped rather than sliced, and that is a correctness rule rather than politeness: these
/// comments are Portuguese, so byte 300 lands inside a `ç`, an `ã` or a `§` often enough, and
/// `&source[start..end]` panics there rather than truncating. `map_intent::bounded` learned this
/// over a whole document and `map_triage::clipped` over an answer; this is the third place, and a
/// daemon that died because a comment happened to be the wrong length would be invisible until the
/// one file that triggered it.
fn window(source: &str, at: usize) -> &str {
    let mut start = at.saturating_sub(CITATION_RADIUS);
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + CITATION_RADIUS).min(source.len());
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    &source[start..end]
}

/// A run of source as one line, with the marks that make it a comment taken off the front.
///
/// **Not a parser, and it does not need to be.** It strips whatever leads each line — `//!`, `///`,
/// `//`, `--`, `#`, `*` — because the prompt is showing prose to a reader and `//! ` repeated forty
/// times is forty tokens spent on punctuation. A line of real code that loses a `#` off a
/// `#[derive]` is shown slightly wrong, which costs a model nothing it was going to use: what it is
/// being asked is which document the sentence is about.
fn flattened(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut piece = line.trim();
        for marker in ["//!", "///", "//", "/*", "*/", "--", "*", "#"] {
            if let Some(rest) = piece.strip_prefix(marker) {
                piece = rest.trim();
                break;
            }
        }
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(piece);
    }
    out
}

/// The comment a file opens with, whatever language it is written in.
///
/// The leading run of comment lines, stopping at the first line that is neither a comment nor
/// blank, and at the first blank line once something has been collected. That takes a Rust `//!`
/// block, a TypeScript banner, a Go file's comment above `package` and a SQL migration's header,
/// with no reader per language — which is the posture `map_join::citations` keeps and the reason
/// this feature can read four languages at all.
///
/// A shebang is skipped rather than collected: `#!/usr/bin/env bash` is not what the file is about.
fn opening_comment(source: &str) -> String {
    let mut collected: Vec<&str> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#!") && collected.is_empty() {
            continue;
        }
        let is_comment = ["//", "/*", "*", "--", "#"]
            .iter()
            .any(|marker| trimmed.starts_with(marker));
        if is_comment {
            collected.push(trimmed);
            continue;
        }
        if trimmed.is_empty() && collected.is_empty() {
            continue;
        }
        break;
    }
    collected.join("\n")
}

/// Which question this module is asking, for the record rather than for the file.
///
/// **Not written into the header, and that was ruled rather than overlooked.** The obvious move is
/// `§spec <slug> v1`, mirroring `map_triage::TRIAGE_PROMPT_VERSION`, so a header carries the question
/// that produced it. It is wrong here for a reason that does not apply next door: that version sits
/// in a DIGEST which decides whether a stored judgement is still current, and it is read by code.
/// This one would sit in 209 source comments, read by people, as provenance for something no code
/// ever re-derives — noise in the product to record a fact about a run.
///
/// **So it goes in the run's record instead**: this constant, the proposal report Task 3 writes,
/// and the body of the commit that applies the sweep. A header found to be wrong six months from
/// now is then traced by `git log` to the commit, the commit to the run, and the run to the exact
/// question that produced it — which is everything the version in the file would have bought, in
/// the place that already keeps history.
///
/// Version 1 — the first question this module has asked. Bump it when the question materially
/// changed, not when a line was rewrapped, and say in the commit what moved: a constant that moves
/// without anybody narrating what moved is a constant nobody can read back.
pub const ANCHOR_PROMPT_VERSION: u32 = 1;

/// What to ask a model about one file.
///
/// **One question per file, and the whole design of this module is in this string.** What each rule
/// buys:
///
/// - **`none` is offered as an equal and said to be one.** This is the rule the module exists
///   around. A prompt that lists forty documents and asks which one it is has already told the
///   model that one of them is the answer, and a model that cannot tell will pick the most
///   plausible — which is precisely the failure the IDF scorer produced, with a sentence of
///   reasoning attached to make it harder to catch. So abstention is named first, given its own
///   paragraph, and priced out loud: it costs this project nothing and a guess costs it a false
///   confirmation.
/// - **The slug is to be copied exactly.** [`adjudicate`] compares it to the catalogue by equality
///   — the one rejection left in this module — so an
///   abbreviation — `mapa-do-projeto` for `2026-08-24-mapa-do-projeto-design` — is refused as a
///   document this project does not have. Resolving an abbreviation would mean deciding which
///   document it abbreviates, and deciding that from a substring is the scorer coming back in
///   through a side door.
/// - **The documents come with titles.** A list of forty slugs is a list of dates.
/// - **The sections each document has are deliberately absent.** See the module comment: they are
///   what [`Verdict::unaccounted`] is measured against afterwards, and showing them invites
///   exactly the scoring that was measured wrong.
/// - **What was left out is said.** A model shown 20 of a file's 33 sections and not told so is
///   reasoning about a file it believes it has seen whole.
///
/// Says nothing about the language of the reason, for `map_intent`'s reason: these comments are
/// half Portuguese and a translated observation is a paraphrase.
pub fn anchor_prompt(question: &Question, specs: &[Spec]) -> String {
    let path = &question.path;
    let doc = if question.doc.is_empty() {
        "(this file has no opening comment)"
    } else {
        &question.doc
    };
    let shown = question.cited.len();
    let elided = question.elided();
    let elided_note = if elided > 0 {
        format!(", and {elided} more it names that are not shown here")
    } else {
        String::new()
    };
    let windows = question
        .cited
        .iter()
        .map(|cited| format!("  §{} — …{}…", cited.section, cited.around))
        .collect::<Vec<_>>()
        .join("\n");
    let documents = specs
        .iter()
        .map(|spec| format!("  {} — {}", spec.slug, spec.title))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "You are looking at ONE file of this project and answering ONE question: which design \
         document do its bare `§` numbers refer to?\n\
         \n\
         There are two kinds of answer and they are worth exactly the same.\n\
         \n\
         - The slug of one document from the list below, copied EXACTLY as it is written there.\n\
         - \"none\".\n\
         \n\
         Answer \"none\" whenever you cannot tell, whenever two documents fit as well as each \
         other, or whenever the file's section numbers plainly come from more than one document. \
         \"none\" is a complete answer and it is not a failure: the file stays exactly as it is \
         today, which is where every file in this project already is, and somebody decides it later \
         with more in front of them than you have. A guess is the one answer that cannot be used — \
         a file put under the wrong document is then presented to its owner as CONFIRMED, and this \
         map exists because a plan once looked right and was not. If you are not sure, the answer \
         is \"none\".\n\
         \n\
         ----- BEGIN FILE -----\n\
         Path: {path}\n\
         \n\
         What the file says it is:\n\
         {doc}\n\
         \n\
         Where it names a section ({shown} shown{elided_note}). Each line is the text around the \
         mark, not the whole of it:\n\
         {windows}\n\
         ----- END FILE -----\n\
         \n\
         The documents this project has. The slug is the filename and is what you copy; the title \
         is what the document is about:\n\
         \n\
         {documents}\n\
         \n\
         `why` is ONE short sentence saying what in the file told you, or what stopped you telling. \
         Answer with JSON only, shaped exactly like this and nothing else:\n\
         {{\"spec\":\"none\",\"why\":\"...\"}}"
    )
}

/// The shape a local model is sampled into.
///
/// **`spec` is a bare string and NOT an enum of the project's slugs plus `none`**, and that
/// omission is the whole of this function. `map_intent::extraction_format` and
/// `map_triage::triage_format` both argue the identical case and it holds here with the most at
/// stake: a grammar with nowhere to put *I do not know* does not stop the model not knowing, it
/// makes the model spell not-knowing as one of the forty slugs — the guess this module is built to
/// refuse, arriving well-formed and indistinguishable from a real proposal. A grammar admitting a
/// third kind of value which [`parse_proposal`] then refuses is what keeps the failure visible.
///
/// Which is also why there is no `minLength` on `why`. Forcing a sentence out of a model that had
/// nothing to say produces a filled field and an empty thought.
fn anchor_format() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "spec": {"type": "string"},
            "why": {"type": "string"}
        },
        "required": ["spec", "why"]
    })
}

/// Ask one brain about one file, and hand back exactly what it said.
///
/// The raw text and not a [`Proposal`], for `map_triage::ask`'s reason: *nobody answered* and
/// *somebody answered something that is not a proposal* are two different facts, they send whoever
/// is debugging to two different places — the machine and the prompt — and a function that parsed
/// here could only report one of them.
///
/// Which brain is the parameter and there is no fallback, and the type is `map_intent`'s own rather
/// than a third copy of it. Its two arms exist for a behavioural reason that holds here unchanged:
/// `OllamaRunner` wears the `CommandRunner` trait while imposing the mail-triage grammar on every
/// prompt it is handed, so a proposal sent through it comes back a triage array and parses to
/// nothing.
///
/// The local window is `triage::LOCAL_NUM_CTX`, which the startup probe establishes, and the
/// ceiling on this prompt is inside it by construction — see [`CITATION_RADIUS`] for the
/// arithmetic. Unlike the extraction next door, this does not ask for four times the probed size,
/// because unlike a 60 000-byte document it does not need it.
pub async fn ask(
    asked: crate::map_intent::Extractor<'_>,
    question: &Question,
    specs: &[Spec],
) -> std::io::Result<String> {
    let prompt = anchor_prompt(question, specs);
    match asked {
        crate::map_intent::Extractor::Cli(runner) => {
            crate::map_intent::ask_once(runner, prompt, "anchor").await
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
                // Zero, because the question has one right answer about one file, and a sampled one
                // would make two runs over an unchanged repository disagree about which files were
                // safe to annotate.
                serde_json::json!({
                    "num_ctx": crate::triage::LOCAL_NUM_CTX,
                    "temperature": 0
                }),
                Some(anchor_format()),
                false,
            )
            .await
        }
    }
}

/// What a model answered about one file, read but not yet checked against anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The document it named, or `None` when it answered `none`.
    ///
    /// **`None` is an answer and never the absence of one.** It means a model read the file and
    /// declined to place it, which is a fact worth recording and acting on — the file stays bare,
    /// exactly as it is today. Nothing in this module ever produces `None` as a fallback; a failure
    /// to read the answer is an [`Unreadable`] and leaves this type unbuilt.
    pub spec: Option<String>,
    /// One sentence, for whoever reads the proposal. May be empty.
    ///
    /// **Empty is tolerated here and is a parse failure in `map_triage`, and the difference is what
    /// checks the answer.** There the reason is the only mitigation §13 names for a silence nobody
    /// else examines, so a blank one is a filled column and an empty thought. Here the proposal is
    /// scored against a ground truth fixed before the run, and read as a diff by a person — so
    /// refusing a well-formed slug because its note was blank would cost a real answer to gain
    /// nothing.
    pub why: String,
}

/// Why a model's answer produced no proposal at all.
///
/// **Two named failures rather than one, and the names are for the log rather than for the code.**
/// Nothing branches on which happened; both end with the file bare and nobody having decided
/// anything. What differs is where they send somebody debugging: *it emitted no JSON at all* is the
/// runner or the grammar, and *it put a sentence where the slug goes* is the prompt.
///
/// **A slug this project does not have is deliberately NOT one of these.** That answer is perfectly
/// readable — it is a well-formed document name, and the model may even have been shown it and
/// mistyped it. What is wrong with it is a fact about the project rather than about the answer, so
/// it belongs to [`adjudicate`], where it lands as [`Outcome::NoSuchDocument`] and is counted as
/// the rejection it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreadable {
    /// Nothing shaped like an answer came back — prose, an apology, an empty string.
    NotAnAnswer,
    /// The `spec` field held something that is neither a slug nor `none`, quoted so the log can say
    /// what it actually was.
    ///
    /// Bounded when it is built rather than when it is printed, `map_triage::Unreadable`'s rule: a
    /// model can put a page in a field that was asked for one word, and clipping in `Display` alone
    /// leaves the whole page reachable through `Debug`, which is the formatter a `warn!` reaches for
    /// first.
    NotASlug(String),
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAnswer => write!(formatter, "the answer carried no JSON object at all"),
            Self::NotASlug(said) if said.is_empty() => write!(
                formatter,
                "the document was left empty, which is neither a slug nor `none`"
            ),
            Self::NotASlug(said) => write!(
                formatter,
                "the document was `{said}`, which is neither a slug nor `none`"
            ),
        }
    }
}

#[derive(Deserialize)]
struct RawProposal {
    /// `Option<String>` and not `String`, for the reason `map_intent::RawDecision` gives at length:
    /// `#[serde(default)]` fills in for an ABSENT key and does nothing at all for a key present as
    /// `null`, and a model constrained to emit JSON answers with an explicit `null` at least as
    /// readily as by omitting the field. Typed as `String`, that one null would arrive as
    /// [`Unreadable::NotAnAnswer`] — *it emitted no JSON* — about an answer that emitted plenty.
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    why: Option<String>,
}

/// The proposal in a model's answer, or the reason there is none.
///
/// **`Result` and never a fallback, and the two tempting defaults are both worse than the error.**
/// Defaulting to `none` would turn every unreadable answer into an abstention, which reads on a
/// report as *the model looked and declined* about a model that was never understood — the same
/// collapse `map_triage::parse_answer` refuses in the other direction. Defaulting to the first slug
/// in the list, which is the shape a `.unwrap_or(&specs[0])` takes and is written by accident more
/// often than on purpose, would put a file under whichever document sorts earliest and present it as
/// confirmed. There is no third default worth having: the honest outcome is that nobody read
/// anything, and the file stays exactly as bare as it was.
///
/// **The catalogue is deliberately not consulted here.** This answers *is this an answer* and
/// [`adjudicate`] answers *is this a document of this project*, the division `map_join` already
/// keeps between reading a candidate off a citation and checking it against the project's real
/// slugs. Keeping them apart is also what lets the two failures be counted apart — see
/// [`Unreadable`].
///
/// Case is normalised on `none` and that is not leniency: `None` and `NONE` are capitalisations,
/// not third answers. Prose around the JSON is tolerated through the same `json_object` both of the
/// model's other entrances use, because models wrap answers in fences and apologies and refusing
/// that would spend a call on a habit.
pub fn parse_proposal(answer: &str) -> Result<Proposal, Unreadable> {
    let Some(raw) = crate::map_intent::json_object(answer)
        .and_then(|slice| serde_json::from_str::<RawProposal>(slice).ok())
    else {
        return Err(Unreadable::NotAnAnswer);
    };

    let why = raw.why.unwrap_or_default();
    let why = crate::map_triage::clipped(why.trim(), MAX_WHY_BYTES);

    let said = raw.spec.unwrap_or_default();
    let said = said.trim();
    if said.eq_ignore_ascii_case("none") {
        return Ok(Proposal { spec: None, why });
    }
    if !slug_shaped(said) {
        return Err(Unreadable::NotASlug(crate::map_triage::clipped(
            said, MAX_QUOTED,
        )));
    }
    Ok(Proposal {
        spec: Some(said.to_owned()),
        why,
    })
}

/// Whether a string is shaped like a document slug at all.
///
/// The SHAPE half of the two conditions `map_join::names_document` applies, and only that half:
/// lowercase, digits and hyphens, in at least two non-empty hyphen-joined segments. The other half
/// — *and this project actually has that document* — is [`adjudicate`]'s, because it needs the
/// catalogue and because a well-formed name for a document that does not exist is a rejection
/// with a reason rather than an answer nobody could read.
///
/// Two segments and not one for `names_document`'s measured reason: a one-word answer is an English
/// word until proven otherwise, and 34 of this project's 40 slugs carry a `design` segment.
fn slug_shaped(said: &str) -> bool {
    let segments: Vec<&str> = said.split('-').collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

/// What became of one file's proposal.
///
/// **Five states, and two of them annotate.** [`Self::Declares`] and [`Self::DeclaresWithGaps`]
/// both produce a header; the difference between them is a fact about the file, not permission to
/// write one. The other three are the three ways an asked-about file ends up bare, and **no two of
/// them may be added together into *not annotated***, because each is fixed somewhere else:
/// [`Self::Abstained`] by a better prompt or a better model, [`Self::NoSuchDocument`] by a prompt
/// that makes copying a slug exactly harder to get wrong, [`Self::Unreadable`] by the runner. A
/// count that merged them would be a number nobody could act on.
///
/// **There is deliberately no *refused by arithmetic* state.** There was one, and it refused nine
/// of 30 hand-verified true pairs — see the module comment for the measurement and for why the
/// case it caught is the harmless one. Adding the variant back is the obvious change and it is the
/// wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The proposal stands, and the named document has a heading for every section the header
    /// would govern.
    ///
    /// **Not a proof, and the distance matters.** Nothing here checked that the proposal is right;
    /// what was checked is that the document exists. This variant means the arithmetic found
    /// nothing further to say, which is a far smaller claim than the name looks like. What says a
    /// proposal is right is the ground truth the run is scored against before anything is applied,
    /// and a person reading the diff.
    Declares,
    /// The proposal stands, and some sections the header would govern are headings the named
    /// document does not have — [`Verdict::unaccounted`], of which [`Verdict::needs_override`] is
    /// the actionable part.
    ///
    /// **Annotated all the same, and that is the whole correction this variant carries.** A missing
    /// section is the harmless case: the citation inherits a document that has no such heading, the
    /// join finds no approved decision there, and it anchors nothing. Refusing the file over it
    /// would cost a true pair to prevent nothing — measured at nine true pairs out of 30, including
    /// the file carrying §8's own worked example.
    DeclaresWithGaps,
    /// The model answered `none`. Nothing was refused, because nothing was proposed.
    Abstained,
    /// A well-formed slug naming no document this project has.
    ///
    /// **The one mechanical rejection that survives**, and the only check here that genuinely has
    /// no false positives: a document either is in the catalogue or is not.
    /// [`Verdict::unaccounted`] stays empty, because there is no heading list to compare against
    /// and reporting *every section this file cites is missing* would be a sentence about a
    /// document that does not exist — an override list nobody could act on, in the one field that
    /// exists to be acted on.
    NoSuchDocument,
    /// Nobody could read the answer. The file is exactly as bare as before anybody asked.
    Unreadable,
}

/// One file's proposal, and the two things the arithmetic can honestly add to it.
///
/// **The two section lists are this module's product and no longer a gate.** Once a file declares
/// document A it stops being evidence for any decision of document B — that is what narrows a
/// decision's anchor set and makes §10's ordering work — so a module genuinely implementing two
/// documents moves decisions from `Ambiguous` to `Silent` unless its exceptional citations carry
/// `§N slug` overrides. Under-reporting is the safe direction and it still LOOKS like the map forgot
/// something, so the sections that would need an override are named per file rather than left to be
/// rediscovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub file: String,
    /// The slug the model named, **kept even when the document does not exist**. A report that
    /// dropped it would make a mistyped proposal indistinguishable from an abstention on the one
    /// axis that says whether the prompt or the model is the problem.
    pub proposed: Option<String>,
    pub outcome: Outcome,
    /// Sections the header would govern that the named document has no heading for. Non-empty
    /// exactly when [`Outcome::DeclaresWithGaps`].
    ///
    /// **Named for what it is and not for what it used to do.** This field was `vetoed_by` and it
    /// rejected the file. It no longer claims a refusal, because the claim was false: the same list
    /// over the same real files refused nine of 30 verified true pairs, and the citations it names
    /// are the ones that anchor nothing anyway.
    pub unaccounted: Vec<String>,
    /// The subset of [`Self::unaccounted`] that **some other document of this project does have** —
    /// the sections a person can actually act on, by writing `§N other-slug` on those citations and
    /// letting the header take the rest.
    ///
    /// **The difference from `unaccounted` is actionability, and it is not cosmetic.** A section no
    /// document anywhere has cannot be overridden onto anything: it is a stale citation, a heading
    /// renumbered away, or a number in prose that was never a citation at all — `map_join.rs`'s
    /// fixtures invent `§42` and `§6.44` precisely so something tests a shape no document has.
    /// Telling the applier to write an override for one would be telling it to name a document that
    /// does not exist, which is the failure this whole module is arranged around.
    ///
    /// **It rescues less than its name suggests, and the number is worth carrying.** Of the 16
    /// sections `map_join.rs` cites that the map document lacks, **13 exist in some other
    /// document** — so this list is long for exactly the files that span two documents, which is
    /// the honest answer rather than a comfortable one.
    pub needs_override: Vec<String>,
    /// The model's one sentence, or — for [`Outcome::Unreadable`] — this daemon's, marked with
    /// `map_triage::DAEMON_MARK` so that a line written by a parser is never read as a model's
    /// opinion.
    pub why: String,
}

/// Read one file's answer against the project's documents.
///
/// Takes the parse's `Result` rather than a `Proposal`, so that **every file asked about produces
/// exactly one verdict**. A file that vanished from the report because nobody could read its answer
/// is the under-report that looks like the map forgot things — which is the specific way this slice
/// is expected to be misread, and the reason the override list exists at all.
///
/// **Rejects on one thing only.** The document has to be in the catalogue. Everything else it
/// computes it reports, and nothing else it computes decides.
pub fn adjudicate(
    question: &Question,
    answer: Result<Proposal, Unreadable>,
    specs: &[Spec],
) -> Verdict {
    let mut verdict = Verdict {
        file: question.path.clone(),
        proposed: None,
        outcome: Outcome::Abstained,
        unaccounted: Vec::new(),
        needs_override: Vec::new(),
        why: String::new(),
    };

    let proposal = match answer {
        Err(why) => {
            verdict.outcome = Outcome::Unreadable;
            verdict.why = crate::map_triage::clipped(
                &format!(
                    "{} the answer could not be read, so nobody has proposed a document for this \
                     file — {why}.",
                    crate::map_triage::DAEMON_MARK
                ),
                MAX_WHY_BYTES,
            );
            return verdict;
        }
        Ok(proposal) => proposal,
    };

    verdict.why = proposal.why;
    let Some(said) = proposal.spec else {
        verdict.outcome = Outcome::Abstained;
        return verdict;
    };
    verdict.proposed = Some(said.clone());

    // Equality and not `names_document`'s contiguous-run rule, which is the looser test a citation's
    // candidate gets. The looseness is right there — somebody wrote `mapa-do-projeto` in prose and
    // meant the document — and wrong here, because the model was handed the list and asked to copy
    // from it. Accepting an abbreviation would mean deciding which of the documents containing that
    // run it abbreviates, and deciding that from a substring is the measured-wrong scorer coming
    // back in through a side door.
    let Some(spec) = specs.iter().find(|spec| spec.slug == said) else {
        verdict.outcome = Outcome::NoSuchDocument;
        return verdict;
    };

    let unaccounted: Vec<String> = question
        .inheriting
        .iter()
        .filter(|section| !spec.sections.contains(*section))
        .cloned()
        .collect();
    if unaccounted.is_empty() {
        verdict.outcome = Outcome::Declares;
        return verdict;
    }

    verdict.needs_override = unaccounted
        .iter()
        .filter(|section| {
            specs
                .iter()
                .any(|other| other.slug != spec.slug && other.sections.contains(*section))
        })
        .cloned()
        .collect();
    verdict.unaccounted = unaccounted;
    // Reported, and NOT refused. See the module comment: refusing here cost nine of 30 verified
    // true pairs and prevented nothing, because a citation inheriting a document that has no such
    // heading anchors nothing when the join goes looking for it.
    verdict.outcome = Outcome::DeclaresWithGaps;
    verdict
}

/// What a whole run came to, with the ways of ending up bare kept apart.
///
/// **Two annotating counts and deliberately no total of them.** `declares + declares_with_gaps` is
/// the number of headers a sweep would write, and a field holding it would be the field everyone
/// quotes — at which point the gaps stop being read, which is the one thing this struct exists to
/// keep visible. A caller wanting the total adds two numbers it has just been shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct AnchorCounts {
    pub files: usize,
    pub declares: usize,
    pub declares_with_gaps: usize,
    pub abstained: usize,
    pub no_such_document: usize,
    pub unreadable: usize,
    /// Files carrying at least one unaccounted section that some other document does have — the
    /// files somebody has to look at by hand. §8's own example is one of these, and so is
    /// `map_join.rs`: it writes `§6.4 workspace-de-projeto` eight times and a **bare** `§6.4` twelve
    /// times, so it needs overrides written into it by hand whatever any sweep does.
    pub needing_overrides: usize,
}

/// The numbers a run reports, counted rather than promised.
pub fn tally(verdicts: &[Verdict]) -> AnchorCounts {
    let mut counts = AnchorCounts {
        files: verdicts.len(),
        ..AnchorCounts::default()
    };
    for verdict in verdicts {
        match verdict.outcome {
            Outcome::Declares => counts.declares += 1,
            Outcome::DeclaresWithGaps => counts.declares_with_gaps += 1,
            Outcome::Abstained => counts.abstained += 1,
            Outcome::NoSuchDocument => counts.no_such_document += 1,
            Outcome::Unreadable => counts.unreadable += 1,
        }
        if !verdict.needs_override.is_empty() {
            counts.needing_overrides += 1;
        }
    }
    counts
}

/// What one pass over a project's citing files came to, with the three ways a file leaves it
/// kept apart.
///
/// **Every file handed in comes back in exactly one of these three lists**, which is
/// [`adjudicate`]'s own rule one level up: a file that vanished from a report because its run
/// died is the under-report that looks like the map forgot something, and this whole slice is
/// arranged around not doing that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sweep {
    /// One per file that was actually asked about — including the ones nobody could read an
    /// answer for, which arrive as [`Outcome::Unreadable`].
    pub verdicts: Vec<Verdict>,
    /// The files that were never a question, with which of [`Skipped`]'s two reasons applied.
    pub skipped: Vec<(String, Skipped)>,
    /// The files whose model run never finished, with what the runner said about it.
    ///
    /// **Not an [`Outcome::Unreadable`], and the line between them is the one [`ask`] already
    /// draws.** That variant means somebody answered and nobody could read it, which sends a
    /// reader to the prompt or the model. This is a fact about the machine — a CLI that exited
    /// non-zero, a rate limit, a network that was not there — and folding it into a verdict would
    /// report a model as having failed to answer a question it was never asked, which is the
    /// same collapse `map_intent::extract` refuses between an empty list and an error. Counted
    /// and named apart, so a report short by nine files says which nine and why, and so that
    /// re-running just those nine is a thing the report tells you how to do.
    pub failed: Vec<(String, String)>,
}

/// Ask one brain about every file in a list, and keep what each answer came to.
///
/// **Sequential on purpose, and the concurrency is the caller's.** How many model calls may be in
/// flight at once is a spending decision about a particular machine and a particular brain — a
/// local model wants one, a cloud CLI wants eight — and a number chosen in here would be that
/// decision made once, for everybody, by the layer with the least idea of it. A caller that wants
/// lanes shards the list and calls this once per shard, which is what the sweep harness below
/// does; a route serving one button would call it once and be right to.
///
/// **A file that cannot be read is read as empty**, exactly as [`crate::project_map::structure`]
/// reads one, and an empty file inherits nothing — so it comes back as
/// [`Skipped::NothingWouldInherit`] rather than as a failure. That is the honest answer: nothing
/// went wrong with any model, and a file this walk could not open names no section it could see.
pub async fn sweep(
    asked: crate::map_intent::Extractor<'_>,
    root: &Path,
    files: &[String],
    specs: &[Spec],
) -> Sweep {
    let mut swept = Sweep::default();
    for path in files {
        let source = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let about = match question(path, &source, specs) {
            Ok(about) => about,
            Err(why) => {
                swept.skipped.push((path.clone(), why));
                continue;
            }
        };
        match ask(asked, &about, specs).await {
            Err(error) => swept.failed.push((path.clone(), error.to_string())),
            Ok(answer) => {
                swept
                    .verdicts
                    .push(adjudicate(&about, parse_proposal(&answer), specs));
            }
        }
    }
    swept
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Two real documents of this repository, because the one rejection left in this module is a
    /// check against documents that actually exist.
    const MAP: &str = "2026-08-24-mapa-do-projeto-design";
    const WORKSPACE: &str = "2026-08-22-workspace-de-projeto-design";

    /// The slug the `§spec` fixtures below name, and **not a document this repository has.**
    ///
    /// `map_join::declaration` is deliberately not a parser, so a complete declaration written out
    /// as a plain string literal in this file would be read as a declaration OF this file the
    /// moment the map walks this repository — and this file carries a good many bare citations of
    /// its own. A real slug would hand every one of them that document and move the junction's
    /// counts on a commit whose whole safety argument is that they cannot move.
    ///
    /// **So every fixture below interpolates rather than spelling the marker out**, which keeps the
    /// whole declaration out of this file's text, and the slug is fictional as well. Two defences
    /// on purpose, exactly as `map_join`'s tests keep them: the first is easy to lose — the next
    /// fixture somebody writes as a plain literal quietly declares this module — and the second
    /// holds whatever happens to the first, because nothing checking a slug against this project's
    /// documents will ever find this one. `this_module_declares_nothing_about_itself` is what says
    /// both are still in force.
    const FIXTURE: &str = "documento-de-fixture";

    fn spec(slug: &str, title: &str, sections: &[&str]) -> Spec {
        Spec {
            slug: slug.to_owned(),
            title: title.to_owned(),
            sections: sections.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// A stand-in catalogue: the two documents this repository's map slice is about, with the
    /// sections each really has.
    fn specs() -> Vec<Spec> {
        vec![
            spec(
                MAP,
                "NucleOS — Mapa do projeto (design)",
                &["1", "4.1", "5.1", "8", "9.2"],
            ),
            spec(
                WORKSPACE,
                "NucleOS — Workspace de projeto (design)",
                &["1", "6.4", "7"],
            ),
        ]
    }

    fn asked(path: &str, source: &str) -> Question {
        question(path, source, &specs()).expect("this fixture should be worth asking about")
    }

    fn answered(question: &Question, said: &str) -> Verdict {
        adjudicate(question, parse_proposal(said), &specs())
    }

    #[test]
    fn a_section_the_proposed_document_lacks_is_reported_and_is_never_a_refusal() {
        // **This test asserted the opposite when it landed, and the measurement is why it turned
        // round.** The map document has no §6.4, so a strict reading says a file citing §6.4 is
        // certainly not under it. That reading refused nine of 30 hand-verified true pairs,
        // `map_join.rs` among them, and the file it refuses here IS `map_join.rs`. A file
        // legitimately names sections its own document lacks: cross-references, fixture numbers,
        // other documents quoted in prose.
        //
        // So it is reported and never refused, and the next test walks through why refusing bought
        // nothing: a citation inheriting a document with no such heading anchors nothing.
        let file = asked(
            "core/src/map_join.rs",
            "//! The junction. §8 asks a citation to name its document, and §6.4 of the workspace              document is the example it uses.
",
        );

        let verdict = answered(
            &file,
            &format!("{{\"spec\":\"{MAP}\",\"why\":\"the module comment is about the map\"}}"),
        );

        assert_eq!(verdict.outcome, Outcome::DeclaresWithGaps);
        assert_ne!(
            verdict.outcome,
            Outcome::NoSuchDocument,
            "the document exists; only a heading is missing, which is a different fact"
        );
        assert_eq!(verdict.unaccounted, ["6.4"]);
        assert_eq!(
            verdict.needs_override,
            ["6.4"],
            "the workspace document has it, so a person can write the override"
        );
        assert_eq!(
            verdict.proposed.as_deref(),
            Some(MAP),
            "and the header goes in, which is the whole of the correction"
        );
    }

    #[test]
    fn a_section_the_declared_document_lacks_anchors_nothing_and_a_section_it_has_is_the_danger() {
        // **The walk-through that withdrew the veto, run against the real join rather than
        // remembered.** Both halves matter and the second is the one nothing in this module can
        // see.
        use crate::map_join::{Anchor, citations, join};
        use crate::map_store::Decision;
        use crate::project_map::{Module, Reader};

        let slugs = vec![MAP.to_owned(), WORKSPACE.to_owned()];
        let decided = |id: i64, slug: &str, section: &str| Decision {
            id,
            spec_slug: slug.to_owned(),
            section: section.to_owned(),
            ordinal: id,
            text: format!("decision {id}"),
            kind: crate::map_intent::Kind::Character,
            brain: "local".to_owned(),
            extracted_at: "2026-08-24T00:00:00Z".to_owned(),
            approved_at: Some("2026-08-24T01:00:00Z".to_owned()),
        };
        let reading = |path: &str, source: &str| Module {
            path: path.to_owned(),
            reader: Reader::Rust,
            declares: crate::project_map::cites_section(source),
            cites: citations(source).into_iter().collect(),
            tested: false,
        };

        // HALF ONE, the harmless case the veto was built to catch. The file declares the map
        // document and writes a bare §6.4. The map document has no §6.4 at all, so no decision of
        // it can sit there; the only §6.4 anybody approved belongs to the workspace document, and
        // the join goes looking for §6.4 OF THE MAP DOCUMENT and finds nothing. **Nothing is
        // confirmed, before or after.**
        let elsewhere = [decided(1, WORKSPACE, "### 6.4 Quatro tipos")];
        let declaring = [reading(
            "core/src/map_join.rs",
            &format!(
                "//! §spec {MAP}
/// and §6.4, bare.
"
            ),
        )];
        let bare = [reading(
            "core/src/map_join.rs",
            "/// and §6.4, bare.
",
        )];

        let after = join(&elsewhere, &declaring, &[], &slugs);
        let before = join(&elsewhere, &bare, &[], &slugs);

        assert_eq!(
            after.counts.declared, 0,
            "no false confirmation is possible"
        );
        assert_eq!(before.counts.declared, 0);
        // What it DOES cost is the guess it used to make: the decision was `Ambiguous` on the
        // strength of a bare number and is now `Silent`, because the file said out loud that it
        // meant another document. That is the wave the slice predicts, and it is a correction
        // rather than damage — the honest half of a junction that admits what it does not know.
        assert_eq!(before.counts.ambiguous, 1);
        assert_eq!(after.counts.silent, 1);
        assert_eq!(after.counts.ambiguous, 0);

        // HALF TWO, the case that can hurt, and no arithmetic in this module sees it. The file
        // declares the map document and writes a bare §1 it really meant of the workspace
        // document. The map document HAS a §1, with an approved decision at it — so the header
        // manufactures an `Anchor::Declared`: the one state the map may present as confirmed,
        // wrong.
        let shared = [decided(2, MAP, "## 1. O problema")];
        let danger = [reading(
            "core/src/x.rs",
            &format!(
                "//! §spec {MAP}
/// and §1, bare.
"
            ),
        )];
        let honest = [reading(
            "core/src/x.rs",
            "/// and §1, bare.
",
        )];

        assert_eq!(join(&shared, &danger, &[], &slugs).counts.declared, 1);
        assert_eq!(join(&shared, &honest, &[], &slugs).counts.declared, 0);
        assert_eq!(
            join(&shared, &honest, &[], &slugs).decisions[0].anchor,
            Anchor::Ambiguous
        );

        // And this is the sentence the whole rework turns on: on that very file, the arithmetic
        // returns a clean bill. It cannot tell a right header from a wrong one, so it is not
        // allowed to decide.
        let asked_about = asked(
            "core/src/x.rs",
            "/// and §1, bare.
",
        );
        let verdict = answered(
            &asked_about,
            &format!("{{\"spec\":\"{MAP}\",\"why\":\"confidently wrong\"}}"),
        );
        assert_eq!(verdict.outcome, Outcome::Declares);
        assert!(verdict.unaccounted.is_empty());
    }

    #[test]
    fn a_proposal_the_model_declined_to_make_is_an_abstention_and_not_a_rejection() {
        // *The model did not know* and *the model was wrong* are different facts about a run, they
        // are fixed in different places, and a report that added them together would say which of
        // the two nobody could act on.
        let file = asked("core/src/gate.rs", "// the gate, and §7 governs it.\n");

        let verdict = answered(
            &file,
            "{\"spec\":\"none\",\"why\":\"two documents fit this file as well as each other\"}",
        );

        assert_eq!(verdict.outcome, Outcome::Abstained);
        assert_eq!(verdict.proposed, None);
        assert!(
            verdict.unaccounted.is_empty() && verdict.needs_override.is_empty(),
            "nothing was measured, because nothing was proposed"
        );
        assert_eq!(
            verdict.why,
            "two documents fit this file as well as each other"
        );

        // `None` and `NONE` are capitalisations and not third answers, exactly as `map_triage`
        // normalises its two verdicts and for the same reason.
        assert_eq!(
            parse_proposal("{\"spec\":\"NONE\",\"why\":\"\"}")
                .expect("a capitalisation is still an abstention")
                .spec,
            None
        );
    }

    #[test]
    fn a_proposal_naming_a_spec_this_project_does_not_have_is_vetoed_without_asking_anything() {
        // Both sections this file cites are real sections of the workspace document, so an
        // implementation that ran the arithmetic anyway would report every one of them as missing
        // and hand the applier an override list naming a document that does not exist. There is
        // nothing to compare against, and saying so in an empty list is the honest answer.
        let file = asked(
            "core/src/workflow_graph.rs",
            "// four kinds, decided by whoever runs the node — §6.4, and §7 beside it.\n",
        );

        let verdict = answered(
            &file,
            &format!("{{\"spec\":\"{FIXTURE}\",\"why\":\"a typo\"}}"),
        );

        assert_eq!(verdict.outcome, Outcome::NoSuchDocument);
        assert_eq!(verdict.proposed.as_deref(), Some(FIXTURE));
        assert!(
            verdict.unaccounted.is_empty(),
            "there is no heading list to be absent from"
        );
        assert!(
            verdict.needs_override.is_empty(),
            "an override list here would be an instruction to name a document nobody has"
        );
    }

    #[test]
    fn an_answer_that_is_neither_a_known_slug_nor_none_is_a_parse_failure() {
        // Never a silent fallback in either direction. Defaulting to `none` would report a model
        // nobody understood as one that looked and declined; defaulting to the first slug — the
        // shape a `.unwrap_or(&specs[0])` takes, which is written by accident more often than on
        // purpose — would put a file under whichever document sorts earliest and then present it as
        // confirmed.
        for said in [
            "{\"spec\":\"I think it is the map document\",\"why\":\"\"}",
            "{\"spec\":\"mapa\",\"why\":\"a single word is an English word until proven otherwise\"}",
            "{\"spec\":\"\",\"why\":\"\"}",
            "{\"why\":\"the field is not there at all\"}",
            "I could not tell which document this is.",
        ] {
            assert!(
                parse_proposal(said).is_err(),
                "read as a proposal rather than refused: {said}"
            );
        }

        let file = asked("core/src/x.rs", "// §1 alone.\n");
        let verdict = adjudicate(
            &file,
            parse_proposal("{\"spec\":\"the map one\",\"why\":\"\"}"),
            &specs(),
        );

        assert_eq!(verdict.outcome, Outcome::Unreadable);
        assert_ne!(verdict.outcome, Outcome::Abstained);
        assert_eq!(
            verdict.proposed, None,
            "and never the first slug in the list"
        );
        assert!(
            verdict.why.starts_with(crate::map_triage::DAEMON_MARK),
            "a sentence a parser wrote must never be read as a model's opinion"
        );

        // The two failures are named apart because they send somebody debugging to two different
        // places: the runner, and the prompt.
        assert_eq!(parse_proposal("nothing here"), Err(Unreadable::NotAnAnswer));
        assert_eq!(
            parse_proposal("{\"spec\":\"mapa\",\"why\":\"\"}"),
            Err(Unreadable::NotASlug("mapa".to_owned()))
        );
    }

    #[test]
    fn a_large_document_is_neither_preferred_nor_penalised_for_its_size() {
        // Fifty-nine sections is the largest document this repository has, and a scorer handed a
        // file citing `§7` ranks it first for exactly that reason — *how many headings a document
        // has* rather than which one the file means. Nothing here ranks, and this test is what says
        // so from both sides: the small document, which is the right answer, comes back with
        // nothing unaccounted for; the large one, which any scorer would prefer, is **still
        // annotated** and simply carries the one section it cannot account for.
        //
        // The second half is the load-bearing one now. It used to assert a refusal, and a test that
        // let the arithmetic pick between two documents is exactly how the arithmetic became the
        // decider in the first place.
        let numbers: Vec<String> = (1..=59).map(|n| n.to_string()).collect();
        let every: Vec<&str> = numbers.iter().map(String::as_str).collect();
        let catalogue = vec![
            spec(MAP, "the large one", &every),
            spec(WORKSPACE, "the small one", &["1", "6.4", "7"]),
        ];
        let file = question(
            "core/src/workflow_graph.rs",
            "// four kinds, decided by whoever runs the node — §6.4, and §7 beside it.
",
            &catalogue,
        )
        .expect("worth asking about");

        let small = adjudicate(
            &file,
            parse_proposal(&format!(
                "{{\"spec\":\"{WORKSPACE}\",\"why\":\"the four kinds are its §6.4\"}}"
            )),
            &catalogue,
        );

        assert_eq!(small.outcome, Outcome::Declares);
        assert!(small.unaccounted.is_empty());

        let large = adjudicate(
            &file,
            parse_proposal(&format!(
                "{{\"spec\":\"{MAP}\",\"why\":\"it contains nearly every section there is\"}}"
            )),
            &catalogue,
        );

        assert_eq!(
            large.outcome,
            Outcome::DeclaresWithGaps,
            "annotated, with §6.4 named as the thing it cannot account for"
        );
        assert_eq!(large.unaccounted, ["6.4"]);
        assert_eq!(large.needs_override, ["6.4"]);
    }

    #[test]
    fn the_sections_a_proposal_cannot_account_for_come_back_as_the_override_list() {
        // Two sections the proposed document does not have, and they are not the same problem.
        // `§6.4` belongs to another document of this project, so writing `§6.4 <slug>` on that one
        // citation lets the header in for everything else. `§42` belongs to nothing anywhere — a
        // heading renumbered away, a stale reference, or a number in prose that was never a
        // citation — and no override can name a document for it. Both go unaccounted for; only one
        // is actionable, and telling the applier otherwise would be telling it to invent a
        // document.
        let file = asked(
            "core/src/map_join.rs",
            "//! §8, and the §6.4 example, and §42 which nothing has.\n",
        );

        let verdict = answered(
            &file,
            &format!("{{\"spec\":\"{MAP}\",\"why\":\"this module is the junction\"}}"),
        );

        assert_eq!(verdict.outcome, Outcome::DeclaresWithGaps);
        assert_eq!(verdict.unaccounted, ["42", "6.4"]);
        assert_eq!(verdict.needs_override, ["6.4"]);
        assert_eq!(tally(&[verdict]).needing_overrides, 1);
    }

    #[test]
    fn a_citation_that_already_names_its_own_document_stays_off_the_override_list() {
        // This repository's own flagship case. `map_join.rs` belongs to the map document and its
        // fixtures write `§6.4 workspace-de-projeto`, which is §8's per-citation override already in
        // place. `citations` makes the header the default and the line the override, so a citation
        // that already carries a document is one the header will never govern — and putting it on
        // the list of things somebody has to go and fix means naming work that is already done.
        //
        // **This used to be a gate, and losing the gate is not a reason to lose this.** Without the
        // exclusion, `map_join.rs` was refused over the very citation §8 tells people to write. The
        // refusal is gone; a list nobody finishes reading is still a list nobody reads.
        let file = asked(
            "core/src/map_join.rs",
            &format!(
                "//! The junction. §8 here, and §6.4 {WORKSPACE} in a fixture.
"
            ),
        );

        assert_eq!(file.inheriting, ["8"]);
        assert_eq!(file.overridden, ["6.4"]);
        let verdict = answered(
            &file,
            &format!("{{\"spec\":\"{MAP}\",\"why\":\"the junction\"}}"),
        );
        assert_eq!(verdict.outcome, Outcome::Declares);
        assert!(verdict.needs_override.is_empty());

        // And a file that ALSO writes the section bare has one for the header to govern, so it
        // belongs on the list. Whether the exclusion applies is a question about every citation of
        // that section and not about the luckiest one.
        let mixed = asked(
            "core/src/map_join.rs",
            &format!(
                "//! The junction. §8 here, §6.4 {WORKSPACE} in a fixture, and a bare §6.4 in the                  prose.
"
            ),
        );

        assert_eq!(mixed.inheriting, ["6.4", "8"]);
        assert!(mixed.overridden.is_empty());
        let verdict = answered(
            &mixed,
            &format!("{{\"spec\":\"{MAP}\",\"why\":\"the junction\"}}"),
        );
        assert_eq!(verdict.outcome, Outcome::DeclaresWithGaps);
        assert_eq!(verdict.needs_override, ["6.4"]);
    }

    #[test]
    fn the_prompt_names_every_spec_slug_with_its_title_and_offers_none_as_plainly_as_a_slug() {
        let file = asked(
            "core/src/map_join.rs",
            "//! The junction between what a document decided and what the code implements. §8, \
             and §6.4 beside it.\n",
        );

        let prompt = anchor_prompt(&file, &specs());

        for spec in specs() {
            assert!(
                prompt.contains(&spec.slug),
                "a model cannot copy a slug it was never shown: {}",
                spec.slug
            );
            assert!(
                prompt.contains(&spec.title),
                "a list of forty slugs is a list of dates: {}",
                spec.title
            );
        }

        // The file itself, or the model is answering about nothing.
        assert!(prompt.contains("core/src/map_join.rs"));
        assert!(prompt.contains("The junction between what a document decided"));
        assert!(prompt.contains("§8"));
        assert!(prompt.contains("§6.4"));

        // Abstention offered first, priced out loud, and repeated where the doubt actually arises.
        // A prompt that lists forty documents and asks which one it is has already told the model
        // that one of them is the answer.
        assert!(prompt.contains("\"none\" is a complete answer and it is not a failure"));
        assert!(prompt.contains("If you are not sure, the answer is \"none\""));

        // §4.1 is a section of the map document that this file does not cite. If the prompt ever
        // starts listing what each document contains, this is the assertion that says so — and that
        // listing is the measured-wrong scorer handed to the model with a rationale attached.
        assert!(
            !prompt.contains("4.1"),
            "the documents' heading lists are read after the answer and are never shown"
        );
    }

    #[test]
    fn this_module_declares_nothing_about_itself() {
        // `map_join::declaration` is deliberately not a parser, so a complete `§spec` line written
        // out as a plain literal anywhere in this file — in a fixture, in a comment explaining the
        // convention — would put THIS module under that document the moment the map walks this
        // repository, and every bare citation in the prose above with it. The fixtures interpolate
        // for exactly that reason, and this is what says they still do.
        let me = include_str!("map_anchor.rs");

        assert_eq!(
            crate::map_join::declaration(me),
            crate::map_join::Declaration::Absent,
            "a fixture in this file has spelled a declaration out, and is now declaring this module"
        );

        // The second defence, independent of the first: the slug the fixtures name is not a
        // document this project has, so a declaration that escaped the first would still be refused
        // rather than believed.
        assert!(
            specs()
                .iter()
                .all(|spec| !crate::map_join::names_document(FIXTURE, &spec.slug))
        );
    }

    #[test]
    fn a_file_that_already_declares_is_never_asked_anything() {
        // `Declaration::Repeated` names the applier for these headers as the caller it exists for,
        // and says that *this file already declares something* is precisely what must not be
        // overwritten. This is that rule one step earlier: a file that has been decided is not a
        // question, and spending a model call on it invites a second header contradicting the
        // first.
        let source = format!("//! §spec {FIXTURE}\n/// and §6.4 below it.\n");

        assert_eq!(
            question("core/src/workflow_graph.rs", &source, &specs()),
            Err(Skipped::AlreadyDeclares(FIXTURE.to_owned()))
        );
    }

    #[test]
    fn a_file_with_nothing_to_inherit_a_declaration_is_not_worth_a_question() {
        // Two ways to have nothing to gain: cite no section at all, or already carry a document on
        // every citation. In both the header would move no count, so asking would spend a model
        // call to learn nothing — and every call spent on those is a call not spent on the files
        // where the answer changes something.
        assert_eq!(
            question(
                "core/src/workflow_graph.rs",
                &format!("//! §6.4 {WORKSPACE} — the four kinds.\n"),
                &specs()
            ),
            Err(Skipped::NothingWouldInherit)
        );
        assert_eq!(
            question(
                "shell/src/ui/Meter.tsx",
                "// a meter, and nothing claims it\n",
                &specs()
            ),
            Err(Skipped::NothingWouldInherit)
        );
    }

    #[test]
    fn the_window_around_a_citation_is_bounded_and_never_splits_a_character() {
        // Not the whole file — `http.rs` is some 20 000 lines — and the sentence around the mark is
        // what says which document it means. The boundary rule is a correctness rule and not
        // politeness: these comments are Portuguese, and slicing at byte 300 inside an `ã` panics
        // rather than truncating.
        let padding = "ã".repeat(2_000);
        let source = format!("//! {padding} — §7 — {padding}\n");

        let file = asked("core/src/voice.rs", &source);

        let shown = &file.cited[0].around;
        assert!(source.len() > 4_000);
        assert!(
            shown.len() <= 2 * CITATION_RADIUS + 4,
            "the window is bounded: {} bytes",
            shown.len()
        );
        assert!(
            shown.contains("§7"),
            "the mark the window was cut around has to be inside it"
        );
    }

    #[test]
    fn a_file_naming_more_sections_than_the_prompt_shows_says_how_many_it_left_out() {
        // Twenty binds on exactly two files here — `http.rs` at 33 distinct sections and
        // `map_join.rs` at 28 — and a model shown 20 of 33 without being told is reasoning about a
        // file it believes it has seen whole. `map_triage::listed` says the same thing about a path
        // list for the same reason.
        let mut source = String::from("//! a file that names a great many sections.\n");
        for number in 1..=25 {
            source.push_str(&format!("// section §{number} is named here.\n"));
        }

        let file = asked("core/src/http.rs", &source);

        assert_eq!(file.inheriting.len(), 25);
        assert_eq!(file.cited.len(), MAX_CITED_SECTIONS);
        assert_eq!(file.elided(), 5);
        assert!(anchor_prompt(&file, &specs()).contains("5 more it names that are not shown here"));
    }

    #[test]
    fn a_document_s_card_reads_its_title_its_numbers_and_not_a_heading_inside_a_fence() {
        // A `#` inside a fenced block would put a section in a document that does not have one,
        // and this list is what decides whether a citation is accounted for — a phantom heading
        // silently accounts for a citation nothing accounts for, which shortens the override list
        // by exactly the entries somebody needed to see. Measured across all 43 documents here, a
        // fence-blind reader invents zero sections today; the rule is kept for the direction of
        // the error, not its size.
        let source = "# NucleOS — Mapa do projeto (design)\n\n\
                      ## 0. Decisões fixadas\n\n\
                      ### 4.1 Três tipos\n\n\
                      ## Contrato\n\n\
                      ```rust\n\
                      # 9.9 a heading of nothing\n\
                      ```\n\n\
                      ## 13. Riscos\n";

        let card = spec_card(MAP, source);

        assert_eq!(card.title, "NucleOS — Mapa do projeto (design)");
        assert_eq!(
            card.sections.iter().cloned().collect::<Vec<_>>(),
            ["0", "13", "4.1"],
            "`## Contrato` carries no number and is not a section anything could cite"
        );
        assert!(!card.sections.contains("9.9"));

        // A document with no top-level heading is named by its file, which is what the owner reads
        // everywhere else in this feature.
        assert_eq!(spec_card(MAP, "## 1. Alfa\n").title, MAP);
    }

    /// A runner with one canned answer, `map_intent`'s own fixture spelled the same way.
    fn fake_answering(stdout: &str) -> crate::runner::FakeCommandRunner {
        crate::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::runner::RunOutcome {
                exit_code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_run_that_nobody_finished_is_an_error_and_never_a_file_the_model_declined() {
        // The collapse this whole module is arranged to refuse, arriving through the runner instead
        // of through the parse. `map_intent::ask_once` reports most CLI failures as `Ok` with a
        // non-zero code, and letting one fall through to a parse produces no JSON, which becomes
        // `Unreadable` — recoverable, and honest. What must never happen is that it becomes an
        // abstention, because a report saying *the model looked at 40 files and declined* about 40
        // files nobody ever read is the false confidence with better pixels.
        let file = asked("core/src/gate.rs", "// the gate, and §7 governs it.\n");
        let failing = crate::runner::FakeCommandRunner {
            fail_times: std::sync::Mutex::new(1),
            ..Default::default()
        };

        assert!(
            ask(crate::map_intent::Extractor::Cli(&failing), &file, &specs())
                .await
                .is_err()
        );

        // And the ordinary path: the prompt reaches the runner, and a reader that could edit the
        // repository is not reading it.
        let runner = fake_answering(&format!("{{\"spec\":\"{MAP}\",\"why\":\"the junction\"}}"));
        let said = ask(crate::map_intent::Extractor::Cli(&runner), &file, &specs())
            .await
            .expect("the runner answered");

        assert_eq!(
            parse_proposal(&said).expect("a proposal").spec.as_deref(),
            Some(MAP)
        );
        assert!(
            runner
                .last_prompt
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|prompt| prompt.contains("core/src/gate.rs"))
        );
        assert_eq!(
            *runner.last_tool_policy.lock().unwrap(),
            Some(crate::runner::ToolPolicy::None)
        );
    }

    #[test]
    fn the_grammar_leaves_the_model_somewhere_to_put_not_knowing() {
        // The temptation is `enum: [<every slug>, "none"]`, which looks like it makes
        // `Unreadable::NotASlug` unreachable. `map_intent::extraction_format` and
        // `map_triage::triage_format` both argue the opposite and it holds here with the most at
        // stake: a grammar with nowhere to put *I do not know* does not stop the model not knowing,
        // it makes the model spell it as one of the forty slugs — arriving well-formed and
        // indistinguishable from a real proposal, which is the one output this module cannot use.
        let format = anchor_format();

        assert_eq!(format["properties"]["spec"]["type"], "string");
        assert!(
            format["properties"]["spec"].get("enum").is_none(),
            "a constrained sampler would manufacture the guess rather than prevent it"
        );
        assert!(
            format["properties"]["why"].get("minLength").is_none(),
            "a sentence forced out of a model that had nothing to say is a filled field and an \
             empty thought"
        );
    }

    #[test]
    fn the_catalogue_is_read_from_where_this_project_keeps_its_documents() {
        // Through `specs_in` and never a walk of its own, so that *where a project keeps its specs*
        // has one answer in this crate. It is also why this repository yields 43 documents and not
        // the 40 the design counts: `docs/specs/` and `docs/superpowers/specs/` are read too.
        let root = std::env::temp_dir().join(format!("nucleos-anchor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ai/specs")).expect("scratch");
        std::fs::write(
            root.join(".ai/specs/2026-01-01-alfa-design.md"),
            "# Alfa (design)\n\n## 3. Beta\n",
        )
        .expect("write");

        let catalogue = catalogue(&root);

        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue[0].slug, "2026-01-01-alfa-design");
        assert_eq!(catalogue[0].title, "Alfa (design)");
        assert!(catalogue[0].sections.contains("3"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_five_outcomes_are_counted_apart_and_never_summed() {
        // Two of the five annotate and three do not, and the three that do not are fixed in three
        // different places — the model, the prompt, the runner. A single *not annotated* total would
        // be a number nobody could act on, and a total of the two that DO annotate would become the
        // number everyone quotes, at which point the gaps stop being read. Neither total exists,
        // and this is the assertion that keeps it that way.
        let mixed = asked(
            "core/src/map_join.rs",
            "//! §8 and §6.4 together.
",
        );
        let clean = asked(
            "core/src/map_store.rs",
            "//! §9.2, and the shape it keeps.
",
        );
        let proposal = |slug: &str| format!("{{\"spec\":\"{slug}\",\"why\":\"a reason\"}}");

        let counts = tally(&[
            answered(&clean, &proposal(MAP)),
            answered(&mixed, &proposal(MAP)),
            answered(&mixed, &proposal(FIXTURE)),
            answered(&mixed, "{\"spec\":\"none\",\"why\":\"cannot tell\"}"),
            answered(&mixed, "an apology, and no JSON"),
        ]);

        assert_eq!(
            counts,
            AnchorCounts {
                files: 5,
                declares: 1,
                declares_with_gaps: 1,
                abstained: 1,
                no_such_document: 1,
                unreadable: 1,
                needing_overrides: 1,
            }
        );
    }

    #[test]
    fn exposes_current_anchor_prompt_version() {
        // Pinned the way `map_triage::exposes_current_triage_prompt_version` pins its own, and for
        // the reason that test gives: a constant that moves without anybody narrating what moved is
        // a constant nobody can read back. This is version 1, the first question this module has
        // asked, so there is nothing yet to narrate.
        //
        // **What it costs to bump is different from next door, and cheaper.** A triage bump goes
        // stale on every judgement in every project at once. This one invalidates nothing, because
        // nothing stores it: it is provenance, carried in the run's report and the sweep's commit
        // message rather than in the 209 headers themselves. See [`ANCHOR_PROMPT_VERSION`] for why
        // that is the right place for it.

        assert_eq!(ANCHOR_PROMPT_VERSION, 1);
    }
    /// A scratch tree, keyed by process the way `project_map`'s own fixtures are keyed.
    fn scratch(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("nucleos-sweep-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        root
    }

    fn write(root: &std::path::Path, relative: &str, body: &str) {
        let full = root.join(relative);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
        std::fs::write(full, body).expect("write");
    }

    /// A scratch project holding exactly one document, named by [`FIXTURE`].
    ///
    /// The fictional slug and not a real one, for the reason `FIXTURE` gives: this file's own text
    /// is walked by the map, and a fixture naming a document this project has would hand these
    /// paragraphs' citations to it. The sections the fixtures below cite are ones this file already
    /// cites, for the other half of the same reason — a new number here would put this module into
    /// the anchor set of every document that has one.
    fn one_document_project(name: &str) -> std::path::PathBuf {
        let root = scratch(name);
        write(
            &root,
            &format!(".ai/specs/{FIXTURE}.md"),
            "# Documento de fixture\n\n## 7. Sete\n",
        );
        root
    }

    #[tokio::test]
    async fn a_sweep_puts_every_file_it_was_handed_into_exactly_one_of_its_three_lists() {
        // The under-report this slice is expected to be misread as. A file that fell out of the
        // report because it was never a question, and a file that fell out because its run died,
        // are indistinguishable from a file the model declined — three different facts, fixed in
        // three different places.
        let root = one_document_project("three-lists");
        write(&root, "core/src/asked.rs", "//! what §7 asks for\n");
        // Nothing would inherit: the one citation already names its own document.
        write(
            &root,
            "core/src/settled.rs",
            &format!("//! §7 {FIXTURE} — already said\n"),
        );
        let files = [
            "core/src/asked.rs".to_owned(),
            "core/src/settled.rs".to_owned(),
            // Not on disk at all, which reads as an empty file and therefore as nothing to inherit.
            "core/src/vanished.rs".to_owned(),
        ];
        let specs = catalogue(&root);
        let runner = fake_answering(&format!(
            "{{\"spec\":\"{FIXTURE}\",\"why\":\"the module comment names it\"}}"
        ));

        let swept = sweep(
            crate::map_intent::Extractor::Cli(&runner),
            &root,
            &files,
            &specs,
        )
        .await;

        assert_eq!(swept.verdicts.len(), 1);
        assert_eq!(swept.verdicts[0].file, "core/src/asked.rs");
        assert_eq!(swept.verdicts[0].outcome, Outcome::Declares);
        assert_eq!(
            swept.skipped,
            vec![
                (
                    "core/src/settled.rs".to_owned(),
                    Skipped::NothingWouldInherit
                ),
                (
                    "core/src/vanished.rs".to_owned(),
                    Skipped::NothingWouldInherit
                ),
            ],
            "a file this walk could not open names no section it could see, which is nobody's \
             model failing"
        );
        assert!(swept.failed.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_run_that_never_happened_is_kept_apart_from_an_answer_nobody_could_read() {
        // `map_intent::extract` refuses to collapse an empty list into an error, and this is that
        // refusal one level up: *the CLI exited non-zero* sends a reader to the machine, and *the
        // model put a paragraph where the slug goes* sends them to the prompt. A sweep reporting
        // both as `Outcome::Unreadable` would say a model failed to answer a question nobody asked
        // it.
        let root = one_document_project("failed-run");
        write(&root, "core/src/asked.rs", "//! what §7 asks for\n");
        let files = ["core/src/asked.rs".to_owned()];
        let specs = catalogue(&root);
        // `fail_times` is a countdown, and one call is all this sweep makes.
        let runner = crate::runner::FakeCommandRunner {
            fail_times: std::sync::Mutex::new(1),
            ..Default::default()
        };

        let swept = sweep(
            crate::map_intent::Extractor::Cli(&runner),
            &root,
            &files,
            &specs,
        )
        .await;

        assert!(swept.verdicts.is_empty(), "nobody was asked anything");
        assert!(
            swept.skipped.is_empty(),
            "and it was a perfectly good question"
        );
        assert_eq!(swept.failed.len(), 1);
        assert_eq!(swept.failed[0].0, "core/src/asked.rs");

        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------------------------
    // The harness: this module's caller, and the ground truth it is judged against.
    // -----------------------------------------------------------------------------------------

    /// The thirty file/document pairs whose right answer was written down **before any model ran**.
    ///
    /// **This table is the whole safety argument of the slice, and its value comes entirely from
    /// when it was written.** With the veto withdrawn there is no mechanical check on whether a
    /// proposal is right, so a hit rate computed from memory at the moment somebody wants it to
    /// pass is exactly the measurement this feature exists to distrust. The answers were fixed in
    /// advance — proposed from each module's subject, then **refuted** where possible against the
    /// candidate document's headings — and anything that stayed ambiguous was dropped rather than
    /// guessed: `voice.rs` has two candidate voice documents and `team.rs` has two team documents,
    /// so neither is here.
    ///
    /// **All thirty are the denominator, and not the twenty-one that pass the withdrawn veto.** The
    /// last nine — from `map_join.rs` down — are the pairs arithmetic refused, and they are the
    /// more interesting half: a file a veto refused is still a file whose right answer is known,
    /// and it is precisely where the gate cost real answers.
    ///
    /// **30/30 applies and anything less stops**, because a wrong answer here is worth roughly
    /// seven wrong files across the two hundred that cite anything, and a wrong slug manufactures a
    /// false [`crate::map_join::Anchor::Declared`] — the one state this map may present as
    /// confirmed. Whoever loosens that has removed the only thing standing between this module and
    /// §1's failure.
    const GROUND_TRUTH: &[(&str, &str)] = &[
        ("core/src/email.rs", "2026-07-28-email-pillar-design"),
        ("core/src/browser.rs", "2026-08-15-pilar-de-browser-design"),
        (
            "core/src/browser_policy.rs",
            "2026-08-15-pilar-de-browser-design",
        ),
        (
            "core/src/browser_wheel.rs",
            "2026-08-15-pilar-de-browser-design",
        ),
        ("core/src/web.rs", "2026-08-01-pilar-de-web-design"),
        ("core/src/web_client.rs", "2026-08-01-pilar-de-web-design"),
        ("core/src/vcs.rs", "2026-08-02-fila-vcs-design"),
        ("core/src/git_exec.rs", "2026-08-02-fila-vcs-design"),
        ("core/src/github.rs", "2026-08-19-modulo-de-github-design"),
        ("core/src/job.rs", "2026-07-29-autopilot-job-graph-design"),
        (
            "core/src/contacts.rs",
            "2026-07-29-correspondent-contacts-design",
        ),
        ("core/src/council.rs", "2026-08-11-council-design"),
        (
            "core/src/errands.rs",
            "2026-08-15-assuntos-fora-de-codigo-design",
        ),
        (
            "core/src/map_intent.rs",
            "2026-08-24-mapa-do-projeto-design",
        ),
        ("core/src/map_store.rs", "2026-08-24-mapa-do-projeto-design"),
        ("core/src/map_stamp.rs", "2026-08-24-mapa-do-projeto-design"),
        (
            "shell/src/project/Triagem.tsx",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "shell/src/project/Juncao.tsx",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "shell/src/project/Carimbos.tsx",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "shell/src/project/ModeMapa.tsx",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "shell/src/canvas/map-model.ts",
            "2026-08-24-mapa-do-projeto-design",
        ),
        ("core/src/map_join.rs", "2026-08-24-mapa-do-projeto-design"),
        (
            "core/src/map_recency.rs",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "core/src/map_anchor.rs",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "core/src/map_triage.rs",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "core/src/project_map.rs",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "shell/src/data/project-map.ts",
            "2026-08-24-mapa-do-projeto-design",
        ),
        (
            "core/src/workflow_graph.rs",
            "2026-08-24-motor-de-workflows-design",
        ),
        (
            "core/src/workflows.rs",
            "2026-08-24-motor-de-workflows-design",
        ),
        (
            "shell/src/canvas/workflow-model.ts",
            "2026-08-24-motor-de-workflows-design",
        ),
    ];

    #[test]
    fn the_ground_truth_names_thirty_pairs_and_no_document_this_project_lacks() {
        // Runs in the ordinary suite, unlike the two harness tests below, because it is the half of
        // the ground truth that costs nothing to check. A pair naming a document this project does
        // not have would score every run against a slug no model could ever answer, and would read
        // as the model being wrong about a file the TABLE is wrong about — which is the one way a
        // ground truth can quietly stop being one.
        assert_eq!(GROUND_TRUTH.len(), 30);

        let root = repository_root();
        let slugs: BTreeSet<String> = catalogue(&root).into_iter().map(|spec| spec.slug).collect();
        for (file, slug) in GROUND_TRUTH {
            assert!(
                root.join(file).is_file(),
                "the ground truth names {file}, which is not in this checkout"
            );
            assert!(
                slugs.contains(*slug),
                "the ground truth scores {file} against {slug}, which this project does not have"
            );
        }
    }

    /// The repository this measurement is about.
    ///
    /// Derived from the crate rather than from the working directory, because `cargo test` runs
    /// with the crate as its cwd — and a harness answering about `core/` would sweep a third of the
    /// files and report a whole number.
    fn repository_root() -> std::path::PathBuf {
        std::env::var("NUCLEOS_ANCHOR_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("the crate sits inside the repository")
                    .to_path_buf()
            })
    }

    /// A cloud brain, one per lane.
    ///
    /// One runner per lane rather than one shared: the struct is three strings, and a lane whose
    /// future owns everything it borrows is a lane that can be spawned.
    fn brain() -> crate::runner::ClaudeCliRunner {
        crate::runner::ClaudeCliRunner {
            model: std::env::var("NUCLEOS_ANCHOR_MODEL").unwrap_or_else(|_| "sonnet".to_owned()),
            plan_model: None,
            review_model: None,
        }
    }

    /// How many model calls a run keeps in flight.
    ///
    /// **A spending decision, and therefore not [`sweep`]'s.** Eight is what a cloud CLI on this
    /// machine takes without the five-hour window complaining; a local brain would want one. The
    /// env var is what makes it somebody's choice rather than this file's.
    fn lanes() -> usize {
        std::env::var("NUCLEOS_ANCHOR_LANES")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|lanes| *lanes > 0)
            .unwrap_or(8)
    }

    /// Shard the list across lanes, sweep each lane, and put the answers back into one order.
    ///
    /// **Strided rather than chunked**, so the lane that draws `http.rs` is not also the lane that
    /// draws every other large file beside it: the list is sorted by path, and a contiguous chunk
    /// of it is a contiguous folder.
    ///
    /// Sorted by file at the end, because the report is read as a diff and an order the scheduler
    /// chose changes between two runs over an unchanged repository.
    async fn sweep_in_lanes(root: &std::path::Path, files: &[String], specs: &[Spec]) -> Sweep {
        let lanes = lanes();
        let mut running = tokio::task::JoinSet::new();
        for lane in 0..lanes {
            let root = root.to_path_buf();
            let specs = specs.to_vec();
            let mine: Vec<String> = files.iter().skip(lane).step_by(lanes).cloned().collect();
            running.spawn(async move {
                let runner = brain();
                sweep(
                    crate::map_intent::Extractor::Cli(&runner),
                    &root,
                    &mine,
                    &specs,
                )
                .await
            });
        }

        let mut whole = Sweep::default();
        while let Some(lane) = running.join_next().await {
            let lane = lane.expect("a lane panicked");
            whole.verdicts.extend(lane.verdicts);
            whole.skipped.extend(lane.skipped);
            whole.failed.extend(lane.failed);
        }
        whole
            .verdicts
            .sort_by(|left, right| left.file.cmp(&right.file));
        whole.skipped.sort_by(|left, right| left.0.cmp(&right.0));
        whole.failed.sort_by(|left, right| left.0.cmp(&right.0));
        whole
    }

    /// Where a run writes itself down, and it refuses to start without one.
    ///
    /// No default path, deliberately: a measurement that quietly wrote its report somewhere is a
    /// measurement whose stale copy somebody reads six months later.
    fn report_path() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var("NUCLEOS_ANCHOR_OUT")
                .expect("NUCLEOS_ANCHOR_OUT must name where the proposal is written"),
        )
    }

    /// One verdict as the report carries it.
    fn row(verdict: &Verdict) -> serde_json::Value {
        serde_json::json!({
            "file": verdict.file,
            "proposed": verdict.proposed,
            "outcome": verdict.outcome,
            "unaccounted": verdict.unaccounted,
            "needs_override": verdict.needs_override,
            "why": verdict.why,
        })
    }

    /// The header every report carries, so a header found to be wrong in six months is traceable to
    /// the question that produced it.
    ///
    /// [`ANCHOR_PROMPT_VERSION`]'s own doc comment is the argument: the version is deliberately not
    /// written into the two hundred source files, so the run's record is the only place it exists
    /// at all. A report without it is a list of slugs nobody can attribute to a question.
    fn provenance(swept: &Sweep, files: usize) -> serde_json::Value {
        let counts = tally(&swept.verdicts);
        let sections: usize = swept
            .verdicts
            .iter()
            .map(|verdict| verdict.needs_override.len())
            .sum();
        serde_json::json!({
            "anchor_prompt_version": ANCHOR_PROMPT_VERSION,
            "run_at": chrono::Utc::now().to_rfc3339(),
            "model": brain().model,
            "lanes": lanes(),
            "root": repository_root().to_string_lossy(),
            "files_walked": files,
            "counts": counts,
            "skipped": swept.skipped.len(),
            "failed": swept.failed.len(),
            "override_backlog": {"files": counts.needing_overrides, "sections": sections},
        })
    }

    /// The report's two lists of files nobody proposed anything for, shaped for JSON.
    ///
    /// Named apart in the file as they are named apart in [`Sweep`], because collapsing them is
    /// exactly what that type exists to prevent.
    fn unasked(swept: &Sweep) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        (
            swept
                .skipped
                .iter()
                .map(|(file, why)| serde_json::json!({"file": file, "why": format!("{why:?}")}))
                .collect(),
            swept
                .failed
                .iter()
                .map(|(file, error)| serde_json::json!({"file": file, "error": error}))
                .collect(),
        )
    }

    /// Score the thirty pairs whose answer was known before any model ran.
    ///
    /// **This is the gate, and it is a separate run from the sweep rather than a step inside it.**
    /// `#[ignore]` for `runner`'s reason — it needs the Claude Code CLI installed, an authenticated
    /// session, and about a dollar of somebody's money — and it is a measurement rather than a
    /// property, so it asserts the one thing the slice turns on: thirty out of thirty, or nobody
    /// applies anything.
    ///
    /// It prints every miss with the model's own sentence before it asserts, because **which file
    /// it got wrong matters far more than how many**: a miss on `browser_policy.rs` is a model
    /// confusing two documents about one pillar, and a miss on `project_map.rs` is the IDF spike's
    /// failure arriving through a different door.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns the real Claude CLI once per file and spends money; run with --include-ignored"]
    async fn the_ground_truth_is_scored_before_any_sweep_is_run() {
        let root = repository_root();
        let specs = catalogue(&root);
        let files: Vec<String> = GROUND_TRUTH
            .iter()
            .map(|(file, _)| (*file).to_owned())
            .collect();

        let swept = sweep_in_lanes(&root, &files, &specs).await;

        let answered: std::collections::BTreeMap<&str, &Verdict> = swept
            .verdicts
            .iter()
            .map(|verdict| (verdict.file.as_str(), verdict))
            .collect();
        let mut hits = 0usize;
        let mut pairs = Vec::new();
        for (file, expected) in GROUND_TRUTH {
            let verdict = answered.get(file).copied();
            let proposed = verdict.and_then(|verdict| verdict.proposed.as_deref());
            let hit = proposed == Some(*expected);
            if hit {
                hits += 1;
            } else {
                println!(
                    "MISS {file}\n  expected {expected}\n  proposed {}\n  because  {}",
                    proposed.unwrap_or("(nothing)"),
                    verdict.map_or("(this file was never asked about)", |verdict| verdict
                        .why
                        .as_str()),
                );
            }
            pairs.push(serde_json::json!({
                "file": file,
                "expected": expected,
                "hit": hit,
                "verdict": verdict.map(row),
            }));
        }

        let (skipped, failed) = unasked(&swept);
        let report = serde_json::json!({
            "provenance": provenance(&swept, files.len()),
            "hit_rate": {"hits": hits, "of": GROUND_TRUTH.len()},
            "pairs": pairs,
            "skipped": skipped,
            "failed": failed,
        });
        std::fs::write(
            report_path(),
            serde_json::to_string_pretty(&report).expect("the report serialises"),
        )
        .expect("the report is written");

        println!("ground truth: {hits}/{}", GROUND_TRUTH.len());
        assert_eq!(
            hits,
            GROUND_TRUTH.len(),
            "anything short of thirty stops the sweep and reports the misses BY NAME — the lines \
             above are that report"
        );
    }

    /// Every `#`-headed line of every document this project has, as a decision nobody approved.
    ///
    /// **A stand-in, named as one, and deliberately the same one Task 1 of this slice measured
    /// against.** The product's `map_decisions` table is EMPTY — §4 reserves approval to the owner,
    /// line by line, and that is the whole point of the layer — so [`crate::map_join::join`]
    /// against the real intention layer answers all zeros and discriminates nothing. A number
    /// reported without naming the intention layer it was measured against is reporting nothing, so
    /// the layer is built here in code rather than described in a report: two runs a month apart
    /// are then comparable, which is exactly what a before-and-after is for.
    ///
    /// It is **not** what the product will hold, and the difference is worth stating: a real
    /// extraction proposes a handful of decisions per document, and this proposes every heading,
    /// which is roughly ten times as many and includes headings no owner would ever approve. What
    /// it buys is that it is derived from the repository alone — reproducible by anybody, on any
    /// checkout, without a database.
    ///
    /// `kind`, `brain` and `approved_at` are filled with the same value for every row because
    /// [`crate::map_join::join`] reads none of them; inventing variety there would be detail the
    /// stand-in does not have, dressed up as detail it does.
    fn pseudo_decisions(root: &Path) -> Vec<crate::map_store::Decision> {
        let mut decisions = Vec::new();
        for relative in crate::map_intent::specs_in(root) {
            let Ok(source) = std::fs::read_to_string(root.join(&relative)) else {
                continue;
            };
            let slug = crate::map_intent::spec_slug(&relative);
            // `starts_with` on the untrimmed line, and the two lines of difference are themselves
            // a measurement. Trimming first finds two more `#` lines across these 43 documents —
            // 1244 rather than 1242 — and both are indented, which in Markdown puts them inside a
            // code block and makes them not headings at all. It would also quietly stop this being
            // the SAME stand-in Task 1 took the baseline with, which is the one property a
            // before-and-after has to have.
            for line in source.lines().filter(|line| line.starts_with('#')) {
                decisions.push(crate::map_store::Decision {
                    id: decisions.len() as i64 + 1,
                    spec_slug: slug.clone(),
                    // The heading verbatim, because that is what an approved decision carries:
                    // `Anchored::section` is the string the owner said yes to, and the number is an
                    // internal step `join` takes off it.
                    section: line.trim().to_owned(),
                    ordinal: decisions.len() as i64 + 1,
                    text: line.trim().to_owned(),
                    kind: crate::map_intent::Kind::Character,
                    brain: "stand-in".to_owned(),
                    extracted_at: "2026-08-24T00:00:00Z".to_owned(),
                    approved_at: Some("2026-08-24T00:00:00Z".to_owned()),
                });
            }
        }
        decisions
    }

    /// The numbers this slice is judged by, measured through the stand-in above.
    ///
    /// **Here rather than in `map_join`'s tests, and the reason is what it is.** `join`'s tests
    /// assert properties of `join`; this asserts nothing at all. It is the run's record — the
    /// before-and-after the plan asks for, taken through the shipped `structure` + `join` +
    /// `map_recency::order` rather than through a script that would be a second answer to the same
    /// question and would drift from the product on the first change to either.
    ///
    /// `#[ignore]` because it reads the whole of a particular repository and shells out to git, so
    /// its answer is a fact about a checkout rather than about this code. It spends no money, which
    /// is the one way it differs from the two harness runs above.
    ///
    /// **`StampCounts::guessed` — the plan's first scoreboard number — is not here, and cannot
    /// be.** It counts *stamps*, `map_stamps` is as empty as `map_decisions`, and
    /// `map_stamp::standing` answers `Never` for a decision nobody has stamped — so the honest
    /// value today is zero, and the plan's *"every anchored stamp"* describes a state this machine
    /// has never been in. What stands in for it under this layer is the `ambiguous` count: a
    /// stamped `Ambiguous` decision is precisely what `Watch::Guessed` marks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "reads a whole real repository and shells out to git; run with --include-ignored"]
    async fn the_scoreboard_is_measured_through_the_stand_in_intention_layer() {
        let root = repository_root();
        let decisions = pseudo_decisions(&root);
        let slugs: Vec<String> = crate::map_intent::specs_in(&root)
            .iter()
            .map(|relative| crate::map_intent::spec_slug(relative))
            .collect();
        let structure = crate::project_map::structure(&root).expect("the walk reads this tree");

        let mut junction =
            crate::map_join::join(&decisions, &structure.modules, &structure.foreign, &slugs);
        let walked = crate::map_recency::walk(&root).await;
        let recency = crate::map_recency::order(&mut junction.decisions, &walked);

        let moved: BTreeSet<i64> = recency
            .ages
            .values()
            .filter_map(|age| match age {
                crate::map_recency::Age::Moved { at } => Some(*at),
                _ => None,
            })
            .collect();

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "intention_layer": {
                    "kind": "stand-in: every `#`-headed line of every document specs_in finds",
                    "documents": slugs.len(),
                    "pseudo_decisions": decisions.len(),
                },
                "counts": junction.counts,
                "unclaimed": junction.unclaimed.len(),
                "unmatched": junction.unmatched.len(),
                "recency": {
                    "window_commits": recency.window,
                    "decisions_inside_the_window": recency
                        .ages
                        .values()
                        .filter(|age| matches!(age, crate::map_recency::Age::Moved { .. }))
                        .count(),
                    "distinct_timestamps": moved.len(),
                },
            }))
            .expect("the scoreboard serialises")
        );
    }
    /// Ask about every file this map reads a `§` out of, and write the proposal down.
    ///
    /// **Only ever run once the gate above has passed.** Nothing here enforces that and nothing
    /// could: the two are separate runs against a repository that changes between them, so a check
    /// in here would be a check on a stale answer. What enforces it is that the gate is a test, and
    /// a test that fails is a run somebody has to look at.
    ///
    /// The list is [`crate::project_map::citing_files`]'s and **not** `structure().modules` — see
    /// that function for the two groups a sweep over the modules alone loses in silence.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns the real Claude CLI once per file and spends money; run with --include-ignored"]
    async fn the_repository_is_swept_and_the_proposal_is_written_down() {
        let root = repository_root();
        let specs = catalogue(&root);
        let files = crate::project_map::citing_files(&root).expect("the walk reads this tree");

        let swept = sweep_in_lanes(&root, &files, &specs).await;

        let (skipped, failed) = unasked(&swept);
        let header = provenance(&swept, files.len());
        let report = serde_json::json!({
            "provenance": header,
            "files": swept.verdicts.iter().map(row).collect::<Vec<_>>(),
            "skipped": skipped,
            "failed": failed,
        });
        std::fs::write(
            report_path(),
            serde_json::to_string_pretty(&report).expect("the report serialises"),
        )
        .expect("the report is written");

        println!(
            "{}",
            serde_json::to_string_pretty(&report["provenance"]).expect("the header serialises")
        );
        assert_eq!(
            swept.verdicts.len() + swept.skipped.len() + swept.failed.len(),
            files.len(),
            "every file handed in has to come back in exactly one of the three lists"
        );
    }
}
