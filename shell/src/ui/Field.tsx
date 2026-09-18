import { cloneElement, useId, type ReactElement, type ReactNode } from "react";

/** The two attributes `Field` writes onto its control — read first, so a caller's are kept. */
interface ControlProps {
  id?: string;
  "aria-describedby"?: string;
}

/**
 * A labelled control: the label names it, and the helper describes it.
 *
 * Two relations, and they are two on purpose. This was one wrapping `<label>` with the helper
 * inside it, which made the helper part of the control's accessible *name* — the Runs prompt
 * announced itself as "Prompt Start run waits until this says what the run should do" — and two
 * pages worked around it with an `aria-label` restating the visible word. Now the `<label>` holds
 * the label text alone and points at the control, and the helper is linked by
 * `aria-describedby`, which is what a hint is: said after the name, never as part of it.
 *
 * An `aria-label` on the control still takes precedence as its accessible name, so existing
 * readers and tests keep their specific names. The layout is the column it always was — label,
 * control, helper — carried by a `<div>` now rather than by the label.
 */
export interface FieldProps {
  /** The visible label, at the app's 11px tracked label rank. */
  label: string;
  /**
   * Hide the label from sight but not from a screen reader.
   *
   * For the one case that keeps recurring: a field inside a panel whose heading already says
   * the same word. Never for a field that has no other name on screen.
   */
  labelHidden?: boolean;
  /** One line under the control — a hint, a unit, a constraint. Never an error. */
  helper?: ReactNode;
  /**
   * Exactly one control. `Field` gives it an id for the label to point at — the control's own,
   * if it has one — and adds the helper to its `aria-describedby` without dropping whatever the
   * caller already put there.
   */
  children: ReactElement<ControlProps>;
}

export function Field({ label, labelHidden = false, helper, children }: FieldProps) {
  const fallbackId = useId();
  const helperId = useId();
  const own = children.props;
  const controlId = own.id ?? fallbackId;
  const describedBy =
    helper === undefined
      ? own["aria-describedby"]
      : [own["aria-describedby"], helperId].filter((part) => part !== undefined && part !== "").join(" ");
  return (
    <div className="ui-field">
      <label htmlFor={controlId} className={labelHidden ? "ui-field-label ui-field-said" : "ui-field-label"}>
        {label}
      </label>
      {cloneElement(children, { id: controlId, "aria-describedby": describedBy })}
      {helper === undefined ? null : (
        <span id={helperId} className="ui-field-helper">
          {helper}
        </span>
      )}
    </div>
  );
}
