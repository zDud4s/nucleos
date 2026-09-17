import { useId, useState, type ReactNode } from "react";

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
  // Seven `why?` buttons on `03-waiting` whose accessible name was "why?" and nothing else.
  // The line beside it is the only description this component owns, and it is the right one:
  // it is what the section actually says.
  const saidId = useId();
  const whyId = useId();

  return (
    <div className="ui-quiet" role={announce === true ? "status" : undefined}>
      <p id={saidId}>{says}</p>
      {children === undefined ? null : (
        <button
          type="button"
          className="ui-quiet-ask"
          aria-expanded={why}
          aria-controls={whyId}
          aria-describedby={saidId}
          onClick={() => setWhy(!why)}
        >
          {why ? "less" : "why?"}
        </button>
      )}
      {action === undefined ? null : <div className="ui-quiet-action">{action}</div>}
      {children === undefined ? null : (
        // The ELEMENT is always here so `aria-controls` resolves; the CHILDREN are not, so
        // the reasoning still costs no pixels and no DOM until it is asked for. Rendering
        // the prose behind `hidden` would have been the obvious version and would have made
        // two "costs nothing" assertions pass over text that was in the document
        // (`Quiet.test.tsx:16`, `Waiting.test.tsx:386` — RTL's `queryByText` sees hidden
        // nodes).
        <div className="ui-quiet-why" id={whyId} hidden={!why}>
          {why ? children : null}
        </div>
      )}
    </div>
  );
}
