import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  compactTokens,
  efficiencyTrend,
  gateShare,
  humanMinutes,
  useProjectReadings,
  type ProjectReadings,
} from "../data/project-readings";
import { useBudget, useKillSwitch, useProjects } from "../data/system";
import { driftingWorkflows, useProjectWorkflows } from "../data/workflows";
import { Section } from "../ui";
import { Branches } from "./Branches";
import { Commands } from "./Commands";
import { DeleteFolder } from "./DeleteFolder";
import { Occupancy } from "./Occupancy";
import { OwnedFiles } from "./OwnedFiles";
import { Settings } from "./Settings";
import { WorkflowSummary } from "./Workflows";
import { leadingConcern, toneFor, type LeadingConcern, type ProjectConcerns } from "./priority";

/**
 * "How is this now?"
 *
 * The antidote to a junk drawer is not fewer things — it is that one of them always leads. Two of
 * the design's principles force that together: every screen answers *is everything all right?*
 * first, and exceptions dominate while the normal disappears. A fixed hero card can do neither, so
 * the top of this page depends on the state.
 *
 * **The invariant that makes it safe to read:** the sections below are in the same order and all
 * present whatever the state is. Only the weight of the top one changes. That is what stops the
 * page from jumping under somebody's eyes when a proposal arrives while they are halfway down it,
 * and there is a test that renders both states and compares the set of panels.
 */

export interface ModeStateProps {
  projectId: string;
  /** Has the roster answered at all? Before it has, nothing here is a measurement. */
  answered: boolean;
}

export function ModeState({ projectId, answered }: ModeStateProps) {
  const projects = useProjects();
  const killSwitch = useKillSwitch();
  const budget = useBudget();
  const workflows = useProjectWorkflows(projectId);

  const project = projects.data?.find((row) => row.project_id === projectId);

  /**
   * What is known, and only what is known.
   *
   * Three of the six are real. The other three are passed as "nothing seen" because no route
   * answers them yet, and the calm sentence below is written to claim nothing about them.
   *
   * **The readings endpoint does not fill the gate one, and that is deliberate.** It reports how
   * many gates failed in thirty days, which is a tally and not a live concern: a failure from three
   * weeks ago that was rescued the same afternoon would make this page shout for the rest of the
   * month. The concern needs failures nothing has picked up, which is a different question about
   * different rows.
   *
   * **Workflow drift is real now, and it is two of the four standings and not three.** `drifted`
   * and `missing` are both *the thing this project pinned is not the thing it has*. An ejected copy
   * is neither: it is a decision somebody made deliberately, and a page that led with it would be
   * shouting about a choice — which is how a surface teaches people to stop reading its top.
   */
  const concerns: ProjectConcerns | null =
    !answered || project === undefined
      ? null
      : {
          killSwitch: killSwitch.data?.engaged === true,
          budgetPaused: budget.data?.paused === true,
          openProposals: project.open_review_items,
          failedGatesWithoutRescue: 0,
          interruptedRuns: 0,
          workflowDrift: driftingWorkflows(workflows.data).length > 0,
        };

  const leading = leadingConcern(concerns);

  return (
    <div className="flex flex-col gap-8">
      <Leading concern={leading} project={projectId} budgetReason={budget.data?.reason ?? null} />

      <Section label="Readings">
        <Readings projectId={projectId} />
      </Section>

      <Section label="Occupancy">
        <Occupancy projectId={projectId} />
      </Section>

      <Section label="Branches">
        <Branches projectId={projectId} />
      </Section>

      <Section label="Workflow">
        <WorkflowSummary projectId={projectId} />
      </Section>

      <Section label="Commands">
        <Commands projectId={projectId} />
      </Section>

      {/*
        The two halves of the write boundary, at the bottom and in this order: what the app authors
        in the database, then what it authors on disk. Last because the sections above answer *how
        is this now* and these two answer *what can I do to it* — and the top of this page is
        state-dependent by design, so the settled things belong furthest from it.
      */}
      <Section label="Settings">
        <Settings projectId={projectId} />
      </Section>

      <Section label="Files the app owns">
        <OwnedFiles projectId={projectId} />
      </Section>

      {/*
        Last, and below everything, because it is the only thing on this page that cannot be undone.
        Here rather than on the roster on purpose: that page compares projects, and a control that
        destroys one has no business in a column beside three that describe it. Somebody reaching
        this has already opened the project they mean and scrolled past everything it is doing.
      */}
      <Section label="Leaving">
        <DeleteFolder projectId={projectId} />
      </Section>
    </div>
  );
}

const CONCERN_TEXT: Record<string, string> = {
  "kill-switch": "The kill switch is engaged. Nothing autonomous starts, here or anywhere.",
  "budget-paused": "The budget is holding work.",
  "proposal-waiting": "Waiting on you.",
  "gate-failed": "A gate failed and nobody has picked it up.",
  "run-interrupted": "A run stopped without finishing and without being asked to.",
  "workflow-drift": "This project's workflow differs from the bundle it references.",
};

/**
 * The first paragraph — the one section whose weight changes.
 *
 * Calm is a sentence with air around it: no box, no border, no colour, because the design says the
 * normal disappears. Anything demanding a decision becomes the page's **one** raised layer, and
 * that rule survives whole — elevation is scarce, so it means something.
 *
 * What changed is what "raised" is made of. It used to be a shadow (`--shadow-float`) under a
 * bordered box, and this system is flat: depth is the neutral ladder, not a shadow stack, and
 * `--shadow-float` is applied to nothing else in the app. So the raise is a rung — Working up to
 * Raised — plus the state's own edge, which is the one thing on the page wearing a tone. In the
 * light theme that rung is not a colour change at all (Working and Raised are both `#ffffff`) and
 * the border carries the whole boundary, which is exactly why the border is not optional here.
 *
 * `unknown` is neither. A project the shell has not heard about is not calm, and saying it is would
 * be reporting a measurement nobody took.
 */
function Leading({
  concern,
  project,
  budgetReason,
}: {
  concern: LeadingConcern;
  project: string;
  budgetReason: string | null;
}) {
  const tone = toneFor(concern.kind);

  return (
    <section aria-label="Leading" aria-live="polite" className="min-h-16">
      {concern.kind === "unknown" ? (
        <p className="font-display text-lg text-text-faint">Reading {project}…</p>
      ) : concern.kind === "calm" ? (
        /*
          What it does NOT say matters as much as what it does. There is no "gate green" and no
          "delivered on time" here. Those readings exist now, but they are thirty-day tallies, and a
          calm line that turned a month's average into a claim about right now would be reassuring
          about something it did not check.
        */
        <p className="font-display text-lg leading-snug text-text-muted">
          Nothing waiting on you in {project}.
        </p>
      ) : (
        <div
          role="status"
          className="rounded-lg border bg-surface-raised p-5"
          style={{ borderColor: `var(--tone-${tone}-border)` }}
        >
          {/*
            A count is the one concern with somewhere to go: those proposals are rows in the
            Waiting queue, and the sentence naming them should be the way there, narrowed to
            this project. A kill switch or a budget hold has no queue behind it, so it stays a
            paragraph — a link that led nowhere would be a promise the page cannot keep.
          */}
          {concern.count === null ? (
            <p className="font-display text-xl font-medium leading-snug text-text">
              {CONCERN_TEXT[concern.kind]}
            </p>
          ) : (
            <Link
              to="/waiting"
              search={{ project }}
              className="font-display text-xl font-medium leading-snug"
            >
              {concern.count} {concern.count === 1 ? "decision" : "decisions"} waiting on you.
            </Link>
          )}
          {concern.kind === "budget-paused" && budgetReason !== null ? (
            <p className="mt-2 text-sm text-text-muted">{budgetReason}</p>
          ) : null}
        </div>
      )}
    </section>
  );
}

/**
 * Four readings, at two sizes.
 *
 * One principal and three supporting, never four identical cards: equal cards are a grid you scan
 * and forget, and the point of this row is that one number is the one worth knowing.
 *
 * Every one of them can be absent, and absent is drawn as an em dash with a reason under it. A
 * project too new to have a month behind it is the ordinary case, not the edge one.
 */
function Readings({ projectId }: { projectId: string }) {
  const readings = useProjectReadings(projectId);
  const data = readings.data;

  return (
    <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
      <EfficiencyReading data={data} />
      <CostReading data={data} />
      <GateReading data={data} />
      <DeliveredReading data={data} />
    </div>
  );
}

/** The frame every reading shares, so that four of them cannot drift into four layouts. */
function Card({
  label,
  span = false,
  children,
}: {
  label: string;
  span?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`rounded-lg border border-border bg-surface p-4 ${span ? "md:col-span-3" : ""}`}>
      <p className="text-xs uppercase tracking-wide text-text-faint">{label}</p>
      {children}
    </div>
  );
}

/**
 * An em dash and a reason, never a zero.
 *
 * A reading nobody took and a reading that came back zero are opposite facts, and the whole point
 * of the never-collapse contract is that the second must not be able to impersonate the first.
 */
function Absent({ why, big = false }: { why: string; big?: boolean }) {
  return (
    <>
      <p className={`mt-1 font-display ${big ? "text-3xl" : "text-xl"} text-text-faint`}>—</p>
      <p className="mt-1 text-xs text-text-faint">{why}</p>
    </>
  );
}

const TREND_TEXT = {
  improved: "fewer tokens than the month before",
  worsened: "more tokens than the month before",
  level: "level with the month before",
  unknown: "",
} as const;

function EfficiencyReading({ data }: { data: ProjectReadings | undefined }) {
  if (data === undefined) {
    return (
      <Card label="Token efficiency" span>
        <Absent why="reading…" big />
      </Card>
    );
  }

  const { efficiency } = data;
  if (efficiency.median_total_tokens === null) {
    return (
      <Card label="Token efficiency" span>
        <Absent
          big
          why={
            efficiency.unmeasured_runs > 0
              ? `${efficiency.unmeasured_runs} runs in the last ${data.window_days} days, none reporting usage`
              : `nothing finished in the last ${data.window_days} days`
          }
        />
      </Card>
    );
  }

  const trend = efficiencyTrend(efficiency);
  return (
    <Card label="Token efficiency" span>
      <p className="mt-1 font-display text-3xl font-bold tabular-nums text-text">
        {compactTokens(efficiency.median_total_tokens)}
      </p>
      <p className="text-xs text-text-faint">median per session</p>
      <p className="mt-1 text-xs text-text-muted">
        {efficiency.measured_runs} measured
        {/*
          Said out loud whenever there are any. Silence about the runs that reported nothing would
          let the median look as though it covered everything.
        */}
        {efficiency.unmeasured_runs > 0 ? `, ${efficiency.unmeasured_runs} reporting no usage` : ""}
        {trend === "unknown" ? "" : ` · ${TREND_TEXT[trend]}`}
      </p>
    </Card>
  );
}

function CostReading({ data }: { data: ProjectReadings | undefined }) {
  if (data === undefined) {
    return (
      <Card label="Cost">
        <Absent why="reading…" />
      </Card>
    );
  }
  if (data.cost.runs === 0) {
    return (
      <Card label="Cost">
        <Absent why={`nothing started in the last ${data.window_days} days`} />
      </Card>
    );
  }
  return (
    <Card label="Cost">
      <p className="mt-1 font-display text-xl font-bold tabular-nums text-text">${data.cost.usd.toFixed(2)}</p>
      <p className="mt-1 text-xs text-text-muted">
        over {data.cost.runs} {data.cost.runs === 1 ? "run" : "runs"}, {data.window_days} days
      </p>
    </Card>
  );
}

/**
 * The gate, as a proportion of what was actually judged.
 *
 * `no_gate` is outside the bar and stated separately. A project with fifty ungated runs and two
 * failures is not 96% green: it is two failures out of two measurements, and the fifty are a
 * different fact — nobody ever defined green here — which deserves its own sentence rather than a
 * silent cushion.
 */
function GateReading({ data }: { data: ProjectReadings | undefined }) {
  if (data === undefined) {
    return (
      <Card label="Gate">
        <Absent why="reading…" />
      </Card>
    );
  }

  const share = gateShare(data.gate);
  if (share.judged === 0) {
    return (
      <Card label="Gate">
        <Absent
          why={
            data.gate.no_gate > 0
              ? `${data.gate.no_gate} runs, no gate command configured`
              : `nothing judged in the last ${data.window_days} days`
          }
        />
      </Card>
    );
  }

  return (
    <Card label="Gate">
      <p className="mt-1 font-display text-xl font-bold tabular-nums text-text">
        {Math.round(share.passed * 100)}%
      </p>
      <p className="text-xs text-text-faint">of {share.judged} judged</p>
      <div className="mt-2 flex h-1.5 overflow-hidden rounded-pill bg-surface-sunken">
        <span style={{ width: `${share.passed * 100}%` }} className="bg-tone-active-fg" />
        <span style={{ width: `${share.failed * 100}%` }} className="bg-tone-danger-fg" />
        {/*
          A third colour, not a second. A gate that could not run is not a gate that said no, and
          the two sharing a red would tell somebody their tests broke when the measurement did.
        */}
        <span style={{ width: `${share.errored * 100}%` }} className="bg-tone-paused-fg" />
      </div>
      <p className="mt-1 text-xs text-text-muted">
        {data.gate.failed > 0 ? `${data.gate.failed} failed` : "none failed"}
        {data.gate.errored > 0 ? ` · ${data.gate.errored} could not run` : ""}
        {data.gate.no_gate > 0 ? ` · ${data.gate.no_gate} ungated` : ""}
      </p>
    </Card>
  );
}

function DeliveredReading({ data }: { data: ProjectReadings | undefined }) {
  if (data === undefined) {
    return (
      <Card label="Delivered">
        <Absent why="reading…" />
      </Card>
    );
  }
  if (data.delivered.landed === 0) {
    return (
      <Card label="Delivered">
        <Absent why={`nothing landed in the last ${data.window_days} days`} />
      </Card>
    );
  }
  return (
    <Card label="Delivered">
      <p className="mt-1 font-display text-xl font-bold tabular-nums text-text">
        {data.delivered.landed}
      </p>
      <p className="text-xs text-text-faint">landed</p>
      <p className="mt-1 text-xs text-text-muted">
        {data.delivered.median_minutes === null
          ? "none of them had a run to time from"
          : `${humanMinutes(data.delivered.median_minutes)} median, over ${data.delivered.timed} of ${data.delivered.landed}`}
      </p>
    </Card>
  );
}
