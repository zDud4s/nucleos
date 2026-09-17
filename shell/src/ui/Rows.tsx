import type { ReactNode } from "react";

export interface RowsProps {
  /**
   * What the column is a list of, as the `ul`'s accessible name.
   *
   * Required, and all four sites already supply one — "People", "Feed", "Mail
   * queue", "Memos". A hairline-ruled column has no visible heading of its own
   * by construction; the rules are the only thing saying where it starts and
   * stops, and a rule is not announced. Without the label a screen reader gets
   * "list, 40 items" and no answer to *of what*.
   */
  label: string;
  /**
   * `ol` when the ordering is part of what the list says — a numbered sequence,
   * a chain of steps. `ul` otherwise, which is nearly always.
   */
  as?: "ul" | "ol";
  /**
   * A page's own modifier on the list — a grid, a width, a margin on the column.
   *
   * Added when twenty-eight pages were recomposed onto this list and nearly every
   * one of them carried a layout class beside `.ui-rows`. It is for placing the
   * column, never for restating its ground, border or radius: those three are
   * the hairline mechanism, and a page that re-answers one of them deletes rules.
   */
  className?: string;
  /** {@link Row} elements. Anything else and the ground shows through. */
  children: ReactNode;
}

export interface RowProps {
  /**
   * How the row arranges its own parts.
   *
   * `stack` (the default) puts them on separate lines, which is what the four
   * lists this was extracted from all do. `line` puts them on one baseline —
   * needed by a subsystem row (name, state badge, reason) and a feed row (badge,
   * summary, time), both of which would otherwise have to wrap their contents in
   * a `div` inside every row, which is the box this component exists to remove.
   *
   * An axis and not a `className`, deliberately: see the note on the fill below.
   */
  layout?: "stack" | "line";
  /**
   * This is the one — the row you are on, or the row selected.
   *
   * The narrow exception to this component taking no styling from its caller.
   * `.ui-current` is an inset shadow on the leading edge and touches nothing the
   * row owns, least of all the `background` that keeps the hairline gap from
   * showing through; see its comment in `ui.css`.
   */
  current?: boolean;
  /**
   * Above a handful, the list is being *worked* rather than read, and the row's
   * padding comes in (`.ui-rows-row-dense`). Nothing else changes: the tenth row
   * staying on the same screen as the first is what stops a queue being abandoned
   * halfway.
   */
  dense?: boolean;
  /**
   * A page's own modifier for what is INSIDE the row — its grid tracks, its
   * alignment, a hook a test reaches for.
   *
   * Under the same protest as {@link Inset}'s: never `background`, `padding` or
   * `border`. The fill is what keeps the hairline gap from showing through, so a
   * page that overrides it knocks a rule out of the column rather than restyling
   * a row. A row marked out from its siblings says so with `current`, never with
   * a class of its own.
   */
  className?: string;
  children: ReactNode;
}

/**
 * A column read by scanning down it, not by picking items out of it.
 *
 * The pair with {@link Inset}: those are the app's two ways to present a
 * collection, and the question that picks between them is whether the reader
 * scans the column or reaches into it. Fifty bordered boxes stacked vertically
 * is a pile, and the eye stops reading a pile as a list — which is why the four
 * pages that grew this (`contacts-roster`, `feed-list`, `mail-list`,
 * `voice-list`) each wrote a comment saying so before writing the CSS.
 *
 * **It is a list and it stays a list.** All the sites are semantic lists, and the
 * only reason a component like this ever quietly turns one into a `div` is that
 * `list-style` and the default padding were in the way — which `.ui-rows`
 * already resets.
 *
 * `as` was added when the conversation this component's first draft demanded
 * actually happened: a fleet job's items carry ordinals and round boundaries, so
 * the ordering is content and an `ol` is the truth. It stays two values, and
 * neither of them is `div`.
 *
 * **A pair and not one component with a `rows` array.** The four pages render
 * rows out of four unrelated payloads, each with its own head line, its own
 * empty case and its own actions; a data-driven list would have taken a render
 * prop, which is a child with extra steps. Splitting it also puts the separator
 * mechanism where it belongs — {@link Row} paints its own background because the
 * rules are a 1px `gap` over a `--border` ground, so a row that forgets its fill
 * is a row you can see through. That is the one thing a caller must not have to
 * remember, and now cannot.
 */
export function Rows({ label, as = "ul", className, children }: RowsProps) {
  const Element = as;
  return (
    <Element className={className === undefined ? "ui-rows" : `ui-rows ${className}`} aria-label={label}>
      {children}
    </Element>
  );
}

/**
 * One line of a {@link Rows}.
 *
 * Stacks its children with `--space-1` — the tighter of the two gaps the four
 * copies used, because a row that wants more air can add it and a row that wants
 * less cannot take it back without knowing what it is undoing.
 *
 * It was first written with no `className`, and `layout` was the reason that
 * stayed true for the four lists it was extracted from: the fill it paints is
 * not decoration but the thing that keeps the ground from showing through, so a
 * page overriding `background` here would knock a rule out of the column rather
 * than restyle a row. The recomposition that moved twenty-eight pages onto this
 * list brought row layouts no named axis could carry, so `className` exists now
 * with the narrow brief its doc gives; `layout`, `current` and `dense` remain the
 * way to say the things the shared layer owns.
 */
export function Row({ layout = "stack", current, dense, className, children }: RowProps) {
  const classes = [
    "ui-rows-row",
    layout === "line" ? "ui-rows-row-line" : undefined,
    dense === true ? "ui-rows-row-dense" : undefined,
    current === true ? "ui-current" : undefined,
    className,
  ]
    .filter((c) => c !== undefined)
    .join(" ");
  return <li className={classes}>{children}</li>;
}
