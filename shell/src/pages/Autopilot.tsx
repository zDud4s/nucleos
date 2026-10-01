import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type SetStateAction,
} from "react";
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
  useJudgeOpinionsForDecisions,
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
  ConfirmButton,
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
  StatCard,
  StaleNote,
  StateBadge,
  Teach,
  readState,
} from "../ui";
import { whereWaiting } from "../data/roster";
import {
  EASE,
  caption,
  cardPose,
  circularOffset,
  describeProject,
  evidence,
  fanConfig,
  gateFor,
  headlineFor,
  holdOrder,
  nearestTarget,
  shortPath,
  slideDuration,
  urgencyOf,
  type FanConfig,
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
import { ResolvePanel } from "./AutopilotResolve";
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
        <ProjectCarousel
          rows={rows}
          answered={answered}
          order={order}
          selected={selected}
          onSelect={setChosen}
          refusals={refusals}
          onRefusals={setRefusals}
        />
        <ShadowReviewPanel projectId={selected} project={focused} />
        <ScoreboardPanel projectId={selected} project={focused} />
        <JudgePanel projectId={selected} project={focused} />
        <JudgeReviewPanel projectId={selected} />
        <ResolvePanel projectId={selected} />
        <TriggerKills />
        <JobsPanel rows={rows} selected={selected} />
        <FeedEmbed />
      </div>
    </>
  );
}

/* ------------------------------------------------------------- the reading -- */

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
            : `${projects.length} ${projects.length === 1 ? "project" : "projects"} on the roster`
        }
      />
      <StatCard
        label="Watching in shadow"
        value={shadow}
        detail="deciding without enforcing, to earn the promotion"
      />
      <StatCard
        label="To review"
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

/**
 * Whether the fan should jump rather than slide.
 *
 * Read per slide rather than once, so a preference changed while the page is open is obeyed. No
 * `matchMedia` at all (jsdom) counts as reduced: with no layout there is nothing to animate.
 */
function reducedMotion(): boolean {
  return (
    typeof window.matchMedia !== "function" ||
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
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

/** A drag in progress: where it started, where it is, and the last tenth of a second of it. */
interface Drag {
  pointer: number;
  startX: number;
  lastX: number;
  /** The card the fan rested on when the drag began; a fling travels at most three from it. */
  from: number;
  moved: boolean;
  samples: { t: number; x: number }[];
}

/**
 * The roster as a fan, one project in focus at a time, and the setting for that project under it.
 *
 * Three parts, in the order they are read. The index is every project at once, most urgent first —
 * the answer to "is everything fine?" that one card cannot give — and it is the keyboard's only way
 * in: a tablist with one tab stop whose selection follows focus. The stage is drawing, not
 * controls: hidden from assistive technology as a whole, it takes pointer input only (a drag, a
 * fling, a click on a neighbouring card, a sideways swipe) and never a tab stop. The panel below is
 * the selected project's setting, and nobody else's.
 *
 * One mutation for the whole roster, each refusal kept against the project that caused it: only one
 * project's switch is on screen at a time, and a refusal has to outlive walking to another project
 * and back.
 *
 * The fan moves through CSSOM (`element.style`) inside animation frames, which the production CSP
 * allows and a `<style>` element it refuses — so never `AnimatePresence mode="popLayout"` nor View
 * Transitions, which both build one. The curve is `--ease`, solved in `lib/autopilot-fan.ts`.
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
  /** What the live region says: a change that went through, or a project the stage moved to. */
  const [said, setSaid] = useState("");
  const [pointedAt, setPointedAt] = useState<string | null>(null);
  const [cfg, setCfg] = useState<FanConfig>(() => fanConfig(900));
  const [dragging, setDragging] = useState(false);

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
  const hasStage = total > 0;

  const stageRef = useRef<HTMLDivElement>(null);
  const cards = useRef(new Map<string, HTMLElement>());
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  /** Where the fan is, in cards: fractional while it moves, whole once it rests. */
  const progress = useRef(0);
  const slide = useRef<{ to: number; frame: number } | null>(null);
  const drag = useRef<Drag | null>(null);
  const placed = useRef(false);
  /**
   * What the motion reads between renders. The animation frames and the listeners outlive the
   * render that started them, so they read the latest roster and selection from here.
   */
  const latest = useRef({ shown, index, cfg, refusals, onSelect });

  /** Every card's pose for the current `progress`, written straight to its style. */
  function paint() {
    const now = latest.current.shown;
    const fan = latest.current.cfg;
    now.forEach((row, at) => {
      const card = cards.current.get(row.project_id);
      if (card === undefined) return;
      const pose = cardPose(circularOffset(at, progress.current, now.length), now.length, fan);
      card.style.transform = `translate(${pose.x}px, ${pose.y}px) rotate(${pose.rot}deg) scale(${pose.scale})`;
      card.style.opacity = String(pose.opacity);
      card.style.zIndex = String(pose.z);
      card.style.visibility = pose.hidden ? "hidden" : "visible";
      const dim = card.querySelector<HTMLElement>(".ap-fan-dim");
      if (dim !== null) dim.style.opacity = String(pose.dim);
      const facts = card.querySelector<HTMLElement>(".ap-fan-facts");
      if (facts !== null) facts.style.opacity = String(pose.factsOpacity);
    });
  }

  /** A selection made by the stage, which the index cannot announce: said in the live region. */
  function chooseFromStage(at: number) {
    const row = latest.current.shown[at];
    if (row === undefined) return;
    latest.current.onSelect(row.project_id);
    setSaid(describeProject(row, latest.current.refusals.has(row.project_id)));
  }

  /** Where the fan came to rest decides the selection — unless something already chose it. */
  function settle() {
    const count = latest.current.shown.length;
    if (count === 0) return;
    const rounded = Math.round(progress.current);
    const next = count >= 3 ? wrap(rounded, count) : clamp(rounded, 0, count - 1);
    progress.current = next;
    paint();
    if (next !== latest.current.index) chooseFromStage(next);
  }

  /** Slide to `target` on the app's curve: 240 ms, half as long again past one card, none reduced. */
  function animateTo(target: number) {
    const count = latest.current.shown.length;
    const goal = count >= 3 ? target : clamp(target, 0, Math.max(0, count - 1));
    if (slide.current !== null) cancelAnimationFrame(slide.current.frame);
    slide.current = null;
    const from = progress.current;
    const duration = slideDuration(Math.abs(goal - from), reducedMotion());
    if (duration === 0) {
      progress.current = goal;
      paint();
      settle();
      return;
    }
    const start = performance.now();
    const step = (now: number) => {
      const k = Math.min(1, (now - start) / duration);
      progress.current = from + (goal - from) * EASE(k);
      paint();
      if (k < 1) {
        slide.current = { to: goal, frame: requestAnimationFrame(step) };
      } else {
        slide.current = null;
        settle();
      }
    };
    slide.current = { to: goal, frame: requestAnimationFrame(step) };
  }

  // After every commit: the motion reads the new roster, and every card — new ones included —
  // takes its pose. Layout effects, so a card is never painted once at the origin first.
  useLayoutEffect(() => {
    latest.current = { shown, index, cfg, refusals, onSelect };
    paint();
  });

  // The index, a card click, a swipe or a settled drag chose a project: the fan catches up, the
  // near way round a ring. The first time there is a roster it is simply placed.
  useLayoutEffect(() => {
    if (total === 0) return;
    if (!placed.current) {
      placed.current = true;
      progress.current = index;
      paint();
      return;
    }
    const from = slide.current?.to ?? progress.current;
    animateTo(nearestTarget(index, from, total));
    // `paint`/`animateTo` read everything else through `latest`; only these two start a slide.
  }, [index, total]);

  // Sized by the stage and not the window: inside the app the rail takes 15rem of it. A width of
  // zero is a stage nobody can see (or jsdom), and measuring it would shrink the cards to nothing.
  useLayoutEffect(() => {
    const stage = stageRef.current;
    if (stage === null) return;
    const measure = () => {
      const width = stage.clientWidth;
      if (width === 0) return;
      const next = fanConfig(width);
      stage.style.setProperty("--ap-fan-card-w", `${next.cardW}px`);
      stage.style.setProperty("--ap-fan-card-h", `${next.cardH}px`);
      setCfg((current) =>
        current.cardW === next.cardW && current.compact === next.compact ? current : next,
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(stage);
    return () => observer.disconnect();
  }, [hasStage]);

  // A trackpad's sideways swipe, and only that — a vertical wheel still scrolls the page. Bound
  // by hand because React's `onWheel` is passive and could not keep the page from scrolling.
  useEffect(() => {
    const stage = stageRef.current;
    if (stage === null) return;
    let swept = 0;
    const onWheel = (event: WheelEvent) => {
      if (Math.abs(event.deltaX) <= Math.abs(event.deltaY)) return;
      event.preventDefault();
      swept += event.deltaX;
      if (Math.abs(swept) <= 60 || slide.current !== null) return;
      const count = latest.current.shown.length;
      const at = latest.current.index + Math.sign(swept);
      swept = 0;
      const next = count >= 3 ? wrap(at, count) : clamp(at, 0, count - 1);
      if (next !== latest.current.index) chooseFromStage(next);
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => stage.removeEventListener("wheel", onWheel);
  }, [hasStage]);

  useEffect(
    () => () => {
      if (slide.current !== null) cancelAnimationFrame(slide.current.frame);
    },
    [],
  );

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

  function onStageDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (total <= 1 || event.button !== 0) return;
    // The name and path of the card in focus can be selected and copied; a drag there would steal it.
    if (event.target instanceof Element && event.target.closest(".ap-fan-card-focus .ap-fan-selectable") !== null) {
      return;
    }
    event.currentTarget.setPointerCapture?.(event.pointerId);
    drag.current = {
      pointer: event.pointerId,
      startX: event.clientX,
      lastX: event.clientX,
      from: Math.round(slide.current?.to ?? progress.current),
      moved: false,
      samples: [{ t: event.timeStamp, x: event.clientX }],
    };
  }

  function onStageMove(event: ReactPointerEvent<HTMLDivElement>) {
    const held = drag.current;
    if (held === null || event.pointerId !== held.pointer) return;
    const dx = event.clientX - held.lastX;
    held.lastX = event.clientX;
    if (!held.moved && Math.abs(event.clientX - held.startX) > 4) {
      held.moved = true;
      setDragging(true);
      if (slide.current !== null) {
        cancelAnimationFrame(slide.current.frame);
        slide.current = null;
      }
    }
    if (!held.moved) return;
    let delta = -dx / (cfg.xStep * 1.2);
    // Two cards do not wrap, so past either end the drag resists instead of travelling.
    if (total < 3 && (progress.current + delta < 0 || progress.current + delta > total - 1)) {
      delta *= 0.3;
    }
    progress.current += delta;
    held.samples.push({ t: event.timeStamp, x: event.clientX });
    while (held.samples.length > 2 && event.timeStamp - held.samples[0].t > 100) held.samples.shift();
    paint();
  }

  function onStageUp(event: ReactPointerEvent<HTMLDivElement>) {
    const held = drag.current;
    if (held === null || event.pointerId !== held.pointer) return;
    drag.current = null;
    if (!held.moved) {
      // Whatever the browser painted on top is what was clicked — no geometry to keep in step.
      const hit = document.elementFromPoint?.(event.clientX, event.clientY)?.closest(".ap-fan-card");
      const at = hit instanceof HTMLElement ? Number(hit.dataset.index) : Number.NaN;
      if (Number.isInteger(at) && at !== index) chooseFromStage(at);
      return;
    }
    setDragging(false);
    const first = held.samples[0];
    const last = held.samples[held.samples.length - 1];
    const velocity = ((last.x - first.x) / Math.max(1, last.t - first.t)) * 1000;
    const projected = progress.current - velocity / (cfg.xStep * 4.5);
    animateTo(clamp(Math.round(projected), held.from - 3, held.from + 3));
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
  const stageClass = [
    "ap-fan-stage",
    total <= 1 ? "ap-fan-stage-single" : "",
    dragging ? "ap-fan-stage-dragging" : "",
    cfg.compact ? "ap-fan-stage-compact" : "",
  ]
    .filter((name) => name !== "")
    .join(" ");

  return (
    <Panel
      title="Projects"
      variant="flat"
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
      {focused !== undefined && (
        <>
          {/* What each setting means, said once rather than on every card, in the map's words. */}
          <p className="ap-note ap-fan-note">
            Three settings and not two. <strong>Off</strong> means {MODE_MEANING.off}.{" "}
            <strong>Shadow</strong> means {MODE_MEANING.shadow}.{" "}
            <strong>Active</strong> means {MODE_MEANING.active}.
          </p>

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
                  <span className="ap-fan-hint">
                    <kbd>←</kbd> <kbd>→</kbd> on the row, or drag the cards
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
            </>
          )}

          <div
            ref={stageRef}
            className={stageClass}
            aria-hidden="true"
            onPointerDown={onStageDown}
            onPointerMove={onStageMove}
            onPointerUp={onStageUp}
            onPointerCancel={onStageUp}
          >
            {shown.map((row, at) => (
              <article
                key={row.project_id}
                ref={(element) => {
                  if (element === null) cards.current.delete(row.project_id);
                  else cards.current.set(row.project_id, element);
                }}
                data-index={at}
                className={at === index ? "ap-fan-card ap-fan-card-focus" : "ap-fan-card"}
              >
                <FanCard
                  project={row}
                  refused={refusals.has(row.project_id)}
                  cardW={cfg.cardW}
                />
              </article>
            ))}
          </div>

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
        </>
      )}
      <p className="sr-only" aria-live="polite">
        {said}
      </p>
    </Panel>
  );
}

/**
 * One card: who the project is, the one thing that needs you if anything does, and its reading.
 *
 * Identity first — the name in the display face and the map's word for its mode, then its folder in
 * mono, shortened from the middle so the end that tells two projects apart survives. The exception
 * band is the only tinted box a card carries.
 */
function FanCard({
  project,
  refused,
  cardW,
}: {
  project: ProjectSummary;
  refused: boolean;
  cardW: number;
}) {
  const room = Math.floor((cardW - 26) / 6.7);
  return (
    <>
      <div className="ap-fan-dim" />
      <div className="ap-fan-id">
        <div className="ap-fan-id-row">
          <h3 className="ap-fan-name ap-fan-selectable" title={project.project_id}>
            {project.project_id}
          </h3>
          <StateBadge domain="autopilot" state={project.mode} />
        </div>
        {project.project_root === null ? (
          <p className="ap-fan-root ap-fan-root-none">no folder named</p>
        ) : (
          <p className="ap-fan-root ap-fan-selectable" title={project.project_root}>
            {shortPath(project.project_root, room)}
          </p>
        )}
      </div>
      {refused ? (
        <p className="ap-fan-exception">Setting refused — see below</p>
      ) : project.queue_full ? (
        <p className="ap-fan-exception">Queue full — review one to release it</p>
      ) : null}
      <FanVisor project={project} />
      <FanFacts project={project} />
    </>
  );
}

/**
 * The reading, in a sunken well. In shadow and off it is the evidence: classes clearing the bar,
 * one segment per class in the mode's tone. Once a project acts the evidence is history, and what
 * matters is how full its review queue is — proposals and shadow decisions together, which is
 * what `open_review_items` counts and what the ceiling holds.
 */
function FanVisor({ project }: { project: ProjectSummary }) {
  if (project.mode === "active") {
    return (
      <div className="ap-fan-visor">
        <span className="ap-fan-visor-label">waiting for review</span>
        <div className="ap-fan-reading">
          <span className="ap-fan-figure">
            {project.open_review_items}
            {project.wip_limit === null ? null : (
              <span className="ap-fan-figure-of">/{project.wip_limit}</span>
            )}
          </span>
          <Meter
            label="waiting for review"
            value={project.open_review_items}
            ceiling={project.wip_limit}
            tone="pending"
            head={false}
          />
          <span className="ap-fan-caption">acting on its own</span>
        </div>
      </div>
    );
  }
  return (
    <div className="ap-fan-visor">
      <span className="ap-fan-visor-label">clearing the bar</span>
      <div className="ap-fan-reading">
        {project.classes_total === 0 ? (
          <span className="ap-fan-figure ap-fan-figure-word">none yet</span>
        ) : (
          <span className="ap-fan-figure">
            {project.classes_ready}
            <span className="ap-fan-figure-of">/{project.classes_total}</span>
          </span>
        )}
        {project.classes_total === 0 ? (
          <div className="ap-fan-track ap-fan-track-empty" />
        ) : (
          <div className={`ap-fan-track ap-fan-tone-${project.mode}`}>
            {Array.from({ length: project.classes_total }, (_, at) => (
              <span
                key={at}
                className={at < project.classes_ready ? "ap-fan-seg ap-fan-seg-on" : "ap-fan-seg"}
              />
            ))}
          </div>
        )}
        <span className="ap-fan-caption">{caption(project)}</span>
      </div>
    </div>
  );
}

/** The two facts under the well: what is waiting for a verdict, and how full the queue is. */
function FanFacts({ project }: { project: ProjectSummary }) {
  // Which queue, not just how many: a person reading the bare number went to the proposals list
  // and found it empty, because on that project all of it was shadow decisions. `null` when the
  // daemon sent no split, and then the line says only what the total is.
  const waitingWhere = whereWaiting(project);
  if (project.mode === "active") {
    return (
      <dl className="ap-fan-facts">
        <div className="ap-fan-fact">
          <dt>classes ready</dt>
          <dd>
            {project.classes_ready}/{project.classes_total}
          </dd>
        </div>
        <div className="ap-fan-fact">
          <dt>to review</dt>
          <dd>{project.pending}</dd>
        </div>
      </dl>
    );
  }
  return (
    <dl className="ap-fan-facts">
      <div className="ap-fan-fact">
        <dt>to review</dt>
        <dd>{project.pending}</dd>
      </div>
      <div className="ap-fan-fact">
        <dt>queue</dt>
        <dd>
          {project.wip_limit === null
            ? project.open_review_items
            : `${project.open_review_items} of ${project.wip_limit}`}
          <span className="ap-fan-fact-sub" title={waitingWhere ?? undefined}>
            {waitingWhere ?? (project.wip_limit === null ? "no ceiling" : "waiting for review")}
          </span>
        </dd>
      </div>
    </dl>
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
 * under it never moves as the fan turns: an acting project says how to stop it, an earned one says
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
        Setting for<strong>{project.project_id}</strong>
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
            placeholder="C:\\path\\to\\the\\project"
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
          the núcleo did not answer — this project&apos;s setting is unchanged
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
      {projectId === null && <Quiet says="choose a project above." />}
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
                <Quiet says="the classifier recorded no reason" />
              ) : (
                <p className="ap-reason">{decision.reason}</p>
              )}
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
      {projectId === null && <Quiet says="choose a project above." />}
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
        <div className="ap-group">
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
            </Row>
          );
        })}
      </Rows>
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
  // Which queue is holding it, not just how many items: a person reading the bare number went to
  // the proposals list and found it empty, because all of it was shadow decisions.
  const targetWaitingWhere = target === undefined ? null : whereWaiting(target);
  const blocked = target === undefined || target.queue_full || target.mode === "off";

  return (
    <Panel title="Jobs in flight" aside={<Count n={jobs.data?.length} />}>
      {jobs.isError && jobs.data === undefined && <ListError error={jobs.error} what="the jobs" />}
      {jobs.data !== undefined && live.length === 0 && <Quiet says="nothing is running." />}
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
          {target.project_id} is holding {target.open_review_items} items waiting for review
          {targetWaitingWhere === null ? "" : ` (${targetWaitingWhere})`} against its ceiling of{" "}
          {target.wip_limit ?? "none"} — review something and the brake releases itself.
        </p>
      )}
      {create.isError && <JobError error={create.error} />}
    </Panel>
  );
}

function JobRow({ job }: { job: Job }) {
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
    </Inset>
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
