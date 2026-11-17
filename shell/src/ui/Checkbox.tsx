import { useEffect, useRef, type InputHTMLAttributes, type ReactNode } from "react";

type NativeProps = Omit<InputHTMLAttributes<HTMLInputElement>, "type">;

export interface CheckboxProps extends NativeProps {
  /** Words drawn beside the box; when given, the input is wrapped in a `<label>`. */
  label?: ReactNode;
  /** The mixed state of a "select all" box. Set on the DOM node, as HTML has no attribute for it. */
  indeterminate?: boolean;
}

export interface RadioProps extends NativeProps {
  /** Words drawn beside the dot; when given, the input is wrapped in a `<label>`. */
  label?: ReactNode;
}

function join(...parts: Array<string | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

/**
 * A tick box: a real `<input type="checkbox">` redrawn with `appearance: none` (`.ui-check`,
 * `ui.css`), so role, label, keyboard, form behaviour and the click's `nativeEvent` (shift-click
 * ranges) are the browser's. Every input prop passes through. Without `label` it renders the bare
 * input, for a row that already wraps it in its own `<label>`.
 */
export function Checkbox({ label, indeterminate, className, ...rest }: CheckboxProps) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate === true;
  }, [indeterminate, rest.checked]);
  const input = <input {...rest} ref={ref} type="checkbox" className={join("ui-check", className)} />;
  if (label === undefined) return input;
  return (
    <label className="ui-check-field">
      {input}
      <span className="ui-check-label">{label}</span>
    </label>
  );
}

/** A choice among several: a real `<input type="radio">` redrawn as a ring with a dot. */
export function Radio({ label, className, ...rest }: RadioProps) {
  const input = <input {...rest} type="radio" className={join("ui-check ui-check-radio", className)} />;
  if (label === undefined) return input;
  return (
    <label className="ui-check-field">
      {input}
      <span className="ui-check-label">{label}</span>
    </label>
  );
}
