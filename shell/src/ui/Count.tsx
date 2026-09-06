export interface CountProps {
  /**
   * How many, or `undefined` while nobody has managed to ask yet.
   *
   * `undefined` renders nothing at all, which is the behaviour seven of the nine
   * copies had already written by hand and the one place this component
   * deliberately differs from {@link StatCard}. A stat card is the whole reason
   * its tile exists, so an unread figure there becomes an em dash — the absence
   * has to be visible or the card is a lie. A count is a footnote on a heading
   * that says the same thing without it, so an unread one is best said by not
   * saying anything.
   *
   * Zero is not that case and does render. "0 open" is an answer; a heading that
   * loses its count the moment the list empties reads as a count that failed.
   */
  n: number | undefined;
  /**
   * What is being counted, singular — "hit", "item", "session".
   *
   * Optional because most call sites sit in a `Panel`'s `aside`, where the title
   * beside them already says the noun and repeating it is noise. Where the count
   * travels without its title, the noun is the difference between "3" and "3
   * sessions".
   */
  noun?: string;
  /**
   * The plural, when it is not `noun + "s"` — "people", "memos" needs nothing,
   * "untriaged" needs itself.
   *
   * The `-s` default is the reason this prop exists rather than the pluralising
   * being left to callers: an English `-s` covers almost every noun this app
   * counts, so the two or three that it does not are exactly the ones somebody
   * would otherwise get wrong once and never look at again.
   */
  plural?: string;
}

/**
 * How many, in the corner of a heading.
 *
 * Bare text and **never a pill.** The app has made this decision once already —
 * `LimitChip` was demoted from a bordered chip precisely so a quantity could not
 * be mistaken for a badge — and all nine hand-rolled copies had independently
 * arrived at bare text too. The one pill-shaped count in the system is
 * `.nav-badge`, and it earns the shape because a shut sidebar has no room for
 * the word the number belongs to, so the pill is what is left of the word.
 *
 * **The noun is a prop because the plural was the thing being got wrong.** Seven
 * of the nine copies are the byte-identical `function Count({ n })` that renders
 * a bare number, and the two that do say what they are counting each solved
 * `item`/`items` inline, in a ternary, in their own file. That ternary is the
 * whole of the duplication worth removing: a number is easy and a number with a
 * word after it is where a shared component earns itself.
 *
 * Mono and tabular because the figure came from the daemon and changes under a
 * poll — a column of these down the right edge of a panel head must stay a
 * column rather than wobble as digits change width — and `nowrap` because
 * `.ui-panel-aside` wraps, and "3 waiting" broken across two lines in a corner
 * is the only failure this component has.
 */
export function Count({ n, noun, plural }: CountProps) {
  if (n === undefined) return null;
  if (noun === undefined) return <span className="ui-count">{n}</span>;
  const word = n === 1 ? noun : (plural ?? `${noun}s`);
  return (
    <span className="ui-count">
      {n} {word}
    </span>
  );
}
