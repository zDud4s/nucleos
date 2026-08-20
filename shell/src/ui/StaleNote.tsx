export interface StaleNoteProps {
  /**
   * react-query's `dataUpdatedAt` — when this query last *succeeded*, in epoch
   * milliseconds. Zero means it never has.
   */
  dataUpdatedAt: number;
}

/**
 * The view you are looking at is not current, and here is how old it is.
 *
 * The rule this implements: a page whose refetch failed keeps showing the last
 * good data rather than blanking. An empty screen during a five-second daemon
 * restart destroys more context than it protects, and it is indistinguishable
 * from "there is nothing here" — which is a different and much more alarming
 * fact.
 *
 * The timestamp is the whole point. "Stale" without a time is unusable: three
 * seconds old is fine and three minutes old means the daemon is gone, and only
 * the person can tell which one matters for what they are about to do. Controls
 * that would act on stale data disappear instead of failing after the click —
 * that decision belongs to each page, and this note is what explains why the
 * control went away.
 *
 * `role="status"`: polite. The data is old, not wrong.
 */
export function StaleNote({ dataUpdatedAt }: StaleNoteProps) {
  // A query that has never succeeded has no last-good read to name, and
  // formatting a zero would date the app to 1970 — which reads as a bug in the
  // clock rather than as an absence of data.
  if (dataUpdatedAt <= 0) {
    return (
      <p className="ui-note ui-note-stale" role="status">
        view is stale — no good read yet
      </p>
    );
  }

  // Local wall-clock, 24-hour, seconds included: this is read against the
  // machine's own clock while watching something move, so the useful comparison
  // is with the time in the corner of the screen.
  const clock = new Date(dataUpdatedAt).toTimeString().slice(0, 8);
  return (
    <p className="ui-note ui-note-stale" role="status">
      view is stale — last good read {clock}
    </p>
  );
}
