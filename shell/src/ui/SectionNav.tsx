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
  return (
    <nav className="ui-section-nav" aria-label={label}>
      <ul className="ui-section-nav-list">
        {items.map((item) => (
          <li key={item.id}>
            <a
              className="ui-section-nav-link"
              href={`#${item.id}`}
              aria-current={item.id === current ? "location" : undefined}
            >
              {item.label}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
