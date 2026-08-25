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
}
