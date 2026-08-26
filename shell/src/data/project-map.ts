import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * A project's structure layer, and the join the núcleo now computes over it.
 *
 * **Still not the whole map.** The evidence layer, the stamps and the triage are later slices.
 * What is here is the structure — what modules exist and what they import, derived off disk and
 * always true — plus the {@link Junction}, which reads the approved decisions against that
 * structure and says, one decision at a time, what does *not* match. Decision 3 of the spec: the
 * value is in the junction, and the nodes that matter are the ones that do not match.
 */

export type Reader = "rust" | "typescript";

/**
 * One `§` reference found in a source file.
 *
 * **`named` is a candidate for a document slug and never a document.** The word after a section
 * number has the same shape whether it is a slug or an English word, and nothing lexical tells
 * them apart: `§4.4 rule` yields `"rule"` exactly the way `§6.4 workspace-de-projeto` yields the
 * slug. The núcleo checks the candidate against the project's real spec slugs when it joins and
 * drops it when it is not one — but what arrives here, hanging off a module or a foreign file, is
 * the raw candidate that check has not been applied to. Reading a non-null `named` as *this
 * citation points at that document* is the one wrong conclusion this field invites.
 */
export interface Citation {
  /** The section label, normalized: `7`, `6.4`, `5.3a`. Never the `§`, never the punctuation after. */
  section: string;
  named: string | null;
}

export interface MapModule {
  path: string;
  reader: Reader;
  /**
   * The file's own gesture at a spec section — its source contains a `§`.
   *
   * **Not the same question as {@link MapModule.cites} being empty, and the two disagree on
   * purpose.** A file holding a bare `§` with no number is `declares: true, cites: []`; a module
   * citing nothing itself whose sibling test names `§9.2` is `declares: false, cites: [{9.2}]`.
   * §5.1's *code nobody asked for* is that second question and not this one — it is counted by
   * {@link Counts.unclaimed}, which is the number a surface should show.
   */
  declares: boolean;
  /**
   * The sections this module names, **including the ones only its sibling test names**.
   *
   * A Rust module keeps its tests in the same file, so a `§` inside `#[cfg(test)]` has always
   * counted for free; folding in the `*.test.tsx` sibling is what stops one declaration answering
   * differently in the two languages. It is the symmetry, not a special case for TypeScript.
   *
   * **Not deduplicated by section, so `cites.length` is not a section count.** The núcleo dedups
   * by the whole citation on purpose: a file writing `§9.2 risk` in one place and `§9.2 spike` in
   * another yields two rows, because collapsing them would mean choosing which trailing candidate
   * survives and throwing the other away. Group by `.section` before counting or displaying.
   */
  cites: Citation[];
  tested: boolean;
}

export interface MapImport {
  from: string;
  to: string;
}

/**
 * A file that names a section in a language no reader here understands — the Go sidecars, mostly.
 *
 * **Not a {@link MapModule}, and never counted with one.** Nothing here knows what a Go file
 * imports or whether anything tests it, so calling one a module would let it borrow a confidence
 * this map never earned. It is carried anyway because without it the junction's *declared, with
 * no code* would be a lie: dozens of Go files under `sidecars/` name a `§`, and a decision one of
 * them implements would otherwise come back claimed by nothing at all.
 *
 * **This is the top-level `foreign`, and it is not {@link Anchored.foreign}.** That one is a
 * `string[]` of paths hanging off a single decision; this one is a whole file with its citations.
 * Same word, two levels, two shapes — which is why they have two names here.
 *
 * It has been on the wire since the structure layer shipped and this file never mirrored it.
 * Nothing broke and nothing announced it, because `apiFetch<T>` is a cast and a cast cannot
 * notice a field it was never told about.
 */
export interface ForeignFile {
  path: string;
  cites: Citation[];
}

/**
 * How firmly one approved decision is tied to code.
 *
 * **Four values, and no two of them may be merged.** Collapsing any pair would make a surface
 * claim something nobody measured — the false confidence this whole feature exists to cure,
 * reproduced inside the cure and wearing better pixels.
 *
 * - **`"declared"`** — a **readable** module names this section *and* names a document this
 *   project has, the shape §8 prescribes (`§6.4 workspace-de-projeto`). Certain, and the only
 *   value that may be drawn as a confirmation. **Zero of these exist in this repository today,
 *   and that is §8 unfixed rather than a defect in the join**: not one citation here says which
 *   document its `§` belongs to, so no positive join can be certain yet. A later slice proposes
 *   the edits that put a slug on them.
 * - **`"ambiguous"`** — something names the section and this map cannot confirm it meant *this*
 *   decision. **Two different uncertainties share the value.** One is *which document is this?* —
 *   a bare `§7` may be any spec's §7, and this repository has plenty of both. The other is *this
 *   is code I cannot read* — a Go sidecar names the section and nothing here knows what Go does
 *   with it. **The anchor does not tell them apart; {@link Anchored.modules} and
 *   {@link Anchored.foreign} do.** A surface drawing this value without looking at those two
 *   lists shows the weaker of the two claims for both. A fifth value was considered and refused,
 *   because the two are not disjoint: an unslugged citation inside a Go file is both at once, and
 *   a fifth word would have had to invent a precedence the spec never states.
 * - **`"silent"`** — **nothing anywhere** names this section: no module, no foreign file, nothing
 *   in any language scanned. This is §5.1's *declared, with no code*. **It is the one value here
 *   that is sound despite the ambiguity above** — if nothing names the section at all then
 *   nothing claims it under *any* document, whichever document each bare `§` meant. Decision 3
 *   says the value is in the nodes that do not match, which makes this the answer the map is
 *   certain about and the one it exists to give.
 * - **`"unnumbered"`** — the heading the decision was copied from carries no number, so nothing
 *   could be looked for. **Not a claim about the code.** A decision extracted from `## Contrato`
 *   is approved and real; it simply anchors nothing. Folding it into `"silent"` is the tempting
 *   move and the wrong one — *nothing claims this* is the report of a search, and here no search
 *   ran.
 */
export type Anchor = "declared" | "ambiguous" | "silent" | "unnumbered";

/** One approved decision, with everything the structure layer can say about it and nothing more. */
export interface Anchored {
  decision_id: number;
  /**
   * The line's number within its spec's extraction, carried through untouched.
   *
   * The owner approved that spec as a numbered list, and *line 3 of that document* is how they
   * refer to a decision afterwards — so a client can reproduce that order or choose another.
   */
  ordinal: number;
  spec_slug: string;
  /**
   * The heading as the model copied it — `## 4.1 Três tipos de decisão` — and not the number read
   * off it. The owner approved this string, so this is the string that goes back to them; the
   * number is an internal step and shows up only as {@link Anchored.anchor}.
   */
  section: string;
  text: string;
  /** The same two words {@link MapDecision.kind} carries, for the reason given on {@link DecisionKind}. */
  kind: DecisionKind;
  anchor: Anchor;
  /**
   * Readable modules naming this section, sorted by path. Empty for `"silent"` and
   * `"unnumbered"` — in the first case because nothing was found, in the second because nothing
   * was sought, and those are not the same emptiness.
   */
  modules: string[];
  /**
   * Paths naming it in a language this map cannot read.
   *
   * **A `string[]` of paths, and not the top-level {@link ForeignFile}[]** — the two `foreign`
   * keys on this payload sit at different levels and carry different shapes.
   *
   * **Separate from {@link Anchored.modules} and never merged into it.** *We know something is
   * there* and *we can see what it is* are different facts, and one list would let the second
   * borrow the first's confidence. It is also what keeps `"silent"` honest: reading *declared,
   * with no code* off the module list alone would report a whole language as absent.
   */
  foreign: string[];
}

/**
 * The numbers behind §5.3's header.
 *
 * They reconcile, and the núcleo asserts it rather than promising it:
 * `declared + ambiguous + silent + unnumbered === decisions`, and every module counted is in
 * exactly one of `unclaimed`, `unmatched`, or some {@link Anchored.modules}.
 *
 * **Not a percentage, and never one.** §12 refuses coverage metrics as a percentage outright — a
 * single collapsed number is exactly the collapse §5 forbids, and dividing any pair of these
 * would manufacture one.
 */
export interface Counts {
  decisions: number;
  declared: number;
  ambiguous: number;
  silent: number;
  unnumbered: number;
  unclaimed: number;
  unmatched: number;
}

/**
 * The intention layer read against the structure layer: what does not match.
 *
 * Two of the three lists are §5.1's rows and the third is the day-one reality. `decisions` holds
 * *declared, with no code* under the `"silent"` anchor; `unclaimed` is *code nobody asked for*;
 * `unmatched` is neither, and on day one is nearly every module because approval has barely
 * started (§10). Folding `unmatched` into `unclaimed` would inflate the one number §5.1 exists to
 * put in front of somebody.
 *
 * **Two of §5.1's four derived states are absent here, and the absence is not an omission.**
 * *À espera* and *silenciado* both mean the triager looked, and there is no triager yet (§6). A
 * surface that drew four states off this payload would be inventing two of them.
 */
export interface Junction {
  /** Ordered by `spec_slug`, then `ordinal`, then id — deterministic rather than final. */
  decisions: Anchored[];
  /** Modules naming no section at all. §5.1's *code nobody asked for*. */
  unclaimed: string[];
  /** Modules naming a section no approved decision names. Neither orphan nor matched. */
  unmatched: string[];
  counts: Counts;
}

/**
 * The owner's three answers, and the only three (§5.2).
 *
 * **`snake_case` on the wire because `map_stamp::Verdict` derives it, and the daemon parses this
 * field with the derived `Deserialize` rather than a lenient reader of its own** — a fourth word is
 * refused before the handler runs, which is the earliest place it can be refused and the only one
 * where nothing has yet had a chance to guess what was meant. So a typo here is a `422` and never a
 * verdict nobody gave.
 */
export type Verdict = "settled" | "partial" | "withdrawn";

/**
 * What a settled stamp is standing on: whether it will ever come back to ask, and — when it will
 * not — which of the reasons it will not.
 *
 * **Five, and merging any of them would be the defect this whole feature treats, one level down.**
 * Four of the five are a green that is worth less than it looks, and each of the four has a
 * different cure: `guessed` is repaired by putting a document slug on a citation (§8), `no_anchor`
 * by writing a citation at all, `untracked` by a `.gitignore` line **or** by a `git add` — nothing
 * can tell which — and `no_repository` by nothing, because it is not a defect. A single word for
 * all of them would be a word its reader cannot act on.
 */
export type Watch = "watched" | "guessed" | "no_anchor" | "untracked" | "no_repository";

/**
 * Why a stamp stopped being true, tagged on `kind`.
 *
 * §7 promises a lapsed node shows the diff between what was stamped and what is there now, because
 * *"re-carimbar é um clique quando o diff é cosmético, e é o momento certo para olhar quando não
 * é"* — and that judgement is only available to somebody who can see the paths. Hence three lists
 * inside `moved` and not one: a blob that changed asks *is this still what you wanted?*, a path
 * that appeared asks *did you ever look at this?*, and a path that is gone asks *was that
 * deliberate?*.
 *
 * **`unreadable` is not a `moved` with empty lists and must never be drawn as one.** It says the
 * comparison could not be made at all; reading it as *everything vanished* would report a change
 * that nothing measured, which is the silently-wrong answer this map exists to stop.
 */
export type Lapse =
  | { kind: "moved"; changed: string[]; added: string[]; gone: string[] }
  | { kind: "stale"; note: string }
  | { kind: "unreadable" };

/**
 * What became of one decision's verdict, read at one instant — tagged on `state`.
 *
 * **Five, exhaustive and mutually exclusive**, so {@link StampCounts} reconciles against them the
 * way {@link Counts} reconciles against the anchors.
 *
 * **This is the verdict axis with expiry applied, and it is not §5.1's derived state.** §5 refuses
 * to flatten the two, so nothing here borrows the triager's vocabulary: `lapsed` is what became of
 * something *the owner* said, while §5.1's *à espera* is what the **triager** thinks is worth the
 * owner's attention. Slice 5 builds the second and needs that word; spending it here would also
 * read on screen as the model having decided this, which is the authority §6 takes away from it and
 * §6.1 refuses to hand back through the rendering door.
 */
export type Standing =
  | { state: "never" }
  | { state: "settled"; stamped_at: string; watch: Watch }
  | { state: "partial"; stamped_at: string; note: string }
  | { state: "lapsed"; stamped_at: string; why: Lapse }
  | { state: "withdrawn"; stamped_at: string; note: string | null };

/**
 * §5.3's header, plus the numbers §5.3 does not carry and the panel must.
 *
 * `settled + partial + never + lapsed + withdrawn === decisions`, and the núcleo asserts it rather
 * than promising it — a header is exactly where a reader stops checking, so a total that does not
 * reconcile is §1's false confidence reappearing inside its own cure.
 *
 * **Not a percentage, and never one.** §12 refuses coverage metrics as a percentage outright, and
 * dividing any pair of these would manufacture the single collapsed number §5 forbids.
 *
 * `unwatched` is `no_anchor + untracked + no_repository`, and it is derived in the núcleo rather
 * than here on purpose: §9.3 makes `core/` the single owner of this map's derivations, and a shell
 * adding the three itself would be a second implementation free to drift the day a fourth silence
 * is added. `guessed` is deliberately **not** part of it — a guessed anchor does expire; what is
 * uncertain is whether it is expiring against the right files.
 */
export interface StampCounts {
  /** §5.3's `N carimbadas`. */
  settled: number;
  /** §5.3's `M a meio`. */
  partial: number;
  /** §5.3's `K nunca vistas`. Debt, and it is supposed to be uncomfortable (§5.3, §10). */
  never: number;
  /** §5.3's `J à tua espera` — today. Slice 5's triager adds to it out of `never`, not out of here. */
  lapsed: number;
  /** Not in the header. §5.2 wants a withdrawal out of the way, not out of sight. */
  withdrawn: number;
  /** How many of `settled` expire against an anchor set matched by section number alone. */
  guessed: number;
  /** How many of `settled` are green over a section no readable module names. */
  no_anchor: number;
  /** How many of `settled` name modules git reports none of. */
  untracked: number;
  /** How many of `settled` are in a project with no git repository (§11). */
  no_repository: number;
  /** The greens that will never come back to ask: `no_anchor + untracked + no_repository`. */
  unwatched: number;
  /** Every approved decision, so the five standings above can be seen to reconcile. */
  decisions: number;
}

/**
 * The triager's answer about one decision (§6), and there are two of them.
 *
 * **The missing third is the whole design.** There is no `"approved"`, no `"ok"`, no `"settled"`:
 * turning something green is the owner's act and the owner's alone (§5.2), and a third word here
 * would hand back the authority §6 takes away. `0119`'s `CHECK (verdict IN ('flagged','silenced'))`
 * is the copy of that rule a caller cannot go round, and this type is not allowed to be wider than
 * it.
 *
 * **Neither value is a statement about the code**, which is the sentence a reader is most likely to
 * lose. `"silenced"` does not say the decision is fine; it says the triager saw nothing worth the
 * owner's time — *"o triador não viu nada estranho. Ninguém olhou. Não é verde."* (§5.1). §6.1
 * refuses to let that share a colour with a stamp, and the panel that draws it uses no colour at
 * all.
 */
export type Judgement = "flagged" | "silenced";

/** One triage judgement as the table holds it. Field for field off `map_store::Judged`. */
export interface Judged {
  decision_id: number;
  judgement: Judgement;
  /** Why, in the triager's own words. Never empty — the table refuses it (§6.2). */
  reason: string;
  /** Which brain answered. The other half of §6.2: a pile that will not say who filled it cannot be repaired. */
  model: string;
  computed_at: string;
  /**
   * What the triager looked at, hashed.
   *
   * On the row and never compared here: the comparison needs the digest of the inputs **as they
   * are now**, which means reading the repository. The núcleo does it and hands over what survived,
   * marked by {@link Held.checked}.
   */
  inputs_digest: string;
}

/**
 * A judgement the map is still holding, and whether this reading could re-check it.
 *
 * **Three-valued staleness wearing two fields.** A judgement whose inputs have moved is not an
 * answer about the code as it stands and never arrives at all. One whose digest could not be
 * *computed* — git would not say what the anchors are — is neither current nor expired, and it
 * arrives with `checked: false`.
 *
 * **A surface may not draw such a row as verified, and may not drop it either.** Dropping it was
 * the núcleo's first shape and it was wrong at the scale of a whole project: every stored judgement
 * is written against a readable anchor set, so one git hiccup made all of them stale at once and
 * the map reported a three-hundred-row backlog as *nunca vista* — a fact about the daemon presented
 * as a fact about the project. A flag we could not re-verify is still a flag; a flag we discarded
 * becomes a claim that nobody ever looked.
 */
export interface Held extends Judged {
  checked: boolean;
}

/**
 * §5.3's `K` and `J`, tallied over exactly the judgements the map is holding.
 *
 * `flagged + silenced + untriaged === stamps.never`, and `unseen === silenced + untriaged`, and
 * `waiting === stamps.lapsed + flagged`. The núcleo builds all of it in one pass and asserts the
 * reconciliation rather than promising it — a header is exactly where a reader stops checking.
 *
 * **A silence stays inside `unseen`, and that is the line this whole slice turns on.** §5.1 is
 * explicit that *silenciado* is *"o triador não viu nada estranho. **Ninguém olhou.** Não é
 * verde"*, so a silenced decision is still debt nobody has given a verdict on. Subtracting it would
 * let a triager that silences three hundred decisions drive the debt figure to **zero** over a
 * backlog nobody has read — §1's false confidence, manufactured by the cure's own arithmetic, on
 * the one line §5.3 calls *"dívida, e é suposto incomodar"*. What a silence buys is exactly one
 * thing: **not being in `waiting`.**
 *
 * **Not a percentage, and never one** (§12). Dividing any pair of these manufactures the single
 * collapsed number §5 forbids.
 */
export interface TriageCounts {
  /** In `never`, and the triager asked for the owner's eyes. The only pile that leaves `unseen`. */
  flagged: number;
  /** In `never`, and the triager saw nothing worth the owner's time. **Still inside `unseen`.** */
  silenced: number;
  /** In `never`, and no current judgement describes it: never triaged, or the answer has gone stale. */
  untriaged: number;
  /** §5.3's `K nunca vistas` — `silenced + untriaged`. Derived by the núcleo, never re-derived here. */
  unseen: number;
  /** §5.3's `J à tua espera` — `stamps.lapsed + flagged`. */
  waiting: number;
  /** How many of `flagged + silenced` this reading could **not** re-check. A subset, never a fourth bucket. */
  unchecked: number;
}

/**
 * How long ago the most recent of a decision's anchor files moved — tagged on `state`.
 *
 * **Five values, and the four without a timestamp are four different silences.** They sort in the
 * same region and mean entirely different things, and §10 offers this ordering as *um facto do
 * git*: a screen rendering four silences identically is the false confidence of §1 arriving
 * through the rendering door §6.1 watches.
 *
 * - `"moved"` — inside the window, at that committer date in **Unix seconds** (not milliseconds,
 *   and not a string).
 * - `"older"` — anchored, and nothing moved inside the window. Says nothing about how much older:
 *   two hundred and one commits ago and a thousand are both this, which is the approximation
 *   {@link Recency.window} exists to announce.
 * - `"unanchored"` — nothing to move. A fact about the decision and not about git.
 * - `"foreign_only"` — **nothing this map can read names it, and something it cannot read does.**
 *   It must never be drawn as *nothing implements this*: that is the failure {@link Anchored.foreign}
 *   was added to prevent, on a new axis. Six section numbers in this repository are in this state.
 * - `"unknown"` — anchored, and git would not say. Never written when there is a window.
 */
export type Age =
  | { state: "moved"; at: number }
  | { state: "older" }
  | { state: "unanchored" }
  | { state: "foreign_only" }
  | { state: "unknown" };

/**
 * §10's ordering, and how far it can see.
 *
 * **Beside `junction.decisions` rather than inside it, because the order is only half the answer.**
 * A list carries the sequence and nothing about which part of it is a fact, and a panel drawing
 * this without saying the window's size would present a mostly arbitrary tail with the same
 * confidence as the head.
 *
 * **And the head is not as ordered as it looks, today.** Measured against this repository: 71 of 80
 * decisions land inside the window carrying **19 distinct timestamps between them, and the top
 * eighteen share one**. The cause is §8 — while no citation says which document its `§` belongs to,
 * a decision's anchor set is every module citing that section *number* across all forty documents,
 * so `§5.2` alone collects 42 files and the most recent of 42 files in a repository committing ~35
 * times a day is this morning. A flat ranked list would imply an ordering that is not there, so a
 * tie has to render as a tie.
 */
export interface Recency {
  /**
   * How many commits the walk asked for, or `null` when git would not say — the number a panel puts
   * in *"ordered by what moved in the last N commits"*, and the sentence it must not write when
   * this is absent.
   */
  window: number | null;
  /** Where each decision fell in that window, **keyed by `decision_id` as a string**. */
  ages: Record<string, Age>;
}

/**
 * One row of §6.2's pile: a silencing, with the decision it was about.
 *
 * **The decision's own words travel with it**, or this is a list of ids and reading it means the
 * cross-reference of three hundred rows §1 says the owner cannot perform.
 *
 * **Filtered by neither standing, staleness nor retirement**, which is the exact opposite of what
 * `GET /map` does with the same table and is deliberate on both sides. That route answers *what is
 * true now*; this one answers *what the triager did*, and each of those filters would delete part
 * of the record §13's only mitigation rests on — a stamp is the evidence a silence was premature, a
 * silence written about code that has since moved is the one somebody should reread, and a
 * retirement is reported here as {@link Silencing.retired} rather than by the row vanishing.
 */
export interface Silencing {
  decision_id: number;
  spec_slug: string;
  section: string;
  text: string;
  reason: string;
  model: string;
  computed_at: string;
  /** The decision has since been retired. On the row, because hiding it deletes a bug report by its own subject. */
  retired: boolean;
  /**
   * This sentence was written by the daemon rather than by a model.
   *
   * `model` names the brain that **answered**, which stays true of an answer nobody could read — so
   * without this field a machine's failure note arrives on screen under a model's name, as its
   * opinion. Always `false` on this pile today, because the one producer of a daemon-written reason
   * may only ever reach `"flagged"`; the field is what makes that a fact a client can check rather
   * than a convention it has to remember.
   */
  machine_written: boolean;
}

/**
 * What one triage run did, and — the half that is easy to leave out — what it did **not**.
 *
 * **Counts and never a bare success.** *"Ran the triager and nothing happened"* and *"ran the
 * triager and everything was already current"* are different facts about a project, and a surface
 * reporting only success makes the second look like the first. Every field here is a thing that
 * would otherwise have happened invisibly, and they add up: `in_scope` is the total and everything
 * else is a bucket of it.
 */
export interface TriageReport {
  /** Decisions in `never`, which is the whole of what the triager is allowed to look at (§10). */
  in_scope: number;
  /** Judged already, against inputs that have not moved since. Cost nothing and asked nothing. */
  already_current: number;
  /** Answered and written this run. The only field that grew the table. */
  judged: number;
  /**
   * The model answered, the answer was not a judgement, and a **flagged** row was written saying
   * so — rows written this way, never rows withheld.
   *
   * Apart from {@link TriageReport.unanswered} because the two send whoever is debugging to
   * different places: this one to the prompt, that one to the machine.
   */
  unreadable: number;
  /** Nobody answered: the run itself failed. A machine that is down, not a question that is wrong. */
  unanswered: number;
  /** The decision stopped being one this map holds between the reading and the write. Expected to be zero. */
  vanished: number;
  /**
   * Stale, and the cap stopped the run before reaching them. **The field the report exists for.**
   *
   * The batch is capped, and this repository normally saturates it. A run that truncated and said
   * nothing reads as *covered everything* when it did not, which is §1's failure produced by the
   * feature built to cure it.
   */
  left_over: number;
  /** In scope, and skipped because git would not say what their anchor code is. Skipped, never triaged under a failed digest. */
  unreadable_anchors: number;
}

/**
 * Did the daemon write this sentence, or did a model?
 *
 * **A duplicate of `map_triage::DAEMON_MARK`, and it is on the wrong side of the seam.** The núcleo
 * exposes the answer as a field — `machine_written` — on §6.2's pile, precisely so no client has to
 * remember a convention; but the flagged judgements on `GET /map` carry no such field, and the
 * flagged pile is the **only** place a daemon-written reason can appear today
 * (`map_triage::unreadable_flag` may only ever reach `"flagged"`). So the one payload where the
 * test is always false has it, and the one where it matters does not. Until `Held` carries the
 * field too, this is the test, written once here rather than inline in a panel — a convention with
 * two spellings is one that has already stopped working somewhere.
 *
 * A prefix test and not a parse: the rest of the sentence is prose meant for a person, and anything
 * reading structure out of it would be inventing a format the writer does not keep.
 */
export function daemonWrote(reason: string): boolean {
  return reason.trimStart().startsWith("nucleos:");
}

export interface ProjectMap {
  modules: MapModule[];
  imports: MapImport[];
  /**
   * Files no reader here understands, kept rather than dropped.
   *
   * Mostly real code — the Go sidecars, the SQL migrations, the stylesheets and the scripts —
   * and keeping them is the whole point: a map that showed only what it could read would
   * quietly claim `sidecars/` does not exist. A tail of it was never source at all, being
   * assets and configuration. It measures what this map cannot see, and nothing that is wrong.
   */
  unread: string[];
  /** The subset of {@link ProjectMap.unread} that names a `§` at all, with the sections it names. */
  foreign: ForeignFile[];
  junction: Junction;
  /**
   * Where each decision stands, **keyed by `decision_id` as a string**.
   *
   * `Record<string, Standing>` and not `Record<number, …>`, because the núcleo sends a
   * `BTreeMap<i64, _>` and JSON object keys are strings — there is no number key in the wire
   * format for one to survive as. Typed with `number` this compiles, and then
   * `standings[row.decision_id]` reads `undefined` at runtime for every row on the screen, which
   * is the shape of failure this whole feature exists to prevent: a surface that looks like it
   * measured something and measured nothing. Join with `String(id)`.
   *
   * A map and never a parallel array. `map_join::join` says out loud that its order is
   * deterministic rather than final — §10 wants a different one, by recency of the anchor code's
   * last change — and two arrays that must stay index-aligned would survive that re-sort by
   * drawing one decision's verdict against another's text, with nothing on screen looking wrong.
   */
  standings: Record<string, Standing>;
  /** §5.3's header, tallied by the núcleo from exactly the standings above. */
  stamps: StampCounts;
  /**
   * What the triager said about each decision, **keyed by `decision_id` as a string** — and only
   * where that is still an answer about this map.
   *
   * Two filters stand between the table and this field, and both are required for §5.3's numbers to
   * add up: a judgement about a decision somebody has since stamped describes a decision that has
   * left triage's scope (§10), and a judgement whose inputs have moved describes code that has
   * since changed. Counting either reports something that is not true now.
   *
   * `Record<string, Held>` for {@link ProjectMap.standings}' reason — the núcleo sends a
   * `BTreeMap<i64, _>` and JSON object keys are strings. Join with `String(id)`.
   */
  triage: Record<string, Held>;
  /** §5.3's `J` and `K`, tallied in the same pass that built `triage`. */
  triage_counts: TriageCounts;
  /**
   * **One fact about this whole reading, and never a fact about any single decision.**
   *
   * The route computes one digest for the union of every decision's anchors — the alternative is
   * up to 350 process spawns per open — so a git that will not answer takes every settled stamp in
   * the project to `Lapse.unreadable` at the same instant. This flag is what buys the owner one
   * sentence instead of 350 identical rows saying it, which is a wall of noise nobody reads to the
   * bottom of and is how the one real lapse underneath goes unseen.
   *
   * **`false` for a project with no git repository at all**, which is §11's ordinary case and not
   * a fault: there is nothing there to retry, and the copy under this flag says *try again*. Such
   * a project gets `Watch` `"no_repository"` on its greens instead, which is a permanent fact and
   * reads as one.
   */
  git_would_not_answer: boolean;
  /**
   * §10's ordering — what `junction.decisions` is sorted by, and how far that sort can see.
   *
   * The list arrives already in this order; what this field adds is which part of it the order is a
   * fact about. Drawing the list without {@link Recency.window} would present a tail nothing
   * measured with the same confidence as the head.
   */
  recency: Recency;
}

/**
 * Read when the mode opens, and not polled.
 *
 * The structure of a repository does not move between two ticks of a three-second timer, and
 * putting it on one would mean walking the whole tree off disk on every tick.
 */
export function useProjectMap(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.map(projectId ?? ""),
    queryFn: () => apiFetch<ProjectMap>(`/projects/${encodeURIComponent(projectId ?? "")}/map`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * The intention layer: what a model read out of a spec, waiting for its owner to say yes or no.
 *
 * `kind` really is spelled `countable`/`character` on the wire. The núcleo's own column holds
 * `b`/`c` under a `CHECK`, and translates on the way out on purpose — a list the owner is meant to
 * approve at a glance cannot label its two kinds with letters.
 */

export type DecisionKind = "countable" | "character";

/** Which model reads a spec. See {@link useExtractSpec} for why this is a call-time choice. */
export type Brain = "cloud" | "local";

/** One sentence a model read out of a spec. Field for field off `crate::map_store::Decision`. */
export interface MapDecision {
  id: number;
  spec_slug: string;
  section: string;
  ordinal: number;
  text: string;
  kind: DecisionKind;
  brain: string;
  extracted_at: string;
  /** `null` until the owner answers this line, one way or the other. */
  approved_at: string | null;
}

/**
 * Which specs this project has, named the way the owner reads them.
 *
 * Read once when the mode opens, and not polled — the same posture {@link useProjectMap} takes,
 * for the same reason: the document set does not move between two ticks of a three-second timer.
 */
export function useProjectSpecs(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapSpecs(projectId ?? ""),
    queryFn: () =>
      apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/specs`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * The pile nobody has read yet.
 *
 * Not folded into {@link useProjectMap}: the structure layer is derived off disk on every read and
 * the pile is a table somebody writes to. One query invalidated by an approval would re-walk the
 * whole tree to answer a question the tree cannot answer.
 */
export function useMapDecisions(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapDecisions(projectId ?? ""),
    queryFn: () =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/decisions`),
    enabled: projectId !== null && projectId !== "",
  });
}

/** What {@link useExtractSpec} sends: the document, and which model should read it. */
export interface ExtractSpecInput {
  specSlug: string;
  brain: Brain;
}

/**
 * Ask a model what one spec decided, and get back the whole pile — this extraction's rows landed
 * among whatever was already waiting, which is what the owner reads.
 *
 * `brain` is a parameter here and not a setting anywhere, because the choice is the owner's to make
 * per request and the núcleo will not make it for them: asked for `local` on a machine with none
 * configured, it refuses rather than quietly falling back to the cloud — a substitution nobody
 * would see coming, and one that spends money the owner did not agree to. A setting would turn that
 * refusal into somebody's permanent default; asking each time keeps it a decision.
 */
export function useExtractSpec(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ specSlug, brain }: ExtractSpecInput) =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId)}/map/extract`, {
        method: "POST",
        body: JSON.stringify({ spec_slug: specSlug, brain }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
    },
  });
}

/** What {@link useDecideMapLine} sends: the row, and the owner's answer to it. */
export interface DecideMapLineInput {
  id: number;
  approved: boolean;
}

/**
 * The owner's answer to one line.
 *
 * **204, so there is no body** (`post_project_map_decision`) — `void` is the honest type, and
 * `apiFetch` exempts 204 from its JSON parse, the way {@link useSetShadowVerdict} in `autopilot.ts`
 * already does for its own bodyless verdict. `404` means the row was already answered, belonged to
 * another project, or never existed — three reasons the daemon deliberately collapses into one,
 * because all three mean the same thing to whoever asked: *that line is not yours to answer now*.
 *
 * **Two keys are invalidated, and the map one is not optional.** An approval moves a line out of
 * the pile *and* into the junction — `map_store::approved` is exactly what `GET /projects/{id}/map`
 * joins against — so a map left holding its pre-approval counts is the feature lying about itself:
 * the owner answers a line, watches the pile shrink, and reads a junction that never heard about
 * it. Re-walking the tree is the price, and it is worth paying at the one moment somebody changed
 * the thing the walk is joined against.
 *
 * The two calls are written out even though `keys.projects.map` is a **prefix** of
 * `keys.projects.mapDecisions` and invalidating it alone would already reach the pile. Leaning on
 * that would make the pile's refresh a side effect of two key shapes happening to nest, and the
 * day somebody moves one of them the pile stops refreshing with nothing to say why. Two lines
 * state two intentions. {@link useExtractSpec} deliberately invalidates only the pile: extraction
 * produces rows nobody has approved, and nothing unapproved is in the junction to change.
 */
export function useDecideMapLine(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, approved }: DecideMapLineInput) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/map/decisions/${id}`, {
        method: "POST",
        body: JSON.stringify({ approved }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
      void queryClient.invalidateQueries({ queryKey: keys.projects.map(projectId) });
    },
  });
}

/** What {@link useCarimbar} sends: the line, the owner's verdict on it, and whatever they wrote. */
export interface CarimbarInput {
  decisionId: number;
  verdict: Verdict;
  /**
   * Obligatory on `partial` and optional on the other two (§7).
   *
   * `null` and never `""` for *said nothing*, because the two are different and `0118` stores the
   * first as NULL. The daemon trims before it checks, so a note of one space is an empty one.
   */
  note: string | null;
}

/**
 * The owner's verdict on one decision — the one act in this whole mode (§5.2, §7).
 *
 * **204, so there is no body** (`post_project_map_stamp`) — `void` is the honest type, and
 * `apiFetch` exempts 204 from its JSON parse, the way {@link useDecideMapLine} already does for its
 * own bodyless answer. Four refusals, and they are four different things to tell whoever asked:
 *
 * - `400` — an amber with nothing in its note. §5.2 makes the note the whole of amber, so an empty
 *   one is not a lesser amber, it is not one. **A surface that can send this has a defect**: the
 *   panel must not offer amber without somewhere to type, because a button that can only fail is a
 *   worse answer than a field.
 * - `404` — the decision is not this project's, or nobody approved it, or it never existed. The
 *   daemon collapses the three deliberately: all three mean *that line is not yours to stamp now*,
 *   and telling them apart would say which ids exist in projects the caller cannot see.
 * - `503` — git is there and **would not answer**, and the verdict was `settled`. §7.1 makes *está
 *   como quero* the only verdict the code moving can falsify, so it is the only one that may not be
 *   recorded without knowing what it is anchored to. **It is transient and the copy has to say
 *   *try again*.** The owner did nothing wrong and a second attempt works; reading it as a failure
 *   would put the blame on the person who pressed the button for a git that was busy.
 * - `422` — a verdict outside the three, which is this client having sent something
 *   {@link Verdict} does not admit. Not a case a screen writes copy for.
 *
 * **No optimistic update, and its absence is not laziness.** The `Standing` that goes with a
 * verdict is decided by a git call and by the join — a client guessing `watch: "watched"` would put
 * a certain word on screen over an anchor set nobody has read yet, which is exactly the visibly-green
 * answer resting on a silently-approximate basis this feature may not produce. So the verdict shows
 * up when the map is read again, and not before.
 *
 * **One key invalidated, and it is the map.** A stamp changes nothing about the pile: the pile
 * holds lines nobody has approved, and a decision has to be approved before it can be stamped at
 * all. That the map key is a *prefix* of the pile's, and so drags it along, is a shape of
 * `keys.projects` and not an intention here — the intention is one line, and it is written as one.
 */
export function useCarimbar(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ decisionId, verdict, note }: CarimbarInput) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/map/stamps`, {
        method: "POST",
        body: JSON.stringify({ decision_id: decisionId, verdict, note }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.map(projectId) });
    },
  });
}

/**
 * §6.2's pile: everything this project's triager has ever silenced, and why.
 *
 * **Its own query because it is its own route, and its own route because §6.2 says *sempre
 * acessível*.** Folded into {@link useProjectMap} it would go down with the map — and the map is
 * derived by walking a project's folder, so a project whose folder moved would lose the record of
 * what a model decided the owner need not look at. This route asks nothing of the disk.
 *
 * **Not the same list as `ProjectMap.triage` filtered to `"silenced"`, and the difference is the
 * point.** That map answers *what is true now* and drops a judgement whose decision has since been
 * stamped, whose code has since moved, or whose decision was retired. This is the record of *what
 * the triager did*, and drops none of the three — §13 rates *o triador silencia o que devia
 * mostrar* a **real** residual risk whose only mitigation is that this stays readable, and each of
 * those filters would delete part of the evidence.
 *
 * Read when the mode opens and not polled: nothing writes to this table except a run of the
 * triager, which invalidates it from {@link useTriage}.
 */
export function useSilencedPile(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapSilenced(projectId ?? ""),
    queryFn: () =>
      apiFetch<Silencing[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/silenced`),
    enabled: projectId !== null && projectId !== "",
  });
}

/** What {@link useTriage} sends: which brain answers the one question (§6). */
export interface TriageInput {
  brain: Brain;
}

/**
 * Ask a model which of the decisions nobody has stamped deserve their owner's eyes (§6, §10).
 *
 * **No decision id and no spec slug beside the brain, and that is §10 rather than an omission.**
 * The triager has one scope — everything nobody has stamped — and a route that let a caller name
 * the decisions would let it name the ones whose answer it liked.
 *
 * **The answer is a {@link TriageReport} and never a bare success**, and a surface that showed only
 * `judged` would be reporting the run at its most flattering. The batch is capped, so `left_over`
 * is the number that must reach the screen: a run that truncated in silence reads as *covered
 * everything*.
 *
 * The refusals are `map/extract`'s, for the reason they are the same plumbing: `422` for a brain
 * nobody can read, `404` for a project or folder that is not there, `503` for a `local` this
 * machine has no model for — never a quiet fall back to the cloud — and `500` for the daemon's own
 * failure. Deliberately **no `502`**: by the time a model fails, rows have been written, and a
 * status that discarded the report would hide work actually done.
 *
 * **Two keys, and both are written out.** A run changes the judgements on the map and appends to
 * §6.2's pile, and those are two different answers from two different routes. That
 * `keys.projects.map` is a *prefix* of the pile's key and would drag it along is a shape of
 * `keys.projects` rather than an intention here — the day somebody re-nests them, a pile that
 * stopped refreshing would have nothing to say why.
 */
export function useTriage(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ brain }: TriageInput) =>
      apiFetch<TriageReport>(`/projects/${encodeURIComponent(projectId)}/map/triage`, {
        method: "POST",
        body: JSON.stringify({ brain }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.map(projectId) });
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapSilenced(projectId) });
    },
  });
}
