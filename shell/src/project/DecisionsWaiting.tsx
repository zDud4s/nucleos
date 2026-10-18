// §spec mapa-do-projeto
import { isApiRefusal } from "../data/client";
import { useDecideMapLine, useMapDecisions, type MapDecision } from "../data/project-map";
import { useState } from "react";
import { ErrorNote, Quiet, SectionTitle } from "../ui";
import { DAEMON_NAME } from "../copy";

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
 * reason, and it passes only while the count is two per line — plus the one that shows the rest of
 * the list, which answers nothing.
 */

export interface DecisionsWaitingProps {
  projectId: string;
}

export function DecisionsWaiting({ projectId }: DecisionsWaitingProps) {
  const decisions = useMapDecisions(projectId);
  const [open, setOpen] = useState(false);

  if (decisions.isError) {
    return <ErrorNote>The {DAEMON_NAME} could not say what is waiting to be read here.</ErrorNote>;
  }
  if (decisions.data === undefined) {
    return <p className="text-sm text-text-faint">Reading what is waiting…</p>;
  }

  /*
    A sentence, never an empty area. The two look identical, and reading a broken panel as an empty
    one is the false confidence this whole mode exists to cure.
  */
  if (decisions.data.length === 0) {
    /*
      The sentence stays and the reason behind it moves behind the question, which is what `Quiet`
      is for: the absence itself is one line, and the two ways of arriving at it are worth reading
      once rather than on every visit. Neither half is cut — cutting the second would leave
      "nothing is waiting" reading as a list that failed, which is the confusion above.
    */
    return (
      <section aria-label="Decisions waiting" className="flex flex-col gap-2">
        <Quiet says="Nothing is waiting to be read.">
          Either no spec has been through a model yet, or every line one proposed has already been
          answered.
        </Quiet>
      </section>
    );
  }

  const rows = decisions.data;
  const count = rows.length;
  const shown = open || count <= FIRST ? rows : rows.slice(0, FIRST);
  // Consecutive lines from one document share a heading: twenty rows each repeating the same slug
  // was most of the ink on the list.
  const groups: { slug: string; rows: [MapDecision, number][] }[] = [];
  shown.forEach((row, at) => {
    const last = groups[groups.length - 1];
    if (last?.slug === row.spec_slug) last.rows.push([row, at + 1]);
    else groups.push({ slug: row.spec_slug, rows: [[row, at + 1]] });
  });

  return (
    <section aria-label="Decisions waiting" className="flex flex-col gap-2">
      <SectionTitle level={2}>Waiting on you</SectionTitle>
      <p className="m-0 text-sm text-text-muted">
        {count} {count === 1 ? "line" : "lines"} from the specs, answered one at a time — none is in
        the map yet.
      </p>

      <div className="ws-waiting">
        {groups.map((group) => (
          <div key={`${group.slug}-${group.rows[0][1]}`} className="ws-waiting-group">
            <p className="ws-waiting-spec">{group.slug}</p>
            {/*
              Numbered by position and not by `ordinal`: the pile holds whatever several
              extractions left behind, each numbering itself from one. The number is not drawn —
              the section heading already carries the document's own — but it stays in each
              button's name, which is how a screen reader knows which line it answered.
            */}
            <ol className="m-0 list-none p-0">
              {group.rows.map(([row, number]) => (
                <Line key={row.id} projectId={projectId} row={row} number={number} />
              ))}
            </ol>
          </div>
        ))}
      </div>

      {count > FIRST ? (
        <button type="button" className="ws-waiting-more" onClick={() => setOpen(!open)}>
          {open ? "Show fewer" : `Show all ${count}`}
        </button>
      ) : null}
    </section>
  );
}

/** How many lines show before the rest are asked for. */
const FIRST = 5;

/**
 * §4.1 in a sentence each, on the tag rather than as a paragraph over the list. The two kinds ask
 * different things later — one reaches you only when it breaks, the other is answerable by nothing
 * but a stamp.
 */
const KIND_MEANING: Record<string, string> = {
  countable:
    "Names a number, a set or a coverage claim, so something can later count the code and disagree with it.",
  character: "About what a thing is or is not. No count decides that — only your stamp does.",
};

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
function Line({ projectId, row, number }: { projectId: string; row: MapDecision; number: number }) {
  const decide = useDecideMapLine(projectId);

  return (
    <li className="ws-waiting-row">
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        {/* The decision, in the document's own words. Never a summary of the section it came from. */}
        <p className="m-0 text-sm text-text">{row.text}</p>
        <p className="m-0 flex flex-wrap items-baseline gap-x-2 text-xs text-text-faint">
          <span>{row.section}</span>
          <span className="ws-waiting-kind" title={KIND_MEANING[row.kind]}>
            {row.kind}
          </span>
        </p>
      </div>

      <div className="flex shrink-0 items-center gap-2">
        <button
          type="button"
          aria-label={`approve line ${number}`}
          disabled={decide.isPending}
          onClick={() => decide.mutate({ id: row.id, approved: true })}
          className="rounded-md border border-border px-3 py-1 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
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
          className="rounded-md border border-border px-3 py-1 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          reject
        </button>
      </div>

      {decide.isError ? (
        <div className="basis-full">
          <Refused error={decide.error} />
        </div>
      ) : null}
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
    return <ErrorNote>The {DAEMON_NAME} gave no reply, so this line is still waiting.</ErrorNote>;
  }

  return (
    <ErrorNote>
      {error.status === 404
        ? "That line is not yours to answer now — it has been answered already, or it is gone."
        : error.detail}
    </ErrorNote>
  );
}
