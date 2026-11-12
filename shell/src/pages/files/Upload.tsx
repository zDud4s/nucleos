import { useEffect, useId, useRef, type ReactNode } from "react";
import { pluralWord } from "../../lib/plural";
import { Button } from "../../ui";
import { type ClashChoice, MAX_UPLOAD_LABEL, onEscape, type PendingBatch, type UploadReport } from "./shared";

/* -------------------------------------------------------------- upload -- */

/**
 * The picker. It only hands the files over: the page runs every upload, from
 * this button and from a drop alike, through one loop and one report.
 *
 * The daemon's ceiling is said beside it, before a file is picked, rather than
 * only in the refusal after one too large was.
 */
export function UploadButton({ busy, onFiles }: { busy: boolean; onFiles: (files: File[]) => void }) {
  return (
    <div className="fi-upload">
      <label className="fi-upload-label" title={`Up to ${MAX_UPLOAD_LABEL} each`}>
        <span>Upload</span>
        <input
          type="file"
          className="sr-only"
          multiple
          disabled={busy}
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            // Cleared so picking the same file again is still a change.
            event.target.value = "";
            if (files.length > 0) onFiles(files);
          }}
        />
      </label>
      <span className="fi-upload-limit">up to {MAX_UPLOAD_LABEL} each</span>
    </div>
  );
}

/**
 * The one question an upload asks: some names are already taken where the
 * files are going.
 *
 * It used to be no question at all — the daemon numbers a clash
 * (`report.docx` becomes `report (2).docx`) and the page said so only in the
 * report afterwards, where a person who meant to update the file found a
 * second copy instead. Replace is the answer that means "update": the old file
 * goes to Recently deleted, not away. The pending tone, as the not-empty note:
 * nothing is wrong yet, the page is waiting on a person.
 */
/**
 * How many clashing names the question lists before counting the rest. The
 * answer is one choice for all of them, so the list is there to recognise the
 * batch, not to be read to its end; a drop of forty names pushed the buttons
 * off the screen.
 */
const CLASH_SHOWN = 5;

export function ClashNote({
  batch,
  onChoose,
  onCancel,
}: {
  batch: PendingBatch;
  onChoose: (choice: ClashChoice) => void;
  onCancel: () => void;
}) {
  const headingId = useId();
  const thenId = useId();
  const noteRef = useRef<HTMLDivElement>(null);
  // Focus on the answer that loses nothing, as the not-empty note lands on
  // Leave: Enter pressed on arrival keeps both files rather than replacing one.
  useEffect(() => {
    noteRef.current?.querySelector<HTMLElement>("[data-safe]")?.focus();
  }, []);
  const names = batch.clashes.flatMap((index) => {
    const item = batch.items[index];
    return item === undefined ? [] : [item];
  });
  const one = names.length === 1;
  const others = batch.items.length - names.length;
  // Written as a path, the way the move form writes its destination: "the
  // files root" in the path's mono face read as a folder named that.
  const folder = names[0]?.folder ?? "";
  const where = folder === "" ? "files/" : `files/${folder}/`;

  return (
    <div
      ref={noteRef}
      className="fi-ask"
      role="group"
      aria-labelledby={headingId}
      aria-describedby={thenId}
      onKeyDown={onEscape(onCancel)}
    >
      <p id={headingId}>
        {one ? "A file with this name is already in " : `${String(names.length)} files with these names are already in `}
        <span className="fi-ask-where">{where}</span>:
      </p>
      <ul className="fi-ask-list">
        {names.slice(0, CLASH_SHOWN).map((item, index) => (
          <li key={`${item.folder}/${item.name}-${String(index)}`}>{item.name}</li>
        ))}
        {names.length > CLASH_SHOWN && <li className="fi-ask-more">and {names.length - CLASH_SHOWN} more</li>}
      </ul>
      <div className="fi-ask-actions">
        {/* The answer that loses nothing first, and the filled one — the fill is
            `files.css`'s, on `data-safe`: Replace led, and the pointer met the lossy
            choice before the one focus lands on. */}
        <Button data-safe onClick={() => onChoose("keep")}>
          Keep both
        </Button>
        <Button variant="ghost" onClick={() => onChoose("replace")}>
          Replace
        </Button>
        <Button variant="ghost" onClick={() => onChoose("skip")}>
          {/* Named for what still happens when others are going: "Skip those"
              left a person to work out that the rest went up regardless. */}
          {others > 0 ? "Upload the rest" : `Skip ${one ? "it" : "those"}`}
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          Cancel upload
        </Button>
      </div>
      {/* Under the buttons, as their footnote: above them it was one more
          paragraph to read before the choice it explains came into view. */}
      <p id={thenId} className="fi-ask-then">
        Replace moves the {one ? "old one" : "old ones"} to Recently deleted. Keep both saves the new{" "}
        {one ? "file" : "files"} under a numbered name.
        {others > 0 && ` The other ${String(others)} ${pluralWord(others, "file goes", "files go")} up either way.`}
      </p>
    </div>
  );
}

/**
 * What a batch of uploads came to, whichever door it came through.
 *
 * The names the daemon stored are listed, and a name that differs from the one
 * sent says so beside it — `safe_filename` rewrites what a disk cannot carry,
 * and a Keep both adds a number. Every file that stayed behind is named with
 * its reason, each on its own line even when two reasons read the same.
 */
/**
 * How many lines of one kind the report shows before folding the rest. A drop
 * of two hundred files listed every one back, and pushed the table off the
 * screen until it was dismissed.
 */
const REPORT_SHOWN = 5;

/** A list cut to {@link REPORT_SHOWN}, the rest behind one line that counts them. */
function FoldedList({ children }: { children: ReactNode[] }) {
  const shown = children.slice(0, REPORT_SHOWN);
  const rest = children.slice(REPORT_SHOWN);
  return (
    <>
      <ul className="fi-drop-refusals">{shown}</ul>
      {rest.length > 0 && (
        <details className="fi-drop-more">
          <summary>and {rest.length} more</summary>
          <ul className="fi-drop-refusals">{rest}</ul>
        </details>
      )}
    </>
  );
}

export function UploadReportNote({ report, onDismiss }: { report: UploadReport; onDismiss: () => void }) {
  const from = report.source === "drop" ? "that drop" : "that upload";
  const total = report.saved.length + report.refusals.length;
  // A name the daemon changed first: those are the lines the report is for.
  const saved = [...report.saved].sort(
    (a, b) => Number(b.stored !== b.sent) - Number(a.stored !== a.sent),
  );
  const skippedShown = report.skipped.slice(0, REPORT_SHOWN);
  const skippedMore = report.skipped.length - skippedShown.length;
  return (
    <div className="fi-drop-report">
      <div role="status">
        {report.truncated && <p>The drop was cut short by the host before this window saw everything.</p>}
        {total === 0 ? (
          <p>Nothing from {from} was sent.</p>
        ) : report.refusals.length === 0 ? (
          <p>
            {total === 1 ? `The file from ${from} was uploaded` : `Every file from ${from} was uploaded`}
            {report.saved.length > 0 ? " as:" : "."}
          </p>
        ) : (
          <p>
            {report.refusals.length} of {total} {pluralWord(total, "file")} from {from} stayed behind:
          </p>
        )}
        {report.refusals.length > 0 && (
          <FoldedList>
            {report.refusals.map((refusal, index) => (
              <li key={`${refusal.name}-${String(index)}`}>{`${refusal.name}: ${refusal.reason}`}</li>
            ))}
          </FoldedList>
        )}
        {report.refusals.length > 0 && report.saved.length > 0 && <p>Uploaded as:</p>}
        {saved.length > 0 && (
          <FoldedList>
            {saved.map((item, index) => (
              <li key={`${item.stored}-${String(index)}`}>
                <span>{item.stored}</span>
                {item.stored !== item.sent && <span className="fi-drop-renamed"> (renamed from {item.sent})</span>}
              </li>
            ))}
          </FoldedList>
        )}
        {report.replaced > 0 && (
          <p>
            {report.replaced === 1 ? "The older copy is" : `The ${String(report.replaced)} older copies are`} in Recently
            deleted.
          </p>
        )}
        {report.skipped.length > 0 && (
          <p>
            Skipped, already there: {skippedShown.join(", ")}
            {skippedMore > 0 && ` and ${String(skippedMore)} more`}.
          </p>
        )}
      </div>
      <Button variant="quiet" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}
