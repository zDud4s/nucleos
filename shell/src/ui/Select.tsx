import type { ComponentPropsWithoutRef } from "react";

export interface SelectProps extends Omit<ComponentPropsWithoutRef<"select">, "className" | "multiple" | "size"> {
  /**
   * `md` (default) is the form control, the height of a `Field`'s input beside it. `sm` is for a
   * dense row — a table cell, a toolbar, a settings line — where `md` would set the row's height.
   */
  density?: "md" | "sm";
  /** Fill the container's width, for a select that is the only control in its column. */
  block?: boolean;
  /** The options are the daemon's words (a project id, a model name), so set them in mono. */
  mono?: boolean;
}

/**
 * One choice from a short list: the native `<select>`, dressed once.
 *
 * Every page used to dress its own (`.teams-select`, `.sy-notify-select`, `.unified-scope`, …),
 * each a near copy of the field input with its own border token, radius and focus ring. This is
 * the one. It stays the native element on purpose: the keyboard, the type-ahead, the OS popup
 * list and `userEvent.selectOptions` in tests are the browser's, and `color-scheme` (in
 * `ui.css`) themes the popup and the arrow it draws.
 *
 * Every native prop passes through, `onChange` included, so a `<select>` becomes a `Select` by
 * renaming the tag. It works as the child of a `Field`, which writes its `id` and
 * `aria-describedby`. Single choice only: a multi-select is a list, not this control.
 * `className` is not accepted, as for `Button`.
 */
export function Select({ density = "md", block = false, mono = false, children, ...rest }: SelectProps) {
  const classes = ["ui-select"];
  if (density === "sm") classes.push("ui-select-sm");
  if (block) classes.push("ui-select-block");
  if (mono) classes.push("ui-select-mono");
  return (
    <select {...rest} className={classes.join(" ")}>
      {children}
    </select>
  );
}
