import { useEffect, useState } from "react";
import { useFleetStore } from "./hooks";

/**
 * State changes that matter to a person, said politely: finished, waiting for you, ended without
 * closing. Not the terminal (narrating it is noise), and not the Stop/pin answers (they respond to
 * the owner's own action, and focus stays on the control). The counter re-keys the text so the
 * same sentence twice is announced twice.
 */
export function Announcer() {
  const store = useFleetStore();
  const [said, setSaid] = useState({ n: 0, text: "" });
  useEffect(
    () =>
      store.subscribeTransitions(({ node }) => {
        const text =
          node.state === "done"
            ? `${node.label} finished`
            : node.state === "idle"
              ? `${node.label} is waiting for you`
              : node.state === "lost"
                ? `${node.label} ended without closing`
                : null;
        if (text !== null) setSaid((previous) => ({ n: previous.n + 1, text }));
      }),
    [store],
  );
  return (
    <p className="sr-only" aria-live="polite" aria-atomic="true">
      <span key={said.n}>{said.text}</span>
    </p>
  );
}
