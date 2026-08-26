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
//! Today this repository is entirely the former — 204 of its citations are followed by that
//! shape, and exactly one, `§8.4 approval-pause` in `runs.rs`, is even slug-*looking*, with
//! `approval-pause` an English hyphenated phrase rather than a document. Checking a candidate
//! against the project's real spec slugs is the join's job, and refusing to guess here is what
//! keeps the join's rejection worth anything.
//!
//! **Deliberately approximate, and visibly so.** A `§` inside a string literal counts, and
//! sections sort lexically, so `§10` comes before `§2`. Both are errors a reader sees in the
//! answer itself. The error this module refuses is the silent one — a citation confidently tied
//! to the wrong document — which is why nothing here decides anything.

// This is a bin-only crate, so dead-code reachability starts at `main`, and nothing reaches here
// yet: this task builds the lexical half of the junction and the half that calls it — matching a
// citation against the approved decisions and their spec slugs — is the next one. With the line
// below removed the compiler names all five items and nothing else, so one line here says what
// five scattered `#[allow]` attributes would. The instruction, not a description: DELETE THIS LINE
// with the change that gives `citations` and `section_number` a production caller.
//
// Scoped to the non-test build, the way `errands.rs` and `contacts.rs` scope theirs, so it silences
// only the absence of that caller. Under `cfg(test)` the lint stays live — every item below is
// exercised by this module's tests, and one that stops being exercised has to say so.
#![cfg_attr(not(test), allow(dead_code))]

use serde::Serialize;
use std::collections::BTreeSet;

/// One `§` reference found in a source file.
///
/// Ordering is lexical rather than numeric, because the derived `Ord` compares `section` as the
/// string it is: `§10` sorts before `§2`. Reading order is not what the set is for — determinism
/// is — and every numeric alternative has to first answer what `§5.3a` is worth as a number. A
/// list a reader can re-sort beats a comparison that quietly disagrees with the document.
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
/// Digits, then any number of `.digits` groups, then at most one lowercase letter — and the
/// letter only where a digit just ended, which is what keeps `§4.a` from reading as `4.a`.
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
        // `§5.3a`, `§6c`, `§4.4a` — one letter, never two, and the loop ends on it either way.
        if character.is_ascii_lowercase() && after_digit {
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
/// One space, then a run of `[a-z0-9-]`. Both halves are enforced by the same two lines: a
/// second space, an uppercase letter, or an apostrophe all leave the run empty, and an empty run
/// is not a candidate. That is deliberate — the alternative is a rule per punctuation mark, and
/// each of those is a place where prose could be mistaken for a slug.
///
/// The run stops where the shape stops, so `§7 rule.` offers `rule` and keeps the full stop out
/// of it. Whether `rule` is a document is a question for the join, which has the list.
fn candidate(rest: &str) -> Option<String> {
    let after = rest.strip_prefix(' ')?;
    let word: String = after
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if word.is_empty() { None } else { Some(word) }
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
    let text = heading.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
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
        assert_eq!(citations(source).len(), 1);

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
