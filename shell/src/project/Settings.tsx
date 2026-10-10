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
  MODE_REFUSAL_PROSE,
  PROMOTION_EARNED,
  promotionBlocker,
  promotionConfirmLabel,
  promotionConsequence,
} from "../lib/mode";
import { ErrorNote, Inset, ModeSwitch, Quiet, StaleNote } from "../ui";
import { OnboardPanel } from "./Onboard";

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
    // A roster that refused is not a roster still being read, and must not keep saying it is.
    return projects.isError && projects.data === undefined ? (
      <ErrorNote>
        The núcleo did not answer, so this project&rsquo;s settings are unknown.
      </ErrorNote>
    ) : (
      <p className="text-sm text-text-faint">Reading settings…</p>
    );
  }

  /*
    The roster polls, and a poll that failed after a good one leaves these controls drawn from the
    last answer. The mode switch still sends an absolute value, so it stays; the ceiling's stepper
    sends the number on screen plus or minus one, and that number may be old — so it goes, and the
    note says why, which is the app's rule for an action that would act on stale data.
  */
  const stale = projects.isError;

  /*
    Two blocks side by side and the evidence under both. Two cards sharing a row share its height:
    the owner's rule, since two boxes ending at different lines read as a layout that broke. (The
    old `items-start` answered a different grid, where Mode filled a column holding two others.)
  */
  return (
    <div className="flex flex-col gap-3">
      {stale ? <StaleNote dataUpdatedAt={projects.dataUpdatedAt} /> : null}
      <div className="grid grid-cols-1 items-stretch gap-3 lg:grid-cols-2">
        <ModeChoice project={project} />
        <Ceiling project={project} stale={stale} />
        <div className="lg:col-span-2">
          <Classes projectId={projectId} project={project} />
        </div>
      </div>
    </div>
  );
}

/**
 * A daemon sentence, as a sentence.
 *
 * `lib/mode.ts` writes its strings lower-case and unpunctuated because the Autopilot page splices
 * them into lines of its own. Here each one stands alone as a paragraph, and a paragraph that
 * starts in lower case and stops without a full stop reads as a fragment somebody cut off.
 */
function sentence(text: string): string {
  const trimmed = text.trim();
  if (trimmed === "") return trimmed;
  const capital = trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
  return /[.!?…]$/.test(capital) ? capital : `${capital}.`;
}

/**
 * The frame the three blocks share, so they cannot drift into three layouts.
 *
 * `Inset` is that frame now, and the drift it prevents is wider than this file: the same box was
 * written out eleven times across `project/` and nineteen more across the page stylesheets, in two
 * flavours that disagreed on fill, radius and padding. Three blocks that cannot drift into three
 * layouts is the argument this function was written for; sharing the app's one inset is the same
 * argument at the scale it actually bites.
 */
function Block({
  label,
  id,
  children,
}: {
  label: string;
  /** For a control inside that wants the heading as its name. */
  id?: string;
  children: React.ReactNode;
}) {
  /*
    An `h3`, under the section's `h2`. These were paragraphs, so the one control on the page that
    decides whether a project acts on its own could not be reached by walking the headings.
  */
  return (
    <Inset>
      <h3 id={id} className="text-sm font-semibold text-text-muted">
        {label}
      </h3>
      {children}
    </Inset>
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
 * **No optimistic draw.** The route answers a 422 for four different missing prerequisites, so
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
  /**
   * The id of the blocker sentence, for a "Let it act" that is locked.
   *
   * The switch keeps a locked segment focusable here (`focusableWhenInert`), and a control you
   * can land on but not press owes the reason it cannot be pressed. That reason is the blocker
   * paragraph below; the consequence sentence is for an offer that can be taken.
   */
  const blockerId = useId();

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
      {/*
        `focusableWhenInert`: pressing a segment makes it the setting, which makes it inert — and
        a native `disabled` would drop the focus that pressed it to `<body>`. The same holds while
        the write is in flight, when all three are inert at once.
      */}
      <ModeSwitch
        value={project.mode}
        actAllowed={project.promotable}
        actArmedLabel={promotionConfirmLabel(project)}
        actConsequence={promotionConsequence(project)}
        onArmedChange={setArmed}
        actDescribedBy={project.promotable ? consequenceId : blockerId}
        busy={setMode.isPending}
        focusableWhenInert
        onChoose={change}
      />

      {/* Immediately under the switch and above everything else this block says, so arming
          pushes the standing copy down rather than moving the button itself. In the document
          at rest, hidden, so the switch can be described before it is armed: a description
          attached at the moment of arming lands on a button that already has focus and is
          not re-announced. `.sr-only` is absolutely positioned — nothing moves. */}
      <p className={armed ? "text-xs text-text-muted" : "sr-only"} id={consequenceId}>
        {promotionConsequence(project)}
      </p>

      <p className="text-xs text-text-muted">{sentence(MODE_MEANING[project.mode])}</p>

      {/*
        The gate, stated whether or not it is open — and only while it is still a gate. A project
        already acting has passed it, and repeating the criteria at that point reads as a warning
        about something that is not happening.
      */}
      {project.mode !== "active" ? (
        <p id={blockerId} className="text-xs text-text-muted">
          {sentence(project.promotable ? PROMOTION_EARNED : blocker)}
        </p>
      ) : null}

      {refused !== null ? (
        <ErrorNote>{MODE_REFUSAL_PROSE[refused.code] ?? refused.detail}</ErrorNote>
      ) : null}
      {/* The one refusal this block can resolve in place: onboard, then choose again. */}
      {refused?.code === "not_onboarded" ? (
        <Inset>
          <OnboardPanel projectId={project.project_id} root={project.project_root ?? ""} />
        </Inset>
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
function Ceiling({ project, stale }: { project: ProjectSummary; stale: boolean }) {
  const setLimit = useSetWipLimit();
  const limit = project.wip_limit;
  const open = project.open_review_items;
  const headingId = useId();
  const readingId = useId();
  const busy = setLimit.isPending;
  const refused = setLimit.isError && isApiRefusal(setLimit.error) ? setLimit.error : null;

  function set(next: number | null) {
    setLimit.mutate({ projectId: project.project_id, limit: next });
  }

  /*
    `null` is the brake OFF and is not a ceiling of zero: the daemon compares
    `open >= limit`, so zero would mean "never start anything again" — the opposite end of
    the same axis.

    The word "off" and deliberately NOT an em dash. The dash is this page's mark for a
    reading nobody took, and it is on the screen four times already; a brake somebody chose
    to switch off is the opposite of an absent measurement, and one glyph meaning both
    would be the collapse the rest of the page is built to avoid.
  */
  const figure = (
    <span className="min-w-10 text-center text-sm font-semibold tabular-nums text-text">
      {limit === null ? "off" : limit}
    </span>
  );

  /*
    Polite and live, because this is what a press on the stepper changes and the stepper
    itself says nothing: "Raise the ceiling" is the same label before and after. It is also
    each step's description, so a step that cannot be pressed — lower at zero, "no ceiling"
    when there is none — is read beside the reason.
  */
  const reading = (
    <p id={readingId} aria-live="polite" className="text-base font-medium tabular-nums text-text">
      {limit === null
        ? `no ceiling — ${open} ${open === 1 ? "proposal" : "proposals"} waiting on you`
        : `${open} of ${limit} taken`}
    </p>
  );

  /*
    The same reading as a gauge, one cell per place under the ceiling, so "3 of 4" is seen before
    it is read. Decorative: the sentence above is what a screen reader hears. Past a dozen places
    the cells would be slivers, so it becomes one continuous bar.
  */
  const taken = limit === null ? 0 : Math.min(open, limit);
  const meter =
    limit === null || limit === 0 ? null : (
      <div aria-hidden="true" className="flex gap-1">
        {limit <= 12 ? (
          Array.from({ length: limit }, (_, i) => (
            <span
              key={i}
              className={`h-1.5 flex-1 rounded-full ${i < taken ? (project.queue_full ? "bg-tone-paused-fg" : "bg-tone-active-fg") : "bg-surface-sunken"}`}
            />
          ))
        ) : (
          <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-sunken">
            <span
              className={`block h-full rounded-full ${project.queue_full ? "bg-tone-paused-fg" : "bg-tone-active-fg"}`}
              style={{ width: `${(taken / limit) * 100}%` }}
            />
          </span>
        )}
      </div>
    );

  return (
    <Block label="Open-proposal ceiling" id={headingId}>
      {/* What the number bounds, said once where it is set. The worktree slots above are a
          different ceiling, and a reader meeting two unexplained fours has to guess which is which. */}
      <p className="text-xs text-text-muted">
        How many proposals may wait on you before new work here is held.
      </p>

      {reading}
      {meter}

      {/*
        The controls sit on the card's floor, not under the text: the card shares its row with Mode
        and is as tall as it, and the empty air belongs between the reading and the controls rather
        than under everything. The stepper is one joined control, the way off at the far end.
      */}
      {stale ? null : (
        <div
          role="group"
          aria-labelledby={headingId}
          aria-busy={busy}
          className="mt-auto flex flex-wrap items-center justify-between gap-2 pt-2"
        >
          <span className="inline-flex items-center rounded-md border border-border-strong">
            <Step
              label="Lower the ceiling"
              inert={limit === null || limit <= 0 || busy}
              describedBy={readingId}
              onPress={() => set(limit === null ? null : limit - 1)}
            >
              −
            </Step>
            {figure}
            <Step
              label="Raise the ceiling"
              inert={busy}
              describedBy={readingId}
              onPress={() => set(limit === null ? 1 : limit + 1)}
            >
              +
            </Step>
          </span>
          <Step inert={limit === null || busy} describedBy={readingId} onPress={() => set(null)}>
            no ceiling
          </Step>
        </div>
      )}

      {project.queue_full ? (
        <p className="text-xs text-tone-paused-fg">
          The brake is holding new work here until something is reviewed.
        </p>
      ) : null}

      {/* A failed write used to say nothing, so the number on screen was the only answer — and it
          was the old number, looking like a change that had not landed yet. */}
      {setLimit.isError ? (
        <ErrorNote>
          {refused === null
            ? "The ceiling was not changed — the núcleo did not answer."
            : `The ceiling was not changed — ${refused.detail}`}
        </ErrorNote>
      ) : null}
    </Block>
  );
}

/**
 * One step of the ceiling, inert in the house way: `aria-disabled` and a press that does nothing.
 *
 * Native `disabled` was what these used, and the press that lowers the ceiling to zero is the
 * press that disables "lower" — so the focus that made the change fell to `<body>` as it landed,
 * and "no ceiling" did the same to itself. Kept focusable, a step says it cannot be pressed and is
 * described by the reading beside it, which is the reason.
 */
function Step({
  label,
  inert,
  describedBy,
  onPress,
  children,
}: {
  label?: string;
  inert: boolean;
  describedBy: string;
  onPress: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-disabled={inert ? "true" : undefined}
      aria-describedby={describedBy}
      onClick={() => {
        if (!inert) onPress();
      }}
      className={`${label === undefined ? "min-h-[1.875rem] rounded-md border border-border-strong px-3 text-sm hover:border-text-faint aria-disabled:hover:border-border-strong" : "h-[1.875rem] w-8 rounded-md text-base hover:bg-surface-sunken hover:text-text aria-disabled:hover:bg-transparent"} text-text-muted aria-disabled:cursor-not-allowed aria-disabled:opacity-(--opacity-disabled)`}
    >
      {children}
    </button>
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
        scoreboard.isError ? (
          <ErrorNote>The núcleo did not say what the classifier has been judged on here.</ErrorNote>
        ) : (
          <p className="text-xs text-text-faint">Reading…</p>
        )
      ) : rows.length === 0 ? (
        /*
          Nothing recorded is not "zero of zero clear". A project that has never run in shadow has
          taken no measurement, and saying it failed one would be inventing a result.
        */
        <Quiet says="Nothing recorded in shadow yet — there is no measurement here, which is not the same as a bad one." />
      ) : (
        <>
          {/*
            One verdict, and it comes first: the daemon's count, which is the one that gates the
            third mode. The table under it is the volume of evidence, not a second verdict.

            It used to be the other way round — a row of pills reading `read-local 18/46` above a
            sentence saying all five cleared the bar. A fraction at 39% beside "clears the bar"
            reads as a contradiction, on the panel where somebody decides whether to let go of
            the wheel; the fraction was reviewed-of-decided, and it said so only on hover.
          */}
          <p className="text-sm text-text">
            {project.classes_ready} of {project.classes_total}{" "}
            {project.classes_total === 1 ? "class clears" : "classes clear"} the bar.
          </p>
          <table className="w-full max-w-xl border-collapse text-xs">
            <caption className="sr-only">Shadow decisions by action class</caption>
            <thead>
              <tr className="text-text-faint">
                <th scope="col" className="pb-1 text-left font-normal">
                  Class
                </th>
                <th scope="col" className="pb-1 text-right font-normal">
                  Decided
                </th>
                <th scope="col" className="pb-1 text-right font-normal">
                  Reviewed
                </th>
                <th scope="col" className="pb-1 text-right font-normal">
                  Disagreed
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <ClassRow key={row.action_class} row={row} />
              ))}
            </tbody>
          </table>
          <p className="text-xs text-text-muted">
            The bar counts each distinct command once; this table counts every decision, so its
            numbers run higher than the bar&rsquo;s.
          </p>
        </>
      )}
      {scoreboard.isError && scoreboard.data !== undefined ? (
        <StaleNote dataUpdatedAt={scoreboard.dataUpdatedAt} />
      ) : null}
    </Block>
  );
}

/**
 * One class and its three tallies, each in a column that says what it counts.
 *
 * The class name is the daemon's identifier and so is set in mono (the Three Faces Rule), and the
 * numbers are the daemon's too, tabular so a column stays a column when a poll moves a digit. No
 * pill: that is the badge shape, and these are facts rather than states.
 */
function ClassRow({ row }: { row: ClassTally }) {
  return (
    <tr className="border-t border-border">
      <th scope="row" className="py-1 pr-3 text-left font-mono font-normal text-text">
        {row.action_class}
      </th>
      <td className="py-1 text-right font-mono tabular-nums text-text-muted">{row.total}</td>
      <td className="py-1 text-right font-mono tabular-nums text-text-muted">{row.reviewed}</td>
      <td className="py-1 text-right font-mono tabular-nums text-text-muted">{row.disagree}</td>
    </tr>
  );
}
