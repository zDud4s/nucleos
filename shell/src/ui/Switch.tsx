import type { ReactNode } from "react";

export interface SwitchProps {
  /**
   * What the switch turns on or off, in words. Always its accessible name; drawn beside the track
   * unless `labelHidden` (then it is the input's `aria-label` and a plain string is required).
   */
  label: ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  /**
   * Draw the track alone, for a row whose own text already says what it is. The label stays as
   * the accessible name. Needs `label` to be a string.
   */
  labelHidden?: boolean;
  /** Relations a caller may need, e.g. the id of a line that explains the setting. */
  "aria-describedby"?: string;
  id?: string;
}

/**
 * A boolean setting: a track with a knob.
 *
 * A real `<input type="checkbox" role="switch">`, so role, keyboard (Space) and label are the
 * browser's; only the box is redrawn (`.ui-switch-check`, `ui.css`). On is drawn in the neutral
 * ink and never a state tone: green in this app means work in flight, and "this family is sent"
 * is a setting, not an activity. Not the segmented `.ui-switch` — that is a choice between
 * several things, and this is one thing on or off.
 *
 * The whole `label` element is the target, so the words are clickable as well as the track.
 * `className` is not accepted, as for `Button`.
 */
export function Switch({ label, checked, onChange, disabled, labelHidden, id, ...rest }: SwitchProps) {
  const ariaLabel = labelHidden === true && typeof label === "string" ? label : undefined;
  return (
    <label className="ui-switch-field">
      <input
        {...rest}
        id={id}
        type="checkbox"
        role="switch"
        className="ui-switch-check"
        checked={checked}
        disabled={disabled}
        aria-label={ariaLabel}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
      {labelHidden === true ? null : <span className="ui-switch-label">{label}</span>}
    </label>
  );
}
