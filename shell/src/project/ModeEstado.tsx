import type { ReactNode } from "react";
import { useBudget, useKillSwitch, useProjects } from "../data/system";
import { Occupancy } from "./Occupancy";
import { leadingConcern, toneFor, type LeadingConcern, type ProjectConcerns } from "./priority";

/**
 * "How is this now?"
 *
 * The antidote to a junk drawer is not fewer things — it is that one of them
 * always leads. Two of the design's principles force that together: every screen
 * answers *is everything all right?* first, and exceptions dominate while the
 * normal disappears. A fixed hero card can do neither, so the top of this page
 * depends on the state.
 *
 * **The invariant that makes it safe to read:** the sections below are in the
 * same order and all present whatever the state is. Only the weight of the top
 * one changes. That is what stops the page from jumping under somebody's eyes
 * when a proposal arrives while they are halfway down it, and there is a test
 * that renders both states and compares the set of panels.
 */

export interface ModeEstadoProps {
  projectId: string;
  /** Has the roster answered at all? Before it has, nothing here is a measurement. */
  answered: boolean;
}

export function ModeEstado({ projectId, answered }: ModeEstadoProps) {
  const projects = useProjects();
  const killSwitch = useKillSwitch();
  const budget = useBudget();

  const project = projects.data?.find((row) => row.project_id === projectId);

  /**
   * What is known, and only what is known.
   *
   * Three of the six are real today. The other three need núcleo routes that do
   * not exist yet — the gate tally and the delivered count come with the
   * readings endpoint, workflow drift with the bundle library — and they are
   * passed as "nothing seen" rather than left out, which is a difference the
   * calm sentence below has to be careful about: a concern that cannot be
   * measured must not make the page *claim* the project is fine on that count.
   */
  const concerns: ProjectConcerns | null =
    !answered || project === undefined
      ? null
      : {
          killSwitch: killSwitch.data?.engaged === true,
          budgetPaused: budget.data?.paused === true,
          openProposals: project.open_proposals,
          failedGatesWithoutRescue: 0,
          interruptedRuns: 0,
          workflowDrift: false,
        };

  const leading = leadingConcern(concerns);

  return (
    <div className="flex flex-col gap-8">
      <Leading concern={leading} project={projectId} budgetReason={budget.data?.reason ?? null} />

      <Section label="Readings">
        <Readings />
      </Section>

      <Section label="Occupancy">
        <Occupancy projectId={projectId} />
      </Section>

      <Section label="Branches">
        <NotServedYet
          what="Live branches and their distance from the integration branch"
          why="the núcleo has no git log route yet"
        />
      </Section>

      <Section label="Workflow">
        <NotServedYet
          what="The installed workflow, as a chain with the running node lit"
          why="the workflow library is not built yet"
        />
      </Section>

      <Section label="Commands">
        <NotServedYet
          what="This project's own commands"
          why="the núcleo has no command registry yet"
        />
      </Section>
    </div>
  );
}

/**
 * A panel of the page.
 *
 * The `aria-label` is not decoration: it is the handle the composition test
 * grabs the page by, and it is the same handle a screen reader uses. Protecting
 * the structure somebody hears and the structure somebody sees with one
 * assertion is worth more than protecting either alone.
 */
function Section({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex flex-col gap-3">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        {label}
      </h2>
      {children}
    </section>
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
 * Calm is a sentence with air around it: no box, no border, no colour, because
 * the design says the normal disappears. Anything demanding a decision becomes
 * the page's **one** elevated layer — glass, shadow, a border in the state's
 * tone — which is the rule the whole visual language rests on: elevation is
 * scarce, so it means something.
 *
 * `unknown` is neither. A project the shell has not heard about is not calm, and
 * saying it is would be reporting a measurement nobody took.
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
    <section aria-label="Leading" className="min-h-16">
      {concern.kind === "unknown" ? (
        <p className="font-display text-lg text-text-faint">Reading {project}…</p>
      ) : concern.kind === "calm" ? (
        /*
          What it does NOT say matters as much as what it does. There is no "gate
          green" and no "delivered on time" here, because neither is measured
          yet — and a calm line that lists reassurances it did not check is the
          exact failure §12 is about.
        */
        <p className="font-display text-lg leading-snug text-text-muted">
          Nothing waiting on you in {project}.
        </p>
      ) : (
        <div
          className="rounded-lg border bg-surface-raised p-5 shadow-float"
          style={{ borderColor: `var(--tone-${tone}-border)` }}
        >
          <p className="font-display text-xl font-medium leading-snug text-text">
            {concern.count === null
              ? CONCERN_TEXT[concern.kind]
              : `${concern.count} ${concern.count === 1 ? "decision" : "decisions"} waiting on you.`}
          </p>
          {concern.kind === "budget-paused" && budgetReason !== null ? (
            <p className="mt-2 text-sm text-text-muted">{budgetReason}</p>
          ) : null}
        </div>
      )}
    </section>
  );
}

/**
 * Four readings, at three sizes.
 *
 * One principal and three supporting, never four identical cards: equal cards
 * are a grid you scan and forget, and the point of this row is that one number
 * is the one worth knowing. They are empty until the núcleo has a route that
 * aggregates a project's runs — and "not measured" is the state they will keep
 * having afterwards, for a project too new to have thirty days behind it.
 */
function Readings() {
  return (
    <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
      <Reading label="Token efficiency" span />
      <Reading label="Cost, 30 days" />
      <Reading label="Gate, last 30 runs" />
      <Reading label="Delivered, 30 days" />
    </div>
  );
}

function Reading({ label, span = false }: { label: string; span?: boolean }) {
  return (
    <div
      className={`rounded-lg border border-border bg-surface p-4 ${span ? "md:col-span-3" : ""}`}
    >
      <p className="text-xs uppercase tracking-wide text-text-faint">{label}</p>
      {/*
        An em dash and a reason, never a zero. A reading nobody has taken and a
        reading that came back zero are opposite facts, and the whole §7 contract
        is that the second must never be able to impersonate the first.
      */}
      <p className={`mt-1 font-display ${span ? "text-3xl" : "text-xl"} text-text-faint`}>—</p>
      <p className="mt-1 text-xs text-text-faint">not measured yet</p>
    </div>
  );
}

/**
 * A panel that is designed and not yet served.
 *
 * Dimmed and explained, never hidden. The reason names the *núcleo* rather than
 * this machine — the same rule the sidebar's disabled items follow — because
 * "not built" and "not configured here" send somebody to two different places,
 * and only one of them is something they can do anything about.
 */
function NotServedYet({ what, why }: { what: string; why: string }) {
  return (
    <div className="rounded-lg border border-dashed border-border bg-surface-sunken p-4">
      <p className="text-sm text-text-muted">{what}</p>
      <p className="mt-1 text-xs text-text-faint">Not here yet — {why}.</p>
    </div>
  );
}
