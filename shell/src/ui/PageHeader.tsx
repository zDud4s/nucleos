import type { ReactNode } from "react";

export interface PageHeaderProps {
  title: string;
  /**
   * One derived sentence about the state of this page's subject.
   *
   * Not a description of the page — the title already says what the page is.
   * This is the line that changes: "three projects active, one held by budget",
   * "nothing waiting on you". A page with nothing true to say here says nothing.
   */
  headline?: ReactNode;
  /** The page's own actions, right-aligned. Usually one, occasionally two. */
  actions?: ReactNode;
  /**
   * Whether the header draws its own rule underneath. Default true; false drops it because a
   * tab strip right below the header carries the rule, and two lines would read as a double rule.
   */
  ruled?: boolean;
}

/**
 * The top of a page: what this is, how it is doing, and what you can do to it.
 *
 * The headline slot is the reason this is a component rather than an `h1`. The
 * app's whole posture is that the shell tells you the state of the machine
 * before you go looking for it, and the top of every page is where that
 * sentence goes. Keeping the slot in the shared header is what stops it from
 * being omitted on the pages nobody rewrote.
 */
export function PageHeader({ title, headline, actions, ruled = true }: PageHeaderProps) {
  return (
    <header className={ruled ? "ui-page-header" : "ui-page-header ui-page-header-unruled"}>
      <div className="ui-page-header-text">
        <h1 className="ui-page-title">{title}</h1>
        {headline === undefined ? null : <p className="ui-page-headline">{headline}</p>}
      </div>
      {actions === undefined ? null : <div className="ui-page-actions">{actions}</div>}
    </header>
  );
}
