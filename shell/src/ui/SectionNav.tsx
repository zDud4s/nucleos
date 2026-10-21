import { useId, type MouseEvent } from "react";
import { SlideMark } from "./SlideMark";

export interface SectionNavItem {
  /** The id of the section element this entry jumps to (no leading `#`). */
  id: string;
  /** Visible text, and the accessible name of the link. */
  label: string;
}

export interface SectionNavProps {
  /** Names the landmark; several navs on one page must differ. */
  label: string;
  items: readonly SectionNavItem[];
  /** The id of the section currently in view, when the page tracks it. */
  current?: string;
}

/**
 * An in-page table of contents: anchors to the sections of a long page.
 *
 * A `nav` landmark with its own `aria-label`, and `aria-current="location"` on the entry the
 * reader is at. It is sticky (see `ui.css`), so a page of a dozen panels keeps its map in view.
 * It only links; the page owns the scrolling and decides which section is `current`.
 */
export function SectionNav({ label, items, current }: SectionNavProps) {
  const markId = useId();

  /**
   * Glides to the section instead of jumping. Modified clicks (new tab, etc.) keep the browser's
   * default. The scroll is smooth per call, not via a global `scroll-behavior`, so route-change
   * scroll resets stay instant; reduced motion jumps. The hash is updated without a jump, and focus
   * moves to the section (made programmatically focusable) so keyboard and screen-reader users land
   * there too. The sticky nav is cleared by the section's own `scroll-margin-top`.
   */
  const onClick = (event: MouseEvent<HTMLAnchorElement>, id: string) => {
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey ||
      event.altKey
    ) {
      return;
    }
    const target = document.getElementById(id);
    if (!target) return;
    event.preventDefault();
    const reduced =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    target.scrollIntoView({ behavior: reduced ? "auto" : "smooth", block: "start" });
    history.replaceState(null, "", `#${id}`);
    if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
    target.focus({ preventScroll: true });
  };

  return (
    <nav className="ui-section-nav" aria-label={label}>
      <ul className="ui-section-nav-list">
        {items.map((item) => (
          <li key={item.id}>
            <a
              className="ui-section-nav-link"
              href={`#${item.id}`}
              aria-current={item.id === current ? "location" : undefined}
              onClick={(event) => onClick(event, item.id)}
            >
              {item.label}
              {item.id === current ? <SlideMark layoutId={markId} kind="rule" /> : null}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
