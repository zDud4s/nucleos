// §spec mapa-do-projeto
import { isApiRefusal } from "../data/client";
import { useDecideMapLine, useMapDecisions, type MapDecision } from "../data/project-map";

/**
 * The pile nobody has read, and the one gesture that is missing from it.
 *
 * §4: nothing enters the map without the owner's approval, **line by line**. So this surface draws
 * the decision itself — the sentence a model pulled out of the document, and the heading it came
 * from — and never a summary of it. A summary would be the thousand-line plan again, only shorter,
 * and the whole product here is that twelve numbered lines are read in two minutes.
 *
 * **There is no control that answers more than one line, and its absence is the design.** A button
 * that took the list in one gesture would be precisely the gesture this mode exists to replace: the
 * accepting-without-reading that turns a false sense of confidence into a sense of insecurity the
 * moment somebody notices. There is a test that counts every button on this surface for that
 * reason, and it passes only while the count is two per line and nothing else.
 */

export interface MapaPorAprovarProps {
  projectId: string;
}

export function MapaPorAprovar({ projectId }: MapaPorAprovarProps) {
  const decisions = useMapDecisions(projectId);

  if (decisions.isError) {
    return (
      <p className="text-sm text-text-faint">
        The núcleo could not say what is waiting to be read here.
      </p>
    );
  }
  if (decisions.data === undefined) {
    return <p className="text-sm text-text-faint">Reading what is waiting…</p>;
  }

  /*
    A sentence, never an empty area. The two look identical, and reading a broken panel as an empty
    one is the false confidence this whole mode exists to cure.
  */
  if (decisions.data.length === 0) {
    return (
      <section aria-label="Decisions waiting" className="flex flex-col gap-2">
        <p className="max-w-prose text-sm text-text-muted">
          Nothing is waiting to be read. Either no spec has been through a model yet, or every line
          one proposed has already been answered.
        </p>
      </section>
    );
  }

  const count = decisions.data.length;

  return (
    <section aria-label="Decisions waiting" className="flex flex-col gap-3">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Waiting on you
      </h2>
      <p className="max-w-prose text-sm text-text-muted">
        {count} {count === 1 ? "line" : "lines"}, answered one at a time. Nothing here is in the map
        yet, and an extraction nobody has read is counted apart from the decisions that are.
      </p>
      {/*
        §4.1 in two sentences, once at the top rather than repeated under twelve lines. The two
        kinds ask different things later — one reaches you only when it breaks, the other is
        answerable by nothing but a stamp — and that difference is what decides whether this map
        costs an afternoon a week or thirty seconds a day.
      */}
      <p className="max-w-prose text-xs text-text-faint">
        A countable line names a number, a set or a coverage claim, so something can later count the
        code and disagree with it. A character line is about what a thing is or is not, and no count
        decides that — only your stamp does.
      </p>

      {/*
        Numbered by position and not by `ordinal`: the pile holds whatever several extractions left
        behind, each numbering itself from one, and two lines both called 3 would be worse than no
        number at all. Position is what "twelve numbered lines" means to somebody reading down it.
      */}
      <ol className="flex flex-col gap-2">
        {decisions.data.map((row, at) => (
          <Line key={row.id} projectId={projectId} row={row} number={at + 1} />
        ))}
      </ol>
    </section>
  );
}

/**
 * One decision, and the two answers to it.
 *
 * The mutation is per row, the way `Workflows` gives each installed row its own: a refusal belongs
 * to the line it was refused about, and one shared mutation would put the last failure's sentence
 * under whichever row happened to be looking.
 *
 * Both buttons carry the line's number in their accessible name. Twelve buttons all called
 * "approve" are twelve identical announcements to anybody not looking at the screen, and this is a
 * surface whose entire promise is that you know which line you just answered.
 */
function Line({
  projectId,
  row,
  number,
}: {
  projectId: string;
  row: MapDecision;
  number: number;
}) {
  const decide = useDecideMapLine(projectId);

  return (
    <li className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="font-mono text-xs text-text-faint">{number}</span>
        <span className="text-xs text-text-muted">{row.section}</span>
        <span className="truncate font-mono text-xs text-text-faint">{row.spec_slug}</span>
        <span className="ml-auto text-xs text-text-faint">{row.kind}</span>
      </div>

      {/* The decision, in the document's own words. Never a summary of the section it came from. */}
      <p className="max-w-prose text-sm text-text">{row.text}</p>

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          aria-label={`approve line ${number}`}
          disabled={decide.isPending}
          onClick={() => decide.mutate({ id: row.id, approved: true })}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          approve
        </button>
        {/*
          Beside approve and never smaller than it. A rejected line is not a mistake being cleaned
          up — it is the owner saying the model read the document wrong, which is the answer this
          surface most needs to be easy to give.
        */}
        <button
          type="button"
          aria-label={`reject line ${number}`}
          disabled={decide.isPending}
          onClick={() => decide.mutate({ id: row.id, approved: false })}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          reject
        </button>
      </div>

      {decide.isError ? <Refused error={decide.error} /> : null}
    </li>
  );
}

/**
 * Why an answer did not land, with the line still on screen.
 *
 * The daemon collapses three causes into one 404 on purpose — answered already, another project's,
 * never existed — because all three mean the same thing to whoever asked. What must not happen is
 * the row quietly disappearing as though the answer had been recorded.
 */
function Refused({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
        The núcleo did not answer, so this line is still waiting.
      </p>
    );
  }

  return (
    <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
      {error.status === 404
        ? "That line is not yours to answer now — it has been answered already, or it is gone."
        : error.detail}
    </p>
  );
}
