import { Link } from "@tanstack/react-router";
import { useConcurrency } from "../data/fleet";
import { ErrorNote, Quiet, StaleNote } from "../ui";

/**
 * How full this project is, as slots rather than as a number.
 *
 * The WIP ceiling is the app's main brake and it has always been a figure in a
 * form. Drawn as N boxes — the taken ones carrying what is in them, the free
 * ones outlined — being full is something you see before you have read anything.
 * That is the whole change: the fact was already on screen and nobody looked at
 * it.
 *
 * `GET /concurrency` is the authority here, and the module behind it says so in
 * as many words: the project list it returns is the union of the roster with the
 * projects actually holding slots, precisely so that a slot cannot go invisible
 * by its project having left the roster. Everything else on this panel only lays
 * description over that.
 */

export interface OccupancyProps {
  projectId: string;
}

export function Occupancy({ projectId }: OccupancyProps) {
  const concurrency = useConcurrency();

  const answered = concurrency.data !== undefined;
  const here = concurrency.data?.projects.find((row) => row.project_id === projectId);

  /*
    Refused and still reading are two different facts. Before this, a read that failed kept saying
    "Reading capacity…" for ever — a promise of an answer that was not coming.
  */
  if (!answered) {
    return concurrency.isError ? (
      <ErrorNote>The núcleo did not say which worktree slots are in use.</ErrorNote>
    ) : (
      <p className="text-sm text-text-faint">Reading capacity…</p>
    );
  }

  /*
    A poll that failed after one that worked keeps the last good boxes on screen and says how old
    they are. This panel polls fast, so a slot drawn as taken can be free by now — the note is what
    stops the picture passing for the present.
  */
  const stale = concurrency.isError ? <StaleNote dataUpdatedAt={concurrency.dataUpdatedAt} /> : null;

  /**
   * A project with no row in the readout holds nothing — but that is only true
   * because of what that readout is. Since it unions the roster with everything
   * holding a slot, absence there is positive evidence of an empty project
   * rather than a gap in the answer.
   */
  const slots = here?.slots ?? [];
  /**
   * The slot ceiling, and **only** the slot ceiling.
   *
   * `wip_limit` on the roster row is a different brake with a similar name — it bounds proposals
   * waiting on a person (migration 0017), while this bounds worktrees (0052). Falling back to it
   * would draw one number's boxes for the other number's ceiling. It cannot happen today, because
   * the readout unions the roster with everything holding a slot and so always has a row for a
   * project that exists; it would start happening silently the day that union changed, which is
   * exactly when nobody would be looking.
   */
  const limit = here?.limit ?? null;

  /**
   * `null` is the brake OFF, and it is not a ceiling of zero.
   *
   * The daemon compares `open >= limit`, so a limit of `0` would mean *never
   * start anything again* — the opposite end of the same axis. Drawing boxes for
   * a project with no ceiling would invent a capacity nobody set, so the panel
   * says what is running instead of pretending to know how much fits.
   */
  if (limit === null) {
    /*
      No ceiling and nothing running is the ordinary state of a project nobody has scheduled
      anything in, and it used to cost a sentence plus an empty row. Why it is not a ceiling of
      zero is worth reading once and is not worth a line of every visit, so it moves behind the
      question rather than being deleted.
    */
    if (slots.length === 0 && stale === null) {
      return (
        <Quiet says="no ceiling · nothing in flight">
          A ceiling is this app&rsquo;s main brake, and nothing here bounds how many worktrees may
          be open at once. That is not the same as a ceiling of zero: the daemon starts work while{" "}
          <span className="font-mono">open &lt; limit</span>, so a zero would mean never start
          anything again.
        </Quiet>
      );
    }

    return (
      <div className="flex flex-col gap-2">
        <p className="text-sm text-text-muted">
          No worktree ceiling set — {slots.length} {slots.length === 1 ? "worktree" : "worktrees"}{" "}
          in use.
        </p>
        {slots.length === 0 ? null : (
          <ul aria-label="Worktree slots" className="m-0 flex list-none flex-wrap gap-2 p-0">
            {slots.map((slot) => (
              <Slot
                key={`${slot.owner_kind}-${slot.owner_id}`}
                taken={slot}
                projectId={projectId}
              />
            ))}
          </ul>
        )}
        {stale}
      </div>
    );
  }

  const boxes = Math.max(limit, slots.length);

  return (
    <div className="flex flex-col gap-2">
      {/*
        The count in words, above the boxes. The boxes say "full" before anything is read; this
        says WHICH ceiling they are. The Settings block further down has a ceiling too — on
        proposals waiting on a person, not on worktrees — and two unlabelled fours on one page
        were two numbers a reader had to guess apart.
      */}
      <p className="text-xs text-text-muted">
        {Math.min(slots.length, limit)} of {limit} worktree {limit === 1 ? "slot" : "slots"} in use
      </p>
      <ul aria-label="Worktree slots" className="m-0 flex list-none flex-wrap gap-2 p-0">
        {Array.from({ length: boxes }, (_, index) => {
          const taken = slots[index];
          return taken === undefined ? (
            <Free key={`free-${index}`} />
          ) : (
            <Slot
              key={`${taken.owner_kind}-${taken.owner_id}`}
              taken={taken}
              projectId={projectId}
            />
          );
        })}
      </ul>
      {/*
        Over the ceiling is a real state, not an impossible one: the limit can be
        lowered under work already running. Saying it plainly beats drawing a
        row of boxes that silently has one too many in it.
      */}
      {slots.length > limit ? (
        <p className="text-xs text-tone-paused-fg">
          {slots.length} in flight against a ceiling of {limit} — the ceiling was lowered under work
          already running.
        </p>
      ) : null}
      {stale}
    </div>
  );
}

/*
  List items, and no `aria-label` on any of them. A label on a generic `div` is ignored by several
  screen readers, so "Free slot" was often not said at all; the visible word is the name, and the
  list around the boxes is what says how many there are.
*/
function Free() {
  return (
    <li className="grid h-16 w-40 place-items-center rounded-md border border-dashed border-border text-xs text-text-faint">
      free
    </li>
  );
}

/**
 * One taken slot, and the one action that belongs to it.
 *
 * The command lives *in* the thing it acts on. This is the rule that keeps a page like this from
 * becoming a drawer: a row of buttons at the bottom with no owner is how every dashboard ends up
 * with fourteen of them, and reviewing *this* run is not an action about the project — it is an
 * action about this slot.
 *
 * Only a run gets the link. A job or a team's item holds a worktree too, and the Code mode reads a
 * *run's* checkout — offering the door for an owner it cannot open would be a link that refuses.
 */
function Slot({
  taken,
  projectId,
}: {
  taken: { owner_kind: string; owner_id: number; claimed_at: string };
  projectId: string;
}) {
  const body = (
    <>
      <span className="text-xs uppercase tracking-wide text-text-faint">{taken.owner_kind}</span>
      <span className="font-mono text-sm text-text">#{taken.owner_id}</span>
    </>
  );

  if (taken.owner_kind !== "run") {
    return (
      <li className="flex h-16 w-40 flex-col justify-between rounded-md border border-border bg-surface p-2">
        {body}
      </li>
    );
  }

  return (
    <li className="flex">
      <Link
        to={`/projects/${projectId}/code`}
        search={{ run: taken.owner_id }}
        className="flex h-16 w-40 flex-col justify-between rounded-md border border-border bg-surface p-2 hover:border-border-strong"
        aria-label={`Review run ${taken.owner_id}`}
      >
        {body}
      </Link>
    </li>
  );
}
