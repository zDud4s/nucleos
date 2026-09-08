// §spec mapa-do-projeto
import { useState, type ReactNode } from "react";
import { isApiRefusal } from "../data/client";
import {
  useSilencedPile,
  useTriage,
  type Age,
  type Anchored,
  type Brain,
  type Held,
  type Junction,
  type Recency,
  type Silencing,
  type TriageCounts,
  type TriageReport,
} from "../data/project-map";
import { RelativeTime } from "../ui";
import { GiveStamp } from "./GiveStamp";

/**
 * The triager: what a model thought was worth the owner's eyes, and what it decided was not.
 *
 * The third axis. `JunctionPanel` says what can be known without anybody looking, `StampsPanel` says what the
 * owner said and whether it is still true, and this says what a **model** thought — three readings
 * that §5 refuses to flatten, because *"achatá-las numa só punha o triador e o dono a falar pela
 * mesma boca"*.
 *
 * **This panel owns two of §5.1's four derived states and draws neither of the other two.** *À
 * espera* and *silenciado* are the triager's; *declarado sem código* and *código sem dono* come out
 * of the junction and `JunctionPanel` already draws them. Two panels answering one question is the
 * confusion this mode exists to remove.
 *
 * **Nothing here is drawn in a colour, and that is §6.1 rather than restraint.** *"Se colapsassem,
 * a autoridade que foi retirada ao modelo era-lhe devolvida pela porta da renderização — e o mapa
 * passava a ser a falsa confiança de novo, agora com autoridade de semáforo."* The safest reading of
 * that paragraph is not *a different green*; it is that a silence has no positive visual weight at
 * all, because a silence is a claim about the **triager** — *sem sinal de problema* — and never a
 * claim about the code. `StampsPanel` reached the same place from the other side and for the same
 * reason: every pile is announced by its own heading and its own sentence, which is a form nobody
 * has to be able to separate two hues to read.
 *
 * **A silence buys exactly one thing: not being in §5.3's `J`.** It does not leave `K`. §5.1 is
 * explicit — *"o triador não viu nada estranho. **Ninguém olhou.** Não é verde."* — and the panel
 * says so in words at the point a reader is most likely to mistake a shrinking queue for progress.
 *
 * **The reason and the model are on the row, never behind a hover.** §6.2: the pile is readable
 * *"com a razão de cada silenciamento e o modelo que o produziu"*, because *"um triador que silencia
 * o que não devia é um bug do triador, e um bug só é corrigível se for visível"*. §13 rates that a
 * **real** residual risk whose only mitigation is this pile staying visible, and a bug report behind
 * a click is one nobody reads.
 *
 * **No percentage, no bar, no score** (§12). This panel holds three buckets of one population and a
 * report of seven counts, which is everything a ratio needs — and *"um número único é exactamente o
 * colapso que o §5 proíbe"*. Every number below keeps the words that say which fact it is.
 */

export interface TriagePanelProps {
  projectId: string;
  /** The decisions, already in §10's order — the núcleo sorts before it answers. */
  junction: Junction;
  /** What the triager said, keyed by `decision_id` **as a string**, because JSON keys are strings. */
  triage: Record<string, Held>;
  /** §5.3's `K` and `J`, tallied by the núcleo and never recomputed here. */
  counts: TriageCounts;
  /** §10's ordering, and how far the walk that produced it could see. */
  recency: Recency;
  /**
   * When the triager last answered anything here, or `null` if it never has.
   *
   * **A fact and no longer an inference.** This panel used to conclude *never run* from two empty
   * piles and hedge about it, because both of those describe what is true NOW: a run that flagged
   * everything and whose answers have since gone stale empties them, and that is a different thing
   * from nobody ever having pressed the button. The núcleo answers the question directly now, so
   * the hedge is gone with it.
   */
  lastTriagedAt: string | null;
}

/**
 * How many rows of a pile are drawn before it is summarised.
 *
 * The number `JunctionPanel` and `StampsPanel` both use, and deliberately its own constant rather than a
 * shared one: three panels over three different piles, and the day one of them wants a different cap
 * the others must not move with it. Whatever is cut is counted out loud beside it.
 */
const ROWS = 12;

/** The first `limit` of a pile, and how many were left out. */
function capped<T>(all: T[], limit = ROWS): { shown: T[]; hidden: number } {
  return { shown: all.slice(0, limit), hidden: Math.max(0, all.length - limit) };
}

function plural(count: number, one: string, many: string): string {
  return count === 1 ? one : many;
}

/** A decision and what the triager said about it, which is the only pairing this panel draws. */
interface Judged {
  row: Anchored;
  held: Held;
}

export function TriagePanel({
  projectId,
  junction,
  triage,
  counts,
  recency,
  lastTriagedAt,
}: TriagePanelProps) {
  const pile = useSilencedPile(projectId);

  /*
    `String(id)` and not `id`. The núcleo sends a `BTreeMap<i64, _>` and JSON object keys are
    strings, so a numeric lookup reads `undefined` for every row on screen while typechecking
    perfectly — a panel that looks like it measured something and measured nothing.

    A decision with no judgement is simply absent from these two piles, which is what it is: the
    núcleo has already dropped the judgements that no longer describe this map, and what is left
    over is *nobody asked yet, or the answer went stale*. That is the untriaged number below.
  */
  const judged: Judged[] = junction.decisions.flatMap((row) => {
    const held = triage[String(row.decision_id)];
    return held === undefined ? [] : [{ row, held }];
  });
  const flagged = judged.filter((pair) => pair.held.judgement === "flagged");
  const silenced = judged.filter((pair) => pair.held.judgement === "silenced");

  /*
    §6.2's pile minus what the two lists above already show, and the subtraction is the whole
    reason both exist. `GET /map` answers *what is true now* and therefore drops a judgement whose
    decision has since been stamped, whose anchor code has since moved, or whose decision was
    retired; `GET /map/silenced` answers *what the triager did* and drops none of the three. Each of
    those is a silencing worth MORE afterwards rather than less — a stamp is the evidence the
    silence was premature — so they are shown, under a heading that says why they are not above.

    Keyed on the decision AND on being the newest row for it, because the pile is every silencing
    ever written: a decision silenced twice has one current row up there and an older one down here,
    and dropping the older one by decision id would delete a record §6.2 asks for by name — *"a
    razão de **cada** silenciamento"*.
  */
  const met = new Set<number>();
  const superseded = (pile.data?.rows ?? []).filter((entry) => {
    const newest = !met.has(entry.decision_id);
    met.add(entry.decision_id);
    return !(newest && triage[String(entry.decision_id)]?.judgement === "silenced");
  });

  /*
    A fact, read off one field, and no longer inferred from two empty piles. The inference was
    wrong in a way nothing on screen would have shown: `triage` and the silenced pile both describe
    what is true NOW, so a run that flagged everything and whose answers have since gone stale
    empties both — and the panel would have said *never run* over a project that had been triaged
    that morning. It hedged in words because that was the strongest true sentence available to it;
    the núcleo answers the question now, so the hedge is gone.
  */
  const neverRun = lastTriagedAt === null;

  return (
    <section aria-label="The triager" className="flex flex-col gap-6">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        What a model thought was worth your eyes
      </h2>

      <p className="max-w-prose text-sm text-text-muted">
        A model reads one decision at a time, with the mechanical evidence beside it, and answers a
        single question: does this deserve your eyes? It may say yes, and it may silence with a
        reason on record. It may never turn anything green — that is your word and §6 takes it away
        from the model on purpose.
      </p>

      {neverRun ? (
        <p className="max-w-prose text-sm text-text-muted">
          The triager has never run on this project. Nothing below is an empty pile that a model
          looked at and found nothing in — nothing has been looked at, which is a different answer
          and the one that is true.
        </p>
      ) : null}

      {neverRun ? null : (
        <p className="max-w-prose text-xs text-text-muted">
          The triager last answered <RelativeTime at={lastTriagedAt} />. Everything below is what
          that answer became when it was read against the code as it stands now — which is not the
          same thing, and is why a judgement can be here, gone, or unchecked.
        </p>
      )}

      <Run projectId={projectId} />

      <Ordering window={recency.window} />

      {flagged.length > 0 ? (
        <Flagged projectId={projectId} rows={flagged} recency={recency} />
      ) : null}
      {silenced.length > 0 ? <Silenced rows={silenced} recency={recency} /> : null}

      <Debt counts={counts} />

      <Record query={pile} superseded={superseded} />
    </section>
  );
}

/**
 * The run, and the brain that pays for it.
 *
 * **The brain is chosen here, per press, and is never a setting** — the argument `ExtractSpec`
 * makes about the same choice: a setting turns the núcleo's refusal to substitute one brain for
 * another into somebody's permanent default. `cloud` is preselected because it is the one this
 * machine is certain to be able to serve.
 *
 * **This costs one model call per decision, which is the thing to say before somebody presses it
 * and not after.** The núcleo caps the batch, which is what keeps a three-hundred-row backlog from
 * being one press; the cap is also why the report below has to be read rather than glanced at.
 */
function Run({ projectId }: { projectId: string }) {
  const [brain, setBrain] = useState<Brain>("cloud");
  const triage = useTriage(projectId);

  return (
    <div className="flex flex-col gap-3">
      <div role="group" aria-label="Which brain triages" className="flex flex-wrap gap-2">
        {(["cloud", "local"] as Brain[]).map((candidate) => (
          <button
            key={candidate}
            type="button"
            aria-pressed={candidate === brain}
            disabled={triage.isPending}
            onClick={() => setBrain(candidate)}
            className={
              candidate === brain
                ? "rounded-md border border-border-strong bg-surface-raised px-3 py-1.5 text-sm text-text"
                : "rounded-md border border-border px-3 py-1.5 text-sm text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
            }
          >
            {candidate}
          </button>
        ))}
      </div>
      <p className="max-w-prose text-xs text-text-faint">
        {brain === "cloud"
          ? "The cloud brain answers through the CLI installed on this machine. One question per decision, billed, and the decisions leave the machine."
          : "The local brain answers through the model on this machine. Nothing leaves and nothing is billed — and where no such model is set up, the núcleo refuses rather than quietly asking the cloud instead."}
      </p>

      <div>
        <button
          type="button"
          disabled={triage.isPending}
          onClick={() => triage.mutate({ brain })}
          className="rounded-md border border-border px-3 py-1.5 text-sm text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          run the triager with the {brain} brain
        </button>
      </div>

      {/*
        Said out loud, for `ExtractSpec`'s reason: the route is synchronous and a batch of model
        calls is minutes, so a surface that went quiet would look broken at exactly the moment it is
        working.
      */}
      {triage.isPending ? (
        <p className="max-w-prose text-xs text-text-muted">
          Asking the {brain} brain about the decisions nobody has stamped. That is one question per
          decision and the answer arrives only when the batch has finished.
        </p>
      ) : null}

      {triage.isError ? <Failed error={triage.error} /> : null}
      {triage.isSuccess && triage.data !== undefined ? <Report report={triage.data} /> : null}
    </div>
  );
}

/**
 * What the run did, and — the half that is easy to leave out — what it did **not**.
 *
 * **Every count gets a sentence, and `left_over` gets one whether or not anything was left over.**
 * The batch is capped and this repository normally saturates it: 17 of the 109 `§`-naming files were
 * touched in the last 20 commits, and an anchor set is several files. A run that truncated and said
 * nothing reads as *covered everything* when it did not, which is §1's failure produced by the
 * feature built to cure it. So the sentence is unconditional: silence about a cap is the cap being
 * invisible.
 *
 * **`unreadable` and `unanswered` never share a line**, because they send whoever is debugging to
 * different places: a model that answered something nobody could parse is a question that needs
 * rewriting, and a run that failed is a machine that needs looking at. Lumping them sends somebody
 * to rewrite a question that was never asked.
 *
 * **And *nothing was stale* is not *there was nothing to look at*.** Those are different facts about
 * a project, and a report that only listed its successes would make the second look like the first.
 */
function Report({ report }: { report: TriageReport }) {
  const lines: string[] = [];

  lines.push(
    report.in_scope === 0
      ? "Nothing was in scope: every approved decision already carries a verdict of yours, and the triager only ever looks at the ones that do not."
      : `${report.in_scope} ${plural(report.in_scope, "decision was", "decisions were")} in scope — everything nobody has stamped.`,
  );
  if (report.already_current > 0) {
    lines.push(
      `${report.already_current} already carried a judgement about inputs that have not moved since, so nothing was asked about them and nothing was spent.`,
    );
  }
  lines.push(
    report.judged === 0
      ? "Nothing new was judged, so the table is exactly as it was."
      : `${report.judged} ${plural(report.judged, "was", "were")} answered and written down.`,
  );
  if (report.unreadable > 0) {
    lines.push(
      `${report.unreadable} came back as something that was not one of the two answers. Each of those was written down as a flag by NucleOS rather than dropped, so it costs you a look instead of disappearing — an answer nobody can read is a question that needs rewriting.`,
    );
  }
  if (report.unanswered > 0) {
    lines.push(
      `${report.unanswered} got no answer at all: the run itself failed on them. That is a machine that did not reply, and not a question it could not read.`,
    );
  }
  if (report.unreadable_anchors > 0) {
    lines.push(
      `${report.unreadable_anchors} were skipped because git would not say what their anchor code is. Nothing was written for them, so nothing has to be bought again once git answers.`,
    );
  }
  if (report.vanished > 0) {
    lines.push(
      `${report.vanished} stopped being decisions this map holds between the reading and the write, which is not supposed to be able to happen.`,
    );
  }
  lines.push(
    report.left_over === 0
      ? "Nothing was left over: the batch cap did not stop this run."
      : `${report.left_over} were stale and the batch cap stopped the run before reaching them. Press it again for the next batch.`,
  );

  return (
    <ul aria-label="What the run did" className="flex max-w-prose flex-col gap-1">
      {lines.map((line) => (
        <li key={line} className="text-xs text-text-muted">
          {line}
        </li>
      ))}
    </ul>
  );
}

/**
 * Why the run did not happen, as what it actually was.
 *
 * **503 gets its own sentence and always will.** It is the núcleo refusing to substitute one brain
 * for another, which is the single most important property of the choice above; folded into a
 * generic failure it would read as a broken daemon, and the owner would learn to distrust the one
 * refusal protecting both their bill and their code.
 *
 * **There is deliberately no 502 here**, and the absence is the route's design rather than an
 * omission: by the time a model fails, rows have been written, so the route answers `200` with a
 * report that says how many failed. A status that discarded the report would hide work actually
 * done.
 */
function Failed({ error }: { error: unknown }) {
  const box =
    "max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted";

  if (!isApiRefusal(error)) {
    return <p className={box}>The núcleo did not answer, so nothing was triaged and nothing was recorded.</p>;
  }

  const sentence =
    error.status === 503
      ? "This machine has no local model configured, so the local brain has nothing to answer with. The núcleo refuses rather than spending a cloud you did not ask for — which is the whole reason the choice is yours to make."
      : error.status === 404
        ? "This project is not there any more, or its folder cannot be read."
        : error.status === 422
          ? "The núcleo could not read which brain was asked for."
          : error.detail;

  return <p className={box}>{sentence}</p>;
}

/**
 * §10's ordering, and the honest sentence about how far it can see.
 *
 * *"Dentro do que chega, a ordem é por recência de alteração do código âncora… Recência é um facto
 * do git."* One walk of the log is what makes that cheap, and the window is what makes it
 * approximate: everything older than the window ties, and so does everything with nothing to move.
 * A panel that drew the list without saying so would present a tail nothing measured with the same
 * confidence as the head — which is §1's failure arriving through the ordering rather than through
 * the copy.
 */
function Ordering({ window }: { window: number | null }) {
  if (window === null) {
    return (
      <p className="max-w-prose text-xs text-text-muted">
        Git would not say when anything last moved, so nothing below is in §10&rsquo;s order at all
        — what you are looking at is the map&rsquo;s own order, by document and line. That is a fact
        about this reading and not about the project; read it again in a moment.
      </p>
    );
  }
  return (
    <p className="max-w-prose text-xs text-text-muted">
      Ordered by what moved in the last {window} commits, which is one walk of the git log and the
      whole of what this order knows. A decision whose anchor code last moved a thousand commits ago
      and one whose anchor code has never moved sort the same here.
    </p>
  );
}

/**
 * §5.1's *à espera*: the decisions a model thought deserve the owner's eyes.
 *
 * **The only pile on this panel carrying the verdict control, and that is not generosity.** §5.3
 * takes a flagged decision out of `K` because it has arrived, so `StampsPanel` stops drawing it — and a
 * flag with nowhere to answer it would be a nag with no answer, which is how somebody learns to stop
 * reading a queue. The silenced pile below deliberately has none: those rows are still counted in
 * the debt and `StampsPanel` still draws them, and two stamping controls for one decision on one screen
 * is the confusion this mode removes.
 *
 * **The way out is a stamp and never a dismissal.** A flag stays until its anchor code moves, which
 * looks like a nag with no answer until you notice the answer is §5.2's *a meio, e eu sei* — whose
 * stated purpose is *"converte um não sabia num sabia, que é metade da cura"*. A dismiss button would
 * be that *não sabia* kept and made quiet, and a fourth verdict would be §5.2 widened in the one
 * direction it may never be widened in.
 */
function Flagged({
  projectId,
  rows,
  recency,
}: {
  projectId: string;
  rows: Judged[];
  recency: Recency;
}) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        The triager asked for your eyes
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {rows.length} {plural(rows.length, "decision", "decisions")} nobody had stamped, which a
        model thought you should look at. What it says is that something looked odd to it — never
        that anything is wrong, and never that anything is right.
      </p>
      <p className="max-w-prose text-xs text-text-muted">
        There is no dismiss button here and there must not be one. A flag you have read and decided
        is noise leaves through &ldquo;part-way, and I know&rdquo;, with a note saying so: that
        verdict exists to turn a didn&rsquo;t-know into a knew, which is half the cure. Dismissing
        without saying anything is the didn&rsquo;t-know this map is here to convert.
      </p>
      <Banded rows={shown} recency={recency} label="Decisions the triager flagged">
        {(pair) => <GiveStamp projectId={projectId} row={pair.row} />}
      </Banded>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * §5.1's *silenciado*: what a model decided is not worth the owner's time.
 *
 * **Not a verdict, not a colour, and not progress.** §6.1 spends a paragraph on why this may not
 * share a green with a stamp, and §13 rates *o triador silencia o que devia mostrar* a **real**
 * residual risk whose only mitigation is that this stays readable. So the reason and the model are
 * on every row: the repair for a triager that silences too much is to stop using that triager, and
 * that is not a decision anybody can take about a pile that will not say who filled it.
 */
function Silenced({ rows, recency }: { rows: Judged[]; recency: Recency }) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Silenced, which is not a verdict
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {rows.length} {plural(rows.length, "decision", "decisions")} the triager saw no sign of a
        problem in. That is a claim about the triager and about nothing else. Nobody looked. It is
        not green, and it is still counted in the debt below.
      </p>
      <p className="max-w-prose text-xs text-text-muted">
        Read them when you want to know whether the triager is any good. One that silences what it
        should have shown you is a bug in the triager, and the reason it gave is the only way to
        find it.
      </p>
      <Banded rows={shown} recency={recency} label="Decisions the triager silenced" />
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * §5.3's `K`, said where a reader is most likely to mistake a shrinking queue for progress.
 *
 * **`unseen` and never `untriaged`, and the two look interchangeable until a triager silences three
 * hundred decisions.** A silence does not leave `K`: §5.1 is explicit that *silenciado* is *"o
 * triador não viu nada estranho. **Ninguém olhou.** Não é verde."*, so it is still debt nobody has
 * given a verdict on. Printing `untriaged` here would show the debt collapsing to zero over a
 * backlog nobody has read — §1's false confidence, manufactured by the arithmetic of its own cure,
 * with the sum still reconciling perfectly.
 *
 * The header of `StampsPanel` prints this same number, and the repetition is deliberate rather than
 * an oversight: it is one field read twice, so the two cannot disagree, and this is the one place on
 * the screen where somebody has just watched a queue empty and needs to be told what that did not
 * mean.
 */
function Debt({ counts }: { counts: TriageCounts }) {
  /*
    `unchecked` is in the guard as well as in the body, and it is not the same population as the
    three above it: it is a subset of `flagged + silenced`, so a project whose every judgement is a
    flag has `unseen` at zero and can still be holding judgements nothing could re-verify. Gating
    the whole block on the debt figure dropped that number silently — which is a count of what this
    map could not confirm, disappearing exactly when there is least else on screen to notice it by.
  */
  if (counts.unseen === 0 && counts.unchecked === 0) return null;

  return (
    <div className="flex flex-col gap-1">
      {counts.unseen > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          {`${counts.unseen} ${plural(counts.unseen, "decision", "decisions")} nobody has ever given a verdict on.`}{" "}
          That is §5.3&rsquo;s debt, it is meant to be uncomfortable, and nothing on this panel
          makes it smaller.
        </p>
      ) : null}
      {counts.silenced > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          {counts.silenced} of them the triager silenced, and that leaves the number above exactly
          where it was. Silencing buys one thing: not being in the queue on your desk. It never buys
          having been looked at.
        </p>
      ) : null}
      {counts.untriaged > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          {counts.untriaged} of them carry no judgement at all — never asked about, or asked about
          before their code moved, which makes the old answer an opinion of a file it never saw.
        </p>
      ) : null}
      {counts.unchecked > 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          {counts.unchecked} of the judgements above could not be re-checked against the code as it
          stands, because git would not say what their anchor code is. They still count where they
          are; what is missing is the confirmation that they are still about the same thing.
        </p>
      ) : null}
    </div>
  );
}

/**
 * §6.2's pile, minus what the map itself is still holding.
 *
 * **Two doors onto one table, and neither may be made to look like the other.** `GET /map` answers
 * *what is true now* and drops a judgement whose decision was stamped since, whose code moved since,
 * or whose decision was retired. This one answers *what the triager did* and drops none of the
 * three, because each of those makes a silencing worth **more**: a stamp is the evidence the silence
 * was premature, a reason written about code that has since moved is the one somebody should
 * reread, and a silencing that vanished with its decision is a bug report deleted by its own
 * subject.
 *
 * Its own query, so a map that failed to read a project's folder does not take §6.2's *sempre
 * acessível* down with it.
 */
function Record({
  query,
  superseded,
}: {
  query: ReturnType<typeof useSilencedPile>;
  superseded: Silencing[];
}) {
  if (query.isError) {
    return (
      <p className="max-w-prose text-xs text-text-faint">
        The núcleo could not say what the triager has silenced in this project, so §6.2&rsquo;s
        record is not on screen. What is above is what the map itself is holding, which is less than
        the whole of it.
      </p>
    );
  }
  if (query.data === undefined) {
    return <p className="text-xs text-text-faint">Reading what the triager has silenced…</p>;
  }
  const { rows, total } = query.data;
  if (total === 0) return null;

  // What the núcleo's own cap left out, kept apart from what this panel's cap leaves out. Two
  // truncations sit between the table and the screen and they are not the same one: the route stops
  // at `SILENCED_PAGE` and this list stops at twelve, and a single "and N more" would let a reader
  // believe the rest is one click away when part of it was never sent.
  const uncollected = Math.max(0, total - rows.length);
  const { shown, hidden } = capped(superseded);

  return (
    <div className="flex flex-col gap-2 border-t border-border pt-4">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Every silencing this project has ever had
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        {total} {plural(total, "silencing is", "silencings are")} on record. The list above is only
        the ones that still describe the map: a silencing about a decision you have since stamped,
        about code that has since moved, or about a decision that was retired is not an answer about
        the map as it stands — and it is worth reading more afterwards, not less.
      </p>
      {/*
        The núcleo's cap, said unconditionally. §6.2 asks for the pile to be *sempre acessível* and
        the route answers the newest page of it, so the sentence a reader needs is not *here is the
        pile* but *here is how much of it this is* — the same bargain the run's `left_over` strikes,
        and the reason the payload carries a total that is not the length of what it sent.
      */}
      <p className="max-w-prose text-xs text-text-faint">
        {uncollected === 0
          ? "Every one of them came back on this reading — nothing was left behind by the daemon's own limit."
          : `${uncollected} older ${plural(uncollected, "one is", "ones are")} not on this reading at all: the daemon answers the newest ${rows.length} and keeps the rest. They are in the table, and this list is not the whole record.`}
      </p>
      {superseded.length === 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          All of the ones that came back still describe the map, so there is nothing here you have
          not already seen above.
        </p>
      ) : (
        <>
          <div
            role="group"
            aria-label="Silencings this map no longer holds"
            className="flex flex-col gap-2"
          >
            <ul className="flex flex-col gap-2">
              {shown.map((entry) => (
                <li
                  key={`${entry.decision_id}-${entry.computed_at}`}
                  className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-3"
                >
                  <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                    <span className="text-xs text-text-muted">{entry.section}</span>
                    <span className="truncate font-mono text-xs text-text-faint">
                      {entry.spec_slug}
                    </span>
                    <span className="ml-auto text-xs text-text-faint">
                      <RelativeTime at={entry.computed_at} />
                    </span>
                  </div>
                  <p className="max-w-prose text-sm text-text">{entry.text}</p>
                  <p className="max-w-prose text-sm text-text">{entry.reason}</p>
                  {/*
                    `false`, and stated rather than read off a field, because the field is gone from
                    this payload and its absence is the correction: `unreadable_flag` may only ever
                    reach *flagged* and this route answers the silenced, so no row here can carry the
                    daemon's mark. A field that can never be true is a promise somebody eventually
                    relies on — and it was being relied on for the wrong pile, while the flags, which
                    can carry the mark, had nothing.
                  */}
                  <Whose model={entry.model} machineWritten={false} />
                  {entry.retired ? (
                    <p className="max-w-prose text-xs text-text-muted">
                      That decision has since been retired, so it is not in the map any more. The
                      silencing is kept: a record deleted by its own subject is not one.
                    </p>
                  ) : null}
                </li>
              ))}
            </ul>
          </div>
          {hidden > 0 ? <Hidden count={hidden} /> : null}
        </>
      )}
    </div>
  );
}

/**
 * One pile, cut into the bands §10's ordering actually produces.
 *
 * **A tie has to render as a tie.** Measured against this repository: 71 of 80 decisions land inside
 * the window carrying **19 distinct timestamps between them, and the top eighteen share one**. A
 * flat ranked list would imply an ordering that is not there, and the ordering is offered as *um
 * facto do git* — so what is drawn is bands of equal recency, in order, each saying what it is.
 *
 * The four silences behind the timestamps get four different sentences rather than one, because they
 * are four different facts: not moved lately, nothing to move, named only where this map cannot
 * read, and git would not say. A screen rendering them identically is the false confidence of §1
 * arriving through the rendering door §6.1 watches.
 *
 * Grouped by band rather than by consecutive run, because `Unanchored` and `ForeignOnly` share a
 * rank in the núcleo's sort and therefore interleave: run-based banding would cut those two into a
 * dozen bands of one row, which is a truthful ordering rendered as noise.
 */
function Banded({
  rows,
  recency,
  label,
  children,
}: {
  rows: Judged[];
  recency: Recency;
  label: string;
  children?: (pair: Judged) => ReactNode;
}) {
  const bands: { key: string; age: Age; rows: Judged[] }[] = [];
  const index = new Map<string, { key: string; age: Age; rows: Judged[] }>();
  for (const pair of rows) {
    // Absent is impossible — `ages` is built from these very decisions — and `unknown` is the value
    // that is safe to be wrong with: it claims nothing about git and sorts among the rows the
    // ordering already cannot speak for.
    const age = recency.ages[String(pair.row.decision_id)] ?? { state: "unknown" };
    const key = age.state === "moved" ? `moved:${age.at}` : age.state;
    let band = index.get(key);
    if (band === undefined) {
      band = { key, age, rows: [] };
      index.set(key, band);
      bands.push(band);
    }
    band.rows.push(pair);
  }

  return (
    <div role="group" aria-label={label} className="flex flex-col gap-3">
      {bands.map((band) => (
        <div key={band.key} className="flex flex-col gap-2">
          <BandSentence age={band.age} count={band.rows.length} window={recency.window} />
          <ul className="flex flex-col gap-2">
            {band.rows.map((pair) => (
              <Line key={pair.row.decision_id} pair={pair}>
                {children?.(pair)}
              </Line>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

/** What one band of equal recency is, in the words that fact deserves. */
function BandSentence({ age, count, window }: { age: Age; count: number; window: number | null }) {
  const style = "max-w-prose text-xs text-text-faint";

  if (age.state === "moved") {
    const at = new Date(age.at * 1000).toISOString();
    if (count === 1) {
      return (
        <p className={style}>
          Anchor code last moved <RelativeTime at={at} />.
        </p>
      );
    }
    return (
      <p className={style}>
        {count} whose anchor code last moved at the same moment (<RelativeTime at={at} />) — nothing
        here orders them against each other. That is the ordinary case rather than a coincidence
        today: while no citation says which document its § belongs to, a decision&rsquo;s anchor set
        is every file naming that section number, so most of them are dated by whichever of dozens
        of files was touched last.
      </p>
    );
  }
  if (age.state === "older") {
    return (
      <p className={style}>
        {count} whose anchor code did not move
        {window === null ? " inside the walk" : ` in the last ${window} commits`} — this walk cannot
        see further back, so nothing here says which of them is older than which.
      </p>
    );
  }
  if (age.state === "unanchored") {
    return (
      <p className={style}>
        {count} with nothing to move: no file this map can read, and no file it cannot read, names
        the section at all. That is a different fact from having sat still, and it sorts last for
        that reason.
      </p>
    );
  }
  if (age.state === "foreign_only") {
    return (
      <p className={style}>
        {count} named only by code this map cannot read — a Go sidecar, or a file beside one.
        Something out there does name the section; nothing here can open that file and say what it
        does with it. The map cannot tell, which is not the same answer as having looked and found
        the section unclaimed.
      </p>
    );
  }
  return (
    <p className={style}>
      {count} whose last movement git would not say, so nothing here is ordered by recency.
    </p>
  );
}

/**
 * One decision, what the triager said about it, and who said it.
 *
 * The heading and the document slug sit above the sentence the model pulled out, because *line 3 of
 * that document* is how the owner refers to a decision after approving it. Never a summary:
 * summarising twelve lines would be the thousand-line plan again, only shorter.
 *
 * **The reason is text on the row and never a `title`, a tooltip or a disclosure.** §6.2's entire
 * argument is that a triager silencing what it should not is a bug and *"um bug só é corrigível se
 * for visível"* — visible means readable by somebody who is not going to click three hundred times.
 */
function Line({ pair, children }: { pair: Judged; children?: ReactNode }) {
  const { row, held } = pair;

  return (
    <li className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-xs text-text-muted">{row.section}</span>
        <span className="truncate font-mono text-xs text-text-faint">{row.spec_slug}</span>
        <span className="ml-auto text-xs text-text-faint">
          <RelativeTime at={held.computed_at} />
        </span>
      </div>
      <p className="max-w-prose text-sm text-text">{row.text}</p>
      <p className="max-w-prose text-sm text-text">{held.reason}</p>
      <Whose model={held.model} machineWritten={held.machine_written} />
      {held.checked ? null : (
        <p className="max-w-prose text-xs text-text-muted">
          This map could not re-check what the judgement was about — git would not say what the
          anchor code is. So it neither still stands nor has expired: it is an answer nobody could
          test just now, kept because dropping it would read as nobody ever having looked.
        </p>
      )}
      {children}
    </li>
  );
}

/**
 * Who wrote the sentence above it — and the one case where that is not the model.
 *
 * **A reason opening `nucleos:` was written by this daemon**, and it is the record of an answer
 * nobody could parse: flagged rather than dropped, so a confused model costs a look instead of
 * disappearing. `map_triage.model` names the brain that **answered**, which stays true of an
 * unreadable answer — so a client that printed the sentence under that name unqualified would be
 * presenting a machine's failure note as a model's opinion, which is the attribution §6.2 exists to
 * protect, inverted.
 *
 * **The test lives in the núcleo and is spelled once**, on the payload that can carry the mark.
 * For one slice it was the other way round — the field sat on §6.2's pile, where it is provably
 * always `false`, and this shell re-spelled `map_triage::DAEMON_MARK` as a TypeScript literal to
 * cover the flags, which are the only rows a `nucleos:` sentence can appear on. A convention with
 * two spellings is one that has already stopped working somewhere, and this one decides an
 * attribution §6.2 exists to protect.
 */
function Whose({ model, machineWritten }: { model: string; machineWritten: boolean }) {
  if (machineWritten) {
    return (
      <p className="max-w-prose text-xs text-text-muted">
        Written by NucleOS and not by a model: the {model} brain answered and nobody could read the
        answer, so this flag is the record of that. The sentence above is a machine&rsquo;s note
        about a failure, not an opinion about your code.
      </p>
    );
  }
  return <p className="text-xs text-text-faint">Said by the {model} brain.</p>;
}

/**
 * What a capped list left out.
 *
 * A silent truncation is the same defect this whole feature exists to cure, so the rows are cut and
 * the number never is. The count above every list is the whole pile.
 */
function Hidden({ count }: { count: number }) {
  return (
    <p className="text-xs text-text-faint">
      {count} more not shown here. The count above is the whole pile.
    </p>
  );
}
