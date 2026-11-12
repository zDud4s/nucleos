import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { XIcon } from "lucide-react";
import { IconButton } from "../ui";
import { STATE_TEXT } from "./format";
import { useNodeEntry } from "./hooks";
import { identityOf, useParentHarness } from "./identity";
import { useTranscript } from "./transcript";
import type { TranscriptEntry } from "./types";

const NEAR_BOTTOM = 40;
const OTHER_ESCAPE = "select, input, textarea, [role='dialog'], [role='alertdialog'], [role='listbox'], [role='menu'], [aria-modal='true']";

/**
 * The full transcript of one node, ~480 px on the right: "Load earlier" on top, new lines
 * entering at the bottom, tools expanding on click, Esc closing. Focus goes to the close button
 * on open and back to whatever opened it on close.
 */
export function TranscriptPanel({ id, onClose }: { id: string; onClose(): void }) {
  const entry = useNodeEntry(id);
  const parentHarness = useParentHarness(entry?.node.parent_id ?? null);
  const t = useTranscript(id);
  const body = useRef<HTMLDivElement>(null);
  const panel = useRef<HTMLElement>(null);
  const stick = useRef(true);
  const anchor = useRef<number | null>(null);
  const seenPrepends = useRef(0);

  useEffect(() => {
    const opener = document.activeElement;
    panel.current?.querySelector<HTMLButtonElement>(".fleet-panel-title .ui-icon-button")?.focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      // An Escape meant for a control or dialog elsewhere on the page (the Project select, a modal)
      // is not ours; one from inside the panel, or from nowhere in particular, closes it.
      const target = event.target;
      if (target instanceof Element && !panel.current?.contains(target) && target.closest(OTHER_ESCAPE) !== null) return;
      onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // After a render that added rows: keep the reader's place when an older page went on top,
  // otherwise follow the bottom while they are there.
  useLayoutEffect(() => {
    const el = body.current;
    if (el === null) return;
    if (anchor.current !== null && t.prepended !== seenPrepends.current) {
      el.scrollTop += el.scrollHeight - anchor.current;
      anchor.current = null;
      seenPrepends.current = t.prepended;
    } else if (stick.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [t.rows.length, t.prepended]);

  const label = entry?.node.label ?? id;

  return (
    <aside ref={panel} className="fleet-panel" aria-label={`Transcript of ${label}`}>
      <header className="fleet-panel-head">
        <div className="fleet-panel-title">
          <span className="fleet-label" title={label}>
            {label}
          </span>
          {entry?.node.role != null && <span className="fleet-role">{entry.node.role}</span>}
          <IconButton label="Close the transcript" icon={XIcon} onClick={onClose} />
        </div>
        {entry !== undefined && (
          <div className="fleet-panel-sub">
            {identityOf(entry.node, parentHarness).text} · {STATE_TEXT[entry.node.state]}
          </div>
        )}
      </header>
      <div
        ref={body}
        className="fleet-panel-body"
        onScroll={(event) => {
          const el = event.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM;
        }}
      >
        {t.before !== null && (
          <button
            type="button"
            className="fleet-earlier"
            disabled={t.loadingEarlier}
            onClick={() => {
              anchor.current = body.current?.scrollHeight ?? null;
              t.loadEarlier();
            }}
          >
            Load earlier
          </button>
        )}
        {t.rows.map((row) => (
          <EntryRow key={row.key} entry={row.entry} />
        ))}
        {t.status === "loading" && <p role="status" className="fleet-panel-note">Loading the transcript…</p>}
        {t.status === "none" && <p className="fleet-panel-note">No full transcript is kept for this node.</p>}
        {t.status === "gone" && <p className="fleet-panel-note">This node is gone.</p>}
        {t.status === "failed" && <p className="fleet-panel-note">The transcript could not be read.</p>}
      </div>
    </aside>
  );
}

/** One entry. Anything with a `detail` (a tool's input, a result's body) is a button that opens it. */
function EntryRow({ entry }: { entry: TranscriptEntry }) {
  const [open, setOpen] = useState(false);
  const classes = `fleet-ln fleet-ln-${entry.kind}`;
  if (entry.detail === null || entry.detail === "") return <div className={classes}>{entry.text}</div>;
  return (
    <>
      <button type="button" className={`${classes} fleet-ln-open`} aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        {entry.text}
      </button>
      {open && <pre className="fleet-more">{entry.detail}</pre>}
    </>
  );
}
