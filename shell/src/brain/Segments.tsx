/**
 * One choice out of a few, drawn as the app's segmented track (`.ui-switch`, `ui.css`) — the
 * same control Feed, Fleet and Runs use, so "how it is set now" reads the same on the Brain.
 *
 * A `group` of toggle buttons rather than radios: every caller and test already speaks
 * `aria-pressed`, and the pressed mark in `ui.css` is keyed on it.
 */
export function Segments<T extends string | number>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: readonly (readonly [T, string])[];
  onChange: (next: T) => void;
}) {
  return (
    <div role="group" aria-label={label} className="ui-switch">
      {options.map(([option, text]) => (
        <button
          key={String(option)}
          type="button"
          className="ui-switch-seg"
          aria-pressed={value === option}
          onClick={() => onChange(option)}
        >
          {text}
        </button>
      ))}
    </div>
  );
}
