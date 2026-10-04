import { ChevronRight, Pause, Play } from "lucide-react";
import { createContext, useContext, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { readFeedKind, type FeedEntry } from "../data/feed";
import {
  sequenceProgressAt,
  sequenceStateAt,
  traceLanes,
  type FeedSequence,
  type SequenceState,
} from "../lib/sequences";
import { HOUR, MINUTE, clock, quietGaps, span, stamp, timeTicks } from "../lib/timeline";
import { feedGravityOf, feedMarkTone, type FeedGravity, type FeedLane } from "../ui";

/**
 * The Feed's trace: the window replayed, one row per thing the machine did.
 *
 * Each row is a sequence (`lib/sequences.ts`) — a job, a run and its attempts, or a council — under the lane it belongs to, drawn as a bar from its first line to its last with a
 * mark for every line in between. A sequence still going runs a dashed ghost to now. The column
 * on the right says how each one ended and how long it took, so the night reads down that column
 * before any bar is looked at.
 *
 * **At rest it shows now.** The playhead sits on the right edge and nothing moves until somebody
 * asks: dragging on the ruler, the rows or the rail below — or the play button, which replays the
 * whole window in about twelve seconds — moves it back, and everything after it goes quiet: marks
 * not yet written vanish, bars fill only to the playhead, rows that had not started are muted and
 * rows between their first and last line say "in progress". It never narrows the list under it;
 * the replay is a way to read the window, not a filter on it.
 *
 * **Positions are percentages of the track.** The bars and marks are SVG with percentage
 * coordinates, so the drawing never waits on a measurement; only the ruler's tick density and the
 * width a silence needs before it is labelled read the track's pixel width. The playhead and the
 * quiet stretches are positioned through `style`, as the calendar's now-line is — the CSSOM, which
 * the production CSP allows, and never a style attribute in markup.
 */

/**
 * The mark under the pointer, and where on screen it sits — what the hover card reads.
 *
 * A context rather than a prop, because marks are drawn from three places (a sequence's row, a
 * collapsed lane, a folded fold) and all of them report to the one card the trace owns.
 */
interface MarkTip {
  line: FeedEntry;
  x: number;
  y: number;
}
const MarkHover = createContext<(tip: MarkTip | null) => void>(() => {});

/** Roughly how long a replay of the whole window takes, whatever its length. */
const REPLAY_MS = 12_000;
/** The width a silence needs before its duration is written into it. */
const QUIET_LABEL_PX = 96;
/** jsdom has no layout; a track measured at zero is drawn at a plausible width instead. */
const TRACK_FALLBACK = 720;
/** The rail's knob is inset by its radius, so the ends of the rail are the ends of time. */
const KNOB_INSET = 7;

export interface FeedTraceProps {
  sequences: FeedSequence[];
  /** The window, in epoch ms. `now` is its right edge. */
  start: number;
  now: number;
  /** The window's name, as the verdict says it. */
  title: string;
  /** The playhead, or `null` at rest on now. */
  at: number | null;
  onAt: (at: number | null) => void;
  selected: string | null;
  onSelect: (key: string | null) => void;
  /** A tally pressed in the verdict: rows without a line of this gravity are dimmed. */
  filter: FeedGravity | null;
}

export function FeedTrace({ sequences, start, now, title, at, onAt, selected, onSelect, filter }: FeedTraceProps) {
  const titleId = useId();
  const trackRef = useRef<HTMLDivElement>(null);
  const measured = useWidth(trackRef);
  const width = measured === 0 ? TRACK_FALLBACK : measured;

  const extent = Math.max(now - start, MINUTE);
  const time = at === null ? now : Math.min(Math.max(at, start), now);
  const atNow = at === null;
  const pct = (ms: number) => `${(Math.min(Math.max((ms - start) / extent, 0), 1) * 100).toFixed(3)}%`;

  const lanes = traceLanes(sequences);
  const [collapsed, setCollapsed] = useState<ReadonlySet<FeedLane>>(new Set());
  const [unfolded, setUnfolded] = useState<ReadonlySet<FeedLane>>(new Set());
  const toggle = (set: ReadonlySet<FeedLane>, lane: FeedLane) => {
    const next = new Set(set);
    if (next.has(lane)) next.delete(lane);
    else next.add(lane);
    return next;
  };

  const running = sequences.filter((sequence) => sequenceStateAt(sequence, time, now) === "running").length;
  const chip = atNow
    ? `now · ${running === 0 ? "nothing" : running} still open`
    : `${stamp(time, now)} · ${running === 0 ? "nothing" : running} in progress`;

  const ticks = timeTicks(start, now, width);
  const gaps = quietGaps(sequences.flatMap((sequence) => sequence.lines.map((line) => Date.parse(line.created_at))));

  /* -------------------------------------------------------------- replay -- */

  const [playing, setPlaying] = useState(false);
  const [tip, setTip] = useState<MarkTip | null>(null);
  const frame = useRef(0);
  const timeRef = useRef(time);
  timeRef.current = time;

  const stop = () => {
    window.cancelAnimationFrame(frame.current);
    frame.current = 0;
    setPlaying(false);
  };
  useEffect(() => () => window.cancelAnimationFrame(frame.current), []);

  function play() {
    if (playing) {
      stop();
      return;
    }
    // Reduced motion gets the answer the replay arrives at, without the journey.
    if (typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      onAt(null);
      return;
    }
    let t = atNow ? start : timeRef.current;
    let last = 0;
    const step = (stampMs: number) => {
      const dt = last === 0 ? 0 : Math.min(stampMs - last, 100);
      last = stampMs;
      t += (dt / REPLAY_MS) * extent;
      if (t >= now) {
        onAt(null);
        stop();
        return;
      }
      onAt(t);
      frame.current = window.requestAnimationFrame(step);
    };
    onAt(t);
    setPlaying(true);
    frame.current = window.requestAnimationFrame(step);
  }

  /** Move the playhead; the right edge is now, and now is rest. */
  const seek = (ms: number) => onAt(ms >= now ? null : Math.max(ms, start));

  /* ------------------------------------------------------------ scrubbing -- */

  function scrubbing(inset: number, box: () => HTMLElement | null) {
    const from = (event: PointerEvent<HTMLElement>) => {
      const element = box();
      if (element === null) return;
      const rect = element.getBoundingClientRect();
      const inner = rect.width - inset * 2;
      if (inner <= 0) return;
      seek(start + ((event.clientX - rect.left - inset) / inner) * extent);
    };
    return {
      onPointerDown: (event: PointerEvent<HTMLElement>) => {
        if (event.button !== 0) return;
        if (playing) stop();
        try {
          event.currentTarget.setPointerCapture?.(event.pointerId);
        } catch {
          /* a pointer the browser does not consider active still scrubs inside the element */
        }
        from(event);
      },
      onPointerMove: (event: PointerEvent<HTMLElement>) => {
        if (event.currentTarget.hasPointerCapture?.(event.pointerId)) from(event);
      },
    };
  }
  const onTrack = scrubbing(0, () => trackRef.current);
  const sliderRef = useRef<HTMLDivElement>(null);
  const onRail = scrubbing(KNOB_INSET, () => sliderRef.current);

  function onSliderKey(event: KeyboardEvent<HTMLDivElement>) {
    // A night moves in ten minutes and hours; a week in hours and quarter-days.
    const long = extent > 36 * HOUR;
    const small = long ? HOUR : 10 * MINUTE;
    const large = long ? 6 * HOUR : HOUR;
    const delta: Record<string, number> = { ArrowRight: small, ArrowUp: small, ArrowLeft: -small, ArrowDown: -small, PageUp: large, PageDown: -large };
    if (event.key === "Home") seek(start);
    else if (event.key === "End") seek(now);
    else if (delta[event.key] !== undefined) seek(time + delta[event.key]);
    else return;
    event.preventDefault();
    if (playing) stop();
  }

  /* --------------------------------------------------------------- paint -- */

  const dimmed = (group: FeedSequence[]) =>
    filter !== null && !group.some((sequence) => sequence.lines.some((line) => feedGravityOf(line.kind) === filter));

  const rows: ReactNode[] = [];
  for (const lane of lanes) {
    const open = !collapsed.has(lane.lane);
    rows.push(
      <li key={lane.lane} className="feed-trace-row feed-trace-lane" data-dim={dimmed(lane.sequences) || undefined}>
        <h3 className="feed-trace-lane-head">
          <button
            type="button"
            className="feed-trace-label"
            aria-expanded={open}
            aria-label={`${lane.label}, ${lane.lines} ${lane.lines === 1 ? "line" : "lines"}`}
            onClick={() => setCollapsed(toggle(collapsed, lane.lane))}
          >
            <ChevronRight className="feed-trace-chevron" size={14} aria-hidden="true" />
            <span className="feed-trace-lane-name">{lane.label}</span>
            <span className="feed-trace-lane-n">{lane.lines}</span>
          </button>
        </h3>
        <Bars>{open ? null : <Marks sequences={lane.sequences} time={time} pct={pct} quiet />}</Bars>
        <span className="feed-trace-meta" />
      </li>,
    );
    if (!open) continue;

    const unfold = unfolded.has(lane.lane);
    const drawn = unfold ? lane.sequences : lane.shown;
    for (const sequence of drawn) {
      rows.push(
        <SequenceRow
          key={sequence.key}
          sequence={sequence}
          time={time}
          now={now}
          pct={pct}
          selected={selected === sequence.key}
          dim={dimmed([sequence])}
          onSelect={() => onSelect(selected === sequence.key ? null : sequence.key)}
        />,
      );
    }
    if (lane.folded.length > 0) {
      const n = lane.folded.length;
      rows.push(
        <li key={`${lane.lane}-fold`} className="feed-trace-row feed-trace-seq feed-trace-fold" data-dim={dimmed(lane.folded) || undefined}>
          <button
            type="button"
            className="feed-trace-label"
            aria-expanded={unfold}
            aria-label={`${n} routine ${n === 1 ? "sequence" : "sequences"} in ${lane.label}`}
            onClick={() => setUnfolded(toggle(unfolded, lane.lane))}
          >
            <ChevronRight className="feed-trace-chevron" size={14} aria-hidden="true" />
            <span className="feed-trace-name">{unfold ? `hide ${n} routine` : `${n} routine`}</span>
          </button>
          <Bars>{unfold ? null : <Marks sequences={lane.folded} time={time} pct={pct} quiet />}</Bars>
          <span className="feed-trace-meta">
            <span className="feed-trace-dur">{lane.folded.reduce((sum, sequence) => sum + sequence.lines.length, 0)} lines</span>
          </span>
        </li>,
      );
    }
  }

  return (
    <MarkHover.Provider value={setTip}>
    <section className="feed-trace" aria-labelledby={titleId}>
      <div className="feed-trace-frame">
        <div className="feed-trace-head">
          <span className="feed-trace-live" data-now={atNow || undefined} aria-hidden="true" />
          <h2 className="feed-trace-title" id={titleId}>
            {title}
          </h2>
          <span className="feed-trace-span">
            {stamp(start, now)} → {stamp(now, now)} · {span(now - start)}
          </span>
          <span className="feed-trace-chip">{chip}</span>
        </div>

        <div className="feed-trace-body">
          <div className="feed-trace-ruler" aria-hidden="true">
            <div className="feed-trace-track" ref={trackRef} {...onTrack}>
              {ticks.map((tick) => (
                <span
                  key={tick.at}
                  className={tick.midnight ? "feed-trace-tick feed-trace-tick-day" : "feed-trace-tick"}
                  style={{ left: pct(tick.at) }}
                >
                  {tick.label}
                </span>
              ))}
            </div>
          </div>

          <div className="feed-trace-track feed-trace-grid" aria-hidden="true">
            {ticks.map((tick) => (
              <span
                key={tick.at}
                className={tick.midnight ? "feed-trace-rule feed-trace-rule-day" : "feed-trace-rule"}
                style={{ left: pct(tick.at) }}
              />
            ))}
            {gaps.map((gap) => {
              const px = ((gap.to - gap.from) / extent) * width;
              return (
                <span
                  key={gap.from}
                  className="feed-trace-quiet"
                  style={{ left: pct(gap.from), width: `${(((gap.to - gap.from) / extent) * 100).toFixed(3)}%` }}
                >
                  {px >= QUIET_LABEL_PX && <em className="feed-trace-quiet-label">quiet {span(gap.to - gap.from)}</em>}
                </span>
              );
            })}
          </div>

          <ol className="feed-trace-rows" aria-label={`Sequences, ${title.toLowerCase()}`}>
            {rows}
          </ol>

          <div className="feed-trace-track feed-trace-playhead" aria-hidden="true">
            <span className="feed-trace-playhead-line" style={{ left: pct(time) }} />
          </div>
          <div className="feed-trace-track feed-trace-scrub" aria-hidden="true" {...onTrack} />
        </div>

        <div className="feed-trace-transport">
          <button
            type="button"
            className="feed-trace-play"
            aria-label={playing ? "Pause the replay" : "Replay this window"}
            onClick={play}
          >
            {playing ? <Pause size={14} aria-hidden="true" /> : <Play size={14} aria-hidden="true" />}
          </button>
          <div
            ref={sliderRef}
            className="feed-trace-slider"
            role="slider"
            tabIndex={0}
            aria-label="Replay time"
            aria-valuemin={0}
            aria-valuemax={Math.round(extent / MINUTE)}
            aria-valuenow={Math.round((time - start) / MINUTE)}
            aria-valuetext={atNow ? `${stamp(now, now)}, now` : stamp(time, now)}
            onKeyDown={onSliderKey}
            {...onRail}
          >
            <span className="feed-trace-rail">
              <span className="feed-trace-rail-fill" style={{ width: pct(time) }} />
            </span>
            <span className="feed-trace-knob-track">
              <span className="feed-trace-knob" style={{ left: pct(time) }} />
            </span>
          </div>
          <span className="feed-trace-clock">
            {stamp(time, now)} / {clock(now)}
          </span>
          <button
            type="button"
            className="feed-trace-now"
            disabled={atNow}
            onClick={() => {
              if (playing) stop();
              onAt(null);
            }}
          >
            Now
          </button>
        </div>
      </div>
    </section>
    {tip !== null && <MarkCard tip={tip} now={now} />}
    </MarkHover.Provider>
  );
}

/**
 * What one mark says, on hover: the kind in the map's words, when, whose, and the line itself.
 *
 * Fixed to the viewport through a portal, because the rows clip and the card must not. It is a
 * reading aid and nothing more — `aria-hidden`, like the marks it explains, since every line it
 * shows is also in the list below the trace.
 */
function MarkCard({ tip, now }: { tip: MarkTip; now: number }) {
  const { line } = tip;
  const tone = feedMarkTone(line.kind);
  const label = readFeedKind(line.kind)?.label ?? line.kind;
  const half = 160;
  const left = Math.min(Math.max(tip.x, half + 8), Math.max(window.innerWidth - half - 8, half + 8));
  const below = tip.y < 140;
  return createPortal(
    <div className="feed-mark-card" data-below={below || undefined} aria-hidden="true" style={{ left, top: below ? tip.y + 12 : tip.y - 12 }}>
      <div className="feed-mark-card-head">
        <svg className="feed-mark-card-dot" viewBox="0 0 8 8">
          <circle className={`ui-mark-${tone}`} cx="4" cy="4" r="4" />
        </svg>
        <span className="feed-mark-card-kind">{label}</span>
        <span className="feed-mark-card-at">{stamp(Date.parse(line.created_at), now)}</span>
      </div>
      <p className="feed-mark-card-summary">{line.summary}</p>
      {(line.project_id !== null || line.subject !== null || line.run_id !== null) && (
        <div className="feed-mark-card-foot">
          {[line.project_id, line.subject, line.run_id === null || line.subject === `run:${line.run_id}` ? null : `run:${line.run_id}`]
            .filter((part): part is string => part !== null && part !== "")
            .join(" · ")}
        </div>
      )}
    </div>,
    document.body,
  );
}

/* ------------------------------------------------------------------- rows -- */

function SequenceRow({
  sequence,
  time,
  now,
  pct,
  selected,
  dim,
  onSelect,
}: {
  sequence: FeedSequence;
  time: number;
  now: number;
  pct: (ms: number) => string;
  selected: boolean;
  dim: boolean;
  onSelect: () => void;
}) {
  const state: SequenceState = sequenceStateAt(sequence, time, now);
  // Before its newest line a sequence has not said how it ends yet.
  const series = sequence.family === "series";
  const heard = state === "queued" ? "" : series || time >= sequence.last ? sequence.says : "in progress";
  const says = state === "queued" ? "" : series || time >= sequence.last ? sequence.ending : "in progress";
  const status = state === "queued" ? "not started yet" : heard;
  const end = sequence.open ? now : sequence.last;
  // A series is marks only: nothing ran between its lines.
  const bar = !series && (sequence.open || sequence.lines.length > 1);
  const progress = sequenceProgressAt(sequence, time, now);
  const barWidth = Math.max(end - sequence.start, 0);

  const name = [sequence.name, series ? `${sequence.lines.length} lines` : null, sequence.owner, sequence.says, sequence.attempts === null ? null : `attempt ${sequence.attempts}`, sequence.open ? "still open" : null]
    .filter((part): part is string => part !== null)
    .join(", ");

  return (
    <li
      className={`feed-trace-row feed-trace-seq feed-trace-shade-${sequence.shade}`}
      data-state={state}
      data-selected={selected || undefined}
      data-dim={dim || undefined}
      data-open={sequence.open || undefined}
    >
      <button type="button" className="feed-trace-label" aria-pressed={selected} aria-label={name} onClick={onSelect}>
        <span className="feed-trace-name">{sequence.name}</span>
        {sequence.attempts !== null && <span className="feed-trace-retry">×{sequence.attempts}</span>}
        {sequence.owner !== null && <span className="feed-trace-owner">{sequence.owner}</span>}
      </button>
      <span className="sr-only">{status}</span>
      <Bars>
        {bar && <BarShape open={sequence.open} x={pct(sequence.start)} end={pct(end)} filled={pct(sequence.start + barWidth * progress)} />}
        <Marks sequences={[sequence]} time={time} pct={pct} />
      </Bars>
      <span className="feed-trace-meta" aria-hidden="true">
        <span className="feed-trace-says">{says}</span>
        <span className="feed-trace-dur">{duration(sequence, now)}</span>
      </span>
    </li>
  );
}

/** The row's drawing surface: the track's width, the row's height. */
function Bars({ children }: { children: ReactNode }) {
  return (
    <svg className="feed-trace-bars" aria-hidden="true">
      {children}
    </svg>
  );
}

/**
 * A sequence's bar, from its first line to its last — or, still open, a dashed ghost to now —
 * filled up to the playhead. Percentages in, percentages drawn: `x`, `end` and `filled` are
 * points on the track.
 */
function BarShape({ open, x, end, filled }: { open: boolean; x: string; end: string; filled: string }) {
  const between = (from: string, to: string) => `${Math.max(parseFloat(to) - parseFloat(from), 0).toFixed(3)}%`;
  const fill = between(x, filled);
  return (
    <>
      {open ? (
        <line className="feed-trace-ghost-open" x1={x} x2={end} y1="50%" y2="50%" />
      ) : (
        <rect className="feed-trace-ghost" x={x} y="50%" width={between(x, end)} height={8} rx={4} transform="translate(0 -4)" />
      )}
      {parseFloat(fill) > 0 && <rect className="feed-trace-fill" x={x} y="50%" width={fill} height={8} rx={4} transform="translate(0 -4)" />}
    </>
  );
}

/** A mark per line — solid in its tone when it was an exception, a hollow ring when it was routine. */
function Marks({
  sequences,
  time,
  pct,
  quiet = false,
}: {
  sequences: FeedSequence[];
  time: number;
  pct: (ms: number) => string;
  quiet?: boolean;
}) {
  const hover = useContext(MarkHover);
  const lines = sequences.flatMap((sequence) => sequence.lines);
  // Routine first, so an exception in the same place is drawn over it.
  const ordered = [...lines].sort((a, b) => Number(feedGravityOf(a.kind) !== "routine") - Number(feedGravityOf(b.kind) !== "routine"));
  return (
    <>
      {ordered.map((line) => {
        const at = Date.parse(line.created_at);
        const exception = feedGravityOf(line.kind) !== "routine";
        const tone = feedMarkTone(line.kind);
        const classes = exception
          ? [`ui-mark-${tone}`, "ui-mark-exception"]
          : tone === "shadow"
            ? ["ui-mark-shadow", "ui-mark-routine"]
            : ["feed-trace-ring"];
        if (at > time) classes.push("feed-trace-later");
        return (
          <circle
            key={line.id}
            className={classes.join(" ")}
            cx={pct(at)}
            cy="50%"
            r={exception ? (quiet ? 4.5 : 5.5) : quiet ? 3 : 4}
            onPointerEnter={(event) => {
              const box = event.currentTarget.getBoundingClientRect();
              hover({ line, x: box.left + box.width / 2, y: box.top });
            }}
            onPointerLeave={() => hover(null)}
          />
        );
      })}
    </>
  );
}

/** `44 min` for something that took time, `at 23:48` for a single moment, `8 lines` for a series. */
export function duration(sequence: FeedSequence, now: number): string {
  if (sequence.family === "series") return `${sequence.lines.length} lines`;
  if (!sequence.open && sequence.lines.length === 1) return `at ${clock(sequence.start)}`;
  return span((sequence.open ? now : sequence.last) - sequence.start);
}

/** The track's own width, followed as the window or the rail changes it. */
function useWidth(ref: { current: HTMLElement | null }): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null) return;
    const measure = () => setWidth(element.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}
