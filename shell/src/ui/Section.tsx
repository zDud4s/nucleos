import type { ReactNode } from "react";

export interface SectionProps {
  /** The heading, which is also the name this region answers to. */
  label: string;
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
export function Section({ label, children }: SectionProps) {
  return (
    <section aria-label={label} className="ui-section">
      <h2 className="ui-section-title">{label}</h2>
      {children}
    </section>
  );
}
