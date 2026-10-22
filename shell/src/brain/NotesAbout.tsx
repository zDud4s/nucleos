import { DAEMON_NAME } from "../copy";
import { useNotesAbout, type TargetKind } from "../data/owner-notes";
import { Button, ErrorNote, Quiet, Skeleton } from "../ui";
import { formatItem } from "./item-ref";
import "./NotesAbout.css";

/** Longest first line a row shows before it is cut. */
const MAX_LINE = 80;

/** The plain first non-blank line of a note, cut at `MAX_LINE`: a note has no title of its own. */
function firstLine(text: string): string {
  const first = text.split("\n").find((line) => line.trim() !== "")?.trim() ?? "";
  if (first === "") return "(empty note)";
  return first.length > MAX_LINE ? `${first.slice(0, MAX_LINE - 1)}…` : first;
}

export interface NotesAboutProps {
  kind: TargetKind;
  ref: string;
  /** A note picked: `note:<id>`, the Brain `?item=` format. Without it the rows are read-only. */
  onItem?: (item: string) => void;
}

/**
 * "Notes about this": the notes that link to one target (a project, a contact, a mail...), in the
 * daemon's order (active first). A gone target still lists its notes, so there is no existence check.
 */
export function NotesAbout({ kind, ref, onItem }: NotesAboutProps) {
  const about = useNotesAbout(kind, ref);

  return (
    <section className="brain-sec notes-about" aria-label="Notes about this">
      <h4>Notes about this</h4>
      {about.data === undefined ? (
        // A failed refetch keeps the rows already known; the error shows only when there are none.
        about.isError ? (
          <ErrorNote>the {DAEMON_NAME} is not responding: the notes are not known</ErrorNote>
        ) : (
          <Skeleton label="Loading the notes" lines={3} />
        )
      ) : about.data.length === 0 ? (
        <Quiet says="No notes about this yet." />
      ) : (
        <ul aria-label="Notes about this" className="brain-links notes-about-list">
          {about.data.map(({ note, link_id, link_type }) => (
            <li key={link_id}>
              <span className="brain-link-type">{link_type}</span>
              <span className="brain-link-target">
                {firstLine(note.text)}
                {note.state === "archived" && <span className="brain-node-tag">archived</span>}
              </span>
              {onItem !== undefined && (
                <span className="brain-link-act">
                  <Button
                    variant="quiet"
                    aria-label={`Open note ${note.id}`}
                    onClick={() => onItem(formatItem({ kind: "note", id: note.id }))}
                  >
                    Open
                  </Button>
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
