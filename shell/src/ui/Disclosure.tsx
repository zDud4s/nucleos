import { useId, useState, type ReactNode } from "react";

export interface DisclosureProps {
  /** The toggle's text, which is also its accessible name. */
  label: string;
  children: ReactNode;
  /** Uncontrolled: whether it starts open. Ignored when `open` is given. */
  defaultOpen?: boolean;
  /** Controlled: the caller owns the state and hears changes through `onOpenChange`. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

/**
 * A button that shows and hides a region: the home for what is advanced or rarely needed.
 *
 * `aria-expanded` and `aria-controls` on the button; the region is `hidden` (not unmounted) while
 * closed, so the id `aria-controls` points at always exists. Controlled when `open` is passed,
 * uncontrolled otherwise.
 */
export function Disclosure({
  label,
  children,
  defaultOpen = false,
  open,
  onOpenChange,
}: DisclosureProps) {
  const [inner, setInner] = useState(defaultOpen);
  const regionId = useId();
  const isOpen = open ?? inner;

  function toggle() {
    const next = !isOpen;
    if (open === undefined) setInner(next);
    onOpenChange?.(next);
  }

  return (
    <div className="ui-disclosure">
      <button
        type="button"
        className="ui-disclosure-toggle"
        aria-expanded={isOpen}
        aria-controls={regionId}
        onClick={toggle}
      >
        {label}
      </button>
      <div id={regionId} className="ui-disclosure-region" hidden={!isOpen}>
        {children}
      </div>
    </div>
  );
}
