import type { ReactNode } from "react";
import type { BadgeTone } from "./Badge";

export interface CalloutProps {
  /** The colour vocabulary is `Badge`'s: seven tones, closed. Defaults to the neutral `info`. */
  tone?: BadgeTone;
  /** A short bold lead-in. Optional: most notices are one sentence. */
  title?: string;
  children: ReactNode;
  /**
   * `status` (polite) for the answer to something the reader just did, `alert` for a failure.
   * Absent by default — a page that opens with a notice must not announce it on load.
   */
  role?: "status" | "alert";
}

/**
 * An inline notice: a sentence or two set off from the prose around it.
 *
 * The tone is drawn as a tinted ground and a hairline from the same family (`--tone-*-bg` and
 * `--tone-*-border`), with the text in the page's own ink: the tone colours the box and the
 * words stay at full contrast, because a paragraph set in `--tone-pending-fg` is not readable
 * in the light theme. The title, when there is one, takes the tone's foreground.
 *
 * Not `ErrorNote` or `StaleNote`, which each say one specific thing; this is for the notices
 * nobody wrote a primitive for. `className` is not accepted, as for `Button`.
 */
export function Callout({ tone = "info", title, children, role }: CalloutProps) {
  return (
    <div className={`ui-callout ui-callout-${tone}`} role={role}>
      {title === undefined ? null : <p className="ui-callout-title">{title}</p>}
      <div className="ui-callout-body">{children}</div>
    </div>
  );
}
