import type { ButtonHTMLAttributes } from "react";
import type { LucideIcon } from "lucide-react";

export interface IconButtonProps
  extends Omit<
    ButtonHTMLAttributes<HTMLButtonElement>,
    "className" | "children" | "aria-label" | "title"
  > {
  /**
   * What the button does, as words: its accessible name, and the title a pointer finds on hover.
   * Required, because a glyph is not a name — a `+` tells a screen reader nothing, and on a line
   * of five of them it does not tell the eye which project it starts a job in either.
   */
  label: string;
  icon: LucideIcon;
}

/**
 * A control that is only a glyph.
 *
 * Its own primitive rather than a `Button` variant, for the one rule a variant could not hold:
 * the name is not optional. `Button` takes its name from its children, and a button whose only
 * child is an icon has none unless somebody remembers `aria-label` every time — which is the
 * mistake this type makes impossible to write.
 *
 * At the weight of `quiet` — no box at rest, muted — so it can sit on every line of a list
 * without competing with what the line says, and a 24px square because 24px is the smallest
 * target WCAG 2.2 accepts (2.5.8): a 14px glyph with no padding around it is a 14px target.
 *
 * `type="button"` by default, for `Button`'s reason: inside a form an unqualified button submits.
 */
export function IconButton({ label, icon: Icon, type, ...rest }: IconButtonProps) {
  return (
    <button
      type={type ?? "button"}
      className="ui-icon-button"
      aria-label={label}
      title={label}
      {...rest}
    >
      <Icon className="ui-icon-button-glyph" strokeWidth={1.5} aria-hidden="true" />
    </button>
  );
}
