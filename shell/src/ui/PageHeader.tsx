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
export function PageHeader({ title, headline, actions }: PageHeaderProps) {
  return (
    <header className="ui-page-header">
      <div className="ui-page-header-text">
        <h1 className="ui-page-title">{title}</h1>
        {headline === undefined ? null : <p className="ui-page-headline">{headline}</p>}
      </div>
      {actions === undefined ? null : <div className="ui-page-actions">{actions}</div>}
    </header>
  );
}
