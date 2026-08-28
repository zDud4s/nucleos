// §spec mapa-do-projeto
import { type ReactNode } from "react";
import {
  type Anchored,
  type Held,
  type Junction,
  type Lapse,
  type StampCounts,
  type Standing,
  type TriageCounts,
  type Watch,
} from "../data/project-map";
import { RelativeTime } from "../ui";
import { Carimbar } from "./Carimbar";

/**
 * The stamps: what you said about each decision, and what became of it.
 *
 * The other axis. `Juncao` says what can be known without anybody looking; this says what the
 * owner looked at and whether it is still true, and §5 forbids the two collapsing into one —
 * *"achatá-las numa só punha o triador e o dono a falar pela mesma boca"*. They sit side by side
 * and never merge into a state, a colour or a figure.
 *
 * **No percentage, no bar, no score** (§12). This is the panel with the numbers to build one out
 * of, which is exactly why it may not: a single figure that looks like an answer and was never
 * checked against anything is the false confidence of §1 in its purest form, and it would be
 * printed by the feature built to cure it. The four numbers of §5.3 are four because they are four
 * different facts, and dividing any pair of them would throw away which.
 *
 * **The piles are ordered by what the owner can do about them**, most actionable first, and the
 * debt is last and uncapped in its count. A lapsed stamp is one click away from being true again;
 * an amber's note is either still right or is not; a withdrawal needs a document edited, which
 * this map may not do (§12); a green with nothing to watch needs a citation, or a `git add`, or
 * nothing at all; and the pile nobody has looked at needs an afternoon that §10 refuses to demand.
 *
 * **This is one of the two surfaces in this mode with buttons, and it was the only one for a
 * slice.** `Juncao` has none deliberately — a second place to accept without reading is the failure
 * this mode replaces — but stamping is the owner's act and §6 gives it to nobody else, so it has to
 * happen somewhere. `Triagem` is the second, and it is not a second way to accept without reading:
 * it draws the rows §5.3 takes OUT of this panel's debt because the triager put them in front of
 * the owner, and a flag with no verdict on it would be a nag with no answer. Both use the same
 * three-verdict control from `Carimbar.tsx`, because §5.2 has three verdicts and a fourth would be
 * this map made quieter.
 * **Every row carries them, including the piles where nothing is asking to be stamped.** Three of
 * the six describe verdicts that are already given and whose repair is not a verdict at all, and
 * the first draft left them bare for that reason — which made *"changed my mind"*, one click away
 * on every other row, a mis-click with no way back that anybody could find. `map_store::stamp`
 * appends and §9.2 makes the current state the last row, so a verdict is always revisable; the only
 * thing missing was somewhere to revise it.
 *
 * **Nothing here is drawn in a colour.** Every pile below is one standing, announced by its own
 * heading and its own sentence, so a badge on each row would repeat the heading in a form a tenth
 * of readers cannot separate from the next one. It also keeps §6.1's promise for free: slice 5
 * adds *silenciado* — the triager saying it saw nothing odd, which is a claim about the triager
 * and not about the code — and there is no green here for it to end up sharing.
 */

export interface CarimbosProps {
  projectId: string;
  /** The decisions themselves, in the order the núcleo joined them. */
  junction: Junction;
  /** Where each stands, keyed by `decision_id` **as a string**, because JSON keys are strings. */
  standings: Record<string, Standing>;
  /** §5.3's header, tallied by the núcleo, and never recomputed here. */
  stamps: StampCounts;
  /**
   * What the triager said about each decision, keyed by `decision_id` **as a string**.
   *
   * Read here for exactly one thing: which of the never-stamped rows have been flagged, so this
   * panel stops drawing them. They are §5.3's `J` now, they are in front of the owner on the
   * triage panel with the reason that put them there, and a pile here listing them again would be
   * two panels answering one question — with a count above it that says a different number.
   */
  triage: Record<string, Held>;
  /**
   * §5.3's `K nunca vistas` and `J à tua espera`, which are no longer `stamps.never` and
   * `stamps.lapsed`.
   *
   * **The header would be wrong without this, and wrong in the direction that matters.** `J` is
   * `lapsed + flagged` and `K` is `never − flagged`: a flagged decision has arrived in front of the
   * owner, so counting it in both would put one decision on two lines of a header that is supposed
   * to reconcile. The núcleo builds both in one pass over the same standings this panel reads, so
   * the arithmetic has one owner — this panel's own comment promised exactly that a slice ago, and
   * it has to be honoured by reading a different field rather than by adding two.
   */
  triageCounts: TriageCounts;
  /** One fact about the whole reading: git is there and would not answer. */
  gitWouldNotAnswer: boolean;
}

/**
 * How many rows of a pile are drawn before it is summarised.
 *
 * The same number `Juncao` uses, and deliberately its own constant rather than a shared one: these
 * are two panels over two different piles, and the day one of them wants a different cap the other
 * must not move with it. Whatever is cut is counted out loud beside it — the number never leaves
 * the screen, only the rows do.
 */
const ROWS = 12;

/** The first `limit` of a pile, and how many were left out. */
function capped<T>(all: T[], limit = ROWS): { shown: T[]; hidden: number } {
  return { shown: all.slice(0, limit), hidden: Math.max(0, all.length - limit) };
}

function plural(count: number, one: string, many: string): string {
  return count === 1 ? one : many;
}

/** A decision and where it stands, which is the only pairing this panel ever draws. */
interface Stood<S extends Standing = Standing> {
  row: Anchored;
  standing: S;
}

/** A decision the owner called done. Narrowed, so the piles below can read `watch` without a guard. */
type Green = Stood<Extract<Standing, { state: "settled" }>>;

export function Carimbos({
  projectId,
  junction,
  standings,
  stamps,
  triage,
  triageCounts,
  gitWouldNotAnswer,
}: CarimbosProps) {
  /*
    `String(id)` and not `id`. The núcleo sends a `BTreeMap<i64, _>` and JSON object keys are
    strings, so a numeric lookup here reads `undefined` for every row on screen while typechecking
    perfectly — a panel that looks like it measured something and measured nothing.

    A decision the núcleo sent no standing for is read as `never`, following `Verdict::from_wire`:
    a stamp that cannot be read is no stamp, which lands the line back among the ones nobody has
    looked at. That is visible debt, and it is the one answer that claims nothing. It cannot happen
    — `standings` is built from the same decisions — and if it ever does, this errs towards the
    reading that overstates nothing.
  */
  const stood: Stood[] = junction.decisions.map((row) => ({
    row,
    standing: standings[String(row.decision_id)] ?? { state: "never" },
  }));

  const inState = <K extends Standing["state"]>(state: K) =>
    stood.filter((pair): pair is Stood<Extract<Standing, { state: K }>> =>
      pair.standing.state === state,
    );

  const lapsed = inState("lapsed");
  const partial = inState("partial");
  const withdrawn = inState("withdrawn");
  const green = inState("settled");
  /*
    The never pile, **minus the ones the triager flagged**, and that subtraction is §5.3 rather
    than tidiness: a flagged decision is in `J`, in front of the owner on the triage panel, and
    §5.3 says counting it in both places breaks the sum. What is left is exactly
    `triageCounts.unseen` — the silenced and the untriaged — so the pile below and the number above
    it are the same population, which is the property a header exists to be able to trust.
  */
  const never = inState("never").filter(
    (pair) => triage[String(pair.row.decision_id)]?.judgement !== "flagged",
  );

  const watching = (watch: Watch) => green.filter((pair) => pair.standing.watch === watch);

  return (
    <section aria-label="Your stamps" className="flex flex-col gap-6">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        What you said, and whether it still holds
      </h2>

      {/*
        §5.3's header, written as ONE sentence and not as four figures with captions. The words are
        what make each number a different fact; a figure lifted away from them is the collapse §12
        refuses, and the four would look like parts of a whole that could be divided.

        `J` and `K` are the núcleo's `triage_counts` and no longer `stamps.lapsed` and
        `stamps.never`. Slice 5's triager puts decisions in front of the owner, and those come out
        of `never` — so `J` is `lapsed + flagged` and `K` is `never − flagged`, and reading the two
        stamp fields would now put one decision on two of these four numbers. That the definition
        grows in one place was this comment's promise a slice ago; keeping it meant reading a
        different field, not adding two here.

        **A silence does NOT leave `K`, and the whole slice turns on it.** §5.1: *"o triador não
        viu nada estranho. Ninguém olhou. Não é verde."* — so a silenced decision is still debt
        nobody has given a verdict on. Subtracting it would let a triager that silences three
        hundred decisions print `0 never looked at` over a backlog nobody has read, which is §1's
        false confidence manufactured by the arithmetic of its own cure, on the one line that
        exists to be uncomfortable.
      */}
      <p className="font-display text-2xl text-text">
        {`${stamps.settled} stamped · ${stamps.partial} part-way · ${triageCounts.unseen} never looked at · ${triageCounts.waiting} on your desk`}
      </p>
      {stamps.decisions > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="max-w-prose text-xs text-text-muted">
            The third number is debt, and it is meant to be uncomfortable. On day one it is nearly
            all of them, because history does not get stamped (§10).
          </p>
          {/*
            Said once here rather than under every row that can be stamped. A pile of 350 lines
            would otherwise carry 350 copies of it, which is the wall of identical sentences this
            panel refuses on `git_would_not_answer` for the same reason: nobody reads to the bottom
            of one, and whatever mattered underneath goes unseen.
          */}
          <p className="max-w-prose text-xs text-text-muted">
            Three verdicts, and the middle one always carries a note — §5.2 makes the note the whole
            of amber, so its button waits until you have written one.
          </p>
        </div>
      ) : null}

      {gitWouldNotAnswer ? <GitSilent /> : null}

      {stamps.decisions === 0 ? (
        <NothingApproved />
      ) : (
        <>
          {stamps.settled + stamps.partial + stamps.lapsed + stamps.withdrawn === 0 ? (
            <DayOne never={triageCounts.unseen} />
          ) : null}

          {lapsed.length > 0 ? (
            <OnYourDesk
              projectId={projectId}
              rows={lapsed}
              total={stamps.lapsed}
              flagged={triageCounts.flagged}
            />
          ) : null}
          {partial.length > 0 ? (
            <PartWay projectId={projectId} rows={partial} total={stamps.partial} />
          ) : null}
          {withdrawn.length > 0 ? (
            <Withdrawn projectId={projectId} rows={withdrawn} total={stamps.withdrawn} />
          ) : null}
          {stamps.unwatched > 0 ? (
            <NeverExpires
              projectId={projectId}
              stamps={stamps}
              noAnchor={watching("no_anchor")}
              untracked={watching("untracked")}
              noRepository={watching("no_repository")}
            />
          ) : null}
          {stamps.settled > 0 ? (
            <Guessed
              projectId={projectId}
              stamps={stamps}
              guessed={watching("guessed")}
              certain={watching("watched").length}
            />
          ) : null}
          {never.length > 0 ? (
            <NeverLooked
              projectId={projectId}
              rows={never}
              total={triageCounts.unseen}
              silenced={triageCounts.silenced}
            />
          ) : null}
        </>
      )}
    </section>
  );
}

/**
 * Git is there and would not answer — once, for the whole reading.
 *
 * The route computes one digest for the union of every decision's anchors, because the alternative
 * is up to 350 process spawns per open. The cost of that choice is that a git which will not answer
 * takes every settled stamp to *unreadable* at the same instant, and this sentence is what buys the
 * owner one line instead of 350 rows saying it — a wall of noise nobody reads to the bottom of is
 * exactly how the one real lapse underneath goes unseen.
 *
 * **Says *read it again*, because this is transient.** A project with no git repository at all does
 * not arrive here — that is §11's ordinary case, it is permanent, and pile four says so in words
 * that do not ask anybody to retry something that cannot succeed.
 */
function GitSilent() {
  return (
    <p className="max-w-prose rounded-md border border-tone-pending-border bg-tone-pending-bg p-2 text-xs text-text-muted">
      Git would not answer when this map was read, so not one stamp below could be checked against
      the code. Nothing here reports a change, and nothing here reports that there was none — read
      it again in a moment.
    </p>
  );
}

/**
 * §11 and §4: nothing has been approved, so there is nothing to stamp.
 *
 * A row of zeros would read as a measurement, and nothing has been measured — no decision was
 * looked at and found unstamped, because there is no decision. The header above still prints its
 * four zeros because they are the núcleo's tally of an empty set; this says what that means.
 */
function NothingApproved() {
  return (
    <p className="max-w-prose text-sm text-text-muted">
      No decision has been approved for this project yet, so there is nothing to stamp. A stamp is
      your verdict on a line that is already in the map, and nothing gets into the map without you
      answering it first.
    </p>
  );
}

/**
 * §10, on the day the map first opens: ~350 lines and not one of them stamped.
 *
 * Written so it cannot be read as a call to migrate. §10 is explicit that a grey wall with a
 * migration ritual at the door is the fastest way to kill this, and that the backlog blocks
 * nothing — so the sentence says what is true and asks for nothing.
 */
function DayOne({ never }: { never: number }) {
  return (
    <p className="max-w-prose text-sm text-text-muted">
      Nothing here is stamped. On day one that is the whole map — all {never} of it — and §10 is
      deliberate about it: history does not get stamped, and this backlog blocks nothing.
    </p>
  );
}

/**
 * The stamps that stopped being true, and the diff §7 promises.
 *
 * First, because it is the only pile where the answer is a click. §7: *"re-carimbar é um clique
 * quando o diff é cosmético, e é o momento certo para olhar quando não é"* — and neither half of
 * that is available to somebody who cannot see what moved, so the paths are on the row.
 *
 * **"On your desk" and not "waiting on you".** §5.3 calls this `à tua espera`, and the panel one
 * step up this same screen already spends "Waiting on you" on §4's unapproved pile — two headings
 * a word apart, on one screen, meaning two different things is the confusion this mode removes.
 * The phrase also has to survive slice 5, which adds triager-flagged decisions to this number out
 * of the *never* pile: those are not stamps coming back, so nothing here may say "again".
 */
function OnYourDesk({
  projectId,
  rows,
  total,
  flagged,
}: {
  projectId: string;
  rows: Stood<Extract<Standing, { state: "lapsed" }>>[];
  total: number;
  /** The other half of §5.3's `J`, which this panel does not draw. */
  flagged: number;
}) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        On your desk
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {total} {plural(total, "stamp", "stamps")} you gave stopped being true. §7 puts the diff
        between what you stamped and what is there now on the row: re-stamping is one click when it
        is cosmetic, and it is the right moment to look when it is not.
      </p>
      {/*
        §5.3's `J` is two halves and this panel draws one of them, so the header's number is larger
        than this pile. Said out loud rather than left to be noticed: a count above a list that
        does not match its length is exactly where a reader concludes one of the two is wrong, and
        here neither is — they reach the owner for opposite reasons, one being their own green gone
        stale and the other a model asking.
      */}
      {flagged > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          The other {flagged} on your desk are not stamps of yours at all — the triager put them
          there, and they are on its own panel below with the reason it gave.
        </p>
      ) : null}
      <ul aria-label="Stamps that stopped being true" className="flex flex-col gap-2">
        {shown.map(({ row, standing }) => (
          <Line key={row.decision_id} row={row} at={standing.stamped_at}>
            <Why why={standing.why} />
            <Carimbar projectId={projectId} row={row} />
          </Line>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * Why one stamp stopped being true, in three shapes that are never the same shape.
 *
 * `moved` gets three lists and three questions, because a blob that changed asks *is this still
 * what you wanted?*, a path that appeared asks *did you ever look at this?* — §1's failure
 * verbatim, and the one nobody would have gone looking for — and a path that is gone asks *was
 * that deliberate?*.
 *
 * `unreadable` is **not** a `moved` with three empty lists and is drawn nowhere near one. It says
 * the comparison could not be made; rendering it as *everything vanished* would report a change
 * nothing measured, and rendering it as *nothing changed* would mint a green nobody checked. Those
 * are the two failures this feature exists to prevent, and *I could not look* is the only one of
 * the three answers that is true.
 */
function Why({ why }: { why: Lapse }) {
  if (why.kind === "unreadable") {
    return (
      <p className="max-w-prose text-xs text-text-muted">
        This map could not read what this stamp is watching, so it cannot say whether the code
        moved — neither that it did nor that it did not.
      </p>
    );
  }

  if (why.kind === "stale") {
    return (
      <div className="flex flex-col gap-1">
        <p className="max-w-prose text-xs text-text-muted">
          Your note is older than thirty days, so this map is asking whether it is still true.
          Amber expires by time and not by the code moving, because what rots is the note.
        </p>
        <p className="max-w-prose text-sm text-text">{why.note}</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1">
      <Moved label="changed since you stamped it — is this still what you wanted?" paths={why.changed} />
      <Moved label="appeared since you stamped it — did you ever look at these?" paths={why.added} />
      <Moved label="gone since you stamped it — was that deliberate?" paths={why.gone} />
    </div>
  );
}

/** One of the three ways an anchor set can have moved, drawn only when it happened. */
function Moved({ label, paths }: { label: string; paths: string[] }) {
  if (paths.length === 0) return null;
  const { shown, hidden } = capped(paths, 4);
  return (
    <div className="flex flex-col">
      <p className="text-xs text-text-faint">{label}</p>
      <p className="font-mono text-xs text-text-muted">
        {shown.join(" · ")}
        {hidden > 0 ? ` · and ${hidden} more not shown` : ""}
      </p>
    </div>
  );
}

/**
 * §5.2's amber, and the note that is the whole of it.
 *
 * *"falta migrar as páginas de pilar"* is worth more than the colour is, so the note is text on the
 * row. Not a `title`, not a tooltip, not behind a disclosure: a note nobody reads is amber with
 * extra steps, and the entire value of this verdict is that it converts a *didn't know* into a
 * *knew*.
 */
function PartWay({
  projectId,
  rows,
  total,
}: {
  projectId: string;
  rows: Stood<Extract<Standing, { state: "partial" }>>[];
  total: number;
}) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Part-way, and you know it
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {total} {plural(total, "decision", "decisions")} you stamped with a note. The note is on the
        row and never on a hover — §5.2 makes it the whole of amber. This verdict does not expire
        when the code moves; it expires by time, because what rots is the note.
      </p>
      <ul aria-label="Stamps you gave with a note" className="flex flex-col gap-2">
        {shown.map(({ row, standing }) => (
          <Line key={row.decision_id} row={row} at={standing.stamped_at}>
            <p className="max-w-prose text-sm text-text">{standing.note}</p>
            <Carimbar projectId={projectId} row={row} />
          </Line>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * The decisions the owner abandoned, grouped by the document that has not caught up.
 *
 * §5.2: *"Fica retirada, com o spec marcado por actualizar… Retirar é uma afirmação, não um
 * esquecimento."* This pile is the marked-for-updating half. Without it, withdrawing and deleting
 * the line are the same gesture, and the document goes on lying with nobody able to see that it
 * does — which is the failure the third verdict exists to prevent, not a tidiness question.
 *
 * **Grouped by `spec_slug` rather than listed flat**, because the work these rows imply is per
 * document: somebody opens one file and fixes everything this map says it still claims. A flat list
 * would make that a reading exercise before it is an edit.
 *
 * **It carries the controls even though nothing here is asking to be stamped**, and the first draft
 * of this panel did not — which was a trap. §7.1 says a withdrawal expires never and waits for the
 * document, so the row has no *work* attached to it; but *"changed my mind"* is one click away from
 * every other row on this screen, and a panel that offers no way back has made a mis-click
 * permanent as far as anybody using it can tell. The store appends and §9.2 makes the current state
 * the last row, so a later verdict supersedes this one — the only thing missing was somewhere to
 * give it.
 */
function Withdrawn({
  projectId,
  rows,
  total,
}: {
  projectId: string;
  rows: Stood<Extract<Standing, { state: "withdrawn" }>>[];
  total: number;
}) {
  const bySlug = new Map<string, Stood<Extract<Standing, { state: "withdrawn" }>>[]>();
  for (const pair of rows) {
    const held = bySlug.get(pair.row.spec_slug);
    if (held === undefined) bySlug.set(pair.row.spec_slug, [pair]);
    else held.push(pair);
  }

  return (
    <div className="flex flex-col gap-3">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Withdrawn, and the document has not caught up
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {total} {plural(total, "decision", "decisions")} you changed your mind about. Withdrawing is
        a statement and not a forgetting: it stops the map asking without the line disappearing in
        silence. They are grouped by the document that still claims them, because the document is
        the thing to change — and this map does not change one (§12).
      </p>
      {Array.from(bySlug.entries()).map(([slug, held]) => {
        const { shown, hidden } = capped(held);
        return (
          <div key={slug} className="flex flex-col gap-2">
            <p className="max-w-prose text-xs text-text">
              {`${slug} still claims ${held.length} ${plural(held.length, "decision", "decisions")} you have withdrawn.`}
            </p>
            <ul aria-label={`Withdrawn under ${slug}`} className="flex flex-col gap-2">
              {shown.map(({ row, standing }) => (
                <Line key={row.decision_id} row={row} at={standing.stamped_at}>
                  {standing.note === null ? null : (
                    <p className="max-w-prose text-sm text-text">{standing.note}</p>
                  )}
                  <Carimbar projectId={projectId} row={row} />
                </Line>
              ))}
            </ul>
            {hidden > 0 ? <Hidden count={hidden} /> : null}
          </div>
        );
      })}
    </div>
  );
}

/**
 * The greens that will never come back to ask, and the three different reasons they will not.
 *
 * §7 requires such a stamp be shown rather than enjoyed: a verdict that nothing can falsify is the
 * exact shape of §1's false confidence, and today it is the common case rather than the corner.
 *
 * **Three groups and never one number, because the three cures are different and one of them is
 * *nothing*.** A single line saying *these will not expire* would leave the owner unable to tell
 * which of three things they were being asked to do. `untracked` in particular gets a sentence
 * that names both of its causes and picks neither: git answers *I have no entry for that path* for
 * a gitignored file and for a file nobody has added yet, and nothing this map can see tells the
 * two apart — so sending anybody to their `.gitignore` would be a guess wearing an instruction.
 *
 * **The controls are here too, and what is wrong with these rows is not the verdict.** Nothing on
 * this pile is asking to be re-stamped — the repairs above are a citation, a git command and
 * nothing at all, none of which is a verdict. They are here because §6 gives the verdict to the
 * owner and to nobody else, and a panel that withheld the gesture on three of its six piles would
 * be deciding when they are allowed to change their mind. There is nowhere else in this mode to do
 * it.
 */
function NeverExpires({
  projectId,
  stamps,
  noAnchor,
  untracked,
  noRepository,
}: {
  projectId: string;
  stamps: StampCounts;
  noAnchor: Green[];
  untracked: Green[];
  noRepository: Green[];
}) {
  return (
    <div className="flex flex-col gap-3">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Stamped, and it will never come back to ask
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {`${stamps.unwatched} of your greens are standing on something that cannot move, so nothing will ever expire them.`}{" "}
        §7 says such a stamp is to be shown rather than enjoyed, and the three reasons below want
        three different repairs — one of which is nothing.
      </p>

      <Silence
        projectId={projectId}
        label="Greens with nothing to watch"
        rows={noAnchor}
        sentence={`${stamps.no_anchor} of them: no readable module names the decision's section, so there is nothing to watch. A citation naming the section — and the document it belongs to — is what would give this green something to expire against.`}
      />
      <Silence
        projectId={projectId}
        label="Greens whose files git does not track"
        rows={untracked}
        sentence={`${stamps.untracked} of them: modules name the section and git reports none of them. Either those paths are ignored, or nothing has been added to the repository yet, and nothing this map can see tells the two apart.`}
      />
      <Silence
        projectId={projectId}
        label="Greens in a folder with no repository"
        rows={noRepository}
        sentence={`${stamps.no_repository} of them: this project's folder is not a git repository, so there is nothing here that could ever move. That is an honest answer rather than a fault, and there is nothing to repair.`}
      />
    </div>
  );
}

/** One of the three silences, drawn only when it has rows, with its own reason above them. */
function Silence({
  projectId,
  label,
  rows,
  sentence,
}: {
  projectId: string;
  label: string;
  rows: Green[];
  sentence: string;
}) {
  if (rows.length === 0) return null;
  const { shown, hidden } = capped(rows);
  return (
    <div className="flex flex-col gap-2">
      <p className="max-w-prose text-xs text-text-muted">{sentence}</p>
      <ul aria-label={label} className="flex flex-col gap-2">
        {shown.map(({ row, standing }) => (
          <Line key={row.decision_id} row={row} at={standing.stamped_at}>
            <Carimbar projectId={projectId} row={row} />
          </Line>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * The greens whose anchor was matched by section number alone, and the ones that were not.
 *
 * §8: a `§` in a file here is a number and nothing else, and which of this project's documents it
 * points at is written down nowhere. A green over such an anchor **does** expire — which is why it
 * is not one of the silences above — but what it expires against is a guess, and it can be tripped
 * by a file that was never about this decision at all.
 *
 * **Today this is every anchored stamp in this repository**, because not one citation here names
 * its document. This number reaching zero is the scoreboard for the slice that repairs that, and a
 * better one than counting confirmed joins: it is weighted by what the owner actually stamped, so
 * it measures whether the repair reached the decisions anybody cares about.
 *
 * The certain count is printed beside it and printed even when it is zero, which it is here today.
 * A pile that showed only the doubtful ones would leave *how many of my greens are sound* to be
 * inferred by subtraction, and an inferred number on this panel is the thing the panel is against.
 */
function Guessed({
  projectId,
  stamps,
  guessed,
  certain,
}: {
  projectId: string;
  stamps: StampCounts;
  guessed: Green[];
  certain: number;
}) {
  const { shown, hidden } = capped(guessed);

  return (
    <div className="flex flex-col gap-2">
      {/*
        Named for the question rather than for the doubtful half, because both sentences below live
        under it and one of them is the reassuring one. A heading that said only "guessed" would
        make the certain count read as a footnote to a complaint.
      */}
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        What your greens are watching
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {`${stamps.guessed} of your greens are watching files matched by section number alone. Code names the section and never says which document the section belongs to, so such a stamp will expire — and it may expire because an unrelated file that happens to write the same §N about a different document changed.`}
      </p>
      <p className="max-w-prose text-xs text-text-muted">
        {`${certain} of your greens are watching an anchor this map is certain about: a readable module names the section and names the document it belongs to.`}
      </p>
      {guessed.length > 0 ? (
        <>
          <ul aria-label="Greens over a guessed anchor" className="flex flex-col gap-2">
            {shown.map(({ row, standing }) => (
              <Line key={row.decision_id} row={row} at={standing.stamped_at} paths={row.modules}>
                <Carimbar projectId={projectId} row={row} />
              </Line>
            ))}
          </ul>
          {hidden > 0 ? <Hidden count={hidden} /> : null}
        </>
      ) : null}
    </div>
  );
}

/**
 * §10's debt: the lines nobody has given a verdict on.
 *
 * Last, and counted in full. It is supposed to be large and it is supposed to be uncomfortable, and
 * §10 is explicit that it blocks nothing — so it is a pile to attack when the owner feels like it
 * and never a door they have to walk through. The buttons are here because *accessible and
 * sortable, for when you feel like attacking it* is what §10 asks for; the rows are capped because
 * 350 of them is the thousand-line plan again.
 */
function NeverLooked({
  projectId,
  rows,
  total,
  silenced,
}: {
  projectId: string;
  rows: Stood[];
  total: number;
  /** How many of them the triager silenced — still here, because a silence is not a verdict. */
  silenced: number;
}) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Nobody has looked at these
      </h3>
      {/*
        The ordering is disclosed rather than left to be inferred, and this is the pile where it
        matters: §10 asks for recency of the anchor code's last change — *"o que é que se mexeu
        desde a última vez que olhei?"* — and the núcleo now computes it, so this list arrives in
        that order and the sentence says which order it is. What it may not imply is that the order
        is total: everything whose anchors did not move inside the walk's window ties, and the
        triage panel below is where that window's size is stated.
      */}
      <p className="max-w-prose text-xs text-text-muted">
        {`${total} approved ${plural(total, "decision carries", "decisions carry")} no verdict of yours.`}{" "}
        Most recently moved first, which is the order §10 asks for — and it is a fact about the git
        log rather than a ranking: everything that has not moved lately ties.
      </p>
      {/*
        The silenced are in this pile and not in a pile of their own, which is §5.1 rather than an
        arrangement: *"o triador não viu nada estranho. **Ninguém olhou.** Não é verde."* A silence
        is a claim about the triager and about nothing else, so it changes where a decision sits in
        the queue and never whether it has been looked at.
      */}
      {silenced > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          {silenced} of them the triager silenced, and they are still counted here: silencing says
          the triager saw nothing worth your time, which is a claim about the triager and not about
          the code. Nobody has looked at them.
        </p>
      ) : null}
      <ul aria-label="Decisions nobody has stamped" className="flex flex-col gap-2">
        {shown.map(({ row }) => (
          <Line key={row.decision_id} row={row} at={null}>
            <Carimbar projectId={projectId} row={row} />
          </Line>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * One decision, in the document's own words, with whatever the pile has to add about it.
 *
 * The heading and the document slug sit above the sentence the model pulled out, because *line 3 of
 * that document* is how the owner refers to a decision after approving it. Never a summary:
 * summarising twelve lines would be the thousand-line plan again, only shorter.
 */
function Line({
  row,
  at,
  paths = [],
  children,
}: {
  row: Anchored;
  /** When it was stamped, or `null` for a row nobody has stamped. */
  at: string | null;
  paths?: string[];
  children?: ReactNode;
}) {
  const named = capped(paths, 4);

  return (
    <li className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-xs text-text-muted">{row.section}</span>
        <span className="truncate font-mono text-xs text-text-faint">{row.spec_slug}</span>
        <span className="ml-auto text-xs text-text-faint">
          {at === null ? row.kind : <RelativeTime at={at} />}
        </span>
      </div>
      <p className="max-w-prose text-sm text-text">{row.text}</p>
      {paths.length > 0 ? (
        <p className="font-mono text-xs text-text-faint">
          {named.shown.join(" · ")}
          {named.hidden > 0 ? ` · and ${named.hidden} more not shown` : ""}
        </p>
      ) : null}
      {children}
    </li>
  );
}

/**
 * What a capped list left out.
 *
 * A silent truncation is the same defect this whole feature exists to cure, so the rows are cut and
 * the number never is. The count above every list is the núcleo's, and it is the whole pile.
 */
function Hidden({ count }: { count: number }) {
  return (
    <p className="text-xs text-text-faint">
      {count} more not shown here. The count above is the whole pile.
    </p>
  );
}
