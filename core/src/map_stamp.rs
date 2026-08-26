//! What a stamp means.
//!
//! The verdict is the owner's, and it is the one thing in this map that no amount of reading the
//! repository can produce: structure says what the code does, the junction says which decision it
//! stands under, and neither can say whether that is what was wanted. `map_store.rs` keeps the rows
//! and knows no rules; this module keeps the rules and knows no SQL, for the same reason
//! `map_intent.rs` knows no database — §7.1 is three expiry rules that must stay three, and a rule
//! that can only be exercised through a table is a rule nobody exercises.

use serde::{Deserialize, Serialize};

/// The owner's verdict on one decision (§5.2).
///
/// **Three, and the third is not a convenience.** Without *mudei de ideias* the map nags forever
/// about something its owner abandoned, and the easy way out — deleting the line — leaves the spec
/// lying with nobody the wiser. Withdrawing is an assertion rather than a forgetting.
///
/// **Each one stops being true its own way (§7.1), and that is why this is an enum and not a
/// boolean with a note beside it.** `Settled` is the only claim that changing the code can falsify,
/// so it expires by the anchor digest moving. `Partial` expires by TIME instead, which reads
/// backwards until you see it: you already know the thing is half-done, so the code moving teaches
/// you nothing — what rots is the note, because *falta X* quietly stops being true. `Withdrawn`
/// never expires; it waits for the document to be rewritten, and leaves when the decision does.
///
/// Two wire forms, as [`crate::map_intent::Kind`] also has, and they must be read as two even
/// though they currently agree. [`Self::as_str`] and [`Self::from_wire`] are the STORAGE form, which
/// is what `map_stamps.verdict` holds and what its `CHECK` admits; the derived `Serialize` and
/// `Deserialize` are the JSON form the window speaks. That the two spell the words alike here is a
/// happy accident of English being usable in both places — `Kind` had to choose `b`/`c` for the
/// column and `countable`/`character` for the window — and not a guarantee either side may lean on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// *Está como quero.* The only green in the map, and the only verdict a model may never
    /// produce (§6). Stops being true when the anchor code changes — and a decision with no
    /// readable anchor therefore carries a green that can never expire, which §7 requires be shown
    /// rather than enjoyed.
    Settled,
    /// *A meio, e eu sei.* Amber, and the note is obligatory because the note is the whole of it:
    /// it converts a *didn't know* into a *knew*, which is half the cure. Stops being true by time.
    Partial,
    /// *Mudei de ideias.* The decision is old, not the code. Stays retired with the spec marked as
    /// needing an update, so it stops nagging without disappearing in silence, and never expires on
    /// its own.
    Withdrawn,
}

impl Verdict {
    /// The storage form: what `map_stamps.verdict` holds, and exactly what its `CHECK` admits.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::Partial => "partial",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// The three, and nothing else.
    ///
    /// `Option` rather than a fallback, following [`crate::map_intent::Kind::from_wire`] and not
    /// [`crate::chats::Brain::from_wire`]. A brain nobody can parse has a safe default to fall to;
    /// a verdict does not, because every one of the three is a claim about what the owner said.
    /// Guessing `Settled` would invent a green nobody gave, and guessing either of the others would
    /// put words in their mouth about a decision they may have been perfectly happy with. A stamp
    /// that cannot be read is therefore no stamp, which lands the decision back among the ones
    /// nobody has looked at — visible debt, and the one answer that claims nothing.
    ///
    /// Named after the pair in `map_intent` rather than `FromStr`, because the codebase already has
    /// two of these and a third spelling would make the reader check which is which.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "settled" => Some(Self::Settled),
            "partial" => Some(Self::Partial),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}
