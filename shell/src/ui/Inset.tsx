import type { ReactNode } from "react";

/**
 * The element the box is drawn as.
 *
 * Nineteen of the thirty sites this replaces are an `li` inside a `ul` the page
 * already labels, and eleven are a `div`. `article` was added afterwards for the
 * one shape neither covers: a fleet slot card, which is a standalone thing with
 * a name of its own ("slot 1 — job 41") that fourteen tests grab the page by. It
 * was the only reason that card could not adopt this component after its
 * coloured stripe came off, and the recipe was already identical.
 *
 * `form` and `aside` came the same way and only once each was asked for twice:
 * `files` and `feed` each wrap a search form in this exact recipe, and
 * `WorkflowGraph` draws its node inspector as an `aside aria-label` — a
 * complementary landmark, whose name a bare `div` silently drops.
 *
 * Still deliberately narrow, and the bar for widening it is a real element with
 * a real reason: five names, each admitted because a page could not otherwise
 * adopt the box without losing markup it needed. A box that can be anything is a
 * box whose markup nobody has thought about, and this component exists because
 * thirty places stopped thinking about the same box.
 */
export type InsetAs = "div" | "li" | "article" | "form" | "aside";

export interface InsetProps {
  /**
   * `li` when the box is one item of a list the page has already opened, `div`
   * otherwise. Defaulting to `li` would have matched the majority and produced
   * invalid markup the first time somebody used it outside a `ul`; the default
   * is the one that is never wrong.
   */
  as?: InsetAs;
  /**
   * A page's own modifier on top of the shared recipe — `ap-row-selected`, and
   * nothing else in the app today.
   *
   * The escape hatch is here under protest and with a narrow brief: it is for
   * marking *this* box out from its siblings, never for restating fill, radius,
   * padding or border. Those four are what the thirty copies each answered
   * differently, and re-answering one of them through this prop rebuilds the
   * problem one page at a time.
   */
  className?: string;
  /**
   * This is the one — the row you are on, or the item selected.
   *
   * A prop rather than something a page draws itself, because six pages drew it
   * themselves and all six reached for the brand colour: the neutral answer did
   * not exist until `.ui-current`. See its comment in `ui.css` for why the mark
   * is an edge and not a fill.
   */
  current?: boolean;
  /**
   * The accessible name, for an `article` that stands on its own.
   *
   * Meaningless on an `li` inside a labelled list, which is why it is optional
   * and not implied by `as`.
   */
  label?: string;
  children: ReactNode;
}

/**
 * The box inside a panel body. One rung up the neutral ladder, one rank down.
 *
 * **Not a `Panel` variant, and the reason is structural rather than stylistic.**
 * A `Panel` is a head/body pair — it decides where a title and its aside go, and
 * `.ui-panel-head` carries the rule under them. `.ui-panel-inset` is a single
 * box carrying its own `--space-3` of padding, and `ui.css` says outright not to
 * put a head or a body inside one. An `inset` variant would therefore have had
 * to render different internal markup from the other two variants and silently
 * ignore `title` and `aside` — a prop set that is invalid in one variant is two
 * components sharing a name. The cheaper tell is that `.ui-panel-inset`
 * overrides `.ui-panel` on every property the two share, so the variant would
 * have emitted a class that does nothing.
 *
 * **Not `Card`, either.** DESIGN.md settled on one card, which is `Panel`, and a
 * second name for the same idea is how a system grows two card ranks that
 * disagree. This is named for its rank: it is the *inset*, the thing one step
 * inside something else, and there is no third step — an inset inside an inset
 * is boxes-in-boxes with an extra frame, and the three places that go that deep
 * want no box at all or `.ui-panel-dim`.
 *
 * **It takes no heading and no aside**, which is the surprise given that every
 * one of the nineteen page copies has a `-head` div. Those heads have nothing in
 * common but `display: flex`: one is an id and a mono title, one is a name and a
 * `StateBadge`, one is a cron expression and a timezone. There is no
 * `.ui-panel-inset-head` in `ui.css` to hang them on, and inventing a `head`
 * slot here would mean thirty pages passing thirty different shapes through one
 * prop for the privilege of not writing a `div`. The head stays the page's.
 */
export function Inset({ as = "div", className, current, label, children }: InsetProps) {
  const Element = as;
  const classes = ["ui-panel-inset", current === true ? "ui-current" : undefined, className]
    .filter((c) => c !== undefined)
    .join(" ");
  return (
    <Element className={classes} aria-label={label}>
      {children}
    </Element>
  );
}
