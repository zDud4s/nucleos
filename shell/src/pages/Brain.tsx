import { useEffect, useRef, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import {
  useCreateNote,
  useOwnerNotes,
  useSearchOwnerNotes,
  type NoteState,
  type OwnerNote,
} from "../data/owner-notes";
import { Button, ErrorNote, PageHeader, Panel, Quiet, RelativeTime, Row, Rows } from "../ui";
import "./brain.css";

/**
 * Brain — the owner's own notes, which no agent ever reads.
 *
 * A capture box above a list: what is typed here is kept as written and shown back, nothing more.
 * The `List | Graph` switch is the only place the notes are drawn as a map.
 */
export function Brain() {
  const [view, setView] = useState<"list" | "graph">("list");
  const [state, setState] = useState<NoteState | "all">("active");
  const [query, setQuery] = useState("");

  const searching = query.trim() !== "";
  const notes = useOwnerNotes(state);
  const found = useSearchOwnerNotes(query);
  const shown = searching ? found : notes;
  // A search spans every state, so the state switch narrows its results here.
  const rows = searching
    ? found.data?.filter((note) => state === "all" || note.state === state)
    : notes.data;

  return (
    <>
      <PageHeader title="Brain" headline="your own notes — no agent ever reads them" />

      <Capture />

      <div className="brain-filters">
        <div role="group" aria-label="View">
          {VIEWS.map(([value, label]) => (
            <Button
              key={value}
              variant="quiet"
              aria-pressed={view === value}
              onClick={() => setView(value)}
            >
              {label}
            </Button>
          ))}
        </div>
        {view === "list" && (
          <>
            <div role="group" aria-label="State">
              {STATES.map(([value, label]) => (
                <Button
                  key={value}
                  variant="quiet"
                  aria-pressed={state === value}
                  onClick={() => setState(value)}
                >
                  {label}
                </Button>
              ))}
            </div>
            <input
              type="search"
              className="brain-search"
              aria-label="Search notes"
              placeholder="Search notes"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </>
        )}
      </div>

      {view === "graph" ? (
        <BrainGraphPlaceholder />
      ) : (
        <>
          {shown.isError && (
            <ErrorNote>the núcleo did not answer — your notes are not known</ErrorNote>
          )}
          {rows !== undefined && rows.length === 0 && (
            <Quiet says={searching ? "No note matches that." : "No notes here yet."} />
          )}
          {rows !== undefined && rows.length > 0 && (
            <Panel title={searching ? "Matches" : "Notes"}>
              <Rows label="Notes">
                {rows.map((note) => (
                  <NoteRow key={note.id} note={note} />
                ))}
              </Rows>
            </Panel>
          )}
        </>
      )}
    </>
  );
}

const VIEWS: readonly (readonly ["list" | "graph", string])[] = [
  ["list", "List"],
  ["graph", "Graph"],
];

const STATES: readonly (readonly [NoteState | "all", string])[] = [
  ["active", "Active"],
  ["archived", "Archived"],
  ["all", "All"],
];

/**
 * The `capture` stamp a shortcut puts in the address to say "take a note now". A number or
 * nothing, as `validateVoiceSearch` does for `talk`: anything else is the same as not asking.
 */
export function validateBrainSearch(search: Record<string, unknown>): { capture?: number } {
  const capture = typeof search.capture === "number" ? search.capture : Number(search.capture);
  return Number.isFinite(capture) && search.capture !== undefined && search.capture !== ""
    ? { capture }
    : {};
}

/**
 * The capture box. Ctrl/Cmd+Enter saves; the box clears only once the daemon has answered, so a
 * refused note is still there to be sent again.
 */
function Capture() {
  const create = useCreateNote();
  const [text, setText] = useState("");
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const navigate = useNavigate();
  const { capture } = validateBrainSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const consumedRef = useRef<number | undefined>(undefined);

  // The stamp is removed from the address as it is consumed, so a reload does not refocus; the
  // consumed value is remembered because clearing the address is itself a navigation.
  useEffect(() => {
    if (capture === undefined || capture === consumedRef.current) return;
    consumedRef.current = capture;
    void navigate({ to: "/brain", search: {}, replace: true });
    areaRef.current?.focus();
  }, [capture, navigate]);

  const blank = text.trim() === "";
  const save = () => {
    if (blank || create.isPending) return;
    create.mutate({ text, origin: "shell" }, { onSuccess: () => setText("") });
  };

  return (
    <div className="brain-capture">
      <label className="brain-capture-label" htmlFor="brain-capture-text">
        Capture a note
      </label>
      <textarea
        id="brain-capture-text"
        ref={areaRef}
        className="brain-capture-input"
        rows={3}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            save();
          }
        }}
      />
      <div className="brain-capture-foot">
        <Button variant="approve" onClick={save} disabled={blank || create.isPending}>
          Save
        </Button>
        {create.isError && <ErrorNote>the núcleo did not answer — the note was not saved</ErrorNote>}
      </div>
    </div>
  );
}

function NoteRow({ note }: { note: OwnerNote }) {
  return (
    <Row className="brain-row">
      <p className="brain-text">{note.text}</p>
      <div className="brain-meta">
        <span className="brain-origin">{note.origin}</span>
        {note.state === "archived" && <span className="brain-state">archived</span>}
        <RelativeTime at={note.created_at} />
      </div>
    </Row>
  );
}

/** The Graph side of the switch. Replace this component with the real graph; nothing else here knows. */
export function BrainGraphPlaceholder() {
  return <Quiet says="The graph of your notes is not drawn yet." />;
}
