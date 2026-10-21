import { useId, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { Count } from "./Count";
import { SlideMark } from "./SlideMark";

export interface LinkTab {
  /** Stable identity for the strip; also the React key. */
  key: string;
  to: string;
  label: ReactNode;
  /** Whether this is the place the reader is standing in. Exactly one tab should say so. */
  current: boolean;
  /** A reading beside the label; `undefined` draws nothing (see `Count`). */
  count?: number;
  /** A door somebody has not walked through, receding by tone. Ignored on the current tab. */
  quiet?: boolean;
  title?: string;
}

export interface LinkTabsProps {
  /** What the strip chooses between, as the nav's accessible name ("Project modes"). */
  label: string;
  tabs: readonly LinkTab[];
}

/**
 * A tab strip whose tabs are routes.
 *
 * `<nav>` of links with `aria-current="page"`, and deliberately not `role="tablist"`: each tab is
 * a URL, so it gets a link's behaviour (open in a new window, a status-bar address) for free, and
 * a tablist would trade that for a roving tab stop and `aria-selected` that links cannot keep. The
 * look is the shared tab strip (`.ui-tab-list`, `.ui-tab`): the active mark is `--text` and never
 * `--accent`, drawn from `data-state="active"` on the same rule the Radix `TabsTrigger` uses.
 *
 * Takes `current` per tab rather than reading the router, because the page already knows which
 * mode it renders (a nested route, a search param) and a second opinion here would disagree with
 * it. A tab that is `quiet` yields to `current`, so the strip's one "here" mark is never the
 * dimmest thing on it. `className` is not accepted, as for `Button`.
 */
export function LinkTabs({ label, tabs }: LinkTabsProps) {
  // The current tab's underline slides to the next one; see `SlideMark`.
  const markId = useId();
  return (
    <nav aria-label={label} className="ui-tab-list">
      {tabs.map((tab) => (
        <Link
          key={tab.key}
          to={tab.to}
          title={tab.title}
          aria-current={tab.current ? "page" : undefined}
          data-state={tab.current ? "active" : "inactive"}
          data-quiet={tab.quiet === true && !tab.current ? "" : undefined}
          className="ui-tab"
        >
          {tab.label}
          <Count n={tab.count} />
          {tab.current ? <SlideMark layoutId={markId} kind="rule" /> : null}
        </Link>
      ))}
    </nav>
  );
}
