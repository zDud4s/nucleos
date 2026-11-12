import type { CSSProperties, ReactNode } from "react";
import type { StateBadgeProps } from "./StateBadge";
import { readState } from "./state-map";

/** One track of the ring: the outermost is drawn first. */
export interface RingTrack extends StateBadgeProps {
  /** What this track measures, in the words the sentence uses — `7d`, `5h`. */
  name: string;
  /** In [0,1]. Clamped here, because an arc is one of the few places a bad number still draws. */
  used: number;
  /**
   * Whether the figure is real.
   *
   * `false` draws the track dashed and empty — the convention `.ui-runpipe-unreached` already
   * carries in this app, where a dash says *this did not happen* rather than borrowing a state's
   * colour to say it. It is the difference between a quota that is untouched and a quota nobody
   * could read, and the two must never be one picture.
   */
  measured: boolean;
}

export interface RingProps {
  /** Whose rings these are. Names the reading; not drawn inside the ring. */
  label: string;
  /** Outermost first. Two is what this design draws; the component does not care how many. */
  tracks: RingTrack[];
  /** Outer diameter in pixels. */
  size?: number;
  /**
   * Drawn at the centre, inside the innermost track.
   *
   * A slot and not a `provider` argument, so this primitive goes on knowing nothing about quota:
   * it draws arcs, and it draws whatever it was handed in the hole they leave. `QuotaNotch` is the
   * only thing that knows a provider has a mark, which is where that knowledge belongs — the same
   * division `tracks` already has, where the caller decides what a track means and the ring only
   * decides where it sits.
   *
   * Absent, the middle stays empty. It carries `pointer-events: none` in the stylesheet, so
   * whatever goes in there cannot take the hover the arcs' own `<title>` elements answer.
   */
  mark?: ReactNode;
}

/**
 * The default outer diameter: what fits legibly in a notch beside a second provider.
 *
 * Forty-four and not thirty-four, and what the extra ten pixels buy is the hole rather than the
 * ring. Two tracks cost a fixed `2 * STROKE + GAP` of radius whatever the diameter, so the clear
 * middle is what a small ring runs out of first: at 34 it was 15px across, and a mark with any air
 * round it had nowhere to be. At 44 it is 22px. `.app-main-notched` in `app.css` is measured off
 * this number and has to follow it.
 */
const SIZE = 44;
/** Track thickness, and the gap between two tracks. Both in pixels, at `SIZE`. */
const STROKE = 4;
const GAP = 3;

/**
 * A reading drawn as concentric arcs — the outer track the long window, the inner track the short
 * one.
 *
 * **The ring never chooses its own colour.** A track carries the same `{domain, state}` a
 * `StateBadge` would be handed, and the tone comes back from `readState` — the one map — so a ring
 * and the badge on the same fact cannot disagree. A state the map has no reading for draws in
 * Switched Off Grey, which is the tone with the least claim in it.
 *
 * **The drawing is hidden from assistive tech and a sentence stands in for it**, the pattern
 * `SlotPips` uses and a deliberate choice rather than an imitation: two of the three comparable
 * primitives put `role="img"` and an `aria-label` on the track itself, which suits a single bar.
 * This is a composite of several arcs whose meaning is the combination, so one sentence over the
 * whole thing — "claude: 7d 46% within the window, 5h 56% within the window" — is the reading, and
 * each arc's own title is the hover text.
 *
 * Percentages are rounded for the sentence only. The arc is drawn from the fraction, so a ring at
 * 99.6% is not drawn full — being near the cap and being at it are different facts, and this is the
 * second place that distinction has to survive.
 */
export function Ring({ label, tracks, size = SIZE, mark }: RingProps) {
  const drawn = tracks.map((track, index) => {
    const reading = readState(track.domain, track.state);
    const used = Math.min(1, Math.max(0, track.used));
    // Each track sits one stroke and one gap inside the last, so the outermost is the first given.
    const radius = size / 2 - STROKE / 2 - index * (STROKE + GAP);
    const circumference = 2 * Math.PI * radius;
    return {
      key: `${track.name}-${index}`,
      name: track.name,
      tone: reading?.tone ?? "off",
      // The literal itself when the map has no reading for it — admitting ignorance, the
      // rule `StateBadge` follows. An absent state has nothing to admit and says nothing.
      label: reading?.label ?? (track.state ?? "").trim(),
      measured: track.measured,
      percent: Math.round(used * 100),
      radius,
      circumference,
      // The arc's length, as the dash pattern of a circle: drawn portion, then the rest as gap.
      dash: `${circumference * used} ${circumference}`,
    };
  });

  const sentence = `${label}: ${drawn
    .map((track) =>
      track.measured ? `${track.name} ${track.percent}% ${track.label}` : `${track.name} ${track.label}`,
    )
    .join(", ")}`;

  return (
    <span className="ui-ring">
      <svg
        className="ui-ring-face"
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        aria-hidden="true"
      >
        {drawn.map((track) => (
          <g key={track.key}>
            <title>{`${track.name} — ${track.label}`}</title>
            <circle
              className={`ui-ring-groove${track.measured ? "" : " ui-ring-unmeasured"}`}
              cx={size / 2}
              cy={size / 2}
              r={track.radius}
              strokeWidth={STROKE}
            />
            {track.measured && (
              <circle
                className={`ui-ring-arc ui-ring-${track.tone}`}
                cx={size / 2}
                cy={size / 2}
                r={track.radius}
                strokeWidth={STROKE}
                strokeDasharray={track.dash}
                // What the arc sweeps out from (`.ui-ring-arc`): an empty dash the length of the
                // whole track, so the first frame draws nothing rather than a full ring.
                style={{ "--ui-ring-length": `${track.circumference}` } as CSSProperties}
                // Start at twelve o'clock and run clockwise. Without this an arc begins at three
                // and reads as a dial nobody set.
                transform={`rotate(-90 ${size / 2} ${size / 2})`}
              />
            )}
          </g>
        ))}
      </svg>
      {mark !== undefined && <span className="ui-ring-mark">{mark}</span>}
      <span className="sr-only">{sentence}</span>
    </span>
  );
}
