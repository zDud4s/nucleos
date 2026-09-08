import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { FeedEmbed } from "../app/FeedEmbed";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  READINESS_MIN_AGREE_PERCENT,
  READINESS_MIN_REVIEWED,
  SHADOW_EVIDENCE_MODE,
  readShadowDecision,
  scopeEngaged,
  useScopedKills,
  useScoreboard,
  useSetProjectMode,
  useSetScopedKill,
  useSetShadowVerdict,
  useShadowDecisions,
  type ClassTally,
} from "../data/autopilot";
import { useCreateJob, useLiveJobs, type Job } from "../data/fleet";
import {
  useBudget,
  useKillSwitch,
  useProjects,
  useProposals,
  type AutopilotMode,
  type BudgetView,
  type ProjectSummary,
} from "../data/system";
import {
  Badge,
  Button,
  Count,
  ConfirmButton,
  ErrorNote,
  ModeSwitch,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  Section,
  RelativeTime,
  StatCard,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import {
  MODE_LABEL,
  MODE_SENTENCES,
  MODE_TONE,
  promotionBlocker,
  promotionConsequence,
} from "../lib/mode";
import "./autopilot.css";

/** The three periods the núcleo writes, as the noun each one is: `daily` becomes `day`, not `dai`. */
const PERIOD_NOUN: Record<string, string> = {
  daily: "day",
  weekly: "week",
  monthly: "month",
};

/**
 * Autopilot — the governance cockpit.
 *
 * The page answers one question, in the order a person asks it: *is anything
 * allowed to act right now, and on whose authority?* So the brakes come first
 * (global kill, budget hold, per-scope kills), then the per-project setting that
 * decides whether a project may act at all, then the evidence that setting is
 * earned (the shadow review and the scoreboard), and only then the work itself.
 *
 * **Deciding proposals does not live here.** The cockpit governs; the queue
 * decides. A second approve button on this page would be a second place to
 * answer the same question, and the two would disagree the first time somebody
 * used the wrong one. What is here is a *count* and a way to `/waiting`.
 *
 * **The kill switch is a banner, not a control.** The switch itself is in the
 * frame, on every page. Repeating the control would be a second thing to keep
 * in step; repeating the *fact* is the point of a governance page, because
 * everything below it is suspended while it is engaged.
 */
export function Autopilot() {
  const projects = useProjects();
  const budget = useBudget();
  const kill = useKillSwitch();
  const proposals = useProposals();

  const rows = projects.data ?? [];
  const [chosen, setChosen] = useState<string | null>(null);
  /**
   * The project the shadow panels are about.
   *
   * Falls back to the first of the roster rather than to nothing: a cockpit that
   * shows an empty review panel until somebody clicks a row teaches that there
   * is nothing to review. Held as *state plus a fallback* rather than written
   * during render, so the component stays a pure function of what it was given.
   */
  const selected = rows.some((row) => row.project_id === chosen)
    ? chosen
    : (rows[0]?.project_id ?? null);

  const stale = projects.isError && projects.data !== undefined;

  return (
    <>
      <PageHeader
        title="Autopilot"
        headline={headlineFor(rows, projects.data !== undefined)}
      />

      {kill.data?.engaged === true && <KillBanner />}
      {budget.data?.paused === true && (
        <BudgetPausedBanner budget={budget.data} />
      )}

      <Statusline
        projects={projects.data}
        budget={budget.data}
        pending={proposals.data?.length}
      />

      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {projects.isError && projects.data === undefined && (
        <RosterError error={projects.error} />
      )}

      <div className="ap-sections">
        <ProjectGovernanceList
          rows={rows}
          answered={projects.data !== undefined}
          selected={selected}
          onSelect={setChosen}
        />
        <ShadowReviewPanel
          projectId={selected}
          project={rows.find((row) => row.project_id === selected)}
        />
        <ScoreboardPanel
          projectId={selected}
          project={rows.find((row) => row.project_id === selected)}
        />
        <TriggerKills />
        <JobsPanel rows={rows} selected={selected} />
        <FeedEmbed />
      </div>
    </>
  );
}

/* ------------------------------------------------------------- the reading -- */

/** One derived sentence about who is allowed to act. */
function headlineFor(
  rows: ProjectSummary[],
  answered: boolean,
): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no project is under autopilot";
  const active = rows.filter((row) => row.mode === "active").length;
  const shadow = rows.filter((row) => row.mode === "shadow").length;
  const parts: string[] = [];
  parts.push(
    active === 0
      ? "nothing is acting on its own"
      : `${active} acting on its own`,
  );
  if (shadow > 0) parts.push(`${shadow} watching in shadow`);
  const promotable = rows.filter((row) => row.promotable).length;
  if (promotable > 0) parts.push(`${promotable} ready to be let out of shadow`);
  return parts.join("; ");
}

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about the roster
    </ErrorNote>
  );
}

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so a
 * bare 422 arrives carrying the words "Unprocessable Entity" — the code spelled
 * with capital letters, explaining nothing. Four words is the line between a
 * status word and a sentence somebody wrote on purpose.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ---------------------------------------------------------------- banners -- */

function KillBanner() {
  return (
    <p className="ap-banner ap-banner-kill" role="status">
      <span className="ap-banner-title">Emergency stop is engaged</span>
      <span className="ap-banner-text">
        Nothing autonomous starts while this is on — no scheduled rule, no repo
        trigger, no job. The switch is under the rail, on every page. Everything
        below is what *would* happen once it is released.
      </span>
    </p>
  );
}

function BudgetPausedBanner({ budget }: { budget: BudgetView }) {
  return (
    <p className="ap-banner ap-banner-budget" role="status">
      <span className="ap-banner-title">
        Autonomous work is held by the budget
      </span>
      <span className="ap-banner-text">
        {budget.reason ?? "the núcleo did not say which ceiling"} — $
        {budget.window_spend_usd.toFixed(2)} spent this{" "}
        {PERIOD_NOUN[budget.period] ?? budget.period}
        {budget.limit_usd === null
          ? ""
          : ` against a ceiling of $${budget.limit_usd.toFixed(2)}`}
        . This clears itself when the window rolls or the ceiling is raised; it
        is not a fault.
      </span>
    </p>
  );
}

/* -------------------------------------------------------------- statusline -- */

function Statusline({
  projects,
  budget,
  pending,
}: {
  projects: ProjectSummary[] | undefined;
  budget: BudgetView | undefined;
  pending: number | undefined;
}) {
  const active = projects?.filter((row) => row.mode === "active").length;
  const shadow = projects?.filter((row) => row.mode === "shadow").length;
  const held = projects?.filter((row) => row.queue_full).length ?? 0;

  return (
    <div className="ap-stats">
      <StatCard
        label="Acting on their own"
        value={active}
        detail={
          projects === undefined
            ? undefined
            : `${projects.length} projects on the roster`
        }
      />
      <StatCard
        label="Watching in shadow"
        value={shadow}
        detail="deciding without enforcing, to earn the promotion"
      />
      <StatCard
        label="Proposals open"
        value={pending}
        detail={
          held === 0
            ? "across the roster"
            : `across the roster — ${held} project queue full`
        }
      />
      <StatCard
        label="Window spend"
        value={
          budget === undefined
            ? undefined
            : `$${budget.window_spend_usd.toFixed(2)}`
        }
        detail={
          budget === undefined
            ? undefined
            : budget.limit_usd === null
              ? "no ceiling set"
              : `of $${budget.limit_usd.toFixed(2)} per ${PERIOD_NOUN[budget.period] ?? budget.period}`
        }
      />
    </div>
  );
}

/* ------------------------------------------------------ project governance -- */

/** What each setting means, said once rather than on every row. */
function ProjectGovernanceList({
  rows,
  answered,
  selected,
  onSelect,
}: {
  rows: ProjectSummary[];
  answered: boolean;
  selected: string | null;
  onSelect: (projectId: string) => void;
}) {
  return (
    <Panel
      title="Projects"
      aside={<Count n={answered ? rows.length : undefined} />}
    >
      {answered && rows.length === 0 && (
        <Teach title="No project is under autopilot">
          <p>
            A project appears here once the núcleo has been told where it lives.
            Nothing is broken and nothing is hidden — there is simply no project
            to govern yet.
          </p>
        </Teach>
      )}
      {!answered && <p className="ap-loading">reading the roster…</p>}
      {rows.length > 0 && (
        <>
          <div className="ap-project-head" aria-hidden="true">
            <span>Project</span>
            <span>Mode</span>
            <span className="ap-project-number">To review</span>
            <span className="ap-project-number">Clearing the bar</span>
            <span className="ap-project-number">Open</span>
            <span />
          </div>
          <ul className="ui-rows" aria-label="Projects under autopilot">
            {rows.map((project) => (
              <GovernanceRow
                key={project.project_id}
                project={project}
                selected={project.project_id === selected}
                onSelect={onSelect}
              />
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}

/**
 * One project's setting, and the gate between shadow and acting.
 *
 * Its own mutation rather than the page's, so a refusal is shown against the
 * row that caused it. One shared mutation would paint every row red when one of
 * them was refused.
 */
function GovernanceRow({
  project,
  selected,
  onSelect,
}: {
  project: ProjectSummary;
  selected: boolean;
  onSelect: (projectId: string) => void;
}) {
  const setMode = useSetProjectMode();
  const [root, setRoot] = useState(project.project_root ?? "");
  /** Which change was refused, so the retry with a root sends the same one. */
  const [attempted, setAttempted] = useState<AutopilotMode | null>(null);

  /**
   * A 422 is the *only* refusal that opens the root input.
   *
   * `activation_status` maps four different causes onto one bare 422 with an
   * empty body — no root given, no `.ai/workflow/workflow.md`, no PreToolUse
   * hook pointing at `ask_daemon.py`, or a root that is not a git repository —
   * and the daemon sends nothing that distinguishes them. So the page names the
   * set, says it does not know which, and offers the one of the four it can do
   * something about. Guessing a single cause here would be wrong three times
   * out of four.
   */
  const refused =
    setMode.isError && isApiRefusal(setMode.error) ? setMode.error : null;
  const needsRoot = refused !== null && refused.status === 422;

  const withheld = project.withheld_classes_ready ?? 0;

  function change(mode: AutopilotMode, withRoot: string | null) {
    setAttempted(mode);
    const trimmed = withRoot === null ? "" : withRoot.trim();
    setMode.mutate({
      project_id: project.project_id,
      mode,
      ...(trimmed === "" ? {} : { project_root: trimmed }),
    });
  }

  return (
    <li
      className={
        selected
          ? "ui-rows-row ap-project-row ap-project-row-selected"
          : "ui-rows-row ap-project-row"
      }
    >
      <div className="ap-project-name">
        <Button variant="link" onClick={() => onSelect(project.project_id)}>
          {project.project_id}
        </Button>
        {project.queue_full && <Badge tone="paused">queue full</Badge>}
      </div>
      <Badge tone={MODE_TONE[project.mode]}>{MODE_LABEL[project.mode]}</Badge>

      <span className="ap-project-number" title="shadow decisions to review">
        {project.pending}
      </span>
      <span className="ap-project-number" title="classes clearing the bar">
        {project.classes_ready}/{project.classes_total}
      </span>
      <span className="ap-project-number" title="open proposals">
        {project.open_proposals}
        {project.wip_limit === null
          ? " (no ceiling)"
          : ` of ${project.wip_limit}`}
      </span>

      {/* The wrapper stays: it is this row's sixth grid cell, and it is what puts the
          control at the end of it. `.ap-modes` is layout, `ModeSwitch` is the control. */}
      <div className="ap-modes">
        <ModeSwitch
          value={project.mode}
          actAllowed={project.promotable}
          actConfirmLabel={promotionConsequence(project)}
          busy={setMode.isPending}
          onChoose={(mode) =>
            change(
              mode,
              mode === "off"
                ? null
                : root === ""
                  ? project.project_root
                  : root,
            )
          }
        />
      </div>

      {project.mode !== "active" && !project.promotable ? (
        <div className="ap-project-blocker">
          <Quiet says={promotionBlocker(project, withheld)} />
        </div>
      ) : null}

      {refused !== null && (
        <RefusalNote
          refusal={refused}
          sentences={{
            ...MODE_SENTENCES,
            ...daemonProse(refused),
          }}
        />
      )}
      {setMode.isError && !isApiRefusal(setMode.error) && (
        <ErrorNote>
          the núcleo did not answer — this project&apos;s setting is unchanged
        </ErrorNote>
      )}

      {needsRoot && (
        <div className="ui-panel-inset ap-project-root">
          <label
            className="ap-project-root-label"
            htmlFor={`root-${project.project_id}`}
          >
            Folder for {project.project_id}
          </label>
          <input
            id={`root-${project.project_id}`}
            className="ap-project-root-input"
            type="text"
            value={root}
            spellCheck={false}
            placeholder="C:\\path\\to\\the\\project"
            onChange={(event) => setRoot(event.target.value)}
          />
          <Button
            variant="ghost"
            disabled={
              root.trim() === "" || attempted === null || setMode.isPending
            }
            onClick={() => {
              if (attempted !== null) change(attempted, root);
            }}
          >
            Try again with this folder
          </Button>
        </div>
      )}
    </li>
  );
}

/* --------------------------------------------------------- shadow review -- */

function ShadowReviewPanel({
  projectId,
  project,
}: {
  projectId: string | null;
  project: ProjectSummary | undefined;
}) {
  const decisions = useShadowDecisions(projectId);
  const verdict = useSetShadowVerdict();
  const rows = decisions.data ?? [];

  if (projectId !== null && decisions.data !== undefined && rows.length === 0) {
    return (
      <Section label="Shadow decisions">
        {/* The selected row is the subject; name it because its light-theme mark has no fill weight, while "this project" is only for the first render. */}
        <Quiet says={`nothing is waiting for a verdict on ${project?.project_id ?? "this project"}.`}>
          <p className="ap-note">What the classifier decided while enforcing nothing. Answering these is the only thing that moves a project toward acting on its own — agreeing says the classifier read the action the way you would, disagreeing says it did not, and both are evidence.</p>
        </Quiet>
      </Section>
    );
  }

  return (
    <Panel
      title="Shadow decisions"
      aside={
        <>
          {project !== undefined && <span className="ap-project-name">{project.project_id}</span>}
          <Count n={projectId === null ? undefined : rows.length} />
          <Link className="ap-link" to="/waiting">Go to the queue</Link>
        </>
      }
    >
      {projectId === null && (
        <p className="ap-empty">choose a project above.</p>
      )}
      {projectId !== null &&
        decisions.isError &&
        decisions.data === undefined && (
          <ListError error={decisions.error} what="the shadow decisions" />
        )}
      {projectId !== null &&
        !decisions.isError &&
        decisions.data === undefined && (
          <p className="ap-loading">reading the shadow decisions…</p>
        )}
      {rows.length > 0 && (
        <ul className="ap-list" aria-label="Shadow decisions">
          {rows.map((decision) => (
            <li className="ap-card" key={decision.id}>
              <div className="ap-card-head">
                <span className="ap-card-id">decision #{decision.id}</span>
                <span className="ap-card-title">{decision.tool_name}</span>
                <Badge tone="info">{decision.action_class}</Badge>
                <RelativeTime at={decision.created_at} />
              </div>
              <dl className="ap-decision-facts">
                <div className="ap-decision-fact">
                  <dt>the classifier</dt>
                  <dd>{readShadowDecision(decision.decision)}</dd>
                </div>
                <div className="ap-decision-fact">
                  <dt>run</dt>
                  <dd>
                    <Link className="ap-link" to={`/runs/${decision.run_id}`}>
                      run {decision.run_id}
                    </Link>
                  </dd>
                </div>
                <div className="ap-decision-fact">
                  <dt>classifier version</dt>
                  <dd>{decision.classifier_version}</dd>
                </div>
              </dl>
              <p className="ap-reason">
                {decision.reason === null || decision.reason.trim() === ""
                  ? "the classifier recorded no reason"
                  : decision.reason}
              </p>
              <RawInput raw={decision.tool_input} />
              <div className="ap-actions">
                <ConfirmButton
                  label={`Allow #${decision.id}`}
                  confirmLabel="This should have been allowed"
                  variant="approve"
                  disabled={verdict.isPending}
                  onConfirm={() =>
                    verdict.mutate({
                      decisionId: decision.id,
                      verdict: "approve",
                    })
                  }
                />
                <ConfirmButton
                  label={`Block #${decision.id}`}
                  confirmLabel="This should have been stopped"
                  variant="ghost"
                  disabled={verdict.isPending}
                  onConfirm={() =>
                    verdict.mutate({
                      decisionId: decision.id,
                      verdict: "reject",
                    })
                  }
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      {verdict.isError && <VerdictError error={verdict.error} />}
    </Panel>
  );
}

function VerdictError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the núcleo did not answer — nothing was recorded</ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_found: "that decision is gone, or somebody has already answered it",
        bad_request: "only *allow* and *block* are verdicts",
      }}
    />
  );
}

/**
 * A shadow decision's arguments, verbatim.
 *
 * Verbatim and **not** flattened into fields the way the approval queue does it,
 * and the difference is the point: an approval is a decision about whether an
 * action may happen, so readability wins. This is a decision about whether the
 * *classifier read the action correctly*, and the thing being judged is exactly
 * the text the classifier saw. Prettifying it would judge a different input.
 */
function RawInput({ raw }: { raw: string | null }) {
  if (raw === null || raw.trim() === "") return null;
  return (
    <pre className="ap-raw">
      <code>{raw}</code>
    </pre>
  );
}

/* ------------------------------------------------------------ scoreboard -- */

function ScoreboardPanel({
  projectId,
  project,
}: {
  projectId: string | null;
  project: ProjectSummary | undefined;
}) {
  const scoreboard = useScoreboard(projectId);
  const rows = scoreboard.data ?? [];
  const evidence = rows.filter((row) => row.mode === SHADOW_EVIDENCE_MODE);
  const history = rows.filter((row) => row.mode !== SHADOW_EVIDENCE_MODE);

  if (projectId !== null && scoreboard.data !== undefined && rows.length === 0) {
    return (
      <Section label="Scoreboard">
        <Quiet says={`${project?.project_id ?? "this project"} has recorded no classified decision yet.`}>
          <p className="ap-note">Read-only. The bar is {READINESS_MIN_REVIEWED} reviews at {READINESS_MIN_AGREE_PERCENT}% agreement per action class; the ready/total figure is the núcleo&apos;s own and gates the promote control.</p>
        </Quiet>
      </Section>
    );
  }

  return (
    <Panel
      title="Scoreboard"
      aside={
        project === undefined ? null : (
          <>
            <span className="ap-project-name">{project.project_id}</span>
            <span className="ui-count">
              {`${project.classes_ready}/${project.classes_total} classes ready`}
            </span>
          </>
        )
      }
    >
      {projectId === null && (
        <p className="ap-empty">choose a project above.</p>
      )}
      {projectId !== null &&
        scoreboard.isError &&
        scoreboard.data === undefined && (
          <ListError error={scoreboard.error} what="the scoreboard" />
        )}
      {projectId !== null &&
        !scoreboard.isError &&
        scoreboard.data === undefined && (
          <p className="ap-loading">reading the scoreboard…</p>
        )}
      {evidence.length > 0 && (
        <TallyTable label="Shadow evidence" rows={evidence} />
      )}
      {history.length > 0 && (
        <>
          <p className="ap-subhead">enforced, and not evidence for promotion</p>
          <TallyTable label="Enforced decisions" rows={history} />
        </>
      )}
      {rows.length > 0 && (
        <p className="ap-note ap-note-foot">
          the count that decides it is not the count below: the núcleo counts reviews distinct by tool and arguments, so ten answers to the same command are ten here and one at the bar.
        </p>
      )}
    </Panel>
  );
}

function TallyTable({ label, rows }: { label: string; rows: ClassTally[] }) {
  return (
    <table className="ap-score">
      <caption className="ap-score-caption">{label}</caption>
      <thead>
        <tr>
          <th scope="col">action class</th>
          <th scope="col">mode</th>
          <th scope="col">seen</th>
          <th scope="col">allow</th>
          <th scope="col">ask</th>
          <th scope="col">deny</th>
          <th scope="col">reviewed</th>
          <th scope="col">agreed</th>
          <th scope="col">disagreed</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={`${row.mode}:${row.action_class}`}>
            <th scope="row" className="ap-score-class">
              {row.action_class}
            </th>
            <td>{row.mode}</td>
            <td className="ap-score-num">{row.total}</td>
            <td className="ap-score-num">{row.would_allow}</td>
            <td className="ap-score-num">{row.would_pend}</td>
            <td className="ap-score-num">{row.would_deny}</td>
            <td className="ap-score-num">{row.reviewed}</td>
            <td className="ap-score-num">{row.agree}</td>
            <td className="ap-score-num">{row.disagree}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/* ---------------------------------------------------------- trigger kills -- */

/**
 * The trigger scopes, and whether the núcleo reads them.
 *
 * `reads` is not decoration and is not a guess: each `true` below is a
 * `scoped_kill_engaged(pool, "trigger", …)` call site in `core/src/`, named in
 * the comment beside it. All four scopes are read now, `team` last —
 * `core/src/team_trigger.rs:461` calls `scoped_kill_engaged(&state.pool,
 * "trigger", "team")` before any rule is considered, and its test
 * `the_scoped_kill_stops_the_rules_and_not_the_work` is what pins that.
 *
 * The `reads` field and both branches of the ternary below stay even though
 * every scope now reads `true` — the mechanism is what keeps this panel from
 * ever promising a brake that does not brake, a fifth scope is a matter of
 * time, and deleting the check here would mean the next one arrives with
 * nothing to catch it before it does.
 */
const TRIGGER_SCOPES: {
  id: string;
  label: string;
  what: string;
  reads: boolean;
}[] = [
  {
    id: "scheduled",
    label: "Scheduled rules",
    what: "the cron rules in every project's autopilot.yaml",
    // `scheduler.rs`, the tick's first check.
    reads: true,
  },
  {
    id: "repo",
    label: "Repo triggers",
    what: "the rules that fire on a new commit",
    // `repo_trigger.rs`, before any branch is compared.
    reads: true,
  },
  {
    id: "email",
    label: "E-mail triage",
    what: "the sweep that classifies arriving mail",
    // `triage.rs`, before a sweep starts.
    reads: true,
  },
  {
    id: "team",
    label: "Team triggers",
    what: "the rules that start a team on a clock, on another team ending, or on triaged mail",
    // `team_trigger.rs`, before any rule is considered.
    reads: true,
  },
];

function TriggerKills() {
  const kills = useScopedKills();
  const setKill = useSetScopedKill();

  return (
    <Panel title="Trigger brakes">
      {kills.isError && kills.data === undefined && (
        <ListError error={kills.error} what="the trigger brakes" />
      )}
      {!kills.isError && kills.data === undefined && (
        <p className="ap-loading">reading the trigger brakes…</p>
      )}
      <ul className="ui-rows" aria-label="Trigger brakes">
        {TRIGGER_SCOPES.map((scope) => {
          const engaged = scopeEngaged(kills.data, "trigger", scope.id);
          return (
            <li className="ui-rows-row ap-trigger-row" key={scope.id}>
              <span className="ap-trigger-name">{scope.label}</span>
              {scope.reads ? (
                <Badge tone={engaged ? "paused" : "active"}>
                  {engaged ? "held" : "running"}
                </Badge>
              ) : (
                <Badge tone="off">not read</Badge>
              )}
              {scope.reads ? (
                <Button
                  variant="ghost"
                  intent={engaged ? "go" : "stop"}
                  disabled={kills.data === undefined || setKill.isPending}
                  onClick={() =>
                    setKill.mutate({
                      scope_type: "trigger",
                      scope_id: scope.id,
                      engaged: !engaged,
                    })
                  }
                >
                  {engaged
                    ? `Release ${scope.label.toLowerCase()}`
                    : `Hold ${scope.label.toLowerCase()}`}
                </Button>
              ) : (
                <p className="ap-trigger-note">
                  The núcleo will store this brake and nothing in it reads the
                  value, so engaging it would stop nothing. It becomes a real
                  switch when the Teams slice lands and team triggers exist to
                  be held.
                </p>
              )}
            </li>
          );
        })}
      </ul>
      {setKill.isError && (
        <ErrorNote>
          that brake was not changed — the núcleo refused or did not answer
        </ErrorNote>
      )}
    </Panel>
  );
}

/* ---------------------------------------------------------------- the work -- */

function JobsPanel({
  rows,
  selected,
}: {
  rows: ProjectSummary[];
  selected: string | null;
}) {
  const jobs = useLiveJobs();
  const create = useCreateJob();
  const [prompt, setPrompt] = useState("");
  const live = jobs.data ?? [];

  const target = rows.find((row) => row.project_id === selected);
  const blocked =
    target === undefined || target.queue_full || target.mode === "off";

  return (
    <Panel title="Jobs in flight" aside={<Count n={jobs.data?.length} />}>
      {jobs.isError && jobs.data === undefined && (
        <ListError error={jobs.error} what="the jobs" />
      )}
      {jobs.data !== undefined && live.length === 0 && (
        <p className="ap-empty">nothing is running.</p>
      )}
      {live.length > 0 && (
        <ul className="ap-list" aria-label="Jobs in flight">
          {live.map((job) => (
            <JobRow key={job.id} job={job} />
          ))}
        </ul>
      )}

      <p className="ap-subhead">ask for one</p>
      <div className="ap-form">
        <label className="ap-field-label" htmlFor="ap-job-prompt">
          What should {selected ?? "a project"} do?
        </label>
        <textarea
          id="ap-job-prompt"
          className="ap-field-input"
          rows={2}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
        />
        <Button
          variant="approve"
          intent="go"
          disabled={blocked || prompt.trim() === "" || create.isPending}
          onClick={() => {
            if (selected === null) return;
            create.mutate(
              {
                project_id: selected,
                prompt: prompt.trim(),
                budget_usd: null,
                max_rounds: null,
                // No picker here, exactly as there is none for budget or rounds:
                // this panel is the one-line "start something" and the Fleet page
                // is where a job is specified. Sending null keeps it the queue in
                // one checkout, which is what this button has always started.
                team_id: null,
              },
              { onSuccess: () => setPrompt("") },
            );
          }}
        >
          Start a job
        </Button>
      </div>
      {/* Said before the click rather than after the 409. A project that is off
          starts nothing, and one whose queue is full defers rather than refuses
          — two different reasons the button would not do what it says. */}
      {target !== undefined && target.mode === "off" && (
        <p className="ap-hedge">
          {target.project_id} is off, so the núcleo would not start this.
        </p>
      )}
      {target !== undefined && target.queue_full && (
        <p className="ap-hedge">
          {target.project_id} is holding {target.open_proposals} open proposals
          against its ceiling of {target.wip_limit ?? "none"} — review something
          and the brake releases itself.
        </p>
      )}
      {create.isError && <JobError error={create.error} />}
    </Panel>
  );
}

function JobRow({ job }: { job: Job }) {
  return (
    <li className="ap-job">
      <div className="ap-card-head">
        <span className="ap-card-id">job {job.id}</span>
        <StateBadge domain="job" state={job.status} />
        {job.wait_reason !== null && (
          <StateBadge domain="wait_reason" state={job.wait_reason} />
        )}
        <span className="ap-meta">{job.project_id}</span>
        <RelativeTime at={job.created_at} />
      </div>
      <p className="ap-meta">
        round {job.round + 1} of {job.max_rounds}
        {job.rule_name === null ? "" : ` — ${job.rule_name}`}
        {/* A directed job looks nothing like a sequential one from the inside
            and looked exactly like it from here. Said with the ceiling, because
            the team's name alone does not say what having one buys. */}
        {job.team_id !== null &&
          ` — ${job.team_name ?? job.team_id}, up to ${job.team_max_parallel ?? 1} at once`}
      </p>
    </li>
  );
}

function JobError({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>the núcleo did not answer — no job was started</ErrorNote>
    );
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        kill_switch:
          "the emergency stop is engaged — nothing autonomous starts until it is released",
        conflict: "that project has no room right now",
        locked: "that project is held — nothing new starts here",
      }}
    />
  );
}

/* -------------------------------------------------------------- the feed -- */

/* `FeedEmbed` moved to `app/FeedEmbed.tsx`. Home shows the last five lines and this
   cockpit the last ten, and a block two pages render cannot live inside one of them —
   importing a page from another page is how a route ends up mounting a route. The `ap-`
   rules it is drawn with stayed here, and it imports this stylesheet by name. */

/* ------------------------------------------------------------ the queue -- */

/* -------------------------------------------------------------- shared -- */

function ListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about {what}
    </ErrorNote>
  );
}
