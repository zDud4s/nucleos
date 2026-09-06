import { useState, type ReactNode } from "react";

export interface QuietProps {
  /** What is there, in the fewest words that are still true. It sits on the heading's line. */
  says: string;
  /** The paragraph this replaces — the reason the absence is a state and not a gap. */
  children?: ReactNode;
  /** The one gesture that would fill the space, when there is one. It stays visible. */
  action?: ReactNode;
  /**
   * Announce the line when it appears, because it is the answer to something the
   * reader just did.
   *
   * `role="status"` is polite: it waits for a pause rather than interrupting,
   * which is right for "that provider is unavailable" arriving after a search.
   * Off by default — a page that opens with seven quiet panels must not announce
   * seven absences on load, and that is the common case this component was built
   * for. `web.css` had to wrap this component in a `div role="status"` to get it,
   * which is the wrapper a primitive exists to remove.
   */
  announce?: boolean;
}

/**
 * A section with nothing to report, in one line, with the reasoning one click away.
 *
 * {@link Teach} is the same instinct at the other end of the app, and the difference is *where*.
 * An empty list is a place somebody has arrived at to do something, and a paragraph explaining how
 * the machine works is the most useful thing that space can hold. A quiet section of a page that
 * has seven of them is the opposite case: measured on the State mode, seven panels, 291 words,
 * 43% of them explaining an absence — so the page got LONGER the emptier the project was, which
 * is backwards.
 *
 * **The reasoning is kept rather than cut, and that is the whole point of the disclosure.**
 * "Nothing declared yet" on its own reads as a list that failed to load; the sentence saying a
 * command is *declared* and never detected is what makes the emptiness a decision somebody took.
 * One click for whoever wants it, no pixels for whoever does not.
 *
 * An `action` is not put behind the disclosure with it. What would fill the space is the reason
 * anybody is looking at an empty section, and hiding the only gesture behind a question about why
 * the section is empty is the drawer this app keeps refusing to build.
 */
export function Quiet({ says, children, action, announce }: QuietProps) {
  const [why, setWhy] = useState(false);

  return (
    <div className="ui-quiet" role={announce === true ? "status" : undefined}>
      <p>{says}</p>
      {children === undefined ? null : (
        <button
          type="button"
          className="ui-quiet-ask"
          aria-expanded={why}
          onClick={() => setWhy(!why)}
        >
          {why ? "less" : "why?"}
        </button>
      )}
      {action === undefined ? null : <div className="ui-quiet-action">{action}</div>}
      {why ? <div className="ui-quiet-why">{children}</div> : null}
    </div>
  );
}
