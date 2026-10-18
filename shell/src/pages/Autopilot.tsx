import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type KeyboardEvent as ReactKeyboardEvent,
  type SetStateAction,
} from "react";
import { Link } from "@tanstack/react-router";
import { FeedEmbed } from "../app/FeedEmbed";
import { DAEMON_NAME } from "../copy";
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
  useJudgeOpinionsForDecisions,
  useShadowDecisions,
  type ClassTally,
} from "../data/autopilot";
import { useCancelSlotOwner, useCreateJob, useLiveJobs, type Job } from "../data/fleet";
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
  ConfirmButton,
  Disclosure,
  Count,
  ErrorNote,
  Inset,
  Meter,
  ModeSwitch,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  SectionNav,
  StaleNote,
  StateBadge,
  Teach,
  readState,
  usd,
} from "../ui";
import { whereWaiting } from "../data/roster";
import {
  describeProject,
  evidence,
  gateFor,
  headlineFor,
  holdOrder,
  urgencyOf,
} from "../lib/autopilot-fan";
import {
  MODE_MEANING,
  MODE_REFUSAL_PROSE,
  MODE_SENTENCES,
  promotionConfirmLabel,
  promotionConsequence,
} from "../lib/mode";
import { OnboardPanel } from "../project/Onboard";
import { JudgeOpinionLine, JudgePanel, JudgeReviewPanel } from "./AutopilotJudge";
import { RefusedActionsPanel, SkippedItemsPanel } from "./AutopilotRecords";
import { ResolvePanel } from "./AutopilotResolve";
import "./autopilot.css";

/** The page's map: where each group of panels starts, in the order the page reads. */
const SECTIONS = [
  { id: "ap-setting", label: "Setting" },
  { id: "ap-brakes", label: "Brakes" },
  { id: "ap-evidence", label: "Evidence" },
  { id: "ap-jobs", label: "Jobs" },
  { id: "ap-records", label: "Records" },
];

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
 *
 * **The last two panels before the feed are records, not controls**: what a job put down
 * overnight and what the injection barrier refused. Nothing in them is waiting on a
 * decision, so they live here with the rest of what governance reads back, and not in the
 * Waiting queue.
 */
export function Autopilot() {
  const projects = useProjects();
  const budget = useBudget();
  const kill = useKillSwitch();
  const proposals = useProposals();

  const rows = useMemo(() => projects.data ?? [], [projects.data]);
  const answered = projects.data !== undefined;
  const [chosen, setChosen] = useState<string | null>(null);
  /** Each project's last refused change, kept until a change to that project succeeds. */
  const [refusals, setRefusals] = useState<ReadonlyMap<string, Refused>>(() => new Map());
  const refused = useMemo(() => new Set(refusals.keys()), [refusals]);
  /**
   * The order the carousel draws, most urgent first: ranked once, then held.
   *
   * Re-ranking on every poll would move the card somebody just changed out from under their
   * hand, so the held order is written only when a project arrives or leaves — `holdOrder` keeps
   * every held id where it is — and never during render. Until the effect below has run, a render
   * draws what `holdOrder` answers over whatever is held, which is the list about to be held.
   */
  const [held, setHeld] = useState<readonly string[]>([]);
  const order = useMemo(() => holdOrder(held, rows, refused), [held, rows, refused]);
  useEffect(() => {
    setHeld((current) => (sameMembers(current, order) ? current : order));
  }, [order]);
  /**
   * The project the carousel is on, and so the one the shadow panels are about.
   *
   * Falls back to the first of the held order — the most urgent project — rather than to
   * nothing: a cockpit that shows an empty review panel until somebody picks a project teaches
   * that there is nothing to review. And not to the roster's first row either, which is simply
   * whatever the núcleo listed first. Held as *state plus a fallback* rather than written during
   * render, so the component stays a pure function of what it was given.
   */
  const selected = chosen !== null && order.includes(chosen) ? chosen : (order[0] ?? null);
  const focused = rows.find((row) => row.project_id === selected);

  const stale = projects.isError && projects.data !== undefined;

  return (
    <>
      {/* Always the span, even before the roster answers: the box is two lines tall from the
          first paint, so the page does not move down when the sentence arrives. */}
      <PageHeader
        title="Autopilot"
        headline={<span className="ap-headline">{headlineFor(rows, answered) ?? ""}</span>}
      />

      {kill.data?.engaged === true && <KillBanner />}
      {budget.data?.paused === true && (
        <BudgetPausedBanner budget={budget.data} />
      )}

      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {projects.isError && projects.data === undefined && (
        <RosterError error={projects.error} />
      )}

      <SectionNav label="Autopilot sections" items={SECTIONS} />

      <div className="ap-sections">
        <div id="ap-setting" className="ap-block">
          <ProjectCarousel
            rows={rows}
            answered={answered}
            order={order}
            selected={selected}
            onSelect={setChosen}
            refusals={refusals}
            onRefusals={setRefusals}
          />
        </div>
        {/* The brakes sit with the ledger, before any evidence: the order this page's doc-comment
            promises (brakes, then the setting, then the proof, then the work). */}
        <div id="ap-brakes" className="ap-block">
          <Ledger
            projects={projects.data}
            budget={budget.data}
            pending={proposals.data?.length}
          />
          <TriggerKills />
        </div>
        {/* The panels below act on the project in focus. With none (the roster is still being read,
            or it is empty and the carousel already says so) they would each repeat the same absence. */}
        <div id="ap-evidence" className="ap-block">
          {focused !== undefined && <ProjectBar project={focused} />}
          {selected !== null && <ShadowReviewPanel projectId={selected} project={focused} />}
          {selected !== null && <ScoreboardPanel projectId={selected} project={focused} />}
          {selected !== null && (
            <Disclosure label="Advanced: judge and resolve">
              <JudgePanel projectId={selected} project={focused} />
              <JudgeReviewPanel projectId={selected} />
              <ResolvePanel projectId={selected} project={focused} />
            </Disclosure>
          )}
        </div>
        <div id="ap-jobs" className="ap-block">
          <JobsPanel rows={rows} selected={selected} />
        </div>
        {/* Records of what the night and the barrier already settled. They were on the Waiting
            page, which now holds only what asks for a decision. Records, not controls: shut until
            somebody wants to read back. */}
        <div id="ap-records" className="ap-block">
          <Disclosure label="Records and feed">
            <SkippedItemsPanel />
            <RefusedActionsPanel />
            <FeedEmbed />
          </Disclosure>
        </div>
      </div>
    </>
  );
}

/* ------------------------------------------------------------- the reading -- */

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      Could not reach the {DAEMON_NAME} — nothing is known about the roster.
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
        below is what <em>would</em> happen once it is released.
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
        {budget.reason ?? `The ${DAEMON_NAME} did not say which ceiling`} — $
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

/* ---------------------------------------------------------------- the ledger -- */

/**
 * What the four stat cards used to say that nothing else on the page says: the open proposals and
 * the window's spend. The acting/shadow/off counts are the headline's, and the roster's size is the
 * carousel's own count, so those cards repeated them. Two facts do not earn a card each, nor the top
 * of the page above the roster — one quiet line under the carousel.
 */
function Ledger({
  projects,
  budget,
  pending,
}: {
  projects: ProjectSummary[] | undefined;
  budget: BudgetView | undefined;
  pending: number | undefined;
}) {
  const held = projects?.filter((row) => row.queue_full).length ?? 0;
  if (pending === undefined && budget === undefined) return null;
  return (
    <div className="ap-ledger">
      {pending !== undefined && (
        <p className="ap-ledger-line">
          <Link className="ap-link" to="/waiting">
            {pending} to review across the roster
          </Link>
          {held > 0 &&
            (held === 1 ? " — 1 project's queue full" : ` — ${held} projects' queues full`)}
        </p>
      )}
      {/* The window's spend against its ceiling, drawn: at 80% the meter turns amber and says so. */}
      {budget !== undefined && (
        <div className="ap-ledger-budget">
          <Meter
            label={`Spent this ${PERIOD_NOUN[budget.period] ?? budget.period}`}
            value={budget.window_spend_usd}
            ceiling={budget.limit_usd}
            tone="quantity"
            format={usd}
          />
        </div>
      )}
    </div>
  );
}

/* ------------------------------------------------------ project governance -- */

/** A project's last refused change: the setting that was asked for, and the núcleo's answer. */
interface Refused {
  mode: AutopilotMode;
  refusal: ApiRefusal;
}

/** Whether two id lists hold the same projects, in any order. */
function sameMembers(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  const inA = new Set(a);
  return b.every((id) => inA.has(id));
}

/** `n` modulo `m`, never negative: a ring walked left from its first card lands on its last. */
function wrap(n: number, m: number): number {
  return ((n % m) + m) % m;
}

function clamp(n: number, low: number, high: number): number {
  return Math.max(low, Math.min(high, n));
}

/** The map without one project's entry — the same map when it had none, so nothing re-renders. */
function without(
  map: ReadonlyMap<string, Refused>,
  id: string,
): ReadonlyMap<string, Refused> {
  if (!map.has(id)) return map;
  const next = new Map(map);
  next.delete(id);
  return next;
}

/** The readout under the index: the map's word, the exceptions, and the evidence. */
function readingOf(project: ProjectSummary, refused: boolean): string {
  const bits = [readState("autopilot", project.mode)?.label ?? project.mode];
  if (project.queue_full) bits.push("queue full");
  if (refused) bits.push("setting refused");
  bits.push(evidence(project));
  return bits.join(" · ");
}

/**
 * The roster as an index, one project in focus at a time, and the setting for that project under it.
 *
 * Two parts, in the order they are read. The index is every project at once, most urgent first —
 * the answer to "is everything fine?" that one card cannot give — and it is the keyboard's way in:
 * a tablist with one tab stop whose selection follows focus. Under it the list gives the same
 * triage in words, ten or twenty projects to a screen. The panel is the selected project's
 * setting, and nobody else's.
 *
 * One mutation for the whole roster, each refusal kept against the project that caused it: only one
 * project's switch is on screen at a time, and a refusal has to outlive walking to another project
 * and back.
 */
function ProjectCarousel({
  rows,
  answered,
  order,
  selected,
  onSelect,
  refusals,
  onRefusals,
}: {
  rows: ProjectSummary[];
  answered: boolean;
  order: readonly string[];
  selected: string | null;
  onSelect: (projectId: string) => void;
  refusals: ReadonlyMap<string, Refused>;
  onRefusals: Dispatch<SetStateAction<ReadonlyMap<string, Refused>>>;
}) {
  const setMode = useSetProjectMode();
  /** What the live region says: a change that went through. */
  const [said, setSaid] = useState("");
  const [pointedAt, setPointedAt] = useState<string | null>(null);

  const base = useId();
  const panelId = `${base}-panel`;
  const tabId = (at: number) => `${base}-tab-${at}`;

  const shown = useMemo(() => {
    const byId = new Map(rows.map((row) => [row.project_id, row]));
    return order.flatMap((id): ProjectSummary[] => {
      const row = byId.get(id);
      return row === undefined ? [] : [row];
    });
  }, [rows, order]);
  const total = shown.length;
  const found = shown.findIndex((row) => row.project_id === selected);
  const index = found < 0 ? 0 : found;
  const focused: ProjectSummary | undefined = shown[index];
  const many = total >= 2;

  const tabs = useRef<(HTMLButtonElement | null)[]>([]);

  /** The arrows walk the index — a ring at three or more, a line below — and Home/End go to its ends. */
  function onIndexKey(event: ReactKeyboardEvent<HTMLDivElement>) {
    const from = tabs.current.findIndex((tab) => tab === event.target);
    const at = from < 0 ? index : from;
    let next: number | null = null;
    if (event.key === "ArrowRight") next = total >= 3 ? wrap(at + 1, total) : clamp(at + 1, 0, total - 1);
    else if (event.key === "ArrowLeft") next = total >= 3 ? wrap(at - 1, total) : clamp(at - 1, 0, total - 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = total - 1;
    if (next === null) return;
    event.preventDefault();
    const row = shown[next];
    if (row === undefined) return;
    onSelect(row.project_id);
    tabs.current[next]?.focus();
  }

  /**
   * Send one project's change.
   *
   * The root sent is the one typed, or the recorded one for any setting other than off — exactly
   * what the per-row control sent before. A change that goes through is confirmed on screen by the
   * pressed segment, the badge and the gate, and aloud here; never by a new line, which would push
   * the page down under the hand that made the change. Nothing is re-ranked either.
   */
  function change(project: ProjectSummary, mode: AutopilotMode, withRoot: string | null) {
    const id = project.project_id;
    const trimmed = withRoot === null ? "" : withRoot.trim();
    setMode.mutate(
      {
        project_id: id,
        mode,
        ...(trimmed === "" ? {} : { project_root: trimmed }),
      },
      {
        onSuccess: () => {
          onRefusals((current) => without(current, id));
          setSaid(
            `${id} is now ${readState("autopilot", mode)?.label ?? mode} — ${MODE_MEANING[mode]}`,
          );
        },
        onError: (error) => {
          if (isApiRefusal(error)) {
            onRefusals((current) => new Map(current).set(id, { mode, refusal: error }));
          }
        },
      },
    );
  }

  const failed =
    setMode.isError && !isApiRefusal(setMode.error)
      ? (setMode.variables?.project_id ?? null)
      : null;
  const ranks = shown.map((row) => urgencyOf(row, refusals.has(row.project_id)));
  const pointed = shown.find((row) => row.project_id === pointedAt) ?? focused;
  return (
    // The setting for the project in focus is the first thing under the header, with the roster's
    // index and list under it (no section title above either: the index says "1 of N").
    <section className="ap-fan" aria-label="Projects">
      {answered && rows.length === 0 && (
        <Teach title="No project is under autopilot">
          <p>
            A project appears here once the {DAEMON_NAME} has been told where it lives.
            Nothing is broken and nothing is hidden — there is simply no project
            to govern yet.
          </p>
        </Teach>
      )}
      {!answered && <p className="ap-loading">Reading the roster…</p>}
      {focused !== undefined && (
        <>
          <div className="ap-fan-control">
            <div
              className="ap-fan-focus"
              id={panelId}
              role={many ? "tabpanel" : undefined}
              aria-labelledby={many ? tabId(index) : undefined}
            >
              <FocusSetting
                key={focused.project_id}
                project={focused}
                refused={refusals.get(focused.project_id)}
                failed={failed === focused.project_id}
                busy={setMode.isPending}
                onChange={(mode, withRoot) => change(focused, mode, withRoot)}
              />
            </div>
            {/* What each setting means, said once rather than on every card, in the map's words. */}
            <p className="ap-note ap-fan-note">
              Three settings and not two. <strong>Off</strong> means {MODE_MEANING.off}.{" "}
              <strong>Shadow</strong> means {MODE_MEANING.shadow}.{" "}
              <strong>Active</strong> means {MODE_MEANING.active}.
            </p>
          </div>

          <div className="ap-fan-view">
            {many && (
              <>
                <div className="ap-fan-index">
                  <div
                    className="ap-fan-rail"
                    role="tablist"
                    aria-label="Projects, most urgent first"
                    onKeyDown={onIndexKey}
                    onPointerLeave={() => setPointedAt(null)}
                  >
                    {shown.map((row, at) => (
                      <button
                        key={row.project_id}
                        ref={(element) => {
                          tabs.current[at] = element;
                        }}
                        type="button"
                        role="tab"
                        id={tabId(at)}
                        className={[
                          "ap-fan-tick",
                          `ap-fan-tick-${row.mode}`,
                          ranks[at] === 0 ? "ap-fan-tick-exception" : "",
                          at > 0 && ranks[at - 1] !== ranks[at] ? "ap-fan-tick-gap" : "",
                        ]
                          .filter((name) => name !== "")
                          .join(" ")}
                        aria-selected={at === index}
                        aria-controls={panelId}
                        tabIndex={at === index ? 0 : -1}
                        onClick={() => onSelect(row.project_id)}
                        onPointerEnter={() => setPointedAt(row.project_id)}
                        onFocus={() => setPointedAt(row.project_id)}
                      >
                        {/* The name is content, not `aria-label`: the tab is found by what it says. */}
                        <span className="sr-only">
                          {describeProject(row, refusals.has(row.project_id))}
                        </span>
                        <span className="ap-fan-tick-bar" />
                      </button>
                    ))}
                  </div>
                  <div className="ap-fan-index-meta">
                    <span className="ap-fan-position">
                      {index + 1} of {total}
                    </span>
                    <strong className="ap-fan-position-name">{focused.project_id}</strong>
                    <span className="ap-fan-hint">
                      <kbd>←</kbd> <kbd>→</kbd> on the row
                    </span>
                  </div>
                </div>
                <p className="ap-fan-readout" aria-hidden="true">
                  {pointed === undefined ? null : (
                    <>
                      <strong>{pointed.project_id}</strong> —{" "}
                      {readingOf(pointed, refusals.has(pointed.project_id))}
                    </>
                  )}
                </p>
                <RosterList
                  rows={shown}
                  selected={focused.project_id}
                  refusals={refusals}
                  onSelect={onSelect}
                />
              </>
            )}
          </div>
        </>
      )}
      <p className="sr-only" aria-live="polite">
        {said}
      </p>
    </section>
  );
}

/**
 * The roster as a list, most urgent first: name, the map's word for its mode, and the same reading
 * the index readout gives on hover. It is the same triage in words, and it works with the
 * keyboard and a screen reader as plain buttons. Ten or twenty projects fit on one screen.
 */
function RosterList({
  rows,
  selected,
  refusals,
  onSelect,
}: {
  rows: readonly ProjectSummary[];
  selected: string;
  refusals: ReadonlyMap<string, Refused>;
  onSelect: (projectId: string) => void;
}) {
  return (
    <ul className="ap-roster" aria-label="All projects by urgency">
      {rows.map((row) => (
        <li key={row.project_id}>
          <button
            type="button"
            className="ap-roster-row"
            aria-current={row.project_id === selected ? "true" : undefined}
            onClick={() => onSelect(row.project_id)}
          >
            <span className="ap-roster-name">{row.project_id}</span>
            <StateBadge domain="autopilot" state={row.mode} />
            <span className="ap-roster-reading">
              {readingOf(row, refusals.has(row.project_id))}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

/**
 * The project every panel below acts on, kept in view while the page scrolls.
 *
 * Eight panels read or change the project in focus but the picker is at the top, so somebody in
 * "Start a job" could send work to a project they stopped looking at. This bar sits in the flow
 * after the brakes and sticks once the carousel has scrolled away.
 */
function ProjectBar({ project }: { project: ProjectSummary }) {
  return (
    <div className="ap-project-bar" role="group" aria-label="Project in focus">
      <span className="ap-project-bar-label">Acting on</span>
      <strong className="ap-project-bar-name">{project.project_id}</strong>
      <StateBadge domain="autopilot" state={project.mode} />
    </div>
  );
}

/**
 * The selected project's setting: whose it is, its gate, the switch, and whatever the núcleo said
 * to the last change.
 *
 * Mounted per project (the caller keys it by id), so walking to another project unmounts this one —
 * and an armed "Let it act" with it. An interlock that followed the index would confirm a project
 * the reader is no longer looking at.
 *
 * The gate is a line every project has, always rendered and at least two lines tall, so the switch
 * under it never moves as the project changes: an acting project says how to stop it, an earned one says
 * so, any other says why not in the núcleo's terms. While the third segment is armed the same line
 * says what confirming would do — and that segment is described by it from the first render, because
 * a description attached at the moment of arming lands on a button that already has focus and is
 * not re-announced.
 */
function FocusSetting({
  project,
  refused,
  failed,
  busy,
  onChange,
}: {
  project: ProjectSummary;
  refused: Refused | undefined;
  failed: boolean;
  busy: boolean;
  onChange: (mode: AutopilotMode, withRoot: string | null) => void;
}) {
  const [armed, setArmed] = useState(false);
  const [root, setRoot] = useState(project.project_root ?? "");
  const gateId = useId();
  const fieldId = useId();
  const field = useRef<HTMLInputElement>(null);
  /** The refusal already on screen when this mounted: coming back to it is not a new refusal. */
  const seen = useRef(refused);
  const gate = gateFor(project);

  /**
   * A project nobody onboarded is the one 422 the daemon names (`not_onboarded`), because it is
   * the one this page can resolve in place: the onboarding panel, then the same change again.
   */
  const notOnboarded = refused !== undefined && refused.refusal.code === "not_onboarded";
  const [onboarding, setOnboarding] = useState(false);

  /**
   * A bare 422 is the *only* other refusal that opens the root input.
   *
   * `activation_status` maps three different causes onto one bare 422 with an
   * empty body — no root given, no PreToolUse hook pointing at `ask_daemon.py`,
   * or a root that is not a git repository — and the daemon sends nothing that
   * distinguishes them. So the page names the set, says it does not know which,
   * and offers the one of them it can do something about. Guessing a single
   * cause here would be wrong two times out of three.
   */
  const needsRoot = refused !== undefined && refused.refusal.status === 422 && !notOnboarded;

  // A new 422 hands focus to the one prerequisite the shell can supply. Only a NEW one: walking
  // back to a project refused earlier must not pull focus out of the index.
  useEffect(() => {
    if (refused === seen.current) return;
    seen.current = refused;
    if (refused !== undefined && refused.refusal.status === 422) field.current?.focus();
  }, [refused]);

  return (
    <>
      <p className="ap-fan-focus-title">
        Setting for <strong>{project.project_id}</strong>
      </p>
      <p id={gateId} className={gate.open ? "ap-fan-gate" : "ap-fan-gate ap-fan-gate-locked"}>
        {armed ? promotionConsequence(project) : gate.text}
      </p>
      <ModeSwitch
        value={project.mode}
        actAllowed={project.promotable}
        actArmedLabel={promotionConfirmLabel(project)}
        actConsequence={promotionConsequence(project)}
        onArmedChange={setArmed}
        actDescribedBy={gateId}
        busy={busy}
        focusableWhenInert
        onChoose={(mode) =>
          onChange(mode, mode === "off" ? null : root === "" ? project.project_root : root)
        }
      />

      {needsRoot && (
        <div className="ui-note ui-note-refusal ap-fan-refusal" role="status">
          <span className="ui-note-code">{refused.refusal.code}</span>
          <span className="ui-note-text">{MODE_SENTENCES.unprocessable.lead}</span>
          <ul className="ap-fan-prereqs">
            {[...MODE_SENTENCES.unprocessable.items, MODE_SENTENCES.unprocessable.plus].map(
              (item, at) => (
                <li key={at}>
                  {item.text}
                  {item.path === undefined ? null : <code>{item.path}</code>}
                  {item.after}
                </li>
              ),
            )}
          </ul>
        </div>
      )}
      {needsRoot && (
        <Inset className="ap-project-root">
          <label className="ap-project-root-label" htmlFor={fieldId}>
            Folder for {project.project_id}
          </label>
          <input
            ref={field}
            id={fieldId}
            className="ap-project-root-input"
            type="text"
            value={root}
            spellCheck={false}
            placeholder={"C:\\path\\to\\the\\project"}
            onChange={(event) => setRoot(event.target.value)}
          />
          <Button
            variant="ghost"
            disabled={root.trim() === "" || busy}
            onClick={() => onChange(refused.mode, root)}
          >
            Try again with this folder
          </Button>
        </Inset>
      )}
      {notOnboarded && (
        <div className="ui-note ui-note-refusal ap-fan-refusal" role="status">
          <span className="ui-note-code">{refused.refusal.code}</span>
          <span className="ui-note-text">
            {project.project_id} has not been onboarded to NucleOS, so it cannot be watched or let
            act yet.
          </span>
        </div>
      )}
      {notOnboarded && !onboarding && (
        <Button variant="ghost" onClick={() => setOnboarding(true)}>
          Onboard {project.project_id}
        </Button>
      )}
      {notOnboarded && onboarding && (
        <Inset className="ap-project-root">
          <OnboardPanel
            projectId={project.project_id}
            root={root.trim() === "" ? (project.project_root ?? "") : root}
            onDone={() => {
              setOnboarding(false);
              onChange(refused.mode, root.trim() === "" ? project.project_root : root);
            }}
          />
        </Inset>
      )}
      {refused !== undefined && !needsRoot && !notOnboarded && (
        <RefusalNote
          refusal={refused.refusal}
          sentences={{
            ...MODE_REFUSAL_PROSE,
            ...daemonProse(refused.refusal),
          }}
        />
      )}
      {failed && (
        <ErrorNote>
          Could not reach the {DAEMON_NAME} — this project&apos;s setting is unchanged.
        </ErrorNote>
      )}
    </>
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
  const opinions = useJudgeOpinionsForDecisions(rows.map((decision) => decision.id));
  const opinionOf = (id: number) => opinions.data?.find((opinion) => opinion.shadow_decision_id === id);

  if (projectId !== null && decisions.data !== undefined && rows.length === 0) {
    return (
      <Section label="Shadow decisions">
        {/* The selected row is the subject; name it because its light-theme mark has no fill weight, while "this project" is only for the first render. */}
        <Quiet says={`Nothing is waiting for a verdict on ${project?.project_id ?? "this project"}.`}>
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
      {projectId === null && <Quiet says="Choose a project above." />}
      {projectId !== null &&
        decisions.isError &&
        decisions.data === undefined && (
          <ListError error={decisions.error} what="the shadow decisions" />
        )}
      {projectId !== null &&
        !decisions.isError &&
        decisions.data === undefined && (
          <p className="ap-loading">Reading the shadow decisions…</p>
        )}
      {rows.length > 0 && (
        <ul className="ap-list" aria-label="Shadow decisions">
          {rows.map((decision) => (
            <Inset as="li" key={decision.id}>
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
                {opinionOf(decision.id) !== undefined && (
                  <JudgeOpinionLine opinion={opinionOf(decision.id)!} />
                )}
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
              {decision.reason === null || decision.reason.trim() === "" ? (
                <Quiet says="The classifier recorded no reason." />
              ) : (
                <p className="ap-reason">{decision.reason}</p>
              )}
              <RawInput raw={decision.tool_input} />
              <div className="ap-actions">
                <ConfirmButton
                  label="Allow"
                  subject={`#${decision.id}`}
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
                  label="Block"
                  subject={`#${decision.id}`}
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
            </Inset>
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
      <ErrorNote>Could not reach the {DAEMON_NAME} — nothing was recorded.</ErrorNote>
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
        <Quiet says={`${project?.project_id ?? "This project"} has recorded no classified decision yet.`}>
          <p className="ap-note">Read-only. The bar is {READINESS_MIN_REVIEWED} reviews at {READINESS_MIN_AGREE_PERCENT}% agreement per action class; the ready/total figure is the {DAEMON_NAME}&apos;s own and gates the promote control.</p>
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
      {projectId === null && <Quiet says="Choose a project above." />}
      {projectId !== null &&
        scoreboard.isError &&
        scoreboard.data === undefined && (
          <ListError error={scoreboard.error} what="the scoreboard" />
        )}
      {projectId !== null &&
        !scoreboard.isError &&
        scoreboard.data === undefined && (
          <p className="ap-loading">Reading the scoreboard…</p>
        )}
      {evidence.length > 0 && (
        <TallyTable label="Shadow evidence" rows={evidence} />
      )}
      {history.length > 0 && (
        <div className="ap-block">
          {/* `level={3}` because this heading sits inside a `Panel` that already
              has an `h2`. Announced as a sibling of "Scoreboard" it would tell a
              screen reader the enforced rows are a section of the page rather
              than a group within this one. */}
          <Section label="enforced, and not evidence for promotion" level={3}>
            <TallyTable label="Enforced decisions" rows={history} />
          </Section>
        </div>
      )}
      {rows.length > 0 && (
        <p className="ap-note ap-note-foot">
          the count that decides it is not the count below: the {DAEMON_NAME} counts reviews distinct by tool and arguments, so ten answers to the same command are ten here and one at the bar.
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
          <th scope="col">agreement</th>
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
            <td className="ap-score-agree">
              <Agreement row={row} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * How often the owner agreed with the classifier, against the bar a class must clear.
 *
 * The page used to leave the division to the reader. The bar is the fill's full width: a class that
 * reaches it AND has enough reviews says "ready", one that reaches the percentage on too few reviews
 * says how many more it needs. Nothing reviewed yet is a dash, never 0%.
 */
function Agreement({ row }: { row: ClassTally }) {
  if (row.reviewed === 0) return <span className="ap-agree-none">—</span>;
  const percent = Math.round((row.agree / row.reviewed) * 100);
  const enough = row.reviewed >= READINESS_MIN_REVIEWED;
  const verdict =
    percent < READINESS_MIN_AGREE_PERCENT
      ? "below the bar"
      : enough
        ? "ready"
        : `${READINESS_MIN_REVIEWED - row.reviewed} more reviews needed`;
  return (
    <span className="ap-agree">
      <span className="ap-agree-figure">{percent}%</span>
      <Meter
        label={`Agreement in ${row.action_class}`}
        value={percent}
        ceiling={READINESS_MIN_AGREE_PERCENT}
        tone="active"
        format={(n) => `${n}%`}
        head={false}
      />
      <span className="ap-agree-verdict">{verdict}</span>
    </span>
  );
}

/* ---------------------------------------------------------- trigger kills -- */

/**
 * The trigger scopes, and whether the núcleo reads them.
 *
 * `reads` is not decoration and is not a guess: each `true` below is a
 * `scoped_kill_engaged(pool, "trigger", …)` call site in `core/src/`, named in
 * the comment beside it. All four scopes are read now, `team` last —
 * `core/src/team/trigger.rs:461` calls `scoped_kill_engaged(&state.pool,
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
    // `team/trigger.rs`, before any rule is considered.
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
        <p className="ap-loading">Reading the trigger brakes…</p>
      )}
      <Rows label="Trigger brakes">
        {TRIGGER_SCOPES.map((scope) => {
          const engaged = scopeEngaged(kills.data, "trigger", scope.id);
          return (
            <Row className="ap-trigger-row" key={scope.id}>
              <span className="ap-trigger-name">{scope.label}</span>
              <StateBadge domain="brake" state={scope.reads ? (engaged ? "held" : "released") : "not_read"} />
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
                  aria-label={
                    engaged
                      ? `Release ${scope.label.toLowerCase()}`
                      : `Hold ${scope.label.toLowerCase()}`
                  }
                >
                  {/* The row already names the scope; the verb alone keeps every button one width. */}
                  {engaged ? "Release" : "Hold"}
                </Button>
              ) : (
                <p className="ap-trigger-note">
                  The {DAEMON_NAME} will store this brake and nothing in it reads the
                  value, so engaging it would stop nothing. It becomes a real
                  switch when the Teams slice lands and team triggers exist to
                  be held.
                </p>
              )}
            </Row>
          );
        })}
      </Rows>
      {setKill.isError && (
        <ErrorNote>
          That brake was not changed — the {DAEMON_NAME} refused or could not be reached.
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
  // Which queue is holding it, not just how many items: a person reading the bare number went to
  // the proposals list and found it empty, because all of it was shadow decisions.
  const targetWaitingWhere = target === undefined ? null : whereWaiting(target);
  const blocked = target === undefined || target.queue_full || target.mode === "off";

  return (
    <Panel title="Jobs in flight" aside={<Count n={jobs.data?.length} />}>
      {jobs.isError && jobs.data === undefined && <ListError error={jobs.error} what="the jobs" />}
      {jobs.data !== undefined && live.length === 0 && <Quiet says="Nothing is running. Start one below, or from the Fleet page." />}
      {live.length > 0 && (
        <ul className="ap-list" aria-label="Jobs in flight">
          {live.map((job) => (
            <JobRow key={job.id} job={job} />
          ))}
        </ul>
      )}

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
        {/* Said before the click rather than after the 409, and right above the control it
            explains. A project that is off starts nothing, and one whose queue is full defers
            rather than refuses — two different reasons the button would not do what it says. */}
        {target !== undefined && target.mode === "off" && (
          <p className="ap-hedge">
            {target.project_id} is off, so the {DAEMON_NAME} would not start this.
          </p>
        )}
        {target !== undefined && target.queue_full && (
          <p className="ap-hedge">
            {target.project_id} is holding {target.open_review_items} items waiting for review
            {targetWaitingWhere === null ? "" : ` (${targetWaitingWhere})`} against its ceiling of{" "}
            {target.wip_limit ?? "none"}.{" "}
            <Link className="ap-link" to="/waiting">
              Review {target.open_review_items === 1 ? "1 item" : `${target.open_review_items} items`}
            </Link>{" "}
            and the brake releases itself.
          </p>
        )}
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
      {create.isError && <JobError error={create.error} />}
    </Panel>
  );
}

function JobRow({ job }: { job: Job }) {
  // A job is stopped through its own route (`POST /jobs/{id}/cancel`), never as a run: the job id
  // and a run id are different numbers, and the wrong one would stop somebody else's work.
  const cancel = useCancelSlotOwner();
  return (
    <Inset as="li" className="ap-job">
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
      <div className="ap-actions">
        <ConfirmButton
          label="Stop job"
          confirmLabel="Stop this job"
          subject={`#${job.id}`}
          variant="danger"
          intent="stop"
          disabled={cancel.isPending}
          onConfirm={() => cancel.mutate({ kind: "job", id: job.id })}
        />
        <Link className="ap-link" to="/fleet">
          See it in Fleet
        </Link>
      </div>
      {cancel.isError && (
        <ErrorNote>Could not reach the {DAEMON_NAME} — job {job.id} was not stopped.</ErrorNote>
      )}
    </Inset>
  );
}

function JobError({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>Could not reach the {DAEMON_NAME} — no job was started.</ErrorNote>
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
      Could not reach the {DAEMON_NAME} — nothing is known about {what}.
    </ErrorNote>
  );
}
