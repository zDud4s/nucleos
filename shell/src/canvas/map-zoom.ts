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
 * size of a project. And never smaller than the smallest step — past that the
 * labels stop being legible, and a picture nobody can read is the thing this
 * whole map refuses to draw. Below it, out is not the answer; the drawing has
 * to be entered instead.
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
 * `19` is the cell, from the table's own `h-[19px] w-[19px]`. `6.1` is the
 * advance of the 10px monospace the row labels are set in, and the constant is
 * the padding around them.
 */
export function matrixWidth(columns: number, longestTitle: number): number {
  return columns * 19 + longestTitle * 6.1 + 34;
}
