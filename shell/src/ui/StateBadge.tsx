import { Badge } from "./Badge";
import { readState, type StateDomain } from "./state-map";

export interface StateBadgeProps {
  domain: StateDomain;
  /** The literal the núcleo sent. `null` is a value with its own meaning per domain, not a gap. */
  state: string | null | undefined;
}

/**
 * A domain state, rendered through the one non-collapsing map.
 *
 * Pages never pick a tone for a status themselves — they name the domain and
 * hand over the literal. That is what keeps the fourteen §7 distinctions from
 * being re-decided, differently, on every page that happens to show a run.
 *
 * A state the map has no reading for is shown *as itself* in Switched Off Grey,
 * the tone with the least claim in it, with the ignorance stated in the tooltip. The alternative
 * — falling back to a plausible-looking tone — would be the app asserting a
 * meaning it does not have, which is the precise failure §7 exists to prevent.
 * An absent state with no per-domain reading renders nothing: there is no fact
 * to show, and an empty badge is not a fact.
 */
export function StateBadge({ domain, state }: StateBadgeProps) {
  const reading = readState(domain, state);
  if (reading !== null) {
    return (
      <Badge tone={reading.tone} className={reading.provisional === true ? "ui-state-awaited" : undefined}>
        {reading.label}
      </Badge>
    );
  }

  const literal = state === null || state === undefined ? "" : state.trim();
  if (literal === "") return null;

  return (
    <Badge tone="off" className="ui-state-unmapped" title={`this shell has no reading for ${domain} state "${literal}"`}>
      {literal}
    </Badge>
  );
}
