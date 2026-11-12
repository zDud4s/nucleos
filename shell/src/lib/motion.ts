/**
 * The app's motion values for the `motion` library, which animates through the Web Animations API
 * and so cannot read the CSS custom properties. They mirror `--ease`, `--dur-quick`, `--dur-settle`
 * and `--dur-slide` in `tokens.css` and must change together with them. One curve, no bounce, no
 * spring (DESIGN.md, "Motion").
 */
export const EASE = [0.25, 1, 0.5, 1] as const;

/** Durations in seconds. */
export const DUR = {
  /** Feedback: a hover, a colour. */
  quick: 0.14,
  /** Something arriving and taking its place. */
  settle: 0.22,
  /** The largest gesture the app makes. */
  slide: 0.24,
} as const;

/** The default for every `motion` element, handed down by `MotionConfig` at the root. */
export const transition = { duration: DUR.settle, ease: EASE };

/**
 * How a polled row arrives: its contents fade in, the row itself does not.
 *
 * A hairline list draws its rules as a `--border` ground showing through 1px gaps, so a row that
 * fades as a whole shows that ground through it, a grey block where the row was. The row stays
 * opaque instead, and `--row-content` (read by `.ui-rows-row[data-motion] > *` in `ui.css`) fades
 * what is inside it.
 *
 * Nothing on the way out, and no sliding: a row that leaves, or one that slides up into its place,
 * uncovers that same ground for as long as the move lasts. A row that leaves goes at once.
 */
export const rowMotion = {
  initial: { "--row-content": 0 },
  animate: { "--row-content": 1 },
} as const;
