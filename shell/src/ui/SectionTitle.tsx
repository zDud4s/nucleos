import type { ReactNode } from "react";

export interface SectionTitleProps {
  /**
   * The heading rank. `3` when it sits under a region or panel that already
   * spends the `h2`, which is the common case for this component.
   */
  level?: 2 | 3;
  children: ReactNode;
}

/**
 * A heading at the section rank, and nothing else.
 *
 * {@link Section} is the fuller thing: a heading *plus* the labelled region it
 * names, which is right when the two are the same words. Fifteen headings across
 * `project/` are not that case — they sit inside regions whose accessible name
 * deliberately differs from the visible heading (`The triager`, `Your stamps`,
 * `The junction`, `Decisions waiting`), and three tests query the page by those
 * region names while a fourth asserts the page's exact list of landmarks.
 * Adopting `Section` there would have renamed or duplicated a landmark to reuse
 * four declarations, which is a worse trade than the duplication it removes.
 *
 * So the rank is available on its own. The rule is unchanged — display face,
 * `--tracking-wider`, the same `.ui-section-title` those fifteen were already
 * hand-writing to the declaration — and what a caller no longer gets is the
 * decision about landmarks, which stays where the page can see it.
 */
export function SectionTitle({ level = 3, children }: SectionTitleProps) {
  const Heading = level === 3 ? "h3" : "h2";
  return <Heading className="ui-section-title">{children}</Heading>;
}
