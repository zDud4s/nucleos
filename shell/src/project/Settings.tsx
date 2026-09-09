import { useId, useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  SHADOW_EVIDENCE_MODE,
  useScoreboard,
  useSetProjectMode,
  type ClassTally,
} from "../data/autopilot";
import { useSetWipLimit } from "../data/projects";
import { useProjects, type AutopilotMode, type ProjectSummary } from "../data/system";
import {
  MODE_MEANING,
  MODE_SENTENCES,
  promotionBlocker,
  promotionConfirmLabel,
  promotionConsequence,
} from "../lib/mode";
import { ModeSwitch } from "../ui";

/**
 * The settings this app is the author of — the ones that live in the database.
 *
 * The design's write boundary has two layers, and this is the first half of layer 1: **config the
 * app owns, offered as a form and not as a text box.** Here the app is a better author than an
 * editor is, because it knows the schema — there is no way to set the mode to a fourth value, and
 * no way to write a negative ceiling, because there is nowhere to write one.
 *
 * The other half of layer 1 is a *file*, and it is next door in `OwnedFiles` for a reason worth
 * stating: a form that round-tripped YAML would delete the comment somebody left explaining why a
 * schedule is switched off. "A shell that offered to write it would be a second author of a
 * document git already owns" stays true about the file's SHAPE even where it has stopped being
 * true about the file.
 *
 * Two of the three things here are settings and the third is evidence. The classes are in this
 * section anyway, because they are what unlocks the third mode — and they are drawn as facts
 * rather than as controls, because nothing in the núcleo lets a person grant a class by hand and a
 * chip that looked clickable would be a lie about who decides.
 */

export interface SettingsProps {
  projectId: string;
}

export function Settings({ projectId }: SettingsProps) {
  const projects = useProjects();
  const project = projects.data?.find((row) => row.project_id === projectId);

  if (project === undefined) {
    return <p className="text-sm text-text-faint">Reading settings…</p>;
  }

  return (
    <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
      <ModeChoice project={project} />
      <div className="flex flex-col gap-3">
        <Ceiling project={project} />
        <Classes projectId={projectId} project={project} />
      </div>
    </div>
  );
}

/** The frame the three blocks share, so they cannot drift into three layouts. */
function Block({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="text-xs uppercase tracking-wide text-text-faint">{label}</p>
      {children}
    </div>
  );
}

/**
 * The biggest lever on the page, with the one setting that has to be earned.
 *
 * `promotable` is the daemon's arithmetic and is never recomputed here — a control that unlocked on
 * different numbers from the ones `shadow.rs` enforces would offer a button that always refuses.
 * The blocker sentence comes from `lib/mode.ts` for the same reason: the Autopilot page says the
 * same thing about the same state, and two copies of a sentence about restraint would eventually
 * say two different things.
 *
 * **No optimistic draw.** The route answers a bare 422 for four different missing prerequisites, so
 * a mode drawn ahead of the answer would have to be un-drawn — and whether a project acts on its
 * own is the one thing nobody may be unsure about.
 */
function ModeChoice({ project }: { project: ProjectSummary }) {
  const setMode = useSetProjectMode();
  const refused = setMode.isError && isApiRefusal(setMode.error) ? setMode.error : null;
  const withheld = project.withheld_classes_ready ?? 0;
  const blocker = promotionBlocker(project, withheld);
  /**
   * Whether the third segment is armed, so this block can say what confirming it would do.
   *
   * The sentence is not the armed label any more: 52 characters inside a switch segment wrap,
   * and a control that grows while you are deciding moves the button away from the pointer that
   * has four seconds to press it again. It goes under the switch, where nothing above it moves.
   */
  const [armed, setArmed] = useState(false);
  /** The id the armed switch points at, so the sentence is read as the button's description. */
  const consequenceId = useId();

  function change(mode: AutopilotMode) {
    setMode.mutate({
      project_id: project.project_id,
      mode,
      // The folder the daemon already has. Naming it again is what the Autopilot page does when
      // its input is empty, and a project reached through this page has one by definition — every
      // other panel here reads files.
      ...(project.project_root === null ? {} : { project_root: project.project_root }),
    });
  }

  return (
    <Block label="Mode">
      <ModeSwitch
        value={project.mode}
        actAllowed={project.promotable}
        actArmedLabel={promotionConfirmLabel(project)}
        onArmedChange={setArmed}
        actDescribedBy={armed ? consequenceId : undefined}
        busy={setMode.isPending}
        onChoose={change}
      />

      {/* Immediately under the switch and above everything else this block says, so arming
          pushes the standing copy down rather than moving the button itself. */}
      {armed ? (
        <p className="text-xs text-text-muted" id={consequenceId}>
          {promotionConsequence(project)}
        </p>
      ) : null}

      <p className="text-xs text-text-muted">{MODE_MEANING[project.mode]}.</p>

      {/*
        The gate, stated whether or not it is open — and only while it is still a gate. A project
        already acting has passed it, and repeating the criteria at that point reads as a warning
        about something that is not happening.
      */}
      {project.mode !== "active" ? (
        <p className={project.promotable ? "text-xs text-text-muted" : "text-xs text-text-faint"}>
          {project.promotable
            ? "every class it has exercised clears the bar, and at least one is a class the classifier withheld — it has earned this"
            : blocker}
        </p>
      ) : null}

      {refused !== null ? (
        <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
          {MODE_SENTENCES[refused.code] ?? refused.detail}
        </p>
      ) : null}
    </Block>
  );
}

/**
 * The open-proposal brake, with what it is holding beside it.
 *
 * **This is not the slot ceiling**, and the two are easy to confuse because both are called a
 * limit: `wip_limit` (migration 0017) bounds proposals waiting on a person, and
 * `max_concurrent_slots` (0052) bounds worktrees. The Occupancy panel above draws the second. A
 * stepper that showed one number and governed the other would be the worst kind of control.
 *
 * The count beside it is the point of drawing this here at all. A ceiling of three reads as slack
 * until you know two are already taken, and `queue_full` is the daemon's own verdict rather than
 * this page's arithmetic — `queue_is_full` compares `open >= limit`, and a second implementation of
 * that comparison would eventually disagree with the one that actually defers work.
 */
function Ceiling({ project }: { project: ProjectSummary }) {
  const setLimit = useSetWipLimit();
  const limit = project.wip_limit;
  const open = project.open_proposals;

  function set(next: number | null) {
    setLimit.mutate({ projectId: project.project_id, limit: next });
  }

  return (
    <Block label="Open-proposal ceiling">
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          aria-label="Lower the ceiling"
          disabled={limit === null || limit <= 0 || setLimit.isPending}
          onClick={() => set(limit === null ? null : limit - 1)}
          className="h-8 w-8 rounded-md border border-border text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          −
        </button>
        <span className="min-w-16 text-center font-display text-xl font-bold tabular-nums text-text">
          {/*
            `null` is the brake OFF and is not a ceiling of zero: the daemon compares
            `open >= limit`, so zero would mean "never start anything again" — the opposite end of
            the same axis.

            The word "off" and deliberately NOT an em dash. The dash is this page's mark for a
            reading nobody took, and it is on the screen four times already; a brake somebody chose
            to switch off is the opposite of an absent measurement, and one glyph meaning both
            would be the collapse the rest of the page is built to avoid.
          */}
          {limit === null ? "off" : limit}
        </span>
        <button
          type="button"
          aria-label="Raise the ceiling"
          disabled={setLimit.isPending}
          onClick={() => set(limit === null ? 1 : limit + 1)}
          className="h-8 w-8 rounded-md border border-border text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          +
        </button>
        <button
          type="button"
          disabled={limit === null || setLimit.isPending}
          onClick={() => set(null)}
          className="rounded-md border border-border px-2 py-1 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          no ceiling
        </button>
      </div>

      <p className="text-xs text-text-muted">
        {limit === null
          ? `no ceiling — ${open} ${open === 1 ? "proposal" : "proposals"} waiting on you`
          : `${open} of ${limit} taken`}
      </p>
      {project.queue_full ? (
        <p className="text-xs text-tone-paused-fg">
          The brake is holding new work here until something is reviewed.
        </p>
      ) : null}
    </Block>
  );
}

/**
 * What the classifier has been judged on, class by class.
 *
 * Facts, not controls, and the reason is worth keeping next to them: **no per-chip verdict is
 * computed here.** The bar counts reviews and agreements distinct by `(tool_name, tool_input)`
 * while this tally counts every row, so a chip that decided for itself whether a class had cleared
 * the bar would disagree with the rule that actually gates promotion — on the panel where somebody
 * is deciding whether this project may act on its own. The aggregate is the daemon's.
 *
 * `shadow` rows only. A `worktree`-mode decision was enforced rather than recorded, so it is
 * history and not evidence — `shadow_readiness` reads the same `WHERE`.
 */
function Classes({ projectId, project }: { projectId: string; project: ProjectSummary }) {
  const scoreboard = useScoreboard(projectId);
  const rows = (scoreboard.data ?? []).filter((row) => row.mode === SHADOW_EVIDENCE_MODE);

  return (
    <Block label="Action classes">
      {scoreboard.data === undefined ? (
        <p className="text-xs text-text-faint">Reading…</p>
      ) : rows.length === 0 ? (
        /*
          Nothing recorded is not "zero of zero clear". A project that has never run in shadow has
          taken no measurement, and saying it failed one would be inventing a result.
        */
        <p className="text-xs text-text-faint">
          Nothing recorded in shadow yet — there is no measurement here, which is not the same as a
          bad one.
        </p>
      ) : (
        <>
          <div className="flex flex-wrap gap-1.5">
            {rows.map((row) => (
              <Chip key={row.action_class} row={row} />
            ))}
          </div>
          <p className="text-xs text-text-muted">
            {project.classes_ready} of {project.classes_total} clearing the bar, by the núcleo's own
            count — which deduplicates repeats of the same command and so is lower than these
            tallies.
          </p>
        </>
      )}
    </Block>
  );
}

function Chip({ row }: { row: ClassTally }) {
  return (
    <span
      className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 text-xs text-text-muted"
      title={`${row.total} decided, ${row.reviewed} reviewed, ${row.disagree} disagreed`}
    >
      {row.action_class}
      <span className="ml-1 text-text-faint">
        {row.reviewed}/{row.total}
      </span>
    </span>
  );
}
