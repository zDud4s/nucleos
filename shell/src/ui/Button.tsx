import type { ButtonHTMLAttributes, ReactNode } from "react";

/**
 * How much weight the button carries.
 *
 * `ghost` is the default because most buttons in a dense operations console are
 * not the point of the screen. `approve` and the two dangers are the exceptions
 * that earn their colour, and `link` exists for the controls that are really
 * navigation but must stay buttons for the keyboard and the screen reader.
 *
 * `danger` outlines and `danger-solid` fills: an outline for "this deletes a
 * row", a fill for the handful of gestures that end something irreversibly.
 * Both are usually wrapped in `ConfirmButton` — the colour warns, the interlock
 * protects, and neither substitutes for the other.
 *
 * `quiet` and `link` look alike and are not interchangeable. `link` spends the
 * identity accent, because what it does is navigation; `quiet` is faint until
 * hovered, for a control that has to be on every row of a table and must not
 * compete with the readings in it. Reach for `quiet` whenever the control acts
 * rather than goes somewhere.
 */
export type ButtonVariant = "approve" | "ghost" | "danger" | "danger-solid" | "link" | "quiet";

/**
 * Which way the action points.
 *
 * Orthogonal to variant on purpose: "start this" and "stop that" can both be
 * ghosts, and the direction is worth a glyph and a hair of colour even when the
 * weight is the same. Absent is the normal case — most buttons neither start
 * nor stop anything.
 */
export type ButtonIntent = "go" | "stop" | "create";

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  variant?: ButtonVariant;
  intent?: ButtonIntent;
  children: ReactNode;
}

/**
 * Every clickable thing in the app.
 *
 * `type="button"` by default, and that default is load-bearing: HTML makes an
 * unqualified button inside a form a submit button, and this app is full of
 * inline forms whose fields sit next to controls that must not submit them.
 * Getting this wrong sends a half-filled request the moment someone hits Enter.
 *
 * `className` is deliberately not accepted. The variants are the vocabulary; a
 * page that needs a different-looking button needs a new variant here, where
 * the next page can find it.
 */
export function Button({ variant = "ghost", intent, type, children, ...rest }: ButtonProps) {
  const classes = ["ui-button", `ui-button-${variant}`];
  if (intent !== undefined) classes.push(`ui-button-${intent}`);
  return (
    <button type={type ?? "button"} className={classes.join(" ")} {...rest}>
      {children}
    </button>
  );
}
