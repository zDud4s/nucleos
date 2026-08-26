//! What a stamp means.
//!
//! The verdict is the owner's, and it is the one thing in this map that no amount of reading the
//! repository can produce: structure says what the code does, the junction says which decision it
//! stands under, and neither can say whether that is what was wanted. `map_store.rs` keeps the rows
//! and knows no rules; this module keeps the rules and knows no SQL, for the same reason
//! `map_intent.rs` knows no database — §7.1 is three expiry rules that must stay three, and a rule
//! that can only be exercised through a table is a rule nobody exercises.
//!
//! It also owns the **canonical form of the anchor digest**, even though the `git` call that
//! produces one lands with a later task and will live beside `git_exec.rs`. The form is the
//! contract between whoever writes a digest and whoever reads it back, and the two must agree byte
//! for byte or a stamp compares unequal to the very anchor set it was made from. Keeping it next to
//! the process call would file it as a detail of how this machine happens to ask git, when it is in
//! fact the thing every stored digest is bound by for as long as the row exists.

use crate::map_store::Stamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

/// How long an amber note is trusted before the map asks about it again (§7.1).
///
/// **A defended number rather than a round one**, because amber expires by time and by nothing
/// else — this constant *is* the rule. Pick it badly and the feature either nags about notes that
/// are still true or trusts notes that stopped being true months ago, and neither failure
/// announces itself.
///
/// Thirty days, from two measurements pulling opposite ways. The longest single slice of this map
/// took **5h45** — `2026-08-25 19:37` to `2026-08-26 01:22` by commit timestamp — and the whole
/// feature, from its first commit to this one, is under thirteen hours. A note that survives thirty
/// days has therefore outlived the work it described by two orders of magnitude: *falta migrar as
/// páginas de pilar* is done or abandoned long before the window closes, and in both cases the note
/// has quietly become a lie, which is the exact rot §7.1 says amber expires for. Against that, the
/// window must not be short enough to turn the map into a subscription — at thirty days a quarter
/// asks four times, which is a conversation; at seven it would ask thirteen, which is a chore, and
/// §10 says a chore at the door is the fastest way to kill this.
///
/// Deliberately **not** derived from anything about the decision. A per-decision lifetime would be
/// a judgement about which work rots faster, and §6 reserves judgement of that kind for the owner.
pub const NOTE_LIFETIME: chrono::Duration = chrono::Duration::days(30);

/// What became of one decision's verdict, read at one instant.
///
/// **Five, exhaustive and mutually exclusive, and a property test is what keeps them so.** Every
/// approved decision has exactly one of these, so the five counts in [`StampCounts`] are required
/// to add up to the number of decisions — the discipline [`crate::map_join::Counts`] already
/// enforces, for the same reason: a header is exactly where a reader stops checking, so a total
/// that does not reconcile is §1's false confidence reappearing inside its own cure.
///
/// **This is the verdict axis with expiry applied; it is not §5.1's derived state.** §5 refuses to
/// flatten the two — *"achatá-las numa só punha o triador e o dono a falar pela mesma boca"* — so
/// nothing here may borrow the triager's vocabulary. See [`Standing::Lapsed`], which is the variant
/// that was tempting to misname.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Standing {
    /// Nobody has stamped this decision. §5.3's `K nunca vistas`, and §10's day-one debt.
    ///
    /// **Meant to be large, and meant to stay a count rather than become a queue.** §10: *não se
    /// carimba história* — the backlog is an honest number that is supposed to be uncomfortable and
    /// is not allowed to block anything. A placeholder verdict here would make the absence
    /// something a reader has to interpret instead of something they can count.
    Never,
    /// *Está como quero*, and the anchors are as they were.
    Settled {
        stamped_at: String,
        /// `false` when this decision has no readable anchor, so the stamp can never expire.
        ///
        /// **Reported, never hidden.** A green that will never come back to ask is the precise
        /// shape of the false confidence §1 describes, and today it is the common case rather than
        /// the corner: [`crate::map_join::Anchor::Declared`] has zero instances in this repository
        /// until §8's slug edit lands. The map is allowed to carry such a stamp — the owner may
        /// well be settled about work living in a Go sidecar — and is not allowed to let it look
        /// like the other kind.
        watched: bool,
    },
    /// *A meio, e eu sei*, inside its window. The note is the whole of it (§5.2).
    Partial { stamped_at: String, note: String },
    /// A stamp that stopped being true, and what stopped it.
    ///
    /// **`Lapsed` and not `Waiting`, and the word is load-bearing.** Every variant of this enum
    /// says what became of something *the owner* said. §5.1's *à espera* says what the **triager**
    /// thinks is worth the owner's attention, and slice 5 is what builds it. Spending that word
    /// here would take a name that slice needs, and — worse — would read on screen as the model
    /// having decided this, which is exactly the authority §6 removes from it and §6.1 refuses to
    /// hand back through the rendering door. §5.3's `J à tua espera` is a number these feed; it is
    /// not a name they may wear.
    Lapsed { stamped_at: String, why: Lapse },
    /// *Mudei de ideias.* The code is fine; the document still claims something the owner abandoned.
    ///
    /// Still in the map on purpose. §5.2: *pára de te chatear **sem desaparecer em silêncio***, and
    /// *com o spec marcado por actualizar* needs a row somebody still reads — see
    /// [`crate::map_store::approved`], which spells out what the other reading would have cost.
    Withdrawn {
        stamped_at: String,
        note: Option<String>,
    },
}

/// Why a stamp lapsed, and the diff §7 promises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Lapse {
    /// The anchor code moved. Paths, sorted, in the three ways it can have moved.
    ///
    /// **Three lists rather than one, because they are three different questions to the owner.** A
    /// blob that changed asks *is this still what you wanted?*; a path that appeared asks *did you
    /// ever look at this?*, which is §1's failure verbatim and is the one nobody would have gone
    /// looking for; a path that is gone asks *was that deliberate?*. §7 says re-stamping is one
    /// click when the diff is cosmetic and the right moment to look when it is not, and that
    /// judgement is only available to somebody who can see which of the three happened.
    Moved {
        changed: Vec<String>,
        added: Vec<String>,
        gone: Vec<String>,
    },
    /// The note aged past [`NOTE_LIFETIME`]. Carries the note, because the note is what expired.
    Stale { note: String },
    /// The comparison this verdict's expiry rule needs could not be made at all.
    ///
    /// **Reported rather than resolved either way, which is the entire reason the variant exists.**
    /// The two available guesses are the two failures this feature is built to prevent: calling it
    /// unchanged mints a green nobody checked, and calling it moved nags about code that never
    /// budged. *I could not look* is the only one of the three that is true, and it is also the only
    /// one a reader can act on.
    ///
    /// Three ways to arrive here, and all three are the same fact. The expected one is a settled
    /// stamp whose **current** digest could not be computed — the folder was a git repository when
    /// it was stamped and is not one now, or `git` did not answer. The second is a settled stamp
    /// holding **no** digest, a row `0118`'s `CHECK (verdict <> 'settled' OR code_digest IS NOT
    /// NULL)` refuses at the table; it is answered here anyway because the tempting alternative,
    /// `Settled { watched: false }`, is that CHECK's own defect moved from write time to read time —
    /// a transient *git was unreadable* silently promoted to a permanent *there is nothing to
    /// watch*. The third is an amber whose `stamped_at` will not parse, so its age cannot be
    /// computed; `map_store::stamp` writes `chrono::Utc::now().to_rfc3339()` and so cannot produce
    /// one, and the honest answer to a clock that will not read is still *I could not look*.
    Unreadable,
}

/// §5.3's header, plus the two numbers §5.3 does not carry and the panel must.
///
/// **Required to add up, and a test asserts it rather than a comment promising it** —
/// `settled + partial + never + lapsed + withdrawn == decisions`, exactly as
/// [`crate::map_join::Counts`] is required to reconcile, and for the reason that comment gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StampCounts {
    /// `N carimbadas`.
    pub settled: usize,
    /// `M a meio`.
    pub partial: usize,
    /// `K nunca vistas`. Debt, and it is supposed to be uncomfortable (§5.3, §10).
    pub never: usize,
    /// Stamps that stopped being true. Feeds §5.3's `J à tua espera`.
    ///
    /// **`J` is this number today and becomes a union tomorrow.** Slice 5's triager also puts
    /// decisions in front of the owner, and those come out of [`StampCounts::never`], not out of
    /// here. Written down because a header number whose definition grows quietly between two slices
    /// is the disease this map is the cure for.
    pub lapsed: usize,
    /// Not in the header line. Withdrawn decisions still exist and their documents still lie —
    /// §5.2 wants them out of the way, not out of sight.
    pub withdrawn: usize,
    /// How many of [`StampCounts::settled`] have no anchor to watch: greens that can never expire.
    ///
    /// **The number that keeps `N carimbadas` honest.** Without it the header reports a count of
    /// greens without saying how many of them will never come back to ask, which reads as
    /// confidence and is not. Today, with `Anchor::Declared` at zero instances, this is expected to
    /// equal `settled` outright, and that is the measurement saying §8's edit has not landed.
    pub unwatched: usize,
    /// Every approved decision, so the five above can be asserted to reconcile.
    pub decisions: usize,
}

// This is a bin-only crate, so dead-code reachability starts at `main`, and nothing in production
// reaches this module yet: `standing` and `counts` are read by `GET /projects/{id}/map`, and
// `canonical` by the git reader that computes a digest at stamp time — both of which land with the
// route work in a later task. Measured rather than assumed, the way `map_store.rs` measured its
// pair: with the attributes stripped this module warns about **nine** items, and putting them back
// on these three silences all nine — `#[allow]` seeds a liveness root, so `Standing`, `Lapse`,
// `StampCounts`, `parse` and `moved` stay reachable *through* the entry points and one of them
// going unused would still say so. The instruction, not a description: DELETE ALL THREE ATTRIBUTES
// with the change that adds the route.
//
// Scoped to the non-test build, as `map_store.rs`, `contacts.rs` and `errands.rs` scope theirs.
// Under `cfg(test)` the lint stays live, and this module's tests exercise all three.

/// One decision's standing, at one instant.
///
/// **Both `Option`s are load-bearing and they mean different things.** `stamp` is `None` when
/// **nobody has stamped this decision** — §5.3's `K nunca vistas`, which on day one is nearly every
/// row. `current` is `None` when **this reading could not compute a digest just now**: the folder is
/// not a repository, `git` did not answer, the call failed. `Some("")` is a third thing again —
/// computed, and this decision has no readable anchor to watch — which is a permanent fact about the
/// decision rather than a transient one about this daemon.
///
/// **Neither may be flattened into a default, and the inner one is the dangerous one.** An
/// `unwrap_or_default()` on `current` is all it takes: every settled stamp holding a real digest
/// would then be compared against `""`, every anchor file in the project would land in
/// `Lapse::Moved { gone }` at once, and the panel would report that all of them disappeared when
/// nothing whatsoever happened. That lapse storm costs exactly what the silent green costs, from the
/// other side — a reader nagged about nothing stops reading, and the one real lapse then arrives on
/// a screen nobody looks at. `0118`'s header spends a paragraph keeping the three states apart at
/// write time; this is where that has to survive the read.
///
/// `now` is a parameter and this function never reads a clock, which is what lets §7.1's three rules
/// be exercised as three. Note which of them uses it: amber, and only amber. A settled stamp's
/// `stamped_at` is carried through untouched and never parsed, because time is not an input to its
/// expiry, and a withdrawal's for the same reason — so a corrupt timestamp cannot make a green rot
/// or a withdrawal return.
#[cfg_attr(not(test), allow(dead_code))]
pub fn standing(stamp: Option<&Stamp>, current: Option<&str>, now: DateTime<Utc>) -> Standing {
    let Some(stamp) = stamp else {
        return Standing::Never;
    };
    let stamped_at = stamp.stamped_at.clone();

    // Matched on the verdict and nothing else, so the compiler is what proves §7.1's three rules
    // are three. A boolean with a note beside it — the shape this nearly was — would have let one
    // rule quietly serve two verdicts, which is how *a meio* ends up expiring by code.
    match stamp.verdict {
        Verdict::Settled => match (stamp.code_digest.as_deref(), current) {
            (Some(was), Some(is)) => match moved(was, is) {
                Some(why) => Standing::Lapsed { stamped_at, why },
                // `is` empty here means `was` was too, since nothing moved. That is the green with
                // nothing to watch, and it is the only place `watched` can be false.
                None => Standing::Settled {
                    stamped_at,
                    watched: !is.is_empty(),
                },
            },
            (None, _) | (_, None) => Standing::Lapsed {
                stamped_at,
                why: Lapse::Unreadable,
            },
        },
        Verdict::Partial => {
            // The note is required by `0118` and defaulted here anyway, and the DIRECTION of the
            // default is the argument. An amber with no note is a row that CHECK refuses, so this is
            // unreachable while the table stands; if one ever arrives, showing amber with nothing to
            // say is visibly incomplete, whereas the other candidate — dropping it to
            // `Standing::Never`, which is what `Verdict::from_wire` argues for when the *verdict*
            // cannot be read — would state that nobody has looked at a decision somebody
            // demonstrably looked at, and would put that row in `K nunca vistas` where it does not
            // belong. A missing note loses a sentence; a wrong `Never` loses the fact that the owner
            // answered at all.
            let note = stamp.note.clone().unwrap_or_default();
            let Ok(stamped) = DateTime::parse_from_rfc3339(&stamp.stamped_at) else {
                return Standing::Lapsed {
                    stamped_at,
                    why: Lapse::Unreadable,
                };
            };
            // Strictly greater, so the note is still trusted at the instant the window closes and
            // stops being trusted a moment later. An off-by-one in the other direction nags a day
            // early, every time, forever.
            if now.signed_duration_since(stamped.with_timezone(&Utc)) > NOTE_LIFETIME {
                Standing::Lapsed {
                    stamped_at,
                    why: Lapse::Stale { note },
                }
            } else {
                Standing::Partial { stamped_at, note }
            }
        }
        // No inputs at all beyond the row itself. §7.1: it leaves when the decision is rewritten or
        // taken out of the document, and not before — so neither the clock nor the code may move it,
        // and the way to guarantee that is to never look at either.
        Verdict::Withdrawn => Standing::Withdrawn {
            stamped_at,
            note: stamp.note.clone(),
        },
    }
}

/// §5.3's header numbers, tallied from one standing per approved decision.
///
/// The caller must hand this **every** approved decision, including the ones nobody has stamped as
/// [`Standing::Never`], because `decisions` is `standings.len()` and the reconciliation is what the
/// header's honesty rests on. Counting decisions from some other source would let the five
/// categories quietly stop covering the whole — and a header is the one place that would never be
/// noticed.
#[cfg_attr(not(test), allow(dead_code))]
pub fn counts(standings: &[Standing]) -> StampCounts {
    let mut counts = StampCounts {
        settled: 0,
        partial: 0,
        never: 0,
        lapsed: 0,
        withdrawn: 0,
        unwatched: 0,
        decisions: standings.len(),
    };
    for standing in standings {
        match standing {
            Standing::Never => counts.never += 1,
            Standing::Settled { watched, .. } => {
                counts.settled += 1;
                if !watched {
                    counts.unwatched += 1;
                }
            }
            Standing::Partial { .. } => counts.partial += 1,
            Standing::Lapsed { .. } => counts.lapsed += 1,
            Standing::Withdrawn { .. } => counts.withdrawn += 1,
        }
    }
    counts
}

/// The canonical text of an anchor set: one line per file, `<blob-sha> <path>`, sorted by path,
/// joined with `\n`, no trailing newline.
///
/// **Text, and not a hash of the text.** §7 requires a lapsed decision to show *o diff entre o que
/// estava carimbado e o que está agora*, and a scalar can only ever say that something moved. For a
/// 22-file anchor set — the size of the largest `§` in this repository — this is about 1.2 KB, which
/// is cheap for a column and directly diffable by set difference.
///
/// **The hash comes first and the path last, and that ordering is what makes the form parseable at
/// all.** A path may contain spaces; a blob sha never can, and is always forty hex characters.
/// Putting the fixed-width field first lets [`parse`] split on the first space and take the entire
/// remainder as the path, with no quoting rule to get wrong. `git ls-files -s` orders its columns
/// the same way, which is convenient rather than coincidental.
///
/// The one residual, stated rather than left to be found: a path containing a **newline** would
/// split into two lines and desynchronise the whole form. Windows filenames cannot hold one, and
/// `git ls-files` C-quotes such a path rather than emitting it raw, so it cannot arrive here by the
/// route this feature uses — but a caller assembling entries some other way is the one who has to
/// keep that true.
///
/// **Sorted here rather than trusted from the caller.** git happens to answer in path order today,
/// and a stamp that lapsed because a directory walk came back differently would be a false alarm —
/// and false alarms cost precisely the trust this feature exists to earn.
///
/// One line per path: the entries collapse through a [`BTreeMap`], so a path given twice keeps the
/// last blob. Avoiding that is the caller's job — `git ls-files -s` emits three lines for a path in
/// a merge conflict, one per stage — and collapsing deterministically still beats emitting a digest
/// that would compare unequal to itself.
///
/// Joined with `\n` and never `\r\n`. This working tree is CRLF, and a helper that normalised line
/// endings on the way through would rewrite the meaning of every digest already in the table and
/// lapse every settled stamp in the project on a single read.
#[cfg_attr(not(test), allow(dead_code))]
pub fn canonical<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let sorted: BTreeMap<&str, &str> = entries.into_iter().collect();
    sorted
        .into_iter()
        .map(|(path, blob)| format!("{blob} {path}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A digest read back as `path -> blob sha`.
///
/// The inverse of [`canonical`], and tested as one, because the writer and the reader of a stored
/// digest are two different tasks and a form only one of them agrees with is a stamp that lapses
/// against itself.
///
/// A line carrying no space is dropped rather than guessed at: it names no file, so a diff has
/// nothing to say about it, and inventing an entry with an empty path would put a phantom into
/// `gone` on the next read. `str::lines` also tolerates a `\r\n` that crossed a text boundary
/// somewhere, which on this machine is a real way for a string to arrive.
///
/// The empty digest parses to an empty map, which is the whole point of `''` — *computed, and there
/// is nothing to watch* — and is a different answer from the `None` that means nothing was computed.
pub fn parse(digest: &str) -> BTreeMap<String, String> {
    digest
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(blob, path)| (path.to_owned(), blob.to_owned()))
        .collect()
}

/// What moved between the stamped anchor set and the current one, or `None` when nothing did.
///
/// **Compared as sets and not as strings.** Both texts are canonical, so equal sets give equal text
/// and a string comparison would agree with this one today. It is done this way regardless, because
/// the only way the two could disagree is a digest that reached the table by some other route — and
/// in that case a byte difference no file difference explains would lapse a stamp with an empty diff
/// beside it. A nag that cannot show its reason is worse than either answer, and §7 promises the
/// reason.
///
/// All three lists come out sorted by path, because a [`BTreeMap`] is walked in order and nothing
/// here re-orders them. §7's diff is read by a human, and a diff whose order changes between two
/// readings is one nobody can skim.
fn moved(stamped: &str, current: &str) -> Option<Lapse> {
    let was = parse(stamped);
    let is = parse(current);

    let mut changed = Vec::new();
    let mut gone = Vec::new();
    for (path, blob) in &was {
        match is.get(path) {
            Some(current_blob) if current_blob == blob => {}
            Some(_) => changed.push(path.clone()),
            None => gone.push(path.clone()),
        }
    }
    let added: Vec<String> = is
        .keys()
        .filter(|path| !was.contains_key(*path))
        .cloned()
        .collect();

    if changed.is_empty() && added.is_empty() && gone.is_empty() {
        None
    } else {
        Some(Lapse::Moved {
            changed,
            added,
            gone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// The instant every stamp below was made, in the shape `map_store::stamp` actually writes —
    /// `chrono::Utc::now().to_rfc3339()`, which spells the offset `+00:00` rather than `Z`.
    const STAMPED_AT: &str = "2026-08-26T08:00:00+00:00";

    /// Plausible git blob hashes, and one real one. `EMPTY_BLOB` is the sha1 git gives a file with
    /// no bytes in it, which is the only value that can put *a file whose content hashes to
    /// nothing* next to *no file at all* and ask whether this module tells them apart.
    const BLOB_A: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f901234567";
    const BLOB_B: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const BLOB_C: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    fn stamp_of(verdict: Verdict, code_digest: Option<&str>, note: Option<&str>) -> Stamp {
        Stamp {
            decision_id: 7,
            verdict,
            stamped_at: STAMPED_AT.to_owned(),
            code_digest: code_digest.map(str::to_owned),
            note: note.map(str::to_owned),
        }
    }

    /// An anchor set in canonical form, assembled the way task 3's git reader will assemble it.
    fn digest(entries: &[(&str, &str)]) -> String {
        canonical(entries.iter().copied())
    }

    /// `STAMPED_AT` plus an offset. The only clock these tests have, because the module has none.
    fn at(offset: Duration) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(STAMPED_AT)
            .expect("the tests' own instant parses")
            .with_timezone(&Utc)
            + offset
    }

    fn settled(watched: bool) -> Standing {
        Standing::Settled {
            stamped_at: STAMPED_AT.to_owned(),
            watched,
        }
    }

    fn lapsed(why: Lapse) -> Standing {
        Standing::Lapsed {
            stamped_at: STAMPED_AT.to_owned(),
            why,
        }
    }

    /// Named apart from the module's own `moved` on purpose: this one builds the expectation and
    /// that one computes the answer, and a test where the two share a name is a test that reads as
    /// if it were comparing a function against itself.
    fn moved_to(changed: &[&str], added: &[&str], gone: &[&str]) -> Lapse {
        let owned = |paths: &[&str]| paths.iter().map(|path| (*path).to_owned()).collect();
        Lapse::Moved {
            changed: owned(changed),
            added: owned(added),
            gone: owned(gone),
        }
    }

    /// The property that makes §5.3's header worth reading.
    ///
    /// Over a generated mix and not one hand-built case: every verdict against every state of the
    /// stamped digest, against every state of the current one, at five instants either side of the
    /// amber window — plus the unstamped decision, which on day one is nearly all of them. If any
    /// pair of rules ever overlaps, or any input falls through all five, the sum stops matching the
    /// number of decisions and this fails. That is the same discipline `map_join::Counts` enforces,
    /// for the same reason: a header is exactly where a reader stops checking, so a total that does
    /// not reconcile would be §1's false confidence reappearing inside its own cure.
    #[test]
    fn the_five_standings_always_add_up_to_every_decision() {
        let one = digest(&[("core/src/map_join.rs", BLOB_A)]);
        let two = digest(&[
            ("core/src/map_join.rs", BLOB_B),
            ("core/src/http.rs", BLOB_C),
        ]);
        let three = digest(&[("core/src/map_store.rs", BLOB_C)]);

        let digests = [None, Some(""), Some(one.as_str()), Some(two.as_str())];
        let currents = [
            None,
            Some(""),
            Some(one.as_str()),
            Some(two.as_str()),
            Some(three.as_str()),
        ];
        let notes = [None, Some("falta migrar as páginas de pilar")];
        let verdicts = [Verdict::Settled, Verdict::Partial, Verdict::Withdrawn];
        let clocks = [
            Duration::zero(),
            Duration::seconds(1),
            NOTE_LIFETIME,
            NOTE_LIFETIME + Duration::seconds(1),
            Duration::days(4000),
        ];

        let mut every = Vec::new();
        for current in currents {
            for clock in clocks {
                every.push(standing(None, current, at(clock)));
                for verdict in verdicts {
                    for stamped in digests {
                        for note in notes {
                            let stamp = stamp_of(verdict, stamped, note);
                            every.push(standing(Some(&stamp), current, at(clock)));
                        }
                    }
                }
            }
        }

        let tally = counts(&every);
        assert_eq!(tally.decisions, every.len());
        assert_eq!(
            tally.settled + tally.partial + tally.never + tally.lapsed + tally.withdrawn,
            tally.decisions,
            "the five standings must cover every decision exactly once: {tally:?}"
        );
        assert!(
            tally.unwatched <= tally.settled,
            "an unwatched stamp is a settled one, so it can never outnumber them: {tally:?}"
        );

        // A property nothing exercises is a property nobody proved. Each of the five, and the
        // unwatched green, has to actually occur in the mix above or the assertion is vacuous.
        assert!(tally.never > 0, "{tally:?}");
        assert!(tally.settled > 0, "{tally:?}");
        assert!(tally.partial > 0, "{tally:?}");
        assert!(tally.lapsed > 0, "{tally:?}");
        assert!(tally.withdrawn > 0, "{tally:?}");
        assert!(tally.unwatched > 0, "{tally:?}");
    }

    #[test]
    fn a_settled_stamp_survives_its_anchors_being_unchanged() {
        let anchors = digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&anchors), None);

        // Eleven years on, because time is not an input to this rule and the test says so rather
        // than a comment promising it.
        assert_eq!(
            standing(Some(&stamp), Some(&anchors), at(Duration::days(4000))),
            settled(true)
        );
    }

    #[test]
    fn a_settled_stamp_lapses_when_one_anchor_blob_changes_and_says_which() {
        let was = digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let now = digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_C),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&was), None);

        // §7 promises the lapsed node shows WHAT moved, which is why the digest is text and not a
        // hash of it. Naming the one file that moved — and not the one that did not — is that
        // promise being kept.
        assert_eq!(
            standing(Some(&stamp), Some(&now), at(Duration::zero())),
            lapsed(moved_to(&["core/src/map_store.rs"], &[], &[]))
        );
    }

    #[test]
    fn a_settled_stamp_lapses_when_an_anchor_appears_or_disappears() {
        let was = digest(&[("core/src/map_join.rs", BLOB_A)]);
        let grown = digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&was), None);

        // A new file citing the section is as much a change as an edit to an old one — arguably
        // more, because it is code nobody weighed when the stamp was made.
        assert_eq!(
            standing(Some(&stamp), Some(&grown), at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/map_store.rs"], &[]))
        );

        let grown_stamp = stamp_of(Verdict::Settled, Some(&grown), None);
        assert_eq!(
            standing(Some(&grown_stamp), Some(&was), at(Duration::zero())),
            lapsed(moved_to(&[], &[], &["core/src/map_store.rs"]))
        );

        // The case slice 6 will produce in bulk: stamped when nothing readable named the section,
        // read once a slug makes a module declare against it. The green was given over code that
        // did not exist, so it has to come back and ask.
        let unwatched = stamp_of(Verdict::Settled, Some(""), None);
        assert_eq!(
            standing(Some(&unwatched), Some(&was), at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/map_join.rs"], &[]))
        );
    }

    #[test]
    fn a_settled_stamp_with_no_readable_anchor_is_watched_false_and_never_lapses() {
        // Not a bug and not hidden. `Anchor::Declared` has zero instances in this repository today,
        // so a green with nothing to watch is the COMMON case rather than the corner, and it is
        // exactly the silent green this feature exists to kill — reported, and reported as such.
        let stamp = stamp_of(Verdict::Settled, Some(""), None);

        assert_eq!(
            standing(Some(&stamp), Some(""), at(Duration::zero())),
            settled(false)
        );
        assert_eq!(
            standing(Some(&stamp), Some(""), at(Duration::days(4000))),
            settled(false)
        );
    }

    #[test]
    fn a_settled_stamp_that_cannot_be_compared_says_so_rather_than_guessing_either_way() {
        let anchors = digest(&[("core/src/map_join.rs", BLOB_A)]);

        // `None` current: the folder was a repository when it was stamped and is not one now, or
        // `git` did not answer. Unchanged would be a green nobody checked; moved would nag over
        // nothing; *I could not look* is the only one of the three that is true.
        let stamp = stamp_of(Verdict::Settled, Some(&anchors), None);
        assert_eq!(
            standing(Some(&stamp), None, at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );

        // The same answer when the stamp is the unreadable half. A settled row with no digest is
        // one `0118`'s CHECK refuses, and the reason it is answered here anyway is that the
        // tempting alternative — `Settled { watched: false }` — is D2's defect moved to read time:
        // a transient *git was unreadable* promoted to a permanent *there is nothing to watch*.
        let undigested = stamp_of(Verdict::Settled, None, None);
        assert_eq!(
            standing(Some(&undigested), Some(&anchors), at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );

        // And when the stamp says *nothing readable* but this reading cannot confirm it still holds.
        let unwatched = stamp_of(Verdict::Settled, Some(""), None);
        assert_eq!(
            standing(Some(&unwatched), None, at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );
    }

    #[test]
    fn an_amber_stamp_ignores_its_anchors_moving_entirely() {
        // §7.1, and it is the counter-intuitive rule: you already know it is half-done, so the code
        // moving teaches you nothing. Only the note rots.
        let note = "falta migrar as páginas de pilar";
        let was = digest(&[("core/src/map_join.rs", BLOB_A)]);
        let stamp = stamp_of(Verdict::Partial, Some(&was), Some(note));
        let amber = Standing::Partial {
            stamped_at: STAMPED_AT.to_owned(),
            note: note.to_owned(),
        };

        let elsewhere = digest(&[("sidecars/web/main.go", BLOB_C)]);
        assert_eq!(
            standing(Some(&stamp), Some(&elsewhere), at(Duration::zero())),
            amber
        );
        assert_eq!(
            standing(Some(&stamp), Some(""), at(Duration::zero())),
            amber
        );
        assert_eq!(standing(Some(&stamp), None, at(Duration::zero())), amber);
    }

    #[test]
    fn an_amber_stamp_lapses_on_the_day_after_its_window_and_not_before() {
        let note = "falta migrar as páginas de pilar";
        let stamp = stamp_of(Verdict::Partial, None, Some(note));
        let amber = Standing::Partial {
            stamped_at: STAMPED_AT.to_owned(),
            note: note.to_owned(),
        };
        let stale = lapsed(Lapse::Stale {
            note: note.to_owned(),
        });

        // Both sides of the boundary, because an off-by-one here nags a day early forever, and the
        // window is stated against the constant so the two cannot drift apart.
        assert_eq!(
            standing(Some(&stamp), None, at(NOTE_LIFETIME - Duration::seconds(1))),
            amber
        );
        assert_eq!(standing(Some(&stamp), None, at(NOTE_LIFETIME)), amber);
        assert_eq!(
            standing(Some(&stamp), None, at(NOTE_LIFETIME + Duration::seconds(1))),
            stale
        );
        assert_eq!(
            standing(Some(&stamp), None, at(NOTE_LIFETIME + Duration::days(1))),
            stale
        );

        // The number is argued in `NOTE_LIFETIME`'s doc comment, and it is written down twice on
        // purpose: changing it is changing the feature, not tuning a knob, so it should cost a
        // failing test and a reader who goes and reads the argument.
        assert_eq!(NOTE_LIFETIME.num_days(), 30);
    }

    #[test]
    fn a_withdrawal_never_lapses_however_far_the_clock_or_the_code_moves() {
        let note = Some("o §4 vai ser reescrito");
        let was = digest(&[("core/src/map_join.rs", BLOB_A)]);
        let stamp = stamp_of(Verdict::Withdrawn, Some(&was), note);
        let withdrawn = Standing::Withdrawn {
            stamped_at: STAMPED_AT.to_owned(),
            note: note.map(str::to_owned),
        };

        assert_eq!(
            standing(Some(&stamp), Some(&was), at(Duration::zero())),
            withdrawn
        );
        assert_eq!(
            standing(
                Some(&stamp),
                Some(&digest(&[("core/src/http.rs", BLOB_C)])),
                at(Duration::days(4000))
            ),
            withdrawn
        );
        assert_eq!(
            standing(Some(&stamp), None, at(Duration::days(4000))),
            withdrawn
        );

        // The one that has to hold for the third verdict to mean anything: it waits for the
        // document, and nothing this module can measure is allowed to move it.
        let bare = stamp_of(Verdict::Withdrawn, None, None);
        assert_eq!(
            standing(Some(&bare), None, at(Duration::days(4000))),
            Standing::Withdrawn {
                stamped_at: STAMPED_AT.to_owned(),
                note: None,
            }
        );
    }

    #[test]
    fn the_digest_of_no_files_is_not_the_digest_of_a_file_that_hashes_to_nothing() {
        // `''` means *no anchor*, and it must never collide with *an anchor whose content is
        // empty*. If it did, a decision anchored to one empty file would carry `watched: false` and
        // a green that never comes back to ask.
        let nothing = canonical(std::iter::empty());
        let empty_file = digest(&[("core/src/placeholder.rs", EMPTY_BLOB)]);

        assert_eq!(nothing, "");
        assert_ne!(empty_file, nothing);

        let stamp = stamp_of(Verdict::Settled, Some(&nothing), None);
        assert_eq!(
            standing(Some(&stamp), Some(&empty_file), at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/placeholder.rs"], &[]))
        );
        assert_eq!(
            standing(
                Some(&stamp_of(Verdict::Settled, Some(&empty_file), None)),
                Some(&empty_file),
                at(Duration::zero())
            ),
            settled(true)
        );
    }

    #[test]
    fn the_digest_is_stable_under_the_order_the_paths_arrive_in() {
        // Sorted by path, so the same anchor set stamped twice compares equal. Without it a stamp
        // lapses because a directory walk came back in a different order — a false alarm, and false
        // alarms cost exactly the trust this feature is trying to build.
        let entries = [
            ("core/src/map_store.rs", BLOB_B),
            ("core/src/http.rs", BLOB_C),
            ("core/src/map_join.rs", BLOB_A),
        ];
        let forwards = canonical(entries.iter().copied());
        let backwards = canonical(entries.iter().rev().copied());

        assert_eq!(forwards, backwards);
        assert_eq!(
            forwards,
            format!(
                "{BLOB_C} core/src/http.rs\n{BLOB_A} core/src/map_join.rs\n{BLOB_B} core/src/map_store.rs"
            )
        );

        let stamp = stamp_of(Verdict::Settled, Some(&forwards), None);
        assert_eq!(
            standing(Some(&stamp), Some(&backwards), at(Duration::zero())),
            settled(true)
        );
    }

    #[test]
    fn the_canonical_form_and_its_parse_are_inverses() {
        // Task 3 writes one side of this and the reader above consumes the other, so the pair has
        // to round-trip or a stamp compares unequal to the anchor set it was made from. The path
        // with a space in it is the case the field order exists for: the hash is fixed-width and
        // goes first, so everything after the first space is the path and no quoting rule can be
        // got wrong.
        let entries = [
            ("shell/src/project/Modo Mapa.tsx", BLOB_A),
            ("core/src/map_join.rs", BLOB_B),
        ];
        let text = canonical(entries.iter().copied());
        let read = parse(&text);

        assert_eq!(read.len(), 2);
        assert_eq!(
            read.get("shell/src/project/Modo Mapa.tsx")
                .map(String::as_str),
            Some(BLOB_A)
        );
        assert_eq!(
            canonical(
                read.iter()
                    .map(|(path, blob)| (path.as_str(), blob.as_str()))
            ),
            text
        );

        // The empty digest is a real value and not a missing one: *computed, and there is nothing
        // to watch*.
        assert!(parse("").is_empty());
    }
}
