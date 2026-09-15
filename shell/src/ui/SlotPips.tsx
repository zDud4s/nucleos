import type { StateBadgeProps } from "./StateBadge";
import { readState } from "./state-map";

/**
 * Up to this many slots, every slot is a pip. Past it the free ones are not drawn: a project
 * allowed twenty at once would be a row of twenty hollow rings, which reads as a fence rather than
 * as room and pushes whatever sits beside the row off its line. So past eight only what is held is
 * drawn, and the count beside it (`12/20`) says out of how many.
 */
const EVERY_PIP_UP_TO = 8;

export interface SlotPipsProps {
  /**
   * One per held slot, in slot order: the domain and the literal that slot's own badge is handed.
   * The same shape as `StateBadge`'s props on purpose, so a pip and the badge on its slot's card
   * cannot wear two tones for one state.
   */
  held: StateBadgeProps[];
  /** How many slots the project may hold at once. */
  limit: number;
}

/**
 * A project's slots as a row of lamps: lit for each slot held, hollow for each one free.
 *
 * The tone comes through `readState`, the one map, and never from the caller — a caller hands over
 * what it would hand a `StateBadge`, and the pip lights in the tone that badge would wear. A literal
 * the map has no reading for lights in Switched Off Grey, as `StateBadge` draws it: the tone with
 * the least claim in it.
 *
 * **The drawing is hidden from assistive tech, and a sentence stands in for it.** Colour is all a
 * pip has, so colour cannot be the only thing that says it: "2 of 3 slots held (implementing,
 * awaiting reconciliation), room for 1" is the same reading in words, each held slot named by its
 * badge's label. The labels are the pips' hover text too.
 */
export function SlotPips({ held, limit }: SlotPipsProps) {
  const lamps = held.map(({ domain, state }) => {
    const reading = readState(domain, state);
    return { tone: reading?.tone ?? "off", label: reading?.label ?? (state ?? "").trim() };
  });
  const room = Math.max(0, limit - held.length);
  const every = limit <= EVERY_PIP_UP_TO;
  const named = lamps.map((lamp) => lamp.label).filter((label) => label !== "");
  const sentence =
    `${held.length} of ${limit} slots held` +
    (named.length > 0 ? ` (${named.join(", ")})` : "") +
    (room > 0 ? `, room for ${room}` : "");

  return (
    <span className="ui-pips">
      <span className="ui-pips-row" aria-hidden="true">
        {lamps.map((lamp, index) => (
          <span
            key={index}
            className={`ui-pip ui-pip-${lamp.tone}`}
            title={lamp.label === "" ? undefined : lamp.label}
          />
        ))}
        {every &&
          Array.from({ length: room }, (_, index) => (
            <span key={`free-${index}`} className="ui-pip ui-pip-free" />
          ))}
        {!every && (
          <span className="ui-pips-count">
            {held.length}/{limit}
          </span>
        )}
      </span>
      <span className="sr-only">{sentence}</span>
    </span>
  );
}
