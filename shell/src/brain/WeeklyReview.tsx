import { useId, useState } from "react";
import { DAEMON_NAME } from "../copy";
import { useWeeklyReview, type OwnerNote, type WeeklyReview as Review } from "../data/owner-notes";
import { ErrorNote, Quiet, Skeleton } from "../ui";
import { formatItem } from "./item-ref";
import "./WeeklyReview.css";

/** First line of a note, trimmed: a note has no title of its own. */
function firstLine(text: string): string {
  const first = text.trim().split("\n")[0] ?? "";
  return first.length > 80 ? `${first.slice(0, 79)}…` : first || "(empty note)";
}

const DAY = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });

/**
 * "Oct 19 - Oct 25" in local time. `week_end` is exclusive (the next Monday), so the last day shown
 * is the one before it.
 */
export function weekRange(start: string, end: string): string {
  const from = new Date(start);
  const to = new Date(end);
  if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime())) return "";
  const last = new Date(to.getTime() - 1);
  return `${DAY.format(from)} - ${DAY.format(last)}`;
}

/** "This week: 4 written, 3 unlinked, 2 to revisit, 1 proposal waiting". */
export function weekSummary(review: Review): string {
  const proposals = review.waiting_proposals;
  return (
    `This week: ${review.created.length} written, ${review.unlinked_total} unlinked, ` +
    `${review.resurfaced.length} to revisit, ${proposals} ${proposals === 1 ? "proposal" : "proposals"} waiting`
  );
}

function NoteRows({ notes, onItem }: { notes: OwnerNote[]; onItem: (item: string) => void }) {
  return (
    <ul className="brain-week-list">
      {notes.map((note) => (
        <li key={note.id}>
          <button
            type="button"
            className="brain-week-row"
            onClick={() => onItem(formatItem({ kind: "note", id: note.id }))}
          >
            {firstLine(note.text)}
          </button>
        </li>
      ))}
    </ul>
  );
}

function Section({
  title,
  notes,
  quiet,
  onItem,
  children,
}: {
  title: string;
  notes: OwnerNote[];
  quiet: string;
  onItem: (item: string) => void;
  children?: React.ReactNode;
}) {
  return (
    <section className="brain-week-section">
      <h4>{title}</h4>
      {notes.length === 0 ? <Quiet compact says={quiet} /> : <NoteRows notes={notes} onItem={onItem} />}
      {children}
    </section>
  );
}

/**
 * The owner's week in the Brain: what was written, which notes still link to nothing, a few old notes
 * brought back, and how many proposals wait. Collapsed to one line until it is opened.
 */
export function WeeklyReview({ onItem }: { onItem: (item: string) => void }) {
  const review = useWeeklyReview();
  const [open, setOpen] = useState(false);
  const bodyId = useId();

  if (review.isPending) return <Skeleton label="Loading this week" lines={2} />;
  if (review.isError || review.data === undefined) {
    return (
      <ErrorNote onRetry={() => void review.refetch()}>
        the {DAEMON_NAME} is not responding: this week is not available
      </ErrorNote>
    );
  }

  const data = review.data;
  const hidden = data.unlinked_total - data.unlinked.length;
  return (
    <section className="brain-week" aria-label="This week">
      <div className="brain-week-head">
        <h3>This week</h3>
        <span className="brain-week-range">{weekRange(data.week_start, data.week_end)}</span>
        <button
          type="button"
          className="brain-week-toggle"
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={() => setOpen(!open)}
        >
          {open ? "Hide" : "Show"}
        </button>
      </div>
      {open ? (
        <div id={bodyId} className="brain-week-body">
          <Section title="Written" notes={data.created} quiet="Nothing written this week." onItem={onItem} />
          <Section title="Unlinked" notes={data.unlinked} quiet="Every note is linked." onItem={onItem}>
            {hidden > 0 && (
              <p className="brain-week-more">
                showing {data.unlinked.length} of {data.unlinked_total}
              </p>
            )}
          </Section>
          <Section title="Resurfaced" notes={data.resurfaced} quiet="Nothing old to revisit." onItem={onItem} />
          <p className="brain-week-proposals">
            {data.waiting_proposals} {data.waiting_proposals === 1 ? "proposal" : "proposals"} waiting
          </p>
        </div>
      ) : (
        <p id={bodyId} className="brain-week-summary">
          {weekSummary(data)}
        </p>
      )}
    </section>
  );
}
