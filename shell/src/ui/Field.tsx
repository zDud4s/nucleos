import type { ReactNode } from "react";

/**
 * A labelled control with an implicit association; an `aria-label` on the
 * control still takes precedence as its accessible name, so existing readers
 * and tests keep their specific names.
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
  children: ReactNode;
}

export function Field({ label, labelHidden = false, helper, children }: FieldProps) {
  return (
    <label className="ui-field">
      <span className={labelHidden ? "ui-field-label ui-field-said" : "ui-field-label"}>{label}</span>
      {children}
      {helper === undefined ? null : <span className="ui-field-helper">{helper}</span>}
    </label>
  );
}
