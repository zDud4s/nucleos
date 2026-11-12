import type { ReactNode } from "react";

export interface SectionProps {
  /** The heading, which is also the name this region answers to. */
  label: string;
  /**
   * The heading rank, when this section sits inside something that already has
   * one.
   *
   * Defaults to 2, which is right for a section that is a direct child of the
   * page. It is a prop rather than a guess because a component cannot see what
   * is above it, and getting this wrong is not cosmetic: a run's stream name
   * inside a `Panel` titled "Stored output" announced as an `h2` tells a screen
   * reader the two are siblings, which is the opposite of what the page means.
   * Found at `RunDetail`, where the page kept a hand-rolled `h3` rather than
   * adopt a primitive that would have flattened its outline.
   *
   * Two ranks and no more. A third would mean a section inside a section inside
   * a section, and the answer to that is a different page, not a deeper heading.
   */
  level?: 2 | 3;
  children: ReactNode;
}

/**
 * One panel of a page, under a heading that is also its name.
 *
 * The `aria-label` is not decoration: it is the handle a composition test grabs the page by, and
 * the same handle a screen reader uses. Protecting the structure somebody hears and the structure
 * somebody sees with one assertion is worth more than protecting either alone.
 *
 * **A section whose panel is quiet puts its heading and its body on one baseline**, and that lives
 * in `ui.css` as `:has(> .ui-quiet)` rather than as a prop here. Whether a panel has anything to
 * say is a fact about the data that panel fetched; a prop would mean the section above and the
 * panel inside both holding an opinion about it, and two holders of one fact are two things free
 * to disagree.
 */
export function Section({ label, level = 2, children }: SectionProps) {
  const Heading = level === 3 ? "h3" : "h2";
  return (
    <section aria-label={label} className="ui-section">
      <Heading className="ui-section-title">{label}</Heading>
      {children}
    </section>
  );
}
