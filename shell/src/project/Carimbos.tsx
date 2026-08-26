import { useState, type ReactNode } from "react";
import { isApiRefusal } from "../data/client";
import {
  useCarimbar,
  type Anchored,
  type Junction,
  type Lapse,
  type StampCounts,
  type Standing,
  type Watch,
} from "../data/project-map";
import { RelativeTime } from "../ui";

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
 * **This is the one surface in this mode with buttons, and the only one.** `Juncao` has none
 * deliberately — a second place to accept without reading is the failure this mode replaces — but
 * stamping is the owner's act and §6 gives it to nobody else, so it has to happen somewhere.
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
  const never = inState("never");

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

        `J` is `stamps.lapsed` today and becomes a union tomorrow: slice 5's triager also puts
        decisions in front of the owner, and those come out of `never`, not out of here. The number
        is read off the núcleo's tally for exactly that reason — when the definition grows, it grows
        in one place, and this header does not need renumbering.
      */}
      <p className="font-display text-2xl text-text">
        {`${stamps.settled} stamped · ${stamps.partial} part-way · ${stamps.never} never looked at · ${stamps.lapsed} on your desk`}
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
            <DayOne never={stamps.never} />
          ) : null}

          {lapsed.length > 0 ? (
            <OnYourDesk projectId={projectId} rows={lapsed} total={stamps.lapsed} />
          ) : null}
          {partial.length > 0 ? (
            <PartWay projectId={projectId} rows={partial} total={stamps.partial} />
          ) : null}
          {withdrawn.length > 0 ? <Withdrawn rows={withdrawn} total={stamps.withdrawn} /> : null}
          {stamps.unwatched > 0 ? (
            <NeverExpires
              stamps={stamps}
              noAnchor={watching("no_anchor")}
              untracked={watching("untracked")}
              noRepository={watching("no_repository")}
            />
          ) : null}
          {stamps.settled > 0 ? (
            <Guessed
              stamps={stamps}
              guessed={watching("guessed")}
              certain={watching("watched").length}
            />
          ) : null}
          {never.length > 0 ? (
            <NeverLooked projectId={projectId} rows={never} total={stamps.never} />
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
}: {
  projectId: string;
  rows: Stood<Extract<Standing, { state: "lapsed" }>>[];
  total: number;
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
      <ul aria-label="Stamps that stopped being true" className="flex flex-col gap-2">
        {shown.map(({ row, standing }) => (
          <Line key={row.decision_id} row={row} at={standing.stamped_at}>
            <Why why={standing.why} />
            <Stamp projectId={projectId} row={row} />
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
            <Stamp projectId={projectId} row={row} />
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
 * **No buttons.** §7.1 says a withdrawal expires never and waits for the spec — it leaves the list
 * when the decision is rewritten or taken out of the document, and not before. The cure is an edit
 * to a document, and §12 keeps this map out of documents.
 */
function Withdrawn({
  rows,
  total,
}: {
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
 * **No buttons.** These are already stamped, and what is wrong with them is not the verdict. A
 * second stamp would change nothing, and offering one would suggest it might.
 */
function NeverExpires({
  stamps,
  noAnchor,
  untracked,
  noRepository,
}: {
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
        label="Greens with nothing to watch"
        rows={noAnchor}
        sentence={`${stamps.no_anchor} of them: no readable module names the decision's section, so there is nothing to watch. A citation naming the section — and the document it belongs to — is what would give this green something to expire against.`}
      />
      <Silence
        label="Greens whose files git does not track"
        rows={untracked}
        sentence={`${stamps.untracked} of them: modules name the section and git reports none of them. Either those paths are ignored, or nothing has been added to the repository yet, and nothing this map can see tells the two apart.`}
      />
      <Silence
        label="Greens in a folder with no repository"
        rows={noRepository}
        sentence={`${stamps.no_repository} of them: this project's folder is not a git repository, so there is nothing here that could ever move. That is an honest answer rather than a fault, and there is nothing to repair.`}
      />
    </div>
  );
}

/** One of the three silences, drawn only when it has rows, with its own reason above them. */
function Silence({ label, rows, sentence }: { label: string; rows: Green[]; sentence: string }) {
  if (rows.length === 0) return null;
  const { shown, hidden } = capped(rows);
  return (
    <div className="flex flex-col gap-2">
      <p className="max-w-prose text-xs text-text-muted">{sentence}</p>
      <ul aria-label={label} className="flex flex-col gap-2">
        {shown.map(({ row, standing }) => (
          <Line key={row.decision_id} row={row} at={standing.stamped_at} />
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
  stamps,
  guessed,
  certain,
}: {
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
              <Line key={row.decision_id} row={row} at={standing.stamped_at} paths={row.modules} />
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
}: {
  projectId: string;
  rows: Stood[];
  total: number;
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
        desde a última vez que olhei?"* — and nothing computes that yet, so the first twelve of 350
        are the first twelve of a document. A reader who assumed otherwise would think the rows in
        front of them were the ones that moved.
      */}
      <p className="max-w-prose text-xs text-text-muted">
        {`${total} approved ${plural(total, "decision carries", "decisions carry")} no verdict of yours.`}{" "}
        The ones shown are the first by document and line, not the ones that moved most recently —
        that order is what §10 asks for, and nothing computes it yet.
      </p>
      <ul aria-label="Decisions nobody has stamped" className="flex flex-col gap-2">
        {shown.map(({ row }) => (
          <Line key={row.decision_id} row={row} at={null}>
            <Stamp projectId={projectId} row={row} />
          </Line>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * The three verdicts, on one line (§5.2).
 *
 * The mutation is per row, the way the pile one panel up gives each line its own: a refusal belongs
 * to the row it was refused about, and one shared mutation would put the last failure's sentence
 * under whichever row happened to be looking.
 *
 * **The note field is above the buttons and is always there.** The table refuses an empty amber and
 * the daemon answers `400`, so a middle button with nowhere to type is a button that can only fail
 * — a worse answer than a field. It is *disabled* until something is written rather than hidden:
 * hiding it would be this panel deciding which of the three verdicts the owner is allowed to give,
 * and §6 reserves that to them. Why it waits is said once at the top of the panel and not under
 * every row, for the reason `git_would_not_answer` gets one sentence: 350 copies of a true sentence
 * is a wall nobody reads to the bottom of.
 *
 * The names carry the document and the section, because a screen full of buttons all called
 * "part-way" is a screen full of identical announcements to anybody not looking at it, and this is
 * a surface whose whole promise is that you know what you just answered.
 */
function Stamp({ projectId, row }: { projectId: string; row: Anchored }) {
  const [note, setNote] = useState("");
  const carimbar = useCarimbar(projectId);

  const name = `${row.spec_slug} ${row.section}`;
  // Trimmed here against a daemon that trims before it checks, so the button is disabled for
  // exactly the notes the núcleo would refuse and for no others.
  const written = note.trim();
  const send = (verdict: "settled" | "partial" | "withdrawn") =>
    carimbar.mutate({
      decisionId: row.decision_id,
      verdict,
      // `null` and never `""`: *said nothing* and *said the empty string* are different, and the
      // núcleo stores the first as NULL.
      note: written === "" ? null : written,
    });

  return (
    <div className="flex flex-col gap-2">
      <input
        type="text"
        aria-label={`note for ${name}`}
        value={note}
        onChange={(event) => setNote(event.target.value)}
        placeholder="what is missing, in your words"
        className="rounded-md border border-border bg-surface-sunken px-2 py-1 text-xs text-text placeholder:text-text-faint"
      />
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          aria-label={`stamp ${name} as what you want`}
          disabled={carimbar.isPending}
          onClick={() => send("settled")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          as I want it
        </button>
        <button
          type="button"
          aria-label={`stamp ${name} as part-way`}
          disabled={carimbar.isPending || written === ""}
          onClick={() => send("partial")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          part-way, and I know
        </button>
        <button
          type="button"
          aria-label={`stamp ${name} as changed your mind`}
          disabled={carimbar.isPending}
          onClick={() => send("withdrawn")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          changed my mind
        </button>
      </div>
      {carimbar.isError ? <Refused error={carimbar.error} /> : null}
    </div>
  );
}

/**
 * Why a verdict did not land, with the row still on screen.
 *
 * The `503` is the sentence that had to be written carefully. It means git is there and would not
 * say what this decision is anchored to, and §7.1 makes *está como quero* the only verdict the code
 * moving can falsify — so it is the only one that may not be recorded without knowing what it is
 * watching. It is transient, a second attempt works, and the owner did nothing wrong. Copy that
 * read as a failure would put the blame for a busy git on the person who pressed the button, which
 * is the opposite of what this feature is buying.
 */
function Refused({ error }: { error: unknown }) {
  const box =
    "max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted";

  if (!isApiRefusal(error)) {
    return <p className={box}>The núcleo did not answer, so nothing was recorded.</p>;
  }

  if (error.status === 503) {
    return (
      <p className={box}>
        Git would not say what this decision is anchored to just now, so the green was not
        recorded. Nothing you asked for was wrong — try again in a moment.
      </p>
    );
  }
  if (error.status === 404) {
    return (
      <p className={box}>
        That decision is not yours to stamp now — it belongs to another project, or nobody approved
        it.
      </p>
    );
  }
  if (error.status === 400) {
    return <p className={box}>An amber needs a note, and this one arrived empty.</p>;
  }
  return <p className={box}>{error.detail}</p>;
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
