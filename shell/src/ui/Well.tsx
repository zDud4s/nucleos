import type { ReactNode } from "react";

export interface WellProps {
  /**
   * `pre` when the content's own line breaks and spacing are part of what it
   * says — a payload, a diff, a stack trace. `ol` when it is a sequence and the
   * ordering is part of the content, such as a navigation chain. `div`
   * otherwise.
   *
   * Not a default worth guessing at: rendering a JSON body in a `div` collapses
   * its whitespace and silently changes what is on screen, and rendering a
   * one-line path in a `pre` gives it a scrollbar it does not need.
   */
  as: "div" | "pre" | "ol";
  /** The accessible name, when the well stands for something a label names elsewhere. */
  label?: string;
  /**
   * The content is something a person reads, not something a machine produced.
   *
   * The default face is mono at `--text-xs`, and that is a claim about
   * authorship rather than a size. Six pages hit it; `council` put it best — a
   * seat's answer in mono, above a synthesis in the body face, asserts two
   * different authors. Set this for a message body, a notebook, an answer.
   */
  reads?: boolean;
  /**
   * Stop growing at `--well-cap` and scroll.
   *
   * For content that is unbounded by nature. Four pages had each picked their
   * own ceiling before this existed.
   */
  capped?: boolean;
  children: ReactNode;
}

/**
 * A recess cut into a surface — the third rank, and not a third card.
 *
 * The system's rule is that a box inside a box inside a box does not exist: an
 * inset inside an inset either goes flat or goes down. This is what going down
 * means. It was prescribed without being provided, so three pages built it
 * separately — `waiting-payload-raw`, `ap-root`, `ap-raw` — and one of the three
 * gave it a border, which made it a third card again and put the nesting back.
 *
 * What goes in one is nearly always something the machine produced: a path, a
 * payload, a raw response. The mono face in `ui.css` is that claim, and it is
 * the reason a well is not simply "a quieter panel".
 */
export function Well({ as, label, reads, capped, children }: WellProps) {
  const Element = as;
  const classes = [
    "ui-well",
    reads === true ? "ui-well-reading" : undefined,
    capped === true ? "ui-well-capped" : undefined,
  ]
    .filter((c) => c !== undefined)
    .join(" ");
  return (
    <Element className={classes} aria-label={label}>
      {children}
    </Element>
  );
}
