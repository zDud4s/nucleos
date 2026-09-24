// §spec mapa-do-projeto

/**
 * How far out a drawing has to stand to be seen at all.
 *
 * The map's pictures are the one part of this app whose size is decided by the
 * project rather than by the design: 64 communities is a matrix 1,350px wide,
 * and a layered community can be wider still. Drawn at their own size inside a
 * column they overflow in both directions, and what somebody gets is a window
 * onto a corner of an answer — the whole complaint that produced this module,
 * in the owner's words: *"o mapa que lá está está demasiado grande para
 * conseguir observar, precisa de estar mais zoomed out"*.
 *
 * **Pure, and separate from the component, because the choice is arithmetic and
 * the component is pixels.** Which step fits is a function of two numbers and
 * has nothing to do with React, so it is tested as arithmetic — and a starting
 * zoom that quietly stopped fitting would otherwise be invisible until somebody
 * opened the page with a big enough project.
 */

/**
 * The steps, and why they are a list rather than a slider.
 *
 * A slider gives 137%, which nobody wants and which makes two screenshots of
 * the same drawing incomparable. These are the round factors a reader actually
 * reaches for, and `fit` lands on one of them, so "what am I looking at" always
 * has a short answer.
 */
export const ZOOM_STEPS = [0.25, 0.33, 0.5, 0.67, 0.8, 1, 1.25, 1.5] as const;

/** Full size, and the value everything starts at when nothing has to shrink. */
export const NO_ZOOM = 1;

/**
 * The width to assume before anything has been measured.
 *
 * The stage measures its own box and re-fits, but the first render happens
 * before a layout exists — and under jsdom no layout ever does, so a test that
 * asked what the fit was would otherwise be asking about a box of zero width
 * and would get the smallest step every time. This is the main column at the
 * app's own 1440 desktop, which is where the pictures are actually looked at.
 */
export const ASSUMED_ROOM = 1040;

/**
 * The largest step at which `natural` fits inside `room`.
 *
 * Never larger than 1: a small drawing blown up to fill the frame is a bigger
 * lie than one that leaves space, because it makes eleven boxes look like the
 * size of a project. And never smaller than the smallest step, because below
 * that standing further out stops being an answer at all; the drawing has to
 * be entered instead.
 *
 * **The clamp is silent, and that was the hole.** This used to say the floor
 * was also where legibility ended — that at the steps above it the labels
 * still read. They do not: `text-xs` is 12px, so the third step renders it at
 * 6px, and 6px is what the 64-community matrix opens at on a 1440 desktop.
 * That claim was never measured and nothing checked it. {@link unreadableAt}
 * is the check, and it answers about the step this returns.
 */
export function fitZoom(natural: number, room: number): number {
  if (natural <= 0 || room <= 0) return NO_ZOOM;
  const wanted = room / natural;
  let best: number = ZOOM_STEPS[0];
  for (const step of ZOOM_STEPS) {
    if (step <= wanted && step <= NO_ZOOM) best = step;
  }
  return wanted >= NO_ZOOM ? NO_ZOOM : best;
}

/** One step out, or one step in, stopping at the ends rather than wrapping. */
export function zoomBy(current: number, direction: 1 | -1): number {
  const at = ZOOM_STEPS.indexOf(current as (typeof ZOOM_STEPS)[number]);
  // A value the steps do not hold — nothing produces one today, and a caller
  // that did would otherwise get `ZOOM_STEPS[-1 + 1]`, silently 0.25.
  if (at === -1) return current;
  const next = at + direction;
  if (next < 0 || next >= ZOOM_STEPS.length) return current;
  return ZOOM_STEPS[next];
}

/** How a factor is said out loud: `80%`, never `0.8` and never `80.00000001%`. */
export function zoomLabel(zoom: number): string {
  return `${Math.round(zoom * 100)}%`;
}

/**
 * How wide the community matrix wants to be, before anything shrinks it.
 *
 * Estimated rather than measured, and the estimate is what `fit` is allowed to
 * be wrong about: it decides the step a reader opens at, and both controls are
 * right there if it opened one step off. Measuring would mean rendering the
 * table at full size first — which is the flash of an unreadable picture this
 * exists to avoid.
 *
 * **An estimate that runs short is a scrollbar at `fit`**, and that is how
 * this was found: it used `6.1`px a character for labels actually set at 12px
 * (`text-xs`), counted the title without the file count printed after it, and
 * left out the cells' collapsed borders. The margin that hid it went the day
 * titles grew a side prefix (`shell·council`), and the matrix opened at `fit`
 * with a horizontal scrollbar. So every term is now the drawing's own:
 *
 * - `CELL`: the `w-[19px]` cell plus its share of the collapsed 1px border.
 * - `LABEL_ADVANCE`: 0.6em of the 12px monospace, rounded up — a monospace
 *   advance is a fact of the face, and rounding up is the side of the error
 *   that costs a step of zoom rather than a scrollbar.
 * - `longestLabel` is the whole row header in characters — title, space and
 *   count — which the caller measures, because only it knows what it prints.
 * - `FRAME`: the table's `m-3` either side, the header's `px-1`, and a margin.
 */
const CELL = 20;
/** The advance of one character of the 12px monospace the labels are set in. */
export const LABEL_ADVANCE = 7.5;
const FRAME = 24 + 8 + 12;

export function matrixWidth(columns: number, longestLabel: number): number {
  return columns * CELL + longestLabel * LABEL_ADVANCE + FRAME;
}

/**
 * The type a drawing's smallest labels are set in: Tailwind's `text-xs`.
 *
 * The matrix sets three things in it — the cell numbers, the row rail and the
 * rotated column rail — so one number covers everything a reader has to read
 * off that picture.
 */
export const SMALLEST_TYPE = 12;

/**
 * Rendered pixels below which a digit stops being a digit.
 *
 * A floor taken from the common bound on legible interface type rather than
 * measured here, and said out loud for that reason. What IS measured is the
 * case it was written for: the matrix of 64 communities is 1,354px wide, the
 * stage on a 1440 desktop is 883px, so {@link fitZoom} opens it at `0.5` and
 * sets every number in it at 6px. Against the steps it lands in a clean place
 * — `0.8` and up read, `0.67` and below do not.
 */
export const LEGIBLE_TYPE = 9;

/**
 * What a drawing stops saying at this zoom, in the reader's words. Empty means
 * it still says everything.
 *
 * **Deliberately not a refusal, which is what separates it from
 * `reasonsNotToDraw`.** That one judges a layered graph, whose failure is
 * crossings: an unreadable one is unreadable at every size, so the honest
 * answer is not to draw it at all. A matrix fails the other way. It carries two
 * readings on one picture — the shape, which is where the marks fall either
 * side of the diagonal, and the detail, which is the numbers inside them — and
 * shrinking takes the second while leaving the first. Refusing the whole
 * picture to protect the half that broke would throw away the half that works,
 * and the shape is the reading the matrix was chosen over a node-link drawing
 * to give in the first place.
 *
 * So this names what is gone rather than hiding what is left. The principle
 * around it is unchanged and is why it exists: a picture nobody can read is
 * worse than a sentence saying why, because the picture still looks like an
 * answer. A picture read for more than it is still offering is the same fault
 * one step quieter, and it had nothing watching for it.
 */
export function unreadableAt(zoom: number): string[] {
  const type = SMALLEST_TYPE * zoom;
  if (type >= LEGIBLE_TYPE) return [];
  return [
    `numbers and labels set at ${Math.round(type)}px, under the ${LEGIBLE_TYPE}px this reader can still make out`,
  ];
}
