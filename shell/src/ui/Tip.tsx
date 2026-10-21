import {
  useCallback,
  useEffect,
  useId,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";

/** What a render-prop trigger is handed, so it can wire its own control to the tip. */
export interface TipState {
  /** The id of the tooltip element — for `aria-describedby` / `aria-controls`. */
  tipId: string;
  /** The tip is showing, for any reason. */
  open: boolean;
  /** A press is holding it open (what touch and a keyboard activation produce). */
  pinned: boolean;
  /** Pin or unpin it. */
  toggle: () => void;
}

export interface TipProps {
  /** The reasoning. It floats over the page, so reading it moves nothing. */
  tip: ReactNode;
  /**
   * The trigger. Plain content is wrapped in a focusable span; a function receives the
   * state and draws its own control (that is how `Quiet` keeps its "why?" button).
   * Omit it, with a `name`, to get the ⓘ mark.
   */
  children?: ReactNode | ((state: TipState) => ReactNode);
  /** The accessible name of the ⓘ mark. Used only when there are no `children`. */
  name?: string;
  /** Keep the tip in the document while closed, for a description that is always read. */
  persist?: boolean;
  /** Extra class for the root. */
  className?: string;
}

/**
 * A tip: reasoning that opens beside the thing it explains, without moving anything.
 *
 * It opens on hover, on keyboard focus and on a press (touch has no hover, and the press is
 * what every older test already used). Escape closes it from anywhere, without taking focus
 * off the trigger. A pinned tip also closes on a press elsewhere. The trigger is described
 * by the tip through `aria-describedby`, and the tip sits inside the root so the pointer can
 * travel from the trigger onto the text without the tip going away.
 *
 * The tip's text is in the document only while it shows (a closed one costs no DOM and no
 * pixels) unless `persist` keeps it there, hidden.
 */
export function Tip({ tip, children, name, persist, className }: TipProps) {
  const tipId = useId();
  const [root, setRoot] = useState<HTMLSpanElement | null>(null);
  const [hover, setHover] = useState(false);
  const [focus, setFocus] = useState(false);
  const [pinned, setPinned] = useState(false);
  const open = hover || focus || pinned;

  const close = useCallback(() => {
    setHover(false);
    setFocus(false);
    setPinned(false);
  }, []);
  const toggle = useCallback(() => setPinned((value) => !value), []);

  // Escape and an outside press only matter while the tip is up, so the listeners only exist then.
  useEffect(() => {
    if (!open) return;
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, close]);
  useEffect(() => {
    if (!pinned) return;
    const onPress = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && root !== null && root.contains(target)) return;
      setPinned(false);
    };
    document.addEventListener("pointerdown", onPress);
    return () => document.removeEventListener("pointerdown", onPress);
  }, [pinned, root]);

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape" && open) close();
  };

  let trigger: ReactNode;
  if (typeof children === "function") {
    trigger = children({ tipId, open, pinned, toggle });
  } else if (children === undefined || children === null || children === false) {
    trigger = (
      <button
        type="button"
        className="ui-tip-mark"
        aria-label={name}
        aria-describedby={tipId}
        onClick={toggle}
      >
        i
      </button>
    );
  } else {
    trigger = (
      <span
        className="ui-tip-text"
        role="button"
        tabIndex={0}
        aria-expanded={pinned}
        aria-describedby={tipId}
        onClick={toggle}
        onKeyDown={(event) => {
          // Enter and Space pin the tip like a click does, as a native button would.
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            toggle();
          }
        }}
      >
        {children}
      </span>
    );
  }

  return (
    <span
      className={className === undefined ? "ui-tip" : `ui-tip ${className}`}
      ref={setRoot}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      onFocus={() => setFocus(true)}
      onBlur={() => setFocus(false)}
      onKeyDown={onKeyDown}
    >
      {trigger}
      <span className="ui-tip-pop" id={tipId} role="tooltip" hidden={!open}>
        {open || persist === true ? <span className="ui-tip-box">{tip}</span> : null}
      </span>
    </span>
  );
}
