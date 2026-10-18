import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  isBlockError,
  type Autopilot,
  type Gate,
  type Jobs,
  type Landing,
  type PipelineReport,
  type Postgate,
  type Pressure,
} from "../../data/analytics-pipeline";
import { compactTokens } from "../../data/project-readings";
import { AreaChart, Badge, ShareBar, usd, type ChartKey } from "../../ui";
import { versus } from "../series";
import { joined, plural, summary, type PanelContent } from "../sections";
import { EMPTY, PRESSURE_KEY, series, stuckCount, type PipelineKey } from "./cards";
import { NO_DATA, clauses, distLine, humanSeconds, percentOr } from "./format";

/**
 * The detail panel of each pipeline card (spec §8): the one chart, the short lists, and the figures
 * the card has no room for. One function per block, each taking the block already narrowed; a block
 * that is `{error}` gets the same sentence from `pipelinePanel` and nothing else.
 *
 * **Adding a panel pair** (e.g. a block's new field): append a `<Pairs>` or `<ShareBar>` to that
 * block's function. **Adding a panel**: write `xPanel` and register it in `PANELS` below.
 */

const DETAIL_HEIGHT = 220;
const count = (value: number) => String(Math.round(value));
const seconds = (ms: number) => humanSeconds(ms / 1000);

/** Label and figure chips, `RunsPanel`'s `an-pairs`. */
export function Pairs({ label, rows }: { label: string; rows: Array<[name: string, figure: string]> }) {
  if (rows.length === 0) return null;
  return (
    <ul className="an-pairs" aria-label={label}>
      {rows.map(([name, figure]) => (
        <li key={name} className="an-pair">
          <span className="an-pair-mode">{name}</span>
          <span className="an-pair-count">{figure}</span>
        </li>
      ))}
    </ul>
  );
}

/** A project's state view, where its red or its stuck job is dealt with. */
function ProjectLink({ projectId, children }: { projectId: string; children: ReactNode }) {
  return (
    <Link to="/projects/$projectId/$view" params={{ projectId, view: "state" }}>
      {children}
    </Link>
  );
}

function Rows({ label, children }: { label: string; children: ReactNode }) {
  return (
    <ul className="an-rows" aria-label={label}>
      {children}
    </ul>
  );
}

/** The date a window starts on, and whether the record begins after it. */
function recordingNote(since: string | null, from: string): ReactNode {
  if (since === null) return null;
  const date = since.slice(0, 10);
  if (date <= from) return null;
  return <p className="an-note">{`Recording since ${date}. No data for earlier periods yet.`}</p>;
}

/* -------------------------------------------------------------- post-merge -- */

function postgatePanel(postgate: Postgate, report: PipelineReport): PanelContent {
  const key: ChartKey = { key: "red", label: "Red time", className: "ui-chart-failed" };
  const data = series(postgate.red_s_daily, key, (day) => day.red_s);
  const label = `Red time per day, last ${report.days} days`;
  return {
    title: "Red time per day",
    aside: joined([
      `${postgate.red_events} red`,
      plural(postgate.reverts, "revert", "reverts"),
      versus(postgate.red_s_in_window, postgate.prev_red_s, report.days),
    ]),
    body: (
      <>
        {recordingNote(report.history.postgate_since, report.window.from)}
        <AreaChart
          data={data}
          keys={[key]}
          label={label}
          format={humanSeconds}
          empty="No red in this window"
          tone="ui-chart-failed"
          height={DETAIL_HEIGHT}
        />
        {summary(data, humanSeconds)}
        {postgate.red_now ? (
          <Pairs
            label="Red now"
            rows={[
              ...(postgate.red_groups.length > 0 ? [["Groups", postgate.red_groups.join(", ")] as [string, string]] : []),
              ...(postgate.culprit_sha === null ? [] : [["Culprit", postgate.culprit_sha.slice(0, 8)] as [string, string]]),
            ]}
          />
        ) : null}
        {report.scope === "global" && postgate.per_project.length > 0 ? (
          <Rows label="Red time by project">
            {postgate.per_project.map((one) => (
              <li key={one.project_id} className="an-row">
                <ProjectLink projectId={one.project_id}>{one.project_id}</ProjectLink>
                <span className="an-row-figures">
                  {clauses([one.red_now ? "red now" : "", `${humanSeconds(one.red_s)} red`])}
                </span>
              </li>
            ))}
          </Rows>
        ) : null}
      </>
    ),
  };
}

/* ----------------------------------------------------------------- landing -- */

function landingPanel(landing: Landing, report: PipelineReport): PanelContent {
  const key: ChartKey = { key: "total", label: "Landings" };
  const data = series(landing.daily, key, (day) => day.total);
  const { status_mix: mix, failure_buckets: buckets } = landing;
  return {
    title: "Landings per day",
    aside: joined([
      `${landing.total} in ${report.days} days`,
      landing.unexecutable > 0 ? `${landing.unexecutable} unexecutable` : "",
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={[key]}
          label={`Landings per day, last ${report.days} days`}
          format={count}
          empty={EMPTY}
          tone="ui-chart-runs"
          height={DETAIL_HEIGHT}
        />
        {summary(data, count)}
        <ShareBar
          label="Landings by result"
          format={count}
          items={[
            { key: "succeeded", label: "Succeeded", value: mix.succeeded, className: "ui-chart-passed" },
            { key: "failed", label: "Failed", value: mix.failed, className: "ui-chart-failed" },
            { key: "escalated", label: "Escalated", value: mix.escalated, className: "ui-chart-errored" },
            { key: "blocked", label: "Blocked", value: mix.blocked, className: "ui-chart-ungated" },
            { key: "cancelled", label: "Cancelled", value: mix.cancelled, className: "ui-chart-unknown" },
            { key: "other", label: "Other", value: mix.other, className: "ui-chart-other" },
          ]}
        />
        <ShareBar
          label="Why landings went wrong"
          format={count}
          items={[
            { key: "conflict", label: "Conflict", value: buckets.conflict },
            { key: "gate", label: "Gate", value: buckets.gate },
            { key: "blocked", label: "Blocked", value: buckets.blocked },
            { key: "interrupted", label: "Interrupted", value: buckets.interrupted },
            { key: "other", label: "Other", value: buckets.other },
          ]}
        />
        <Pairs
          label="Landing times"
          rows={[
            ["Wait (last attempt)", distLine(landing.wait_s)],
            ["Wait, machine only", distLine(landing.wait_s_machine)],
            ["Run", distLine(landing.run_s)],
          ]}
        />
        {landing.conflicts === null ? null : (
          <Pairs
            label="Conflicts"
            rows={[
              ["Conflict rate", `${percentOr(landing.conflicts.rate)} · ${landing.conflicts.count} conflicts`],
              ["Auto-resolved", String(landing.conflicts.auto_resolved)],
              ["Work lost: nothing", String(landing.conflicts.work_lost.nothing)],
              ["Work lost: lost", String(landing.conflicts.work_lost.lost)],
            ]}
          />
        )}
      </>
    ),
  };
}

/* -------------------------------------------------------------------- gate -- */

function gatePanel(gate: Gate, report: PipelineReport): PanelContent {
  const key: ChartKey = { key: "p50", label: "Median duration" };
  const data = series(gate.duration_ms_daily, key, (day) => day.p50 ?? 0);
  const multi = report.scope === "global";
  return {
    title: "Gate duration per day",
    aside: joined([
      `${gate.terminal} finished`,
      gate.retry_share === null ? "" : `${percentOr(gate.retry_share)} needed a retry`,
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={[key]}
          label={`Median gate duration per day, last ${report.days} days`}
          format={seconds}
          empty={EMPTY}
          tone="ui-chart-runs"
          height={DETAIL_HEIGHT}
        />
        {summary(data, seconds)}
        {gate.most_red.length === 0 ? null : (
          <Rows label="Groups that fail most">
            {gate.most_red.map((one) => (
              <li key={one.group} className="an-row">
                <span>{one.group}</span>
                <span className="an-row-figures">{`${one.red} of ${one.terminal} · ${percentOr(one.rate)}`}</span>
              </li>
            ))}
          </Rows>
        )}
        {gate.cache === null && gate.bypass === null ? null : (
          <Pairs
            label="Cache and bypass"
            rows={[
              ...(gate.cache === null
                ? []
                : [
                    [
                      "Cache hit rate",
                      clauses([
                        percentOr(gate.cache.hit_rate),
                        gate.cache.prev_hit_rate === null ? "" : `before ${percentOr(gate.cache.prev_hit_rate)}`,
                      ]),
                    ] as [string, string],
                  ]),
              ...(gate.bypass === null
                ? []
                : [
                    [
                      "Bypass",
                      clauses([
                        `${gate.bypass.observed} of ${gate.bypass.verify_units} verify units`,
                        percentOr(gate.bypass.ratio),
                      ]),
                    ] as [string, string],
                  ]),
            ]}
          />
        )}
        {gate.legacy_runs > 0 ? <p className="an-note">{`${gate.legacy_runs} older runs without a group`}</p> : null}
        {gate.flaky.length === 0 ? null : (
          <Rows label="Flaky groups">
            {gate.flaky.map((one) => (
              <li key={`${one.project_id}:${one.group}`} className="an-row">
                <span>{multi ? `${one.project_id} · ${one.group}` : one.group}</span>
                <span className="an-row-figures">
                  {`${one.flaky} of ${one.observed} · ${percentOr(one.rate)} `}
                  {one.suspect ? <Badge tone="paused">suspect</Badge> : null}
                </span>
              </li>
            ))}
          </Rows>
        )}
      </>
    ),
  };
}

/* -------------------------------------------------------------------- jobs -- */

const WAITING_LABELS: Array<[keyof Jobs["stuck"]["waiting"], string]> = [
  ["slot", "A slot"],
  ["budget", "Budget"],
  ["quota", "Quota"],
  ["disk", "Disk"],
  ["excluded", "Excluded"],
  ["kill-switch", "Kill switch"],
  ["attention", "Your attention"],
  ["other", "Other"],
];

function jobsPanel(jobs: Jobs, report: PipelineReport): PanelContent {
  const key: ChartKey = { key: "created", label: "Jobs created" };
  const data = series(jobs.daily, key, (day) => day.created);
  const { stuck } = jobs;
  const waiting = WAITING_LABELS.filter(([reason]) => stuck.waiting[reason] > 0).map(
    ([reason, name]) => [name, String(stuck.waiting[reason])] as [string, string],
  );
  return {
    title: "Jobs created per day",
    aside: joined([
      `${jobs.completed} of ${jobs.total} completed`,
      jobs.duration_s.n > 0 ? `duration ${distLine(jobs.duration_s)}` : "",
      versus(jobs.completion_rate, jobs.prev_completion_rate, report.days),
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={[key]}
          label={`Jobs created per day, last ${report.days} days`}
          format={count}
          empty={EMPTY}
          tone="ui-chart-delivered"
          height={DETAIL_HEIGHT}
        />
        {summary(data, count)}
        <ShareBar
          label="Jobs by outcome"
          format={count}
          items={[
            { key: "completed", label: "Completed", value: jobs.completed, className: "ui-chart-passed" },
            { key: "failed", label: "Failed", value: jobs.failed, className: "ui-chart-failed" },
            { key: "interrupted", label: "Interrupted", value: jobs.interrupted, className: "ui-chart-errored" },
            { key: "live", label: "Still live", value: jobs.live, className: "ui-chart-ungated" },
          ]}
        />
        {stuckCount(jobs) === 0 && waiting.length === 0 ? null : (
          <>
            <Pairs
              label="Stuck jobs, by signal"
              rows={[
                ["Past their lifetime", String(stuck.age_over_lifetime)],
                ["Dry rounds at the limit", String(stuck.dry_rounds_at_limit)],
                ["Rounds exhausted", String(stuck.rounds_exhausted)],
                ["Retrying the gate", String(stuck.gate_retry_loop)],
                ["Item stuck", String(stuck.item_stuck)],
              ]}
            />
            <Pairs label="Waiting on" rows={waiting} />
          </>
        )}
        {stuck.jobs.length === 0 ? null : (
          <Rows label="Stuck jobs">
            {stuck.jobs.map((one) => (
              <li key={one.id} className="an-row">
                <ProjectLink projectId={one.project_id}>{`Job ${one.id}`}</ProjectLink>
                <span className="an-row-figures">{one.project_id}</span>
              </li>
            ))}
          </Rows>
        )}
      </>
    ),
  };
}

/* --------------------------------------------------------------- autopilot -- */

const ms = (value: number) => humanSeconds(value / 1000);

function autopilotPanel(autopilot: Autopilot): PanelContent {
  const { judge, classifier, refused_actions: refused } = autopilot;
  const empty = judge.n === 0 && classifier.length === 0 && refused.total === 0;
  return {
    title: "Autopilot agreement",
    aside: joined([`${judge.n} judged`, `${refused.total} refused`]),
    body: empty ? (
      <p className="an-note">{EMPTY}</p>
    ) : (
      <>
        <Pairs
          label="Classifier"
          rows={classifier.map((one) => [
            one.action_class,
            clauses([`${one.n}`, `agree ${percentOr(one.agree)}`, one.pending > 0 ? `${one.pending} pending` : ""]),
          ])}
        />
        <Pairs
          label="Judge"
          rows={[
            ["Latency", judge.latency_ms.p50 === null ? NO_DATA : distLine(judge.latency_ms, ms)],
            ["Input tokens", compactTokens(judge.input_tokens)],
            ["Cost", usd(judge.cost_usd)],
            ["Errors", percentOr(judge.error_rate)],
            ["Enforced", percentOr(judge.enforced_share)],
            ["Agree", `${percentOr(judge.agree)} · ${judge.reviewed} with a human verdict`],
          ]}
        />
        <ShareBar
          label="Refused actions by status"
          format={count}
          items={Object.entries(refused.by_status).map(([status, value]) => ({ key: status, label: status, value }))}
        />
      </>
    ),
  };
}

/* ---------------------------------------------------------------- pressure -- */

const tenth = (value: number) => value.toFixed(1);

function pressurePanel(pressure: Pressure, report: PipelineReport): PanelContent {
  const data = series(pressure.daily, PRESSURE_KEY, (day) => day.avg_in_use ?? 0);
  return {
    title: "Slots in use per day",
    aside: joined([
      pressure.limit === null ? "" : `limit ${pressure.limit}`,
      versus(pressure.at_limit_share, pressure.prev_at_limit_share, report.days),
    ]),
    body: (
      <>
        {recordingNote(report.history.slots_since, report.window.from)}
        <AreaChart
          data={data}
          keys={[PRESSURE_KEY]}
          label={`Average slots in use per day, last ${report.days} days`}
          format={tenth}
          empty="No slot samples in this window"
          tone="ui-chart-runs"
          height={DETAIL_HEIGHT}
        />
        <Pairs
          label="Slot pressure"
          rows={[
            ["At the limit", percentOr(pressure.at_limit_share)],
            ["Peak in use", pressure.peak_in_use === null ? NO_DATA : String(pressure.peak_in_use)],
            [
              pressure.slot_wait_share_is_lower_bound ? "Waiting for a slot, at least" : "Waiting for a slot",
              percentOr(pressure.slot_wait_share),
            ],
          ]}
        />
      </>
    ),
  };
}

/* ------------------------------------------------------------------ choose -- */

const PANELS: Record<PipelineKey, (report: PipelineReport) => PanelContent> = {
  postgate: (r) => (isBlockError(r.postgate) ? failed("Post-merge gate") : postgatePanel(r.postgate, r)),
  landing: (r) => (isBlockError(r.landing) ? failed("Landing queue") : landingPanel(r.landing, r)),
  gate: (r) => (isBlockError(r.gate) ? failed("Gate") : gatePanel(r.gate, r)),
  jobs: (r) => (isBlockError(r.jobs) ? failed("Jobs") : jobsPanel(r.jobs, r)),
  autopilot: (r) => (isBlockError(r.autopilot) ? failed("Autopilot") : autopilotPanel(r.autopilot)),
  pressure: (r) =>
    r.pressure === null || isBlockError(r.pressure) ? failed("Slot pressure") : pressurePanel(r.pressure, r),
};

function failed(title: string): PanelContent {
  return { title, aside: "", body: <p className="an-block-error">Could not compute this block.</p> };
}

/** The selected card's panel content. */
export function pipelinePanel(key: PipelineKey, report: PipelineReport): PanelContent {
  return PANELS[key](report);
}
