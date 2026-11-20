import { useState } from "react";
import { classifyWriteFailure, useSettingDoc, useSettingsIndex, useWriteSetting } from "../data/settings";
import { Button, ConflictNote, Tip } from "../ui";

const NAME = "browser";
const KEY = "local_origins";

/**
 * A loopback origin: http/https, localhost / 127.0.0.1 / [::1], an explicit port, no path.
 * Client-side shape check only; the núcleo is the judge and its sentence is shown if it refuses.
 */
const ORIGIN = /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\]):(\d{1,5})$/i;

/** Returns the trimmed origin if it has a valid loopback shape, else null. */
export function parseLocalOrigin(raw: string): string | null {
  const text = raw.trim();
  const match = ORIGIN.exec(text);
  if (match === null) return null;
  const port = Number(match[2]);
  return port >= 1 && port <= 65535 ? text : null;
}

/**
 * The loopback origins this project's agent browser may open (`browser.local_origins`), edited as a
 * list with Add and Remove. Each change is a `PUT` of the whole `browser` document at the version
 * that was read; the núcleo answers 422 with a sentence, shown as it came.
 */
export function LocalOrigins({ projectId }: { projectId: string }) {
  const scope = `project:${projectId}`;
  const index = useSettingsIndex(scope);
  if (index.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;
  const listed = index.data?.some((row) => row.scope === scope && row.name === NAME);
  if (index.data !== undefined && listed !== true) {
    return <p className="text-sm text-text-muted" title={`This project has no ${NAME} document to edit.`}>Not set up</p>;
  }
  return <Editor scope={scope} />;
}

function storedOrigins(body: unknown): string[] {
  if (typeof body !== "object" || body === null) return [];
  const raw = (body as Record<string, unknown>)[KEY];
  return Array.isArray(raw) ? raw.filter((v): v is string => typeof v === "string") : [];
}

function Editor({ scope }: { scope: string }) {
  const doc = useSettingDoc(scope, NAME);
  const write = useWriteSetting(scope, NAME);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);

  if (doc.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;
  if (doc.data === undefined) return <p className="text-sm text-text-muted">Could not read the browser settings.</p>;

  const origins = storedOrigins(doc.data.body);
  const base: Record<string, unknown> =
    typeof doc.data.body === "object" && doc.data.body !== null ? { ...doc.data.body } : {};
  const version = doc.data.version;

  function commit(next: string[], onDone?: () => void) {
    setError(null);
    setStale(false);
    write.mutate(
      { body: { ...base, [KEY]: next }, version },
      {
        onSuccess: () => onDone?.(),
        onError: (failure) => {
          const f = classifyWriteFailure(failure);
          if (f.kind === "stale") setStale(true);
          else setError(sentenceOf(f));
        },
      },
    );
  }

  function add() {
    const origin = parseLocalOrigin(draft);
    if (origin === null) {
      setError("Use http(s)://localhost, 127.0.0.1 or [::1] with a port and no path.");
      return;
    }
    if (origins.includes(origin)) {
      setError("Already in the list.");
      return;
    }
    commit([...origins, origin], () => setDraft(""));
  }

  return (
    <div className="flex flex-col gap-2">
      {stale ? <ConflictNote>Someone else changed the browser settings. Showing the latest version — try again.</ConflictNote> : null}

      <div className="text-sm">
        <Tip tip="Loopback origins the project's agent browser may open, e.g. http://localhost:5173">
          Local origins
        </Tip>
      </div>

      <ul aria-label="Local origins" className="flex flex-col gap-1">
        {origins.map((origin) => (
          <li key={origin} className="flex items-center gap-2">
            <span className="font-mono text-sm">{origin}</span>
            <Button
              variant="ghost"
              aria-label={`Remove ${origin}`}
              disabled={write.isPending}
              onClick={() => commit(origins.filter((o) => o !== origin))}
            >
              Remove
            </Button>
          </li>
        ))}
      </ul>

      <div className="flex items-center gap-2">
        <input
          aria-label="New origin"
          placeholder="http://localhost:5173"
          value={draft}
          spellCheck={false}
          onChange={(event) => {
            setDraft(event.target.value);
            setError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") add();
          }}
          className="box-border min-w-0 flex-1"
        />
        <Button variant="ghost" disabled={draft.trim() === "" || write.isPending} onClick={add}>
          Add
        </Button>
      </div>

      {error !== null ? (
        <p role="alert" className="ui-note ui-note-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** One sentence for each way a write can fail; the núcleo's own words win where it has them. */
function sentenceOf(failure: ReturnType<typeof classifyWriteFailure>): string {
  switch (failure.kind) {
    case "invalid":
    case "refused":
      return failure.message;
    case "kill_switch":
      return "The emergency stop is engaged, so nothing was saved.";
    case "not_ours":
      return "The daemon does not let this window edit this document, so nothing was saved.";
    case "unreachable":
      return "The daemon gave no reply, so nothing was saved.";
    case "stale":
      return "Someone else changed the browser settings.";
  }
}
