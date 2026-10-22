import { DAEMON_NAME } from "../copy";
import { useEffect, useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { ApiRefusal } from "../data/client";
import { useContacts } from "../data/contacts";
import { useKnowledge } from "../data/knowledge";
import { useMailQueue } from "../data/mail";
import {
  LINK_TYPES,
  TARGET_KINDS,
  useAddLink,
  useSearchOwnerNotes,
  type LinkType,
  type TargetKind,
} from "../data/owner-notes";
import { useRuns } from "../data/runs";
import { useProjects } from "../data/system";
import { Button, ErrorNote, Quiet, RefusalNote, Select, Skeleton } from "../ui";
import "./LinkPicker.css";

/**
 * The "Add link" form of a note, with a search-as-you-type picker in place of a free-text target.
 *
 * Which hook answers a kind, and what its `target_ref` is (the same ref the core stores and the
 * graph labels):
 *
 * | kind        | source                     | ref                       |
 * |-------------|----------------------------|---------------------------|
 * | `note`      | `useSearchOwnerNotes(q)`   | the note id               |
 * | `knowledge` | `useKnowledge()`, filtered | the knowledge id          |
 * | `project`   | `useProjects()`, filtered  | the project id            |
 * | `contact`   | `useContacts()`, filtered  | the contact id (not the address) |
 * | `mail`      | `useMailQueue(q)`          | the email id              |
 * | `job`       | `useRuns({ q })`           | the run id (a job is a run) |
 * | `file`      | none: free text            | a path relative to the files root |
 *
 * `file` stays free text: its only search is a recursive walk of the whole tree on the daemon,
 * too heavy to fire at every keystroke for a field whose value is a path the owner already knows.
 *
 * Only the active kind's hook is mounted: each source is its own component, rendered under a
 * `key` of the kind, so the other kinds never fetch. A typed value that matches nothing is still
 * a valid ref (submit takes it as typed), so a gone or unlisted target can still be linked.
 */

/** One row of the picker's list. */
interface Choice {
  /** What is sent as `target_ref`. */
  ref: string;
  label: string;
  /** The small second line that tells two rows with the same label apart. */
  detail: string;
}

/** What a source answers for the current query. */
interface Found {
  choices: Choice[];
  loading: boolean;
  failed: boolean;
  /** What to say while the query is blank and there is nothing to list. */
  idle?: string;
}

interface SourceProps {
  query: string;
  /** The note being linked from: it is never offered as its own target. */
  noteId: number;
  children: (found: Found) => ReactNode;
}

const MAX_CHOICES = 8;
const DEBOUNCE_MS = 200;
const EMPTY: never[] = [];

/** Failure of the add itself: the daemon's refusal as written, or "no answer". */
function Failure({ error }: { error: unknown }) {
  if (error instanceof ApiRefusal) return <RefusalNote refusal={error} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding</ErrorNote>;
}

/** A value that follows `value` after it has been still for `DEBOUNCE_MS`: one request per pause, not per key. */
function useDebounced(value: string): string {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [value]);
  return settled;
}

/** First line of a text, trimmed and cut: a note or a prompt has no title of its own. */
function firstLine(text: string, fallback: string): string {
  const first = text.trim().split("\n")[0] ?? "";
  if (first === "") return fallback;
  return first.length > 60 ? `${first.slice(0, 59)}…` : first;
}

function matches(query: string, ...fields: (string | null | undefined)[]): boolean {
  const needle = query.trim().toLowerCase();
  if (needle === "") return true;
  return fields.some((field) => field?.toLowerCase().includes(needle) === true);
}

/* ---------------------------------------------------------------- sources -- */

function NoteSource({ query, noteId, children }: SourceProps) {
  const settled = useDebounced(query);
  const search = useSearchOwnerNotes(settled);
  const choices = (search.data ?? EMPTY)
    .filter((note) => note.id !== noteId)
    .map((note) => ({ ref: String(note.id), label: firstLine(note.text, "(empty note)"), detail: `#${note.id} · ${note.state}` }));
  return children({
    choices: settled === query ? choices : [],
    loading: search.isLoading || settled !== query,
    failed: search.isError,
    idle: "Type to search your notes.",
  });
}

function KnowledgeSource({ query, children }: SourceProps) {
  const knowledge = useKnowledge();
  const choices = (knowledge.data ?? EMPTY)
    .filter((row) => matches(query, row.title, row.body))
    .map((row) => ({ ref: String(row.id), label: row.title, detail: `#${row.id} · ${row.layer} · ${row.status}` }));
  return children({ choices, loading: knowledge.isLoading, failed: knowledge.isError });
}

function ProjectSource({ query, children }: SourceProps) {
  const projects = useProjects();
  const choices = (projects.data ?? EMPTY)
    .filter((project) => matches(query, project.project_id, project.project_root))
    .map((project) => ({
      ref: project.project_id,
      label: project.project_id,
      detail: project.project_root ?? "no root on disk yet",
    }));
  return children({ choices, loading: projects.isLoading, failed: projects.isError });
}

function ContactSource({ query, children }: SourceProps) {
  const contacts = useContacts();
  // The roster has one row per address; two rows sharing a contact id are one person.
  const seen = new Set<number>();
  const choices: Choice[] = [];
  for (const row of contacts.data ?? EMPTY) {
    if (seen.has(row.contact_id) || !matches(query, row.display_name, row.address)) continue;
    seen.add(row.contact_id);
    choices.push({ ref: String(row.contact_id), label: row.display_name ?? row.address, detail: row.address });
  }
  return children({ choices, loading: contacts.isLoading, failed: contacts.isError });
}

function MailSource({ query, children }: SourceProps) {
  const settled = useDebounced(query);
  const mail = useMailQueue(settled);
  const choices = (mail.data ?? EMPTY).map((email) => ({
    ref: String(email.id),
    label: email.subject?.trim() || "(no subject)",
    detail: email.from_name ?? email.from_addr,
  }));
  return children({
    choices: settled === query ? choices : [],
    loading: mail.isLoading || settled !== query,
    failed: mail.isError,
  });
}

function JobSource({ query, children }: SourceProps) {
  const settled = useDebounced(query);
  const runs = useRuns({ q: settled });
  const choices = (runs.data ?? EMPTY).map((run) => ({
    ref: String(run.id),
    label: firstLine(run.prompt_excerpt, `run ${run.id}`),
    detail: `#${run.id} · ${run.status}${run.project_id !== null ? ` · ${run.project_id}` : ""}`,
  }));
  return children({
    choices: settled === query ? choices : [],
    loading: runs.isLoading || settled !== query,
    failed: runs.isError,
  });
}

/** The kinds with a searchable source; a kind missing here keeps the plain field. */
const SOURCES: Partial<Record<TargetKind, (props: SourceProps) => ReactNode>> = {
  note: NoteSource,
  knowledge: KnowledgeSource,
  project: ProjectSource,
  contact: ContactSource,
  mail: MailSource,
  job: JobSource,
};

const PLACEHOLDERS: Record<TargetKind, string> = {
  note: "Search notes",
  knowledge: "Search lessons",
  project: "Search projects",
  contact: "Search contacts",
  mail: "Search mail",
  file: "id or path",
  job: "Search runs",
};

/* --------------------------------------------------------------- combobox -- */

interface ComboboxProps {
  found: Found;
  kind: TargetKind;
  text: string;
  /** A row was chosen: the text is its label, no longer a query. */
  picked: boolean;
  onType: (value: string) => void;
  onPick: (choice: Choice) => void;
}

/**
 * The input and its list, as siblings: the input stays a direct child of the form, where the
 * form's own styles reach it.
 *
 * Focus never leaves the input; the highlighted row is announced through `aria-activedescendant`.
 * A row is picked by Enter (when one is highlighted, otherwise Enter submits what was typed) or by
 * a click; Esc closes the list and nothing else.
 */
function Combobox({ found, kind, text, picked, onType, onPick }: ComboboxProps) {
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const listId = useId();
  const choices = found.choices.slice(0, MAX_CHOICES);
  const showing = open && !picked;
  const expanded = showing && choices.length > 0;
  const active = Math.min(highlight, choices.length - 1);

  function pick(choice: Choice) {
    setOpen(false);
    setHighlight(-1);
    onPick(choice);
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape" && showing) {
      // The list closes; whatever panel holds the form stays.
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      setHighlight(-1);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (choices.length === 0) return;
      event.preventDefault();
      setOpen(true);
      const down = event.key === "ArrowDown";
      setHighlight(down ? (active + 1) % choices.length : active <= 0 ? choices.length - 1 : active - 1);
    } else if (event.key === "Enter" && expanded && active >= 0) {
      event.preventDefault();
      const choice = choices[active];
      if (choice !== undefined) pick(choice);
    }
  }

  let note: ReactNode = null;
  if (showing && choices.length === 0) {
    if (found.failed) note = <Quiet compact says="Search is unavailable: type the ref." />;
    else if (found.loading) note = <Skeleton label="Searching" />;
    else if (text.trim() === "" && found.idle !== undefined) note = <Quiet compact says={found.idle} />;
    else note = <Quiet compact says="No match: the typed value is used as the ref." />;
  }

  return (
    <>
      <input
        role="combobox"
        aria-label="Target ref"
        aria-autocomplete="list"
        aria-expanded={expanded}
        aria-controls={expanded ? listId : undefined}
        aria-activedescendant={expanded && active >= 0 ? `${listId}-${active}` : undefined}
        autoComplete="off"
        placeholder={PLACEHOLDERS[kind]}
        value={text}
        onChange={(event) => {
          setOpen(true);
          setHighlight(-1);
          onType(event.target.value);
        }}
        onFocus={() => setOpen(true)}
        onClick={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onKeyDown={onKeyDown}
      />
      {expanded && (
        <div className="brain-pick">
          <ul id={listId} role="listbox" aria-label="Matching targets" className="brain-pick-list">
            {choices.map((choice, index) => (
              <li
                key={choice.ref}
                id={`${listId}-${index}`}
                role="option"
                aria-selected={index === active}
                className="brain-pick-row"
                // Keep focus in the input: a blur would close the list before the click lands.
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => pick(choice)}
              >
                <span className="brain-pick-label">{choice.label}</span>
                <span className="brain-pick-detail">{choice.detail}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {note !== null && <div className="brain-pick">{note}</div>}
    </>
  );
}

/* ------------------------------------------------------------------- form -- */

export function AddLinkForm({ id }: { id: number }) {
  const add = useAddLink();
  const [type, setType] = useState<LinkType>("relates");
  const [kind, setKind] = useState<TargetKind>("note");
  // `text` is what the input shows; `ref` is what is sent. They differ once a row is picked
  // (the label is shown, the id is sent) and agree while the owner types.
  const [text, setText] = useState("");
  const [ref, setRef] = useState("");
  const [picked, setPicked] = useState(false);
  const blank = ref.trim() === "";

  function clear() {
    setText("");
    setRef("");
    setPicked(false);
  }

  const Source = SOURCES[kind];
  const typed = (value: string) => {
    setText(value);
    setRef(value.trim());
    setPicked(false);
  };

  return (
    <form
      className="brain-panel-form brain-addlink"
      aria-label="Add link"
      onSubmit={(event) => {
        event.preventDefault();
        if (blank || add.isPending) return;
        add.mutate({ noteId: id, link_type: type, target_kind: kind, target_ref: ref.trim() }, { onSuccess: clear });
      }}
    >
      <Select
        density="sm"
        mono
        aria-label="Link type"
        value={type}
        onChange={(event) => setType(event.target.value as LinkType)}
      >
        {LINK_TYPES.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </Select>
      <Select
        density="sm"
        mono
        aria-label="Target kind"
        value={kind}
        onChange={(event) => {
          setKind(event.target.value as TargetKind);
          // A ref names one kind's thing: it does not carry over to the next kind.
          clear();
        }}
      >
        {TARGET_KINDS.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </Select>
      {Source === undefined ? (
        <input
          aria-label="Target ref"
          placeholder={PLACEHOLDERS[kind]}
          value={text}
          onChange={(event) => typed(event.target.value)}
        />
      ) : (
        // Keyed by the kind: switching mounts the new source and unmounts the old one, so only
        // the active kind's hook is ever live.
        <Source key={kind} query={picked ? "" : text} noteId={id}>
          {(found) => (
            <Combobox
              found={found}
              kind={kind}
              text={text}
              picked={picked}
              onType={typed}
              onPick={(choice) => {
                setText(choice.label);
                setRef(choice.ref);
                setPicked(true);
              }}
            />
          )}
        </Source>
      )}
      <Button type="submit" variant="ghost" disabled={blank || add.isPending}>
        Add link
      </Button>
      {add.isError && <Failure error={add.error} />}
    </form>
  );
}
