import { useEffect, useRef } from "react";
import { Chip } from "./Chip";
import { FilterBar } from "./FilterBar";

export interface ListFilterChip {
  id: string;
  label: string;
  count?: number;
}

export interface ListFilterProps {
  /** What the list holds, plural: "projects" gives "Search projects" and "Filter projects". */
  noun: string;
  query: string;
  onQueryChange: (query: string) => void;
  /** Optional chips; `selected` holds the ids that are on. */
  chips?: ListFilterChip[];
  selected?: readonly string[];
  onToggleChip?: (id: string) => void;
  /** How many rows survive the current filters. Drives Enter and the live count. */
  resultCount?: number;
  /** Enter with exactly one result opens it. */
  onOpenOnly?: () => void;
  onClear?: () => void;
}

function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName);
}

/**
 * Search box plus chips for a list that has outgrown scanning.
 *
 * Keyboard: `/` focuses the box from anywhere that is not already a text field, `Escape` clears
 * the text (and blurs on a second press), and `Enter` calls `onOpenOnly` when `resultCount` is
 * exactly 1 — the common "type three letters, go" path. The shortcut is a listener on `document`
 * owned by this component, so it exists only while a list is on screen.
 *
 * The result count is a polite live region, so filtering is not silent for a screen reader. The
 * box is `type="search"` named by `aria-label`; the chips are `Chip`s, so the pressed state is
 * `aria-pressed` and never colour alone.
 */
export function ListFilter({
  noun,
  query,
  onQueryChange,
  chips = [],
  selected = [],
  onToggleChip,
  resultCount,
  onOpenOnly,
  onClear,
}: ListFilterProps) {
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
      if (isTyping(event.target)) return;
      event.preventDefault();
      input.current?.focus();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const activeCount = selected.length + (query.trim() === "" ? 0 : 1);

  return (
    <FilterBar label={`Filter ${noun}`} activeCount={activeCount} onClear={onClear}>
      <input
        ref={input}
        type="search"
        className="ui-list-filter-input"
        aria-label={`Search ${noun}`}
        placeholder={`Search ${noun} ( / )`}
        value={query}
        onChange={(event) => onQueryChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            if (query !== "") onQueryChange("");
            else event.currentTarget.blur();
          } else if (event.key === "Enter" && resultCount === 1 && onOpenOnly !== undefined) {
            event.preventDefault();
            onOpenOnly();
          }
        }}
      />
      {chips.map((chip) => (
        <Chip
          key={chip.id}
          pressed={selected.includes(chip.id)}
          count={chip.count}
          onToggle={() => onToggleChip?.(chip.id)}
        >
          {chip.label}
        </Chip>
      ))}
      {resultCount !== undefined ? (
        <span className="ui-list-filter-count" role="status" aria-live="polite">
          {`${resultCount} ${resultCount === 1 ? "result" : "results"}`}
        </span>
      ) : null}
    </FilterBar>
  );
}
