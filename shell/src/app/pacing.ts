import { focusManager, type Query, type QueryClient } from "@tanstack/react-query";
import { BLURRED_CADENCE, currentAttention } from "../data/poll";

// Defined in `data/poll.ts` (which `backgroundCadence` needs, and which this module already imports);
// re-exported so this stays the name the sweep and its tests use.
export { BLURRED_CADENCE };

/** How often the blurred window checks which of its queries have come due. */
const SWEEP_MS = 1_000;

/** The interval one observer asks for right now, or `false` when it is not polling. */
function intervalOf(query: Query): number | false {
  let shortest: number | false = false;
  for (const observer of query.observers) {
    const options = observer.options;
    if (options.enabled === false) continue;
    // The background pollers keep their own interval through a blur; sweeping them too would only
    // fetch them twice.
    if (options.refetchIntervalInBackground) return false;
    const asked =
      typeof options.refetchInterval === "function"
        ? options.refetchInterval(query)
        : options.refetchInterval;
    if (typeof asked !== "number" || asked <= 0) continue;
    shortest = shortest === false ? asked : Math.min(shortest, asked);
  }
  return shortest;
}

/**
 * Whether a query is due for the blurred window's sweep: it polls, nothing is fetching it, and
 * the longer of its own cadence and {@link BLURRED_CADENCE} has passed since its last answer.
 */
export function dueWhileBlurred(query: Query, now: number): boolean {
  const interval = intervalOf(query);
  if (interval === false) return false;
  if (query.state.fetchStatus !== "idle") return false;
  const last = Math.max(query.state.dataUpdatedAt, query.state.errorUpdatedAt);
  // A conversation with a live turn or a pending ask says so through its query meta, and keeps its
  // own cadence: a blurred window must still show the answer arriving and the ask waiting.
  const urgent =
    (query.meta as { urgentWhileBlurred?: (q: Query) => boolean } | undefined)?.urgentWhileBlurred?.(
      query,
    ) === true;
  return now - last >= (urgent ? interval : Math.max(interval, BLURRED_CADENCE));
}

/**
 * Teach the cache that a window somebody is not using is not a window to poll at full speed.
 *
 * React-query's own notion of focus is page visibility, so a window left open behind another app
 * polled exactly as hard as the one being typed into — every page's 1.5–3 s cadence, all day.
 * Here focus also means the window has keyboard focus. While it does not, the regular intervals
 * stand still (they already skip ticks when unfocused) and a one-second sweep refreshes whatever
 * has come due at the blurred cadence instead. Focus coming back refetches everything stale at
 * once, which is react-query's refetch on focus, so the window is current the moment it is used.
 *
 * Returns the teardown. Installed once, by the main window: the notch is never focused by design
 * and its one query already polls once a minute.
 */
export function installPacing(client: QueryClient): () => void {
  focusManager.setEventListener((setFocused) => {
    const update = () => setFocused(currentAttention() === "focused");
    window.addEventListener("focus", update);
    window.addEventListener("blur", update);
    document.addEventListener("visibilitychange", update);
    update();
    return () => {
      window.removeEventListener("focus", update);
      window.removeEventListener("blur", update);
      document.removeEventListener("visibilitychange", update);
    };
  });

  const sweep = window.setInterval(() => {
    if (currentAttention() !== "blurred") return;
    const now = Date.now();
    void client.refetchQueries({
      type: "active",
      predicate: (query) => dueWhileBlurred(query, now),
    });
  }, SWEEP_MS);

  return () => {
    window.clearInterval(sweep);
    // Back to react-query's own visibility listener.
    focusManager.setEventListener((setFocused) => {
      const update = () => setFocused(undefined);
      document.addEventListener("visibilitychange", update);
      return () => document.removeEventListener("visibilitychange", update);
    });
  };
}
