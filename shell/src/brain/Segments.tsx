import { Segmented } from "../ui";

/**
 * `Segmented` with its options as `[value, label]` tuples, the shape the Brain's constant tables
 * (`DEPTHS`, `TYPE_FILTERS`, …) are written in. The control itself is the shared one, so "how it
 * is set now" reads the same here as on Feed, Fleet and Runs.
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
    <Segmented
      label={label}
      value={value}
      options={options.map(([option, text]) => ({ value: option, label: text }))}
      onChange={onChange}
    />
  );
}
