import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";

export interface CrumbProps {
  /** Where back is. */
  to: string;
  /** The place, in the words the rail uses for it. */
  children: ReactNode;
  /**
   * What this page is, when the crumb should say it. Rendered after the link, never
   * linked — except where a page genuinely has two ways out, which is why the slot
   * takes a node rather than a string. The arrow belongs to the first one.
   */
  here?: ReactNode;
}

/**
 * Where this page sits, and the way back out of it.
 *
 * Above the title rather than under the last panel: a way out reached only by reading
 * the whole page is a way out for whoever no longer needs one.
 *
 * **One glyph for the whole app, and it is `←`.** Three surfaces had grown three —
 * `←`, `·` and `‹` — and a reader who has learned one of them has learned nothing
 * about the next page. The glyph is `aria-hidden`, so a screen reader is told the
 * place and not the arrow: the accessible name of the link is the words, exactly.
 *
 * `here` is the page's own kind, said after the link and never inside it. It is what
 * lets a page give its heading to its subject without a reader losing what kind of
 * thing they are looking at.
 */
export function Crumb({ to, children, here }: CrumbProps) {
  return (
    <p className="ui-crumb">
      <Link to={to}>
        <span className="ui-crumb-mark" aria-hidden="true">
          ←
        </span>
        {children}
      </Link>
      {here === undefined ? null : <span className="ui-crumb-here">{here}</span>}
    </p>
  );
}
