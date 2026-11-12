import type { ReactNode } from "react";
import { Button } from "./Button";

export interface FilterBarProps {
  /** Names the group for a screen reader: "Filter runs". */
  label: string;
  /** How many filters are on. Zero hides the clear control. */
  activeCount?: number;
  /** Turns every filter off. Absent, there is no clear control. */
  onClear?: () => void;
  /** The search box, the scope control and the chips, in the order they should be read. */
  children: ReactNode;
}

/**
 * The one strip of filters above a list.
 *
 * Before this each list page laid out its own eight or nine controls, duplicated between the
 * header and the body. This is the container only — what goes in it is a `ListFilter`, a
 * `Chip` row, a scope control — so a page cannot invent a second arrangement.
 *
 * `role="group"` with a name, and not `search`: a landmark per list would give the page four
 * of them and a landmark list nobody can use. The clear control names how many filters it
 * will turn off, so it is never an unlabelled "Clear".
 */
export function FilterBar({ label, activeCount = 0, onClear, children }: FilterBarProps) {
  return (
    <div className="ui-filter-bar" role="group" aria-label={label}>
      {children}
      {onClear !== undefined && activeCount > 0 ? (
        <Button variant="quiet" onClick={onClear}>
          {`Clear ${activeCount} ${activeCount === 1 ? "filter" : "filters"}`}
        </Button>
      ) : null}
    </div>
  );
}
