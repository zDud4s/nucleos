export interface CountProps {
  /** How many. `undefined` while the list that would be counted has not answered yet. */
  n: number | undefined;
}

/**
 * How many things are in the list below, in a heading's corner.
 *
 * Text and never a pill. A count is a reading, not a state: put it in a filled
 * capsule and it lands in the same shape as a state badge, a granted power and a
 * specialist's name, and a page with all four becomes a row of tokens that have
 * to be read one at a time before any of them can be told apart. `ui.css`
 * reserves the filled capsule for `.ui-badge` alone.
 *
 * **Nothing at all before the first answer.** A count with no list behind it is
 * not zero and is not a dash — it is a question nobody has asked yet, and both
 * of the other renderings are a claim about a list that has not come back. The
 * heading stands on its own for the second it takes.
 */
export function Count({ n }: CountProps) {
  if (n === undefined) return null;
  return <span className="ui-count">{n}</span>;
}
