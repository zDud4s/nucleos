import type { ReactNode } from "react";

/** Vocabulário único de estado — espelha as classes .badge--* em ui.css. */
export type BadgeTone = "active" | "shadow" | "off" | "pending" | "paused";

export function Badge({ tone, children }: { tone: BadgeTone; children: ReactNode }) {
  return <span className={`badge badge--${tone}`}>{children}</span>;
}
