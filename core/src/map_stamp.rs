//! What a stamp means.
//!
//! The verdict is the owner's, and it is the one thing in this map that no amount of reading the
//! repository can produce: structure says what the code does, the junction says which decision it
//! stands under, and neither can say whether that is what was wanted. `map_store.rs` keeps the rows
//! and knows no rules; this module keeps the rules and knows no SQL, for the same reason
//! `map_intent.rs` knows no database — §7.1 is three expiry rules that must stay three, and a rule
//! that can only be exercised through a table is a rule nobody exercises.
//!
//! It also owns the **anchor digest** — both its canonical form and the `git ls-files` call that
//! produces one. This paragraph used to say the call would land later and live beside `git_exec.rs`;
//! it landed here instead, and the reason is the sentence that already followed. The form is the
//! contract between whoever writes a digest and whoever reads it back, and the two must agree byte
//! for byte or a stamp compares unequal to the very anchor set it was made from — which is an
//! argument for keeping the producer and the form on one screen, not for putting them in two
//! modules. Beside the process call the form would have been filed as a detail of how this machine
//! happens to ask git, when it is in fact the thing every stored digest is bound by for as long as
//! the row exists; and `git_exec.rs` is transport that knows nothing about rows or decisions, which
//! is precisely why it is the wrong home for something that knows what an anchor is.

use crate::git_exec::run_git;
use crate::map_store::Stamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

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
        /// Whether this green will ever come back to ask, and — when it will not — which of the
        /// two different reasons it will not.
        ///
        /// **Reported, never hidden.** A green that will never come back to ask is the precise
        /// shape of the false confidence §1 describes, and today it is the common case rather than
        /// the corner: [`crate::map_join::Anchor::Declared`] has zero instances in this repository
        /// until §8's slug edit lands. The map is allowed to carry such a stamp — the owner may
        /// well be settled about work living in a Go sidecar — and is not allowed to let it look
        /// like the other kind.
        watch: Watch,
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

/// Whether a settled stamp has anything to watch, and why not when it has not.
///
/// **Three, and the third arrived from a live measurement rather than from the design.** This was a
/// `watched: bool` until task 3 ran the digest over this repository's own decisions: of ten real
/// anchor paths, **eight came back with a blob and two did not — `AGENTS.md` and `CLAUDE.md`, which
/// this very repository gitignores.** So `false` was quietly carrying two facts at once, and they
/// have different cures. One is §8 unfixed and is repaired by slice 6 putting slugs on citations;
/// the other is a line in a `.gitignore` and has nothing to do with §8 at all. A single number that
/// means both is a number its owner cannot act on — which is the shape of the problem this whole
/// map exists to cure, reappearing one level down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Watch {
    /// Anchors exist, git tracks them, this stamp expires when they move.
    Watched,
    /// No readable module names this decision's section. §8 unfixed; slice 6 is the repair.
    NoAnchor,
    /// Modules name it and git tracks none of them. A `.gitignore` question, not a §8 one.
    Untracked,
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
    /// a settled green with nothing to watch, is that CHECK's own defect moved from write to read —
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
    /// How many of [`StampCounts::settled`] are green over a section no readable module names.
    ///
    /// **The pair of numbers that keeps `N carimbadas` honest.** Without them the header reports a
    /// count of greens without saying how many of them will never come back to ask, which reads as
    /// confidence and is not. Two counts rather than one for the reason [`Watch`] is three-valued:
    /// this one is §8 unfixed and is repaired by slice 6, and [`StampCounts::untracked`] beside it
    /// is repaired by editing a `.gitignore`. One number would leave the owner unable to tell which
    /// of the two they were being asked to do.
    pub no_anchor: usize,
    /// How many of [`StampCounts::settled`] name modules that git tracks none of.
    ///
    /// Expected to be small and expected to be non-zero: this repository gitignores `AGENTS.md` and
    /// `CLAUDE.md`, both of which real decisions anchor to. See [`Watch::Untracked`].
    pub untracked: usize,
    /// Every approved decision, so the five above can be asserted to reconcile.
    pub decisions: usize,
}

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
///
/// **`anchors` is how many readable modules name this decision's section — `Anchored::modules.len()`
/// and nothing else** — and it exists because a digest of `""` cannot say which of [`Watch`]'s two
/// silences produced it. Zero anchors means nothing was ever asked of git; a non-zero count with an
/// empty digest means git was asked about real files and tracks none of them. Only a caller holding
/// both the join and the digest can tell those apart, so the distinction is passed in rather than
/// guessed at here — which is also what keeps this function pure, and §7.1's three rules exercisable
/// without a repository. Handing it the count of *tracked* anchors instead would collapse the two
/// again and always report [`Watch::NoAnchor`].
pub fn standing(
    stamp: Option<&Stamp>,
    current: Option<&str>,
    anchors: usize,
    now: DateTime<Utc>,
) -> Standing {
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
                // nothing to watch, and it is the only place `watch` can be anything else.
                None => Standing::Settled {
                    stamped_at,
                    watch: watch(is, anchors),
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

/// Which of [`Watch`]'s three a green with this digest is standing on.
///
/// A digest with anything in it is being watched, whatever the anchor count says — a decision whose
/// modules are half tracked still expires when the tracked half moves, and reporting it as unwatched
/// because one file is gitignored would hide an expiry that genuinely works.
fn watch(current: &str, anchors: usize) -> Watch {
    match (current.is_empty(), anchors) {
        (false, _) => Watch::Watched,
        (true, 0) => Watch::NoAnchor,
        (true, _) => Watch::Untracked,
    }
}

/// §5.3's header numbers, tallied from one standing per approved decision.
///
/// The caller must hand this **every** approved decision, including the ones nobody has stamped as
/// [`Standing::Never`], because `decisions` is `standings.len()` and the reconciliation is what the
/// header's honesty rests on. Counting decisions from some other source would let the five
/// categories quietly stop covering the whole — and a header is the one place that would never be
/// noticed.
pub fn counts(standings: &[Standing]) -> StampCounts {
    let mut counts = StampCounts {
        settled: 0,
        partial: 0,
        never: 0,
        lapsed: 0,
        withdrawn: 0,
        no_anchor: 0,
        untracked: 0,
        decisions: standings.len(),
    };
    for standing in standings {
        match standing {
            Standing::Never => counts.never += 1,
            Standing::Settled { watch, .. } => {
                counts.settled += 1;
                match watch {
                    Watch::Watched => {}
                    Watch::NoAnchor => counts.no_anchor += 1,
                    Watch::Untracked => counts.untracked += 1,
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

/// How long one `git ls-files` is given before the answer becomes *I could not look*.
///
/// **Its own constant rather than [`crate::git_exec::OPERATION_TIMEOUT`], and nearly two orders of
/// magnitude smaller.** That 300s is the budget for a whole queued VCS operation — a worktree add, a
/// merge, a push across a network — and it is the right size for one. This is a single read of an
/// index git has already built, on a route the window calls every time the map opens, with a person
/// waiting.
///
/// **Five seconds, and it came down from thirty because the measurement said thirty was not a
/// ceiling.** Task 3 timed the whole of this repository through [`digest`] — 732 paths in, 52 771
/// bytes of `ls-files` output — at **77 ms**, and re-timing the bare git call for this change gave
/// 54–84 ms over seven runs. Thirty seconds is four hundred times the worst of those: not a bound on
/// a slow answer but a hang, on a read the map performs every time it opens, and a hang is
/// indistinguishable to whoever is looking at it from an application that has broken. Five seconds
/// is still 65× the measured figure — so it cannot fire because a cold cache or a busy disk made one
/// call slow — and it is short enough that the failure arrives as `Lapse::Unreadable`, which is a
/// sentence the owner can read, rather than as a window that never finishes loading.
const LS_FILES_TIMEOUT: Duration = Duration::from_secs(5);

/// The fixed argv every call carries, before the anchor paths.
///
/// **`--literal-pathspecs`, because a pathspec is not a path.** `core/src/map[1].rs` is a filename
/// to every editor and a glob to git, and a glob would fingerprint files nobody anchored: a stamp
/// that lapses when an unrelated file moves, plus a path in §7's diff that the route cannot match
/// back to the decision it came from. One global flag covers every path in the call; the
/// alternative, a `:(literal)` prefix, costs ten characters *per path* against a command-line
/// ceiling this function is already chunking to stay under.
///
/// **`-z`, because otherwise git C-quotes any path holding a byte above `0x7F`** — `café.rs` comes
/// back as `"caf\303\251.rs"`. That form is self-consistent enough to compare against itself, and it
/// would still be wrong twice: §7 shows the owner a diff, and a spelling no editor of theirs uses is
/// not a diff they can read; and the map route slices one git call's answer per decision **by
/// matching paths**, so a quoted path matches no decision and its anchor quietly becomes `Some("")`
/// — the silent green, arriving through the back door. What `-z` gives up is git's escaping of a
/// path containing a newline, which [`canonical`] cannot represent; [`ls_files_entry`] refuses such
/// a record rather than letting it through.
///
/// `--` last, so an anchor called `-s` is a file and not a flag.
const LS_FILES_ARGV: [&str; 5] = ["--literal-pathspecs", "ls-files", "-s", "-z", "--"];

/// What Windows will not accept in one command line — measured, not looked up.
///
/// A throwaway spawning `git -C C:/Projects/nucleos --literal-pathspecs ls-files -s -z --` with a
/// growing list of 20-character pathspecs, bisected: **1 557 paths (32 761 characters) spawned;
/// 1 558 (32 782) failed with `os error 206`, "the filename or extension is too long"** — before git
/// ran at all. The documented `CreateProcessW` cap is 32 767 characters counting the terminating
/// NUL, and the bisection brackets it. It is written down as a measurement because the failure it
/// prevents does not look like a length problem: `run_git` returns `Err`, [`digest`] returns `None`,
/// and every anchored decision in the project reports *I could not look* at once — which reads as a
/// broken repository and sends whoever chases it to git.
///
/// Windows-shaped, and applied everywhere regardless. Linux's `ARG_MAX` is two megabytes, so
/// chunking there costs one extra process per 32 KB of paths and buys nothing; a `cfg` to skip it
/// would be a second code path exercised on neither machine this is developed on.
const COMMAND_LINE_CEILING: usize = 32_767;

/// Held back from [`COMMAND_LINE_CEILING`], so [`argv_cost`] never has to be exact.
///
/// What that estimate does not model is the backslash-doubling Rust's argv escaping applies in front
/// of a quote, which can cost a path more than the three characters counted for it. Five hundred and
/// twelve characters is room for that to be wrong about a hundred paths in a chunk and still spawn,
/// and it costs one extra process every 64 chunks — a trade worth making in the direction where
/// being wrong is not a crash.
const COMMAND_LINE_HEADROOM: usize = 512;

/// What one argument costs on the command line, over-counted on purpose.
///
/// Its length, one separating space, and the two quotes Rust's escaping wraps around an argument
/// containing one. Over-counts by two for the ordinary path, which has no space in it, and cannot
/// under-count for it.
fn argv_cost(argument: &str) -> usize {
    argument.len() + 3
}

/// The anchor paths split into runs that each fit inside one command line.
///
/// **Split by characters rather than by a count of paths**, because paths are not one length: 1 557
/// of this repository's 20-character module paths fit in a single call, and forty of a 700-character
/// one would not. A count would have to be picked for the worst case and would then spawn dozens of
/// processes for the ordinary one.
///
/// The fixed cost is computed from `root` rather than assumed, because `git -C <root>` carries the
/// project's own path into every command line and a project living twelve directories deep spends
/// that budget before a single anchor is named.
///
/// A path too long to fit even on its own still gets a chunk of its own rather than being dropped.
/// It will fail at the spawn and take the whole digest to `None`, which is the honest answer — *I
/// could not look* — where dropping it would quietly report that anchor as gone.
fn chunked<'a>(root: &Path, paths: &'a [String]) -> Vec<Vec<&'a str>> {
    let fixed = "git".len()
        + argv_cost("-C")
        + argv_cost(&root.to_string_lossy())
        + LS_FILES_ARGV.iter().copied().map(argv_cost).sum::<usize>();
    let budget = COMMAND_LINE_CEILING.saturating_sub(COMMAND_LINE_HEADROOM + fixed);

    let mut chunks: Vec<Vec<&'a str>> = Vec::new();
    let mut spent = 0;
    for path in paths {
        let cost = argv_cost(path);
        match chunks.last_mut() {
            Some(chunk) if spent + cost <= budget => {
                chunk.push(path.as_str());
                spent += cost;
            }
            _ => {
                chunks.push(vec![path.as_str()]);
                spent = cost;
            }
        }
    }
    chunks
}

/// One `<mode> <blob> <stage>\t<path>` record from `git ls-files -s -z`, or `None` when it is not
/// one.
///
/// **Refused rather than repaired, and the caller turns a refusal into `None` for the whole digest.**
/// A record this cannot read means git said something this module does not understand, and the two
/// ways to carry on from there are both worse than stopping: skipping it drops an anchor, which the
/// next read reports as `Lapse::Moved { gone }` — a file that never went anywhere — and guessing at
/// its fields invents a blob. *I could not look* is the only true answer, and [`standing`] already
/// has a shape for it.
///
/// A path holding a `\n` or a `\r` is refused for the same reason and a different cause: it parses
/// perfectly, and [`canonical`] joins entries with `\n`, so it would silently desynchronise every
/// line after it in the digest. `canonical`'s own note says a caller assembling entries is the one
/// who has to keep that from happening, and `-z` gave up the escaping that used to make it
/// impossible — so this is where it is kept. Windows cannot produce such a filename at all; a Linux
/// project can.
fn ls_files_entry(record: &str) -> Option<(u8, &str, &str)> {
    let (meta, path) = record.split_once('\t')?;
    let fields: Vec<&str> = meta.split(' ').collect();
    let [_mode, blob, stage] = fields.as_slice() else {
        return None;
    };
    if path.is_empty() || path.contains('\n') || path.contains('\r') {
        return None;
    }
    Some((stage.parse().ok()?, blob, path))
}

/// The blob hashes git already computed for one decision's anchor files, in [`canonical`] form.
///
/// **git's blobs, and not a hash of the bytes on disk.** §7 asks for exactly this and gets two things
/// for it: the hashes are free, because git computed them when the files were staged; and the stamp
/// expires at a granularity coarser than the keystroke, *"que é a granularidade a que a pergunta
/// 'isto ainda está como eu queria?' faz sentido"*. A digest taken from the working tree would go
/// amber while its owner was still typing, and a map that nags mid-edit is a map nobody leaves open.
///
/// **§7 says that granularity is the commit and §7 is wrong; it is `git add`, and this code is
/// right.** `ls-files -s` reports the **index**, not `HEAD`. Measured while task 3 was written: with
/// `map_stamp.rs` edited but unstaged, `ls-tree HEAD` and `ls-files -s` both said `d70b0dd` while
/// `hash-object` on the working file said `79816ad`, and this function reported `d70b0dd` — so
/// editing does not lapse a stamp and staging does. Written down here because the tempting
/// correction is to make the code match the sentence by reading `ls-tree HEAD` instead, and it is
/// the wrong direction. **The index lapses earlier, never later.** Reading `HEAD` would hold a stamp
/// green over code its owner has already staged — a green over changed code, which is the precise
/// failure this whole feature exists to cure — while erring early costs a re-stamp, and §7 already
/// says re-stamping is one click when the diff is cosmetic. The gap between the two is one `git
/// commit`, and it only ever opens in the direction of asking.
///
/// **Three answers, and the whole reason this returns an `Option` is that they are three.**
///
/// - `None` — *could not compute*. git is not there, the folder is not a repository, the call failed
///   or came back non-zero. A fact about this daemon at this instant, and transient.
/// - `Some("")` — *computed, and there is no readable anchor to watch*. A fact about the decision,
///   permanent until §8's slug edit lands, and what makes a green that can never expire.
/// - `Some(text)` — the digest.
///
/// An `unwrap_or_default()` anywhere between here and the column collapses the first into the second
/// and mints exactly the silent green §1 describes. `0118`'s `CHECK (verdict <> 'settled' OR
/// code_digest IS NOT NULL)` refuses the collapse at the table and [`standing`] refuses it on the way
/// back out; this is the third side of the same argument, on the way in. The `warn!` on every `None`
/// is the other half — a transient failure nobody can see in a log is one nobody can tell apart from
/// a permanent one.
///
/// **An empty `paths` never asks git anything**, and that is a correctness rule rather than an
/// optimisation. Measured: `git ls-files -s -z --` with nothing after the `--` does not list nothing,
/// it lists the **whole repository** — 732 entries here. A decision with no readable anchor would
/// come away fingerprinted against every file in the project and lapse on the next commit to any one
/// of them.
///
/// **One call per command line's worth of paths, merged.** `git ls-files` has no `--stdin` (2.50.1:
/// `error: unknown option 'stdin'`) and `run_git` hands the child a null stdin regardless, so there
/// is no streaming door — see [`chunked`] and [`COMMAND_LINE_CEILING`] for where the chunk size comes
/// from. A chunk that fails takes the whole digest with it: a digest assembled from only the chunks
/// that answered is missing anchors, and the next read would report every one of them as `gone`.
///
/// **The lowest stage wins**, which is stage 0 whenever the index is settled and the merge base while
/// it is not. `git ls-files -s` prints three records for a path in an unresolved merge — stages 1, 2
/// and 3, and no 0 — and `canonical` says out loud that de-duplicating them is the caller's job. This
/// is not a tie-break but §7's own rule applied: the base is the last state that was committed, so
/// the digest holds still through a conflict and moves when the merge lands. Taking `ours` or
/// `theirs` would lapse every anchored stamp the moment a merge began and un-lapse them if it were
/// abandoned.
///
/// `paths` are repository-relative, as [`crate::map_join::Anchored`] holds them, and come back
/// spelled exactly as they went in — which the route depends on to slice one call's answer per
/// decision.
///
/// This is the seventh caller of [`crate::git_exec::run_git`], whose doc names six sanctioned entries
/// and warns that a further one "takes its `Duration` from somewhere else and quietly loses that
/// gate". The gate in question is the VCS queue's per-operation budget, and this is not part of an
/// operation — no worktree is claimed, nothing is written, there is no budget to spend down. It is a
/// read on an HTTP path with a ceiling of its own, [`LS_FILES_TIMEOUT`]; `git_exec`'s list has been
/// amended to say so rather than left to read as though this had slipped past it.
pub async fn digest(root: &Path, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return Some(String::new());
    }

    let mut lowest: BTreeMap<String, (u8, String)> = BTreeMap::new();
    for chunk in chunked(root, paths) {
        let mut argv: Vec<&OsStr> = LS_FILES_ARGV.iter().map(|arg| OsStr::new(*arg)).collect();
        argv.extend(chunk.iter().map(|path| OsStr::new(*path)));

        let answer = match run_git(root, &argv, LS_FILES_TIMEOUT).await {
            Ok(answer) if answer.succeeded() => answer,
            Ok(answer) => {
                tracing::warn!(
                    root = %root.display(),
                    anchors = chunk.len(),
                    exit = ?answer.exit_code,
                    tail = %answer.output_tail.trim(),
                    "git would not list the anchor blobs, so this anchor set has no digest"
                );
                return None;
            }
            Err(reason) => {
                tracing::warn!(
                    root = %root.display(),
                    anchors = chunk.len(),
                    %reason,
                    "could not run git to list the anchor blobs, so this anchor set has no digest"
                );
                return None;
            }
        };

        for record in answer
            .stdout
            .split('\0')
            .filter(|record| !record.is_empty())
        {
            let Some((stage, blob, path)) = ls_files_entry(record) else {
                tracing::warn!(
                    root = %root.display(),
                    record,
                    "git printed something this cannot read as an index entry, so this anchor set has no digest"
                );
                return None;
            };
            match lowest.get(path) {
                Some((held, _)) if *held <= stage => {}
                _ => {
                    lowest.insert(path.to_owned(), (stage, blob.to_owned()));
                }
            }
        }
    }

    // Sorted by `canonical` on the way out, which is what makes merging the chunks above safe to do
    // in whatever order they came back.
    Some(canonical(
        lowest
            .iter()
            .map(|(path, (_, blob))| (path.as_str(), blob.as_str())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::process::Command;

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

    /// An anchor set in canonical form, built by hand from literal pairs.
    ///
    /// Named apart from the module's own [`digest`] for the reason `moved_to` is named apart from
    /// `moved`: this one states an expectation and that one asks git, and a test where the two share
    /// a name reads as if it were comparing a function against itself. It was called `digest` while
    /// task 3 was still ahead of it, which is precisely how the collision arrived.
    fn hand_digest(entries: &[(&str, &str)]) -> String {
        canonical(entries.iter().copied())
    }

    /// `STAMPED_AT` plus an offset. The only clock these tests have, because the module has none.
    fn at(offset: Duration) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(STAMPED_AT)
            .expect("the tests' own instant parses")
            .with_timezone(&Utc)
            + offset
    }

    fn settled(watch: Watch) -> Standing {
        Standing::Settled {
            stamped_at: STAMPED_AT.to_owned(),
            watch,
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
        let one = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);
        let two = hand_digest(&[
            ("core/src/map_join.rs", BLOB_B),
            ("core/src/http.rs", BLOB_C),
        ]);
        let three = hand_digest(&[("core/src/map_store.rs", BLOB_C)]);

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
        // Both sides of the distinction `Watch` exists for: a decision no readable module names, and
        // one whose modules are real and none of which git tracks.
        let anchor_counts = [0, 2];

        let mut every = Vec::new();
        for current in currents {
            for clock in clocks {
                for anchors in anchor_counts {
                    every.push(standing(None, current, anchors, at(clock)));
                    for verdict in verdicts {
                        for stamped in digests {
                            for note in notes {
                                let stamp = stamp_of(verdict, stamped, note);
                                every.push(standing(Some(&stamp), current, anchors, at(clock)));
                            }
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
            tally.no_anchor + tally.untracked <= tally.settled,
            "a green with nothing to watch is a green, so the two can never outnumber them: {tally:?}"
        );

        // A property nothing exercises is a property nobody proved. Each of the five, and both
        // silences a green can stand on, has to actually occur in the mix above or the assertion is
        // vacuous.
        assert!(tally.never > 0, "{tally:?}");
        assert!(tally.settled > 0, "{tally:?}");
        assert!(tally.partial > 0, "{tally:?}");
        assert!(tally.lapsed > 0, "{tally:?}");
        assert!(tally.withdrawn > 0, "{tally:?}");
        assert!(tally.no_anchor > 0, "{tally:?}");
        assert!(tally.untracked > 0, "{tally:?}");
    }

    #[test]
    fn a_settled_stamp_survives_its_anchors_being_unchanged() {
        let anchors = hand_digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&anchors), None);

        // Eleven years on, because time is not an input to this rule and the test says so rather
        // than a comment promising it.
        assert_eq!(
            standing(Some(&stamp), Some(&anchors), 2, at(Duration::days(4000))),
            settled(Watch::Watched)
        );
    }

    #[test]
    fn a_settled_stamp_lapses_when_one_anchor_blob_changes_and_says_which() {
        let was = hand_digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let now = hand_digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_C),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&was), None);

        // §7 promises the lapsed node shows WHAT moved, which is why the digest is text and not a
        // hash of it. Naming the one file that moved — and not the one that did not — is that
        // promise being kept.
        assert_eq!(
            standing(Some(&stamp), Some(&now), 2, at(Duration::zero())),
            lapsed(moved_to(&["core/src/map_store.rs"], &[], &[]))
        );
    }

    #[test]
    fn a_settled_stamp_lapses_when_an_anchor_appears_or_disappears() {
        let was = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);
        let grown = hand_digest(&[
            ("core/src/map_join.rs", BLOB_A),
            ("core/src/map_store.rs", BLOB_B),
        ]);
        let stamp = stamp_of(Verdict::Settled, Some(&was), None);

        // A new file citing the section is as much a change as an edit to an old one — arguably
        // more, because it is code nobody weighed when the stamp was made.
        assert_eq!(
            standing(Some(&stamp), Some(&grown), 2, at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/map_store.rs"], &[]))
        );

        let grown_stamp = stamp_of(Verdict::Settled, Some(&grown), None);
        assert_eq!(
            standing(Some(&grown_stamp), Some(&was), 2, at(Duration::zero())),
            lapsed(moved_to(&[], &[], &["core/src/map_store.rs"]))
        );

        // The case slice 6 will produce in bulk: stamped when nothing readable named the section,
        // read once a slug makes a module declare against it. The green was given over code that
        // did not exist, so it has to come back and ask.
        let unwatched = stamp_of(Verdict::Settled, Some(""), None);
        assert_eq!(
            standing(Some(&unwatched), Some(&was), 1, at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/map_join.rs"], &[]))
        );
    }

    /// The two silences a green can stand on, told apart by the one input that can tell them apart.
    ///
    /// Neither is a bug and neither is hidden. `Anchor::Declared` has zero instances in this
    /// repository today, so a green with nothing to watch is the COMMON case rather than the corner,
    /// and it is exactly the silent green this feature exists to kill — reported, and reported as
    /// such. What this test pins is that the report says WHICH: the same empty digest is `NoAnchor`
    /// when no module names the section and `Untracked` when modules name it and git tracks none of
    /// them, and the two have different cures — slice 6 for the first, a `.gitignore` line for the
    /// second.
    #[test]
    fn a_green_with_nothing_to_watch_says_which_of_the_two_silences_it_is() {
        let stamp = stamp_of(Verdict::Settled, Some(""), None);

        assert_eq!(
            standing(Some(&stamp), Some(""), 0, at(Duration::zero())),
            settled(Watch::NoAnchor)
        );
        assert_eq!(
            standing(Some(&stamp), Some(""), 0, at(Duration::days(4000))),
            settled(Watch::NoAnchor),
            "nothing to watch means nothing time can do to it either"
        );

        // The live case the bool could not express: `AGENTS.md` and `CLAUDE.md` are named by real
        // decisions in this repository and gitignored by it, so git answers about them with silence.
        assert_eq!(
            standing(Some(&stamp), Some(""), 2, at(Duration::zero())),
            settled(Watch::Untracked)
        );
        assert_eq!(
            standing(Some(&stamp), Some(""), 2, at(Duration::days(4000))),
            settled(Watch::Untracked)
        );

        // And a digest with anything in it is watched however many modules were named, because the
        // tracked half really does expire when it moves.
        let anchored = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);
        let half = stamp_of(Verdict::Settled, Some(&anchored), None);
        assert_eq!(
            standing(Some(&half), Some(&anchored), 2, at(Duration::zero())),
            settled(Watch::Watched)
        );
    }

    #[test]
    fn a_settled_stamp_that_cannot_be_compared_says_so_rather_than_guessing_either_way() {
        let anchors = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);

        // `None` current: the folder was a repository when it was stamped and is not one now, or
        // `git` did not answer. Unchanged would be a green nobody checked; moved would nag over
        // nothing; *I could not look* is the only one of the three that is true.
        let stamp = stamp_of(Verdict::Settled, Some(&anchors), None);
        assert_eq!(
            standing(Some(&stamp), None, 1, at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );

        // The same answer when the stamp is the unreadable half. A settled row with no digest is
        // one `0118`'s CHECK refuses, and the reason it is answered here anyway is that the
        // tempting alternative — a green with nothing to watch — is D2's defect moved to read time:
        // a transient *git was unreadable* promoted to a permanent *there is nothing to watch*.
        let undigested = stamp_of(Verdict::Settled, None, None);
        assert_eq!(
            standing(Some(&undigested), Some(&anchors), 1, at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );

        // And when the stamp says *nothing readable* but this reading cannot confirm it still holds.
        // The anchor count is deliberately zero here, which is the input that would otherwise say
        // `Watch::NoAnchor`: an unreadable digest outranks it, because *I could not look* is a fact
        // about this instant and *there is nothing to watch* is a claim about the decision.
        let unwatched = stamp_of(Verdict::Settled, Some(""), None);
        assert_eq!(
            standing(Some(&unwatched), None, 0, at(Duration::zero())),
            lapsed(Lapse::Unreadable)
        );
    }

    #[test]
    fn an_amber_stamp_ignores_its_anchors_moving_entirely() {
        // §7.1, and it is the counter-intuitive rule: you already know it is half-done, so the code
        // moving teaches you nothing. Only the note rots.
        let note = "falta migrar as páginas de pilar";
        let was = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);
        let stamp = stamp_of(Verdict::Partial, Some(&was), Some(note));
        let amber = Standing::Partial {
            stamped_at: STAMPED_AT.to_owned(),
            note: note.to_owned(),
        };

        let elsewhere = hand_digest(&[("sidecars/web/main.go", BLOB_C)]);
        assert_eq!(
            standing(Some(&stamp), Some(&elsewhere), 1, at(Duration::zero())),
            amber
        );
        assert_eq!(
            standing(Some(&stamp), Some(""), 1, at(Duration::zero())),
            amber
        );
        assert_eq!(standing(Some(&stamp), None, 1, at(Duration::zero())), amber);
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
            standing(
                Some(&stamp),
                None,
                1,
                at(NOTE_LIFETIME - Duration::seconds(1))
            ),
            amber
        );
        assert_eq!(standing(Some(&stamp), None, 1, at(NOTE_LIFETIME)), amber);
        assert_eq!(
            standing(
                Some(&stamp),
                None,
                1,
                at(NOTE_LIFETIME + Duration::seconds(1))
            ),
            stale
        );
        assert_eq!(
            standing(Some(&stamp), None, 1, at(NOTE_LIFETIME + Duration::days(1))),
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
        let was = hand_digest(&[("core/src/map_join.rs", BLOB_A)]);
        let stamp = stamp_of(Verdict::Withdrawn, Some(&was), note);
        let withdrawn = Standing::Withdrawn {
            stamped_at: STAMPED_AT.to_owned(),
            note: note.map(str::to_owned),
        };

        assert_eq!(
            standing(Some(&stamp), Some(&was), 1, at(Duration::zero())),
            withdrawn
        );
        assert_eq!(
            standing(
                Some(&stamp),
                Some(&hand_digest(&[("core/src/http.rs", BLOB_C)])),
                1,
                at(Duration::days(4000))
            ),
            withdrawn
        );
        assert_eq!(
            standing(Some(&stamp), None, 1, at(Duration::days(4000))),
            withdrawn
        );

        // The one that has to hold for the third verdict to mean anything: it waits for the
        // document, and nothing this module can measure is allowed to move it.
        let bare = stamp_of(Verdict::Withdrawn, None, None);
        assert_eq!(
            standing(Some(&bare), None, 0, at(Duration::days(4000))),
            Standing::Withdrawn {
                stamped_at: STAMPED_AT.to_owned(),
                note: None,
            }
        );
    }

    #[test]
    fn the_digest_of_no_files_is_not_the_digest_of_a_file_that_hashes_to_nothing() {
        // `''` means *no anchor*, and it must never collide with *an anchor whose content is
        // empty*. If it did, a decision anchored to one empty file would carry a green with nothing
        // to watch, and one that never comes back to ask.
        let nothing = canonical(std::iter::empty());
        let empty_file = hand_digest(&[("core/src/placeholder.rs", EMPTY_BLOB)]);

        assert_eq!(nothing, "");
        assert_ne!(empty_file, nothing);

        let stamp = stamp_of(Verdict::Settled, Some(&nothing), None);
        assert_eq!(
            standing(Some(&stamp), Some(&empty_file), 1, at(Duration::zero())),
            lapsed(moved_to(&[], &["core/src/placeholder.rs"], &[]))
        );
        assert_eq!(
            standing(
                Some(&stamp_of(Verdict::Settled, Some(&empty_file), None)),
                Some(&empty_file),
                1,
                at(Duration::zero())
            ),
            settled(Watch::Watched)
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
            standing(Some(&stamp), Some(&backwards), 3, at(Duration::zero())),
            settled(Watch::Watched)
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

    /// A directory in the **system** temp folder, deleted when it drops.
    ///
    /// **Not `git_exec::space_free_tempdir`, which builds its directory under this checkout**, and
    /// the difference is the whole of one test below: a folder inside `C:/Projects/nucleos` is
    /// inside a git repository, so `git ls-files` there answers about nucleos instead of refusing,
    /// and `a_folder_that_is_not_a_repository_is_none_rather_than_an_empty_digest` would pass
    /// forever without once exercising what it names. Measured before relying on it: `git -C %TEMP%
    /// rev-parse --show-toplevel` says *not a git repository*, so nothing above `%TEMP%` on this
    /// machine is one either. A machine where that stops being true fails the test loudly, which is
    /// the direction to fail in.
    ///
    /// A `TempDir` and not a `remove_dir_all` at the bottom of the test body, because `Drop` runs
    /// while a panic unwinds and a line at the bottom of the body does not. This repository already
    /// pays for that difference in stranded `%TEMP%` directories, and each of these fixtures is a
    /// git repository — twenty-seven files for an empty one, five hundred and change for the
    /// chunking fixture. Measured on the way in, because `remove_dir_all` refusing a read-only file
    /// is a real Windows failure and git marks three of its own that way: it removes the whole
    /// repository, `.git` and all.
    fn scratch(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create a temporary directory")
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    /// What one git command printed, for the assertions that check this module's answer against a
    /// **second, independent** computation of the same hash rather than against its own output.
    fn git_says(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// An empty repository with a deterministic identity and no line-ending rewriting.
    ///
    /// `core.autocrlf false` for the reason `git_exec::initialize_repo` gives about the same setting:
    /// it is `true` from the system config on a default Windows install, and the blob hash of a file
    /// git rewrote on the way into the index is not the hash of the bytes the test wrote — which
    /// would make every `hash-object` cross-check below disagree with a correct implementation.
    fn repository(prefix: &str) -> tempfile::TempDir {
        let dir = scratch(prefix);
        git_in(dir.path(), &["init", "-q"]);
        git_in(dir.path(), &["config", "user.email", "test@x"]);
        git_in(dir.path(), &["config", "user.name", "test"]);
        git_in(dir.path(), &["config", "core.autocrlf", "false"]);
        dir
    }

    fn write(root: &Path, path: &str, contents: &str) {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("create the file's directory");
        }
        std::fs::write(file, contents).expect("write the file");
    }

    fn commit(root: &Path, message: &str) {
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", message]);
    }

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    /// A directory name and a file count chosen so the argument list is over the measured ceiling
    /// and not far over it: 500 paths of 81 characters is 42 000 characters of command line against
    /// [`COMMAND_LINE_CEILING`]'s 32 767, which forces exactly two chunks. Two is the number that
    /// tests the merge; twenty would only test it more slowly.
    const CHUNKED_DIR: &str = "anchors/a-directory-name-long-enough-to-make-one-command-line-hurt";
    const CHUNKED_FILES: usize = 500;

    /// A repository holding more anchor paths than one `git ls-files` can be handed.
    fn many_anchors(prefix: &str) -> (tempfile::TempDir, Vec<String>) {
        let repo = repository(prefix);
        let paths: Vec<String> = (0..CHUNKED_FILES)
            .map(|which| format!("{CHUNKED_DIR}/anchor-{which:04}.rs"))
            .collect();
        for (which, path) in paths.iter().enumerate() {
            write(repo.path(), path, &format!("//! §7 — anchor {which}\n"));
        }
        commit(repo.path(), "many anchors");
        (repo, paths)
    }

    #[tokio::test]
    async fn the_digest_names_every_anchor_and_its_blob() {
        let repo = repository("nucleos-digest-anchors-");
        let root = repo.path();
        write(root, "core/src/map_join.rs", "//! §7 junction\n");
        write(root, "core/src/map_store.rs", "//! §7 rows\n");
        write(root, "core/src/unrelated.rs", "//! nothing to do with it\n");
        commit(root, "seed");

        let anchors = owned(&["core/src/map_join.rs", "core/src/map_store.rs"]);
        let text = digest(root, &anchors)
            .await
            .expect("a repository can be read");
        let read = parse(&text);

        // Checked against a SECOND computation of the same hash rather than against a sha copied out
        // of this function's own output: `git hash-object` hashes the bytes on disk and `ls-files -s`
        // reports what the index holds, and the two agreeing is what says the digest names the file
        // it claims to rather than merely being stable.
        assert_eq!(read.len(), 2, "{text}");
        for anchor in &anchors {
            assert_eq!(
                read.get(anchor).map(String::as_str),
                Some(git_says(root, &["hash-object", anchor]).as_str()),
                "{anchor} in {text}"
            );
        }

        // The pathspec is a scope and not a suggestion. A digest that quietly carried every file in
        // the repository would satisfy every assertion above and lapse on the next commit to
        // anything at all.
        assert!(!read.contains_key("core/src/unrelated.rs"), "{text}");
    }

    #[tokio::test]
    async fn an_empty_anchor_list_never_calls_git_and_is_not_the_whole_repository() {
        let repo = repository("nucleos-digest-empty-");
        let root = repo.path();
        write(root, "a.rs", "//! one\n");
        write(root, "b.rs", "//! two\n");
        commit(root, "seed");

        // The mistake this test exists for does not announce itself: `git ls-files -s -z --` with
        // nothing after the `--` lists the WHOLE repository — 732 entries in this project, and the
        // two below in a fixture. A decision with no readable anchor would come away fingerprinted
        // against every file in the project and lapse on the next commit to any one of them. The
        // count is asserted from git rather than assumed, so the test still means this if the
        // fixture grows.
        assert_eq!(git_says(root, &["ls-files"]).lines().count(), 2);
        assert_eq!(digest(root, &[]).await, Some(String::new()));

        // And the proof that git was never asked, rather than asked and ignored: a folder that is
        // not a repository is the one input that makes a git call fail, and the answer here is still
        // `Some("")`. If the empty list ever reaches the process spawn, this line turns red.
        let outside = scratch("nucleos-digest-empty-outside-");
        assert_eq!(digest(outside.path(), &[]).await, Some(String::new()));
    }

    /// **Absent, and the absence is the answer: an untracked anchor contributes no entry at all.**
    ///
    /// The alternative was a placeholder — an all-zero sha, or the file's on-disk hash — and it is
    /// wrong in the direction this feature cannot afford. git has never seen the file, so the stamp
    /// would carry a blob git will not produce, and the first `git add` would move the digest and
    /// lapse the stamp: the map would report that the anchor code changed when not one byte of it
    /// had. §7 ties expiry to what git has recorded, and a file git has never recorded has nothing
    /// to say about whether the code moved.
    ///
    /// The cost, stated rather than hidden: a decision whose only anchor is untracked comes away with
    /// `Some("")` — computed, nothing to watch — which [`standing`] reports as
    /// `Settled { watch: Watch::Untracked }`. That is a green which can never expire, and it is shown
    /// as one rather than enjoyed — and named apart from [`Watch::NoAnchor`], because this one is
    /// cured by a `.gitignore` line and that one by slice 6.
    #[tokio::test]
    async fn a_path_git_does_not_track_is_absent_rather_than_guessed_at() {
        let repo = repository("nucleos-digest-untracked-");
        let root = repo.path();
        write(root, "tracked.rs", "//! §7 committed\n");
        commit(root, "seed");
        write(root, "untracked.rs", "//! §7 written and never staged\n");

        let text = digest(root, &owned(&["tracked.rs", "untracked.rs"]))
            .await
            .expect("a repository can be read");
        let read = parse(&text);
        assert_eq!(read.len(), 1, "{text}");
        assert!(read.contains_key("tracked.rs"), "{text}");
        assert!(!read.contains_key("untracked.rs"), "{text}");

        // A path that does not exist on disk at all is the same fact from the other side, and git
        // says so the same way: exit 0 and no record. It is NOT an error, so it must not become one.
        assert_eq!(
            digest(root, &owned(&["tracked.rs", "never/existed.rs"]))
                .await
                .as_deref(),
            Some(text.as_str())
        );

        // A decision anchored to nothing git tracks: `Some("")`, which is *computed, and there is
        // nothing to watch*, and is a different answer from the `None` that means *I could not look*.
        // Collapsing the two is the bug the whole `Option` exists to prevent.
        assert_eq!(
            digest(root, &owned(&["untracked.rs"])).await,
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_is_none_rather_than_an_empty_digest() {
        // §11: a project added from outside has zero specs, and may have no repository either. Both
        // wrong answers here are quiet ones — `Some("")` would say *this decision has nothing to
        // watch*, which is permanent and false, and would mint a green that never comes back to ask.
        // `None` is transient, and `standing` turns it into `Lapse::Unreadable`.
        let outside = scratch("nucleos-digest-not-a-repo-");
        write(outside.path(), "core/src/map_join.rs", "//! §7\n");
        let anchors = owned(&["core/src/map_join.rs"]);

        let answer = digest(outside.path(), &anchors).await;
        assert_eq!(answer, None);
        assert_ne!(answer, Some(String::new()));

        // And a root that is not there at all, which is how a project folder somebody moved arrives.
        // git fails to change directory rather than failing to find a `.git`, and the answer must be
        // the same one.
        assert_eq!(digest(&outside.path().join("gone"), &anchors).await, None);
    }

    #[tokio::test]
    async fn an_anchor_list_far_longer_than_a_command_line_still_gets_one_digest() {
        let (repo, paths) = many_anchors("nucleos-digest-chunked-");
        let root = repo.path();

        // The fixture has to actually be over the ceiling, or this test quietly stops testing what
        // it names the day somebody shortens the directory name. Asserted against the same measured
        // constant the implementation chunks by, so the two cannot drift apart in silence.
        let argv: usize = paths.iter().map(|path| argv_cost(path)).sum();
        assert!(
            argv > COMMAND_LINE_CEILING,
            "the fixture fits in one command line at {argv} characters, so it exercises nothing"
        );
        assert!(chunked(root, &paths).len() > 1, "one chunk is not a merge");

        let text = digest(root, &paths)
            .await
            .expect("a repository can be read");
        let read = parse(&text);

        // Every one of them, and not merely the right count: a chunk silently lost would be a stamp
        // that lapses reporting hundreds of files gone.
        assert_eq!(read.len(), paths.len(), "{} of {}", read.len(), paths.len());
        for path in &paths {
            assert!(read.contains_key(path), "{path} is missing from the digest");
        }

        // One from each end, checked against a second computation, so this is a digest and not a
        // list of paths with something plausible beside them.
        for path in [&paths[0], &paths[CHUNKED_FILES - 1]] {
            assert_eq!(
                read.get(path).map(String::as_str),
                Some(git_says(root, &["hash-object", path]).as_str()),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn the_result_is_path_sorted_however_the_chunks_came_back() {
        let (repo, mut paths) = many_anchors("nucleos-digest-sorted-");
        let root = repo.path();
        paths.reverse();
        assert!(
            chunked(root, &paths).len() > 1,
            "the merge across chunks is the point of this test"
        );

        let text = digest(root, &paths)
            .await
            .expect("a repository can be read");
        let named: Vec<&str> = text
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(_, path)| path)
            .collect();
        let mut sorted = named.clone();
        sorted.sort_unstable();
        assert_eq!(named.len(), CHUNKED_FILES);
        assert_eq!(named, sorted, "the digest must come out path-sorted");

        // The same anchor set handed over the other way round must be the same text, byte for byte.
        // A digest that depended on the order a caller happened to assemble its paths in would lapse
        // a stamp over nothing — and a false alarm costs exactly the trust this feature is trying to
        // earn.
        paths.reverse();
        assert_eq!(digest(root, &paths).await.as_deref(), Some(text.as_str()));
    }

    #[tokio::test]
    async fn an_anchor_in_an_unresolved_merge_reports_the_state_both_sides_started_from() {
        // `canonical` hands this job to its caller by name: `git ls-files -s` prints THREE records
        // for a path in an unresolved merge — stages 1, 2 and 3, and no stage 0 — and a digest
        // carrying three entries for one path would collapse to whichever arrived last, which is
        // `theirs` today and whatever git decides tomorrow.
        //
        // The lowest stage wins, which is stage 0 whenever the index is settled and the merge BASE
        // while it is not. That is §7's own rule rather than a tie-break: the base is the last state
        // that was committed, so an anchored stamp holds still through the conflict and is asked
        // again when the merge lands — *ao commit, não a cada tecla*. Taking `ours` or `theirs`
        // instead would lapse every anchored stamp in the project the moment a merge began, and
        // un-lapse them all if it were abandoned.
        let repo = repository("nucleos-digest-conflict-");
        let root = repo.path();
        write(root, "a.rs", "//! §7 as both branches found it\n");
        write(root, "b.rs", "//! §7 untouched by either\n");
        commit(root, "base");
        let base_blob = git_says(root, &["rev-parse", "HEAD:a.rs"]);

        git_in(root, &["checkout", "-q", "-b", "theirs"]);
        write(root, "a.rs", "//! §7 their edit\n");
        commit(root, "theirs");
        git_in(root, &["checkout", "-q", "-"]);
        write(root, "a.rs", "//! §7 our edit\n");
        commit(root, "ours");

        // Expected to fail, so it goes through `Command` directly rather than `git_in`, which
        // asserts success — and the failure is asserted, because a fixture that quietly merged
        // cleanly would leave this test green and vacuous.
        let merge = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["merge", "theirs"])
            .output()
            .expect("git should start");
        assert!(
            !merge.status.success(),
            "the fixture must actually conflict"
        );

        let text = digest(root, &owned(&["a.rs", "b.rs"]))
            .await
            .expect("a repository can be read");
        let read = parse(&text);

        assert_eq!(
            read.len(),
            2,
            "one entry per path, not one per stage: {text}"
        );
        assert_eq!(
            read.get("a.rs").map(String::as_str),
            Some(base_blob.as_str())
        );
        assert_eq!(
            read.get("b.rs").map(String::as_str),
            Some(git_says(root, &["hash-object", "b.rs"]).as_str())
        );
    }

    #[test]
    fn a_record_git_did_not_print_is_refused_rather_than_half_read() {
        // The shape, so the parse is pinned to what git actually emits and not to what this module
        // hopes it does. Copied from a live run against this repository.
        assert_eq!(
            ls_files_entry(
                "100644 c7af70690e31a98812b6f83ec58d77be288e0440 0\tcore/src/map_join.rs"
            ),
            Some((
                0,
                "c7af70690e31a98812b6f83ec58d77be288e0440",
                "core/src/map_join.rs"
            ))
        );

        // A path with a space in it survives, because the tab is what separates the fields and a
        // space is only ever inside the path.
        assert_eq!(
            ls_files_entry(
                "100644 0a1b2c3d4e5f60718293a4b5c6d7e8f901234567 2\tshell/src/Modo Mapa.tsx"
            )
            .map(|(stage, _, path)| (stage, path)),
            Some((2, "shell/src/Modo Mapa.tsx"))
        );

        // And the four ways a record is not one. Each returns `None`, which the caller turns into
        // `None` for the whole digest rather than into a digest with a hole in it.
        assert_eq!(ls_files_entry("100644 abc 0 core/src/map_join.rs"), None);
        assert_eq!(ls_files_entry("100644 abc\tcore/src/map_join.rs"), None);
        assert_eq!(ls_files_entry("100644 abc x\tcore/src/map_join.rs"), None);
        assert_eq!(ls_files_entry("100644 abc 0\t"), None);

        // The one `-z` let back in, and the reason this guard exists: a path holding a newline parses
        // perfectly and would then split `canonical`'s output into two lines, desynchronising every
        // entry after it. Windows cannot make such a filename; a Linux project can.
        assert_eq!(ls_files_entry("100644 abc 0\tcore/src/two\nlines.rs"), None);
        assert_eq!(ls_files_entry("100644 abc 0\tcore/src/carriage\r.rs"), None);
    }
}
