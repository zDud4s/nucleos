import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { FeedEmbed } from "../app/FeedEmbed";
import {
  isAggregateTimeout,
  useBudget,
  useProjects,
  useProposals,
  useSystemHealth,
  type BudgetView,
  type HealthReadout,
  type ProjectSummary,
  type Proposal,
  type SubsystemReadout,
} from "../data/system";
import { PageHeader, Section, StatCard } from "../ui";
import { headlineFor as systemHeadline } from "./System";

/**
 * The first screen: four numbers and two doors.
 *
 * Home is a *reading*, not a console — nothing on this page mutates anything.
 * That is a deliberate constraint rather than an accident of what has been
 * built: the screen the app opens on is the one people look at while doing
 * something else, and a stop/start control on it would eventually be pressed by
 * someone who was looking at the state of five seconds ago. Every action lives
 * one click away, on the page that also shows you what you are acting on.
 *
 * All three queries run at the fast cadence, because all three are answers to
 * "what is the machine doing right now".
 */
export function Home() {
  const projects = useProjects();
  const proposals = useProposals();
  const budget = useBudget();
  const health = useSystemHealth();

  const roster = projects.data;
  const queue = proposals.data;
  const spend = budget.data;
  const subsystems = health.data?.subsystems;
  const healthy = subsystems?.filter((row) => row.status === "ok").length;

  const active = roster?.filter((project) => project.mode === "active").length;
  const shadow = roster?.filter((project) => project.mode === "shadow").length;
  // Summed rather than counted: `pending` is how many shadow decisions a project
  // is holding, and a project holding nine is not the same news as nine projects
  // holding one.
  const shadowDecisions = roster?.reduce((total, project) => total + project.pending, 0);

  return (
    <>
      <PageHeader title="Home" headline={headline(roster, queue, spend, health.data)} />

      {/*
        Five cards, always five. They do not recede when everything is well, and that is a
        decision rather than an omission: the four autopilot readings are standing readings, not
        exceptions, and a card that appeared only when something was wrong would teach the reader
        that an absent card is an absent fact — the opposite of the honesty this page is being
        fixed for. What was lying here was the headline; the cards were already right.

        A conditional card would also change the page's shape under the eyes of somebody halfway
        down it, which is the invariant `project/ModeState.tsx:31-34` already defends for a
        project page.
      */}
      <div className="app-home-stats">
        <StatCard
          label="Projects"
          value={roster?.length}
          detail={
            active === undefined || shadow === undefined ? undefined : `${active} active · ${shadow} shadow`
          }
        />
        <StatCard
          label="Shadow decisions pending"
          value={shadowDecisions}
          detail="what the autopilot would have done, waiting to be read"
        />
        <StatCard
          label="Approval queue"
          value={queue?.length}
          detail={queue === undefined ? undefined : queue.length === 0 ? "nothing waiting on you" : "waiting on you"}
        />
        <StatCard
          label="Window spend"
          value={spend === undefined ? undefined : `$${spend.window_spend_usd.toFixed(2)}`}
          detail={ceiling(spend)}
        />
        {/*
          The fifth, and the one that is not about the autopilot: whether the machine
          under it is well. `headlineFor` is System's own sentence, imported rather than
          rewritten — the first screen is where somebody finds out a subsystem is down,
          and two screens describing one readout in two ways is how a person learns to
          check both.
        */}
        <StatCard
          label="Subsystems healthy"
          value={
            subsystems === undefined || healthy === undefined
              ? undefined
              : `${healthy}/${subsystems.length}`
          }
          detail={systemHeadline(health.data)}
        />
      </div>

      {/*
        Two doors, and no cards around them. They were bordered blocks with a title and a
        paragraph each — the same weight as the four readings above, for two links that
        say where a link goes. A `Section` puts them under a heading with no frame, which
        is what a list of two places is.
      */}
      <Section label="Where to look next">
        <div className="flex flex-col gap-2">
          <p className="text-sm text-text-muted">
            <Link to="/autopilot" className="ui-button ui-button-link">
              Autopilot
            </Link>{" "}
            — the mode of every project, the bar one has to clear to leave shadow, and the
            ceilings that hold work back.
          </p>
          <p className="text-sm text-text-muted">
            <Link to="/waiting" className="ui-button ui-button-link">
              Waiting
            </Link>{" "}
            — everything that stopped to ask you something, of every kind, in one queue.
          </p>
        </div>
      </Section>

      {/* What has actually happened, which is the question the four figures above raise
          and none of them answers. Five lines, not the cockpit's ten: this is the last
          block of the first screen, not a feed reader. */}
      <FeedEmbed lines={5} />
    </>
  );
}

/**
 * The ceiling line under the spend.
 *
 * `limit_usd === null` is **no ceiling**, and it is never rendered as a zero or
 * as a missing value. A ceiling of `0.00` stops all autonomous work; no ceiling
 * stops none of it. Printing the first where the second is true — or the other
 * way round — is the shell inventing a spending policy.
 */
function ceiling(spend: BudgetView | undefined): string | undefined {
  if (spend === undefined) return undefined;
  if (spend.limit_usd === null) return `no ceiling · ${spend.period}`;
  return `of $${spend.limit_usd.toFixed(2)} · ${spend.period}`;
}

/**
 * One derived sentence about the state of the machine — the worst thing first.
 *
 * Not a description of the page: the title already says what this is. This is the
 * line that changes, and it is the reason the shell can be glanced at rather than
 * read. Which is why the order it picks in is a ladder and not a preference:
 *
 *   1. the worst live fact about the machine — a subsystem down, or degraded;
 *   2. a ceiling holding autonomous work;
 *   3. the modes the projects are in.
 *
 * A subsystem being down outranks a ceiling holding work because one says the
 * machine is broken and the other says it is being restrained, and both outrank a
 * count of who is acting, which is only news while nothing is wrong.
 *
 * The wrong-fact clause is a **link**, and that is the substance of it rather than
 * decoration. `/system` is where the answer to *which one, and why* is kept, so a
 * sentence that names a problem and then goes nowhere leaves the reader to carry
 * the fact across the window by hand — and a reader who has had to do that twice
 * stops reading the sentence. Naming a problem with no door to it is the exact
 * failure this page is being repaired for.
 *
 * `ReactNode` and not `string` because of that link; this is the only headline in
 * the app that is not a string.
 */
function headline(
  roster: ProjectSummary[] | undefined,
  queue: Proposal[] | undefined,
  spend: BudgetView | undefined,
  health: HealthReadout | undefined,
): ReactNode {
  const waiting = queue?.length;
  const tail =
    waiting === undefined
      ? ""
      : waiting === 0
        ? "; nothing waiting on you"
        : `; ${String(waiting)} waiting on you`;

  // The worst live fact leads, and it is a door. A subsystem being down outranks a ceiling
  // holding work: one says the machine is broken, the other says it is being restrained.
  const wrong = wrongClause(health);
  if (wrong !== null) {
    return (
      <>
        <Link to="/system">{wrong}</Link>
        {tail}
      </>
    );
  }

  if (spend?.paused === true) {
    return `autonomous work is held — ${spend.reason ?? "a ceiling is holding it"}`;
  }
  if (roster === undefined) return undefined;

  const active = roster.filter((project) => project.mode === "active").length;
  const shadow = roster.filter((project) => project.mode === "shadow").length;

  const modes =
    active === 0 && shadow === 0
      ? "the autopilot is off in every project"
      : `${active} acting, ${shadow} in shadow`;

  if (waiting === undefined) return modes;
  return `${modes}${tail}`;
}

/**
 * The worst live fact about the machine, or nothing when there is none.
 *
 * `down` before `degraded`, both named, and the down ones named BY NAME: "1 subsystem down"
 * sends somebody to /system to find out which, and the answer is three words long. `disabled`
 * is deliberately absent — a subsystem nobody configured is not a fault, which is the same
 * rule `wantsAttention` already follows for the rail's dot.
 */
function wrongClause(readout: HealthReadout | undefined): string | null {
  if (readout === undefined) return null;
  if (isAggregateTimeout(readout))
    return "the health readout timed out before it measured anything";
  const down: SubsystemReadout[] = readout.subsystems.filter((row) => row.status === "down");
  const degraded: SubsystemReadout[] = readout.subsystems.filter(
    (row) => row.status === "degraded",
  );
  if (down.length === 0 && degraded.length === 0) return null;
  const parts: string[] = [];
  if (down.length > 0) {
    const named = down.map((row) => row.name).join(", ");
    parts.push(`${String(down.length)} subsystem${down.length === 1 ? "" : "s"} down (${named})`);
  }
  if (degraded.length > 0) parts.push(`${String(degraded.length)} degraded`);
  return parts.join(", ");
}
