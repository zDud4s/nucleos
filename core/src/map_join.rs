//! The lexical half of the junction between what a spec decided and what the code implements:
//! which sections a source file names.
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
//! spec slugs is the join's job, and refusing to guess at this layer is what keeps the join's
//! rejection worth anything.
//!
//! **Approximate where the error is visible, silent nowhere.** A `§` inside a string literal
//! counts, and the scan above proves the cost is exactly that: a fixture quoting a citation is
//! reported as one. That error is legible in the answer. The one place the approximation is
//! *not* self-announcing is the ordering, so [`Citation`] states it outright rather than leaving
//! it to be found. What this module refuses is the silent error — a citation confidently tied to
//! the wrong document — which is why nothing here decides anything.

// This is a bin-only crate, so dead-code reachability starts at `main`, and nothing reaches here
// yet: this task builds the lexical half of the junction and the half that calls it — matching a
// citation against the approved decisions and their spec slugs — is the next one. With the line
// below removed the compiler names all five items and nothing else. Three scattered attributes
// would cover them — measured, not assumed: an `#[allow]` seeds its item as a liveness root, so
// covering `citations` and `section_number` also covers `leading_number` and `candidate`, which
// nothing but those two call, and only `Citation` needs its own, because its fields are written
// and never read outside the tests. One line beats three that have to be found and dropped
// together. The instruction, not a description: DELETE THIS LINE with the change that gives
// `citations` and `section_number` a production caller.
//
// Scoped to the non-test build so it silences only the absence of that caller. Under `cfg(test)`
// the lint stays live — every item below is exercised by this module's tests, and one that stops
// being exercised has to say so. `errands.rs:28` is the precedent for the module-wide form;
// `contacts.rs` scopes to `not(test)` the same way but hangs its attributes on individual fields,
// which is right there and wrong here, where the whole module is waiting on one caller.
#![cfg_attr(not(test), allow(dead_code))]

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
