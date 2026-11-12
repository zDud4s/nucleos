import { DAEMON_NAME } from "../copy";
import { useRelatedNotes, type RelatedReason } from "../data/owner-notes";
import { Button, ErrorNote, Quiet, Skeleton } from "../ui";
import { formatItem } from "./item-ref";
import "./RelatedNotes.css";

/** Longest first line a row shows before it is cut. */
const MAX_LINE = 80;

/** The daemon's reason codes in the owner's words; the raw score is never shown. */
const REASON_TEXT: Record<RelatedReason, string> = {
  embedding: "similar meaning",
  shared_target: "shared link",
  text: "shared words",
};

/** The plain first non-blank line of a note, cut at `MAX_LINE`: a note has no title of its own. */
function firstLine(text: string): string {
  const first = text.split("\n").find((line) => line.trim() !== "")?.trim() ?? "";
  if (first === "") return "(empty note)";
  return first.length > MAX_LINE ? `${first.slice(0, MAX_LINE - 1)}…` : first;
}

export interface RelatedNotesProps {
  id: number;
  /** A note picked: `note:<id>`, the Brain `?item=` format. Without it the rows are read-only. */
  onItem?: (item: string) => void;
}

/** "Related notes": other active notes close to this one, best first, each with why it is close. */
export function RelatedNotes({ id, onItem }: RelatedNotesProps) {
  const related = useRelatedNotes(id);

  return (
    <section className="brain-sec related-notes" aria-label="Related notes">
      <h4>Related notes</h4>
      {related.data === undefined ? (
        // A failed refetch keeps the rows already known; the error shows only when there are none.
        related.isError ? (
          <ErrorNote>the {DAEMON_NAME} is not responding: the related notes are not known</ErrorNote>
        ) : (
          <Skeleton label="Loading the related notes" lines={3} />
        )
      ) : related.data.length === 0 ? (
        <Quiet says="No related notes." />
      ) : (
        <ul aria-label="Related notes" className="related-notes-list">
          {related.data.map(({ note, reasons }) => {
            const line = firstLine(note.text);
            return (
              <li key={note.id}>
                <span className="related-notes-text">
                  {onItem !== undefined ? (
                    <Button variant="quiet" onClick={() => onItem(formatItem({ kind: "note", id: note.id }))}>
                      {line}
                    </Button>
                  ) : (
                    <span>{line}</span>
                  )}
                  {reasons.length > 0 && (
                    <span className="related-notes-why">{reasons.map((r) => REASON_TEXT[r] ?? r).join(", ")}</span>
                  )}
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
