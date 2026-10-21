import { useId, type ReactNode } from "react";
import { SlideMark } from "./SlideMark";

export interface SegmentedOption<T extends string | number> {
  value: T;
  label: ReactNode;
  /** A segment that cannot be chosen right now; it stays drawn so the track keeps its shape. */
  disabled?: boolean;
}

export interface SegmentedProps<T extends string | number> {
  /** What the control sets, as the group's accessible name ("Window", "Sort by"). */
  label: string;
  value: T;
  options: readonly SegmentedOption<T>[];
  onChange: (next: T) => void;
}

/**
 * One choice out of a few, drawn as the app's segmented track (`.ui-switch`, `ui.css`).
 *
 * The generic form of `brain/Segments`, and the same grammar: a `group` of toggle buttons rather
 * than radios, because every caller and test already speaks `aria-pressed` and the pressed mark
 * in `ui.css` is keyed on it. A screen reader announces "pressed" on a button without a radio
 * group's "1 of 4", and Tab walks the segments like any other buttons.
 *
 * `className` is not accepted, as for `Button`.
 */
export function Segmented<T extends string | number>({ label, value, options, onChange }: SegmentedProps<T>) {
  // The pressed segment's raised ground and rule are one element that slides; see `SlideMark`.
  const markId = useId();
  return (
    <div role="group" aria-label={label} className="ui-switch" data-slide="">
      {options.map((option) => (
        <button
          key={String(option.value)}
          type="button"
          className="ui-switch-seg"
          aria-pressed={value === option.value}
          disabled={option.disabled}
          onClick={() => onChange(option.value)}
        >
          {option.label}
          {value === option.value ? <SlideMark layoutId={markId} kind="segment" /> : null}
        </button>
      ))}
    </div>
  );
}
