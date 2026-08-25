//! The intention layer of a project's map: what a spec decided, one line at a time.
//!
//! **Pure, and that is what makes the hard part testable.** The prompt and the parse are where the
//! design of this slice actually lives, and a function that needs a model running to be exercised
//! is a function nobody exercises. Nothing here knows what SQL is, what HTTP is, or which model
//! answered — it takes text and returns data. `map_store.rs` holds the rows, `http.rs` holds the
//! routes, and the runner that answers is chosen by whoever calls.
//!
//! **The map does not read a decision table; it provokes one.** Two of this repository's 38 specs
//! carry a numbered decision table, so reading one is not a strategy. A model reads the document
//! and proposes; the owner approves line by line; nothing reaches the map unapproved — §4.
//!
//! **The model compresses, and never certifies.** This is a model reading a document a model
//! helped write, and that objection is fair. What answers it is the shape of the product: twelve
//! numbered lines are read in two minutes, and the step that does not happen today — the owner
//! looking — starts happening. Nothing here decides anything; it makes a thousand lines small
//! enough that human attention fits again.

use serde::{Deserialize, Serialize};

/// What kind of decision this is, and therefore what it will one day ask of the owner (§4.1).
///
/// **Two variants, and the missing third is the point.** §4.1 names three kinds. Type A — about
/// scope, process, or the document itself — can be implemented by no code and asks nothing of
/// anybody, so it is dropped when the extraction is approved and never stored. Giving it a variant
/// here would make it a row this table can hold, and the whole argument for dropping it is that it
/// cannot be one.
///
/// **Two wire forms on purpose, and they are not interchangeable.** `as_str`/`from_wire` are the
/// STORAGE form — `b` and `c` — which is what the `map_decisions.kind` column holds and what its
/// `CHECK` constrains it to. The derived `Serialize`/`Deserialize` are the JSON form —
/// `countable` and `character` — which is what the window reads, because a list meant to be
/// approved at a glance cannot label its two kinds `b` and `c`. Both are pinned by tests below,
/// so neither can be renamed into agreement with the other by accident: a `Kind` serialised into
/// the column would fail its `CHECK`, and a column value handed to serde would not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Names a number, a set, or a coverage claim — something that can later be counted and agreed
    /// or disagreed with. Asks the owner nothing while it holds, and reaches them when it breaks.
    Countable,
    /// About character: what a thing is, or is not. No count decides it, so total coverage and the
    /// wrong thing are compatible. This is the kind that only a stamp can answer.
    Character,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Countable => "b",
            Self::Character => "c",
        }
    }

    /// `b` or `c`, and nothing else.
    ///
    /// Returns `None` rather than falling back the way [`crate::chats::Brain::from_wire`] does, and
    /// the difference is deliberate: a brain nobody chose has a safe default, and a decision whose
    /// kind nobody could read has none. Guessing `c` would put a stamp request in front of the
    /// owner for something no stamp was owed on; guessing `b` would silence something that needed
    /// looking at. Dropping the line is the only answer that claims nothing.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "b" => Some(Self::Countable),
            "c" => Some(Self::Character),
            _ => None,
        }
    }
}

/// How much of a document is sent. Beyond this it is cut at a line boundary and the cut is stated.
///
/// Not a token count, because this module does not know which model will answer and the two runners
/// it feeds count differently. Bytes are the honest unit for a limit whose only job is to keep one
/// document from being larger than any of them.
pub const MAX_SPEC_BYTES: usize = 60_000;

/// The document, cut at a line if it must be, and whether it was.
fn bounded(source: &str) -> (&str, bool) {
    if source.len() <= MAX_SPEC_BYTES {
        return (source, false);
    }
    // Backed off to a character boundary before slicing at all. These specs are Portuguese, so
    // byte 60_000 lands inside a `ç`, an `ã` or a `§` often enough, and `&source[..60_000]`
    // panics there rather than truncating. A daemon that died because a document happened to be
    // the wrong length would be the worst shape this failure could take — invisible until the one
    // spec that triggers it, and then fatal.
    let mut ceiling = MAX_SPEC_BYTES;
    while ceiling > 0 && !source.is_char_boundary(ceiling) {
        ceiling -= 1;
    }
    let cut = source[..ceiling].rfind('\n').unwrap_or(ceiling);
    (&source[..cut], true)
}

/// What to ask a model about one spec.
///
/// **The whole design of this slice is in this string**, so it is worth saying what each rule is
/// buying. A model that invents produces an approved line nothing decided, and the owner then owes
/// a stamp on it forever — that is the failure that makes the map worse than no map. A line with no
/// section can never be anchored to code, so it sits in the list being neither true nor false. And
/// the three kinds are what decide whether this costs an afternoon a week or thirty seconds a day,
/// which is why they are spelled out rather than named.
///
/// Deliberately says nothing about the document's language. These specs are Portuguese and the
/// decisions must come back in the document's own words: a translated decision is a paraphrase, and
/// a paraphrase is exactly the thing the owner cannot check at a glance.
pub fn extraction_prompt(spec_slug: &str, source: &str) -> String {
    let (body, was_cut) = bounded(source);
    let cut_note = if was_cut {
        "\n\n[The document was truncated at a line boundary to fit. Decisions after the cut are \
         not yours to guess at.]"
    } else {
        ""
    };

    format!(
        "You are reading one design document and listing the decisions it fixes.\n\
         \n\
         A decision is something the document SETTLES about the product — a rule that code either \
         follows or does not. Three kinds exist, and only two of them belong in your list.\n\
         \n\
         1. About scope, process, or the document itself. No code can implement it. Example: \"Two \
         specs, and the page comes first.\" LEAVE THESE OUT ENTIRELY.\n\
         2. Names something countable — a number, a set, a coverage claim — so that something could \
         later count the code and agree or disagree. Example: \"agent, command, decision, fan\", \
         which is four kinds and an enum that has four. kind = \"b\".\n\
         3. About character: what a thing IS or IS NOT. No count decides it. Example: \"Code mode is \
         a review surface, not an IDE.\" kind = \"c\".\n\
         \n\
         Rules:\n\
         - One entry per decision. Never merge two into one line.\n\
         - `section` is the heading the decision came from, copied verbatim from the document, \
         including its number. If you cannot point at one heading, leave it out.\n\
         - `text` is ONE sentence saying what was decided, in the document's own language. Not a \
         summary of the section — the decision.\n\
         - Invent nothing. If the document settles four things, return four. An empty list is a \
         valid answer.\n\
         - At most 20 entries, and prefer fewer.\n\
         \n\
         Answer with JSON only, shaped exactly like this and nothing else:\n\
         {{\"decisions\":[{{\"section\":\"...\",\"text\":\"...\",\"kind\":\"b\"}}]}}\n\
         \n\
         The document is `{spec_slug}`:\n\
         \n\
         ----- BEGIN DOCUMENT -----\n\
         {body}\n\
         ----- END DOCUMENT -----{cut_note}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_survives_the_round_trip_through_the_wire() {
        assert_eq!(Kind::from_wire("b"), Some(Kind::Countable));
        assert_eq!(Kind::from_wire("c"), Some(Kind::Character));
        assert_eq!(Kind::Countable.as_str(), "b");
        assert_eq!(Kind::Character.as_str(), "c");
    }

    #[test]
    fn the_kind_that_is_not_about_the_product_has_no_wire_form_at_all() {
        // §4.1's type A is decisions about scope, process, or the document itself. It is dropped
        // at extraction and never stored, so there is deliberately no `Kind` for it: a variant
        // would be a row this table can hold, and it must not be able to.
        assert_eq!(Kind::from_wire("a"), None);
        assert_eq!(Kind::from_wire(""), None);
        assert_eq!(Kind::from_wire("countable"), None);
    }

    #[test]
    fn the_storage_form_and_the_json_form_are_different_and_both_are_pinned() {
        // Two forms on purpose — the column holds `b`/`c` under a CHECK, the window reads
        // `countable`/`character`. Pinned here because nothing else compares them, and a rename
        // on either side would otherwise be found by whichever consumer broke first.
        assert_eq!(serde_json::to_string(&Kind::Countable).unwrap(), "\"countable\"");
        assert_eq!(serde_json::to_string(&Kind::Character).unwrap(), "\"character\"");
        assert_eq!(
            serde_json::from_str::<Kind>("\"character\"").unwrap(),
            Kind::Character
        );
        // The storage form is NOT the JSON form, and this is the assertion that says so.
        assert!(serde_json::from_str::<Kind>("\"b\"").is_err());
    }

    #[test]
    fn the_prompt_carries_the_document_and_says_what_a_decision_is() {
        let prompt = extraction_prompt("2026-08-22-workspace-de-projeto-design", "## 1. Alfa\n");

        // The document itself, or the model is answering about nothing.
        assert!(prompt.contains("## 1. Alfa"));
        // The slug, so a model that answers about the wrong file is visibly answering about it.
        assert!(prompt.contains("2026-08-22-workspace-de-projeto-design"));
        // The three kinds, because §4.1 is the whole of what makes this cheap to approve.
        assert!(prompt.contains("scope, process, or the document itself"));
        assert!(prompt.contains(r#""b""#));
        assert!(prompt.contains(r#""c""#));
    }

    #[test]
    fn the_prompt_refuses_the_two_failures_that_would_cost_the_most() {
        let prompt = extraction_prompt("slug", "body");

        // Inventing is the failure that makes the map worse than no map: an approved line that
        // nothing decided becomes a decision the owner then owes a stamp on forever.
        assert!(prompt.to_lowercase().contains("invent"));
        // A line with no section can never be anchored to code in slice 3, so it is a line that
        // will sit in the list forever being neither true nor false.
        assert!(prompt.contains("leave it out"));
    }

    #[test]
    fn a_document_too_long_to_send_is_cut_at_a_line_and_says_so() {
        // Not a silent truncation: a model handed half a document with no notice answers
        // confidently about a document that does not exist.
        let long = "x".repeat(MAX_SPEC_BYTES + 500);
        let prompt = extraction_prompt("slug", &long);
        assert!(prompt.contains("truncated"));
        assert!(prompt.len() < long.len() + 4_000);
    }

    #[test]
    fn a_long_document_in_the_language_these_specs_are_written_in_does_not_panic() {
        // The test above uses ASCII, where every byte index is a character boundary. These specs
        // are Portuguese, and `&source[..60_000]` panics when byte 60_000 lands inside a `ã`.
        // Seven bytes per repeat, so it does.
        let long = "çã§x".repeat(MAX_SPEC_BYTES);
        let prompt = extraction_prompt("slug", &long);
        assert!(prompt.contains("truncated"));
        assert!(prompt.len() < long.len());
    }
}
