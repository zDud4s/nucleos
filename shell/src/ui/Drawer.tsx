import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from "react";
import { XIcon } from "lucide-react";
import { IconButton } from "./IconButton";

export interface DrawerProps {
  /** The heading, and the drawer's accessible name. */
  title: string;
  /** Called on Escape and on the close button. The caller owns whether the drawer is mounted. */
  onClose: () => void;
  /**
   * Whether the page behind is shut off from a screen reader (`aria-modal`). Off by default: a
   * drawer here is a place to look, not a decision, and it neither dims nor traps anything.
   */
  modal?: boolean;
  children?: ReactNode;
}

/**
 * A panel on the right edge of the window, over the page and not over the rail.
 *
 * Mounted while it is open and gone when it is not, so there is no `open` prop to forget: the
 * caller renders `{open ? <Drawer …/> : null}`. On mount focus moves into the drawer (its own
 * container, not the close button, so the first Tab lands on the first control and the name is
 * read first), and on unmount it goes back to whatever held it, so a keyboard user returns to the
 * button that opened it.
 *
 * Escape closes it from anywhere inside. It is not bound on the document: a drawer opened out of
 * curiosity must not eat the Escape meant for a modal or a menu opened above it.
 *
 * No scrim and no focus trap, for the reason `.ui-drawer` (`ui.css`) gives: a scrim would make
 * a panel opened to look at something feel like a question that has to be answered. For a
 * decision use `Modal`.
 */
export function Drawer({ title, onClose, modal = false, children }: DrawerProps) {
  const root = useRef<HTMLElement>(null);
  const titleId = useId();

  useEffect(() => {
    const before = document.activeElement;
    root.current?.focus();
    return () => {
      if (before instanceof HTMLElement && before.isConnected) before.focus();
    };
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    event.stopPropagation();
    onClose();
  };

  return (
    <aside
      ref={root}
      className="ui-drawer"
      role="dialog"
      aria-modal={modal}
      aria-labelledby={titleId}
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      <div className="ui-drawer-head">
        <h2 id={titleId} className="ui-drawer-title">
          {title}
        </h2>
        <IconButton label="Close" icon={XIcon} onClick={onClose} />
      </div>
      <div className="ui-drawer-body">{children}</div>
    </aside>
  );
}
