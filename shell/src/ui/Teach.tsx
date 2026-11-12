import type { ReactNode } from "react";

export interface TeachProps {
  /** The heading. A short statement of what this place is for. */
  title: string;
  /** How the thing works, or how something gets here. */
  children: ReactNode;
  /** The one gesture that would fill this space, when there is one. */
  action?: ReactNode;
}

/**
 * An empty list, used to explain the machine.
 *
 * Most of this app's surfaces are empty most of the time, and each emptiness
 * means something specific: no proposals are waiting *because the autopilot is off*; the
 * archive has no pages *because nothing has been read yet*. "Nothing here" is
 * a wasted sentence in every one of those cases.
 *
 * Not an error state. An empty queue is usually the system working.
 */
export function Teach({ title, children, action }: TeachProps) {
  return (
    <div className="ui-teach">
      <h3 className="ui-teach-title">{title}</h3>
      <div className="ui-teach-body">{children}</div>
      {action === undefined ? null : <div className="ui-teach-action">{action}</div>}
    </div>
  );
}
