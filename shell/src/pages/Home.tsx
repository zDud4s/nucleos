import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { FeedEmbed } from "../app/FeedEmbed";
import { keys } from "../data/keys";
import {
  isAggregateTimeout,
  useBudget,
  useProjects,
  useSystemHealth,
  type BudgetView,
  type HealthReadout,
  type ProjectSummary,
  type SubsystemReadout,
} from "../data/system";
import { useWaitingCount } from "../data/waiting";
import { Meter, PageHeader, StaleNote, StatCard, ceilingShare, usd } from "../ui";

/**
 * The first screen: what needs you, whether the machine is well, and what it has spent.
 *
 * Home is a *reading*, not a console — nothing on this page mutates anything.
 * That is a deliberate constraint rather than an accident of what has been
 * built: the screen the app opens on is the one people look at while doing
 * something else, and a stop/start control on it would eventually be pressed by
 * someone who was looking at the state of five seconds ago. Every action lives
 * one click away, on the page that also shows you what you are acting on — and each
 * reading here is a link to that page, which navigates and mutates nothing.
 *
 * All four queries run at the fast cadence, because all four are answers to
 * "what is the machine doing right now".
 */
export function Home() {
  const projects = useProjects();
  const waiting = useWaitingCount();
  const budget = useBudget();
  const health = useSystemHealth();
  // `useWaitingCount` hands back only the number, which cannot tell "not asked yet" from "asked
  // and refused". Read the state of its query (same key) without mounting a second observer: an
  // observer with no queryFn would be a fetcher with nothing to fetch with. Home re-renders on
  // every change of that query because `useWaitingCount` above is subscribed to it.
  const waitingState = useQueryClient().getQueryState<unknown>(keys.waiting.count);
  const waitingRead = {
    isError: waitingState?.status === "error",
    data: waitingState?.data,
    dataUpdatedAt: waitingState?.dataUpdatedAt ?? 0,
  };

  const roster = projects.data;
  const spend = budget.data;
  const subsystems = health.data?.subsystems;
  const healthy = subsystems?.filter((row) => row.status === "ok").length;

  const active = roster?.filter((project) => project.mode === "active").length;
  const shadow = roster?.filter((project) => project.mode === "shadow").length;
  // Summed rather than counted: `pending` is how many shadow decisions a project
  // is holding, and a project holding nine is not the same news as nine projects
  // holding one.
  const shadowDecisions = roster?.reduce((total, project) => total + project.pending, 0);

  // A failed poll over old data keeps the old figure on screen; say so, per card and once
  // under the header with the age of the oldest good read.
  const projectsStale = isStale(projects);
  const waitingStale = isStale(waitingRead);
  const budgetStale = isStale(budget);
  const healthStale = isStale(health);
  const staleReads = [
    projectsStale ? projects.dataUpdatedAt : null,
    waitingStale ? waitingRead.dataUpdatedAt : null,
    budgetStale ? budget.dataUpdatedAt : null,
    healthStale ? health.dataUpdatedAt : null,
  ].filter((at): at is number => at !== null);
  const oldestRead = staleReads.length === 0 ? null : Math.min(...staleReads);

  const notHealthy = subsystems?.filter(
    (row) => row.status === "down" || row.status === "degraded",
  ).length;
  const notConfigured = subsystems?.filter((row) => row.status === "disabled").length;
  const faulty = wrongClause(health.data) !== null;

  return (
    <>
      <PageHeader title="Home" headline={headline(roster, waiting, spend, health.data)} />
      {oldestRead !== null && <StaleNote dataUpdatedAt={oldestRead} />}

      {/*
        Three tiers, always the same three. What can need you today leads, large: the queue that
        holds your decisions and whether the machine under it is well. The two autopilot
        readings are standing readings and sit quieter beneath. Spend is a ceiling, so it is a
        labelled meter row rather than a fourth figure. Nothing recedes when everything is well,
        and that is a decision rather than an omission: a card that appeared only when something
        was wrong would teach the reader that an absent card is an absent fact. What was lying
        here was the headline; the cards were already right.

        A conditional card would also change the page's shape under the eyes of somebody halfway
        down it, which is the invariant `project/ModeState.tsx:31-34` already defends for a
        project page.
      */}
      <div className="app-home-stats">
        <div className="app-home-lead">
          <Cell stale={waitingStale}>
            <StatCard
              label="Waiting on you"
              value={waiting}
              to="/waiting"
              unread={unreadNote(waitingRead)}
              // What the number IS, not a table of contents for another page. Four lists are outside it
              // and for three different reasons: skipped and refused are records, calendar events have no
              // listing route, and a parked run is the run side of an approval already counted.
              detail={
                waiting === undefined
                  ? undefined
                  : waiting === 0
                    ? "nothing waiting on you"
                    : "decisions held for you — not records, and not the calendar"
              }
            />
          </Cell>
          {/*
            Whether the machine under the autopilot is well. The count is Home's own sentence and
            not System's: System writes a headline for the whole page, this is one card's detail,
            and the names of what is wrong are already in the headline above.
          */}
          <Cell stale={healthStale} wrong={faulty}>
            <StatCard
              label="Subsystems healthy"
              value={
                subsystems === undefined || healthy === undefined
                  ? undefined
                  : `${healthy}/${subsystems.length}`
              }
              to="/system"
              unread={unreadNote(health)}
              /* One device for one piece of news: the clause that is wrong wears the tone and the
                 figure stays the figure. See the note under `.ui-stat-detail` in `ui.css`. */
              detail={
                subsystems === undefined ? undefined : (
                  <HealthDetail
                    readout={health.data}
                    total={subsystems.length}
                    notHealthy={notHealthy ?? 0}
                    notConfigured={notConfigured ?? 0}
                  />
                )
              }
            />
          </Cell>
        </div>

        <div className="app-home-quiet">
          <Cell stale={projectsStale}>
            <StatCard
              label="Projects"
              value={roster?.length}
              to="/projects"
              unread={unreadNote(projects)}
              detail={
                active === undefined || shadow === undefined
                  ? undefined
                  : `${active} active · ${shadow} shadow`
              }
            />
          </Cell>
          <Cell stale={projectsStale}>
            <StatCard
              label="Shadow decisions pending"
              value={shadowDecisions}
              to="/autopilot"
              unread={unreadNote(projects)}
              detail="what the autopilot would have done, waiting to be read"
            />
          </Cell>
        </div>

        <Cell stale={budgetStale}>
          <SpendRow spend={spend} read={budget} />
        </Cell>
      </div>

      {/* What has actually happened, which is the question the figures above raise
          and none of them answers. Five lines, not the cockpit's ten: this is the last
          block of the first screen, not a feed reader. */}
      <FeedEmbed lines={5} />
    </>
  );
}

/** A failed poll that still holds an earlier good read: the figure on screen is old. */
function isStale(read: { isError: boolean; data: unknown }): boolean {
  return read.isError && read.data !== undefined;
}

/** Why a figure is missing: still reading, or the núcleo did not answer. */
function unreadNote(read: { isError: boolean }): string {
  return read.isError ? "the núcleo did not answer" : "reading…";
}

/**
 * One grid cell. Stale data gets `.ui-stale` (a dashed edge, never dimmed text) and a fault
 * gets a red edge on top of the glyph and the words, so colour is never the only signal.
 */
function Cell({
  stale,
  wrong = false,
  children,
}: {
  stale: boolean;
  wrong?: boolean;
  children: ReactNode;
}) {
  const classes = ["app-home-cell"];
  if (stale) classes.push("ui-stale");
  if (wrong) classes.push("app-home-cell-wrong");
  return <div className={classes.join(" ")}>{children}</div>;
}

/**
 * The health card's line. Counts only: which subsystems are down is the headline's job and
 * `/system`'s, and a subsystem nobody configured is counted apart because it is not a fault.
 */
function HealthDetail({
  readout,
  total,
  notHealthy,
  notConfigured,
}: {
  readout: HealthReadout | undefined;
  total: number;
  notHealthy: number;
  notConfigured: number;
}) {
  const apart = notConfigured > 0 ? ` · ${String(notConfigured)} not configured` : "";
  if (readout !== undefined && isAggregateTimeout(readout)) {
    return (
      <span className="ui-wrong">
        <span aria-hidden="true">{"⚠︎"} </span>
        the health readout timed out
      </span>
    );
  }
  if (notHealthy === 0) {
    return <>{`all configured subsystems healthy${apart}`}</>;
  }
  return (
    <>
      <span className="ui-wrong">
        <span aria-hidden="true">{"⚠︎"} </span>
        {`${String(notHealthy)} of ${String(total)} not healthy`}
      </span>
      {apart}
    </>
  );
}

/**
 * Spend, as a labelled meter row. The ceiling is the news, so the bar is the figure: `Meter`
 * escalates by itself at 80% and 100% and writes the percentage beside the bar.
 */
function SpendRow({
  spend,
  read,
}: {
  spend: BudgetView | undefined;
  read: { isError: boolean };
}) {
  return (
    <Link to="/system" className="app-home-spend">
      {spend === undefined ? (
        <span className="app-home-spend-unread">
          <span className="app-home-spend-title">Spend</span>
          <span aria-hidden="true">—</span>
          <span className="sr-only">not read</span> {unreadNote(read)}
        </span>
      ) : (
        <Meter
          label={`Spend · ${spend.period}`}
          value={spend.window_spend_usd}
          ceiling={spend.limit_usd}
          tone="quantity"
          format={usd}
        />
      )}
    </Link>
  );
}

/**
 * One derived sentence about the state of the machine — the worst thing first.
 *
 * Not a description of the page: the title already says what this is. This is the
 * line that changes, and it is the reason the shell can be glanced at rather than
 * read. Which is why the order it picks in is a ladder and not a preference:
 *
 *   1. the worst live fact about the machine — a subsystem down, or degraded;
 *   2. a ceiling holding autonomous work, or about to;
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
  waiting: number | undefined,
  spend: BudgetView | undefined,
  health: HealthReadout | undefined,
): ReactNode {
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
        {/* The clause is red and the tail is not: "; 2 waiting on you" is the queue doing its
            job, not a fault, and colouring it with the fault would make the page report two
            problems where there is one. The door keeps the clause's colour rather than the
            accent — see `.ui-wrong-door` in `ui.css` for why that cannot be a utility. */}
        <span className="ui-wrong">
          <Link to="/system" className="ui-wrong-door">
            {wrong}
          </Link>
        </span>
        {tail}
      </>
    );
  }

  if (spend?.paused === true) {
    return `autonomous work is held — ${spend.reason ?? "a ceiling is holding it"}`;
  }

  // Before the pause, not after: a ceiling that is about to hold work is the moment to say so.
  const standing =
    spend === undefined || spend.limit_usd === null || spend.limit_usd <= 0
      ? "under"
      : ceilingShare(spend.window_spend_usd, spend.limit_usd);
  const nearing =
    standing === "near"
      ? "spend is near its ceiling"
      : standing === "over"
        ? "spend is at its ceiling"
        : null;

  if (roster === undefined) return nearing === null ? undefined : `${nearing}${tail}`;

  const activeCount = roster.filter((project) => project.mode === "active").length;
  const shadowCount = roster.filter((project) => project.mode === "shadow").length;

  const modes =
    activeCount === 0 && shadowCount === 0
      ? "the autopilot is off in every project"
      : `${activeCount} acting, ${shadowCount} in shadow`;

  const lead = nearing === null ? modes : `${nearing}; ${modes}`;
  if (waiting === undefined) return lead;
  return `${lead}${tail}`;
}

/**
 * What a subsystem is called to a person. The readout's names are identifiers
 * (`browser_sidecar`); the sidecar suffix is plumbing and the underscores are not words.
 */
function displayName(name: string): string {
  return name.replace(/_sidecar$/, "").replace(/_/g, " ");
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
    const named = down.map((row) => displayName(row.name)).join(", ");
    parts.push(`${String(down.length)} subsystem${down.length === 1 ? "" : "s"} down (${named})`);
  }
  if (degraded.length > 0) parts.push(`${String(degraded.length)} degraded`);
  return parts.join(", ");
}
