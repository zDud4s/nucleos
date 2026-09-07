import { Link } from "@tanstack/react-router";
import { FeedEmbed } from "../app/FeedEmbed";
import {
  useBudget,
  useProjects,
  useProposals,
  useSystemHealth,
  type BudgetView,
  type ProjectSummary,
  type Proposal,
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
      <PageHeader title="Home" headline={headline(roster, queue, spend)} />

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
 * One derived sentence about the state of the autopilot.
 *
 * Not a description of the page — the title already says what this is. This is
 * the line that changes, and it is the reason the shell can be glanced at
 * rather than read.
 */
function headline(
  roster: ProjectSummary[] | undefined,
  queue: Proposal[] | undefined,
  spend: BudgetView | undefined,
): string | undefined {
  if (spend?.paused === true) {
    return `autonomous work is held — ${spend.reason ?? "a ceiling is holding it"}`;
  }
  if (roster === undefined) return undefined;

  const active = roster.filter((project) => project.mode === "active").length;
  const shadow = roster.filter((project) => project.mode === "shadow").length;
  const waiting = queue?.length;

  const modes =
    active === 0 && shadow === 0
      ? "the autopilot is off in every project"
      : `${active} acting, ${shadow} in shadow`;

  if (waiting === undefined) return modes;
  return waiting === 0 ? `${modes}; nothing waiting on you` : `${modes}; ${waiting} waiting on you`;
}
