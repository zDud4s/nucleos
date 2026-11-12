import type { ReactNode } from "react";

export interface ChipProps {
  /** Whether the filter is on. Always a boolean: a chip is never "unknown". */
  pressed: boolean;
  onToggle: () => void;
  /** How many rows the chip would show, when that is known. Drawn after the label. */
  count?: number;
  disabled?: boolean;
  children: ReactNode;
}

/**
 * A filter that is either on or off.
 *
 * `aria-pressed` on a plain button, the same grammar as `ModeSwitch`: a toggle button says
 * "pressed" and a screen reader announces it without a radio group's "1 of 4". The count is part
 * of the name, so "Failed 3" is read as it is drawn, and a chip never relies on colour alone:
 * the pressed state also gets a check mark glyph from CSS (`.ui-chip[aria-pressed="true"]`).
 *
 * `className` is not accepted, as for `Button`.
 */
export function Chip({ pressed, onToggle, count, disabled, children }: ChipProps) {
  return (
    <button
      type="button"
      className="ui-chip"
      aria-pressed={pressed}
      disabled={disabled}
      onClick={onToggle}
    >
      <span className="ui-chip-label">{children}</span>
      {count !== undefined ? <span className="ui-chip-count">{count}</span> : null}
    </button>
  );
}
