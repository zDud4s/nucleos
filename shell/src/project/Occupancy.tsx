import { useConcurrency } from "../data/fleet";
import { useProjects } from "../data/system";

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
  const projects = useProjects();

  const answered = concurrency.data !== undefined;
  const here = concurrency.data?.projects.find((row) => row.project_id === projectId);
  const rosterRow = projects.data?.find((row) => row.project_id === projectId);

  if (!answered) {
    return <p className="text-sm text-text-faint">Reading capacity…</p>;
  }

  /**
   * A project with no row in the readout holds nothing — but that is only true
   * because of what that readout is. Since it unions the roster with everything
   * holding a slot, absence there is positive evidence of an empty project
   * rather than a gap in the answer.
   */
  const slots = here?.slots ?? [];
  const limit = here?.limit ?? rosterRow?.wip_limit ?? null;

  /**
   * `null` is the brake OFF, and it is not a ceiling of zero.
   *
   * The daemon compares `open >= limit`, so a limit of `0` would mean *never
   * start anything again* — the opposite end of the same axis. Drawing boxes for
   * a project with no ceiling would invent a capacity nobody set, so the panel
   * says what is running instead of pretending to know how much fits.
   */
  if (limit === null) {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-sm text-text-muted">
          No ceiling set — {slots.length} {slots.length === 1 ? "worktree" : "worktrees"} in use.
        </p>
        <div className="flex flex-wrap gap-2">
          {slots.map((slot) => (
            <Slot key={`${slot.owner_kind}-${slot.owner_id}`} taken={slot} />
          ))}
        </div>
      </div>
    );
  }

  const boxes = Math.max(limit, slots.length);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        {Array.from({ length: boxes }, (_, index) => {
          const taken = slots[index];
          return taken === undefined ? (
            <Free key={`free-${index}`} />
          ) : (
            <Slot key={`${taken.owner_kind}-${taken.owner_id}`} taken={taken} />
          );
        })}
      </div>
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
    </div>
  );
}

function Free() {
  return (
    <div
      className="grid h-16 w-40 place-items-center rounded-md border border-dashed border-border text-xs text-text-faint"
      aria-label="Free slot"
    >
      free
    </div>
  );
}

function Slot({ taken }: { taken: { owner_kind: string; owner_id: number; claimed_at: string } }) {
  return (
    <div
      className="flex h-16 w-40 flex-col justify-between rounded-md border border-border bg-surface p-2"
      aria-label={`${taken.owner_kind} ${taken.owner_id}`}
    >
      <span className="text-xs uppercase tracking-wide text-text-faint">{taken.owner_kind}</span>
      <span className="font-mono text-sm text-text">#{taken.owner_id}</span>
    </div>
  );
}
