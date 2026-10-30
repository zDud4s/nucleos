import { useCallback, useEffect, useState, type KeyboardEvent } from "react";
import { DAEMON_NAME } from "../copy";
import { ApiRefusal } from "../data/client";
import {
  classifyWriteFailure,
  useSettingDoc,
  useWriteSetting,
  type SettingDoc,
  type WriteFailure,
} from "../data/settings";
import { Button, ErrorNote, Field, RefusalNote, Skeleton } from "../ui";
import { FieldNode } from "./FieldNode";
import { hasField, type SchemaNode } from "./schema";

export interface SettingsFormProps {
  scope: string;
  name: string;
  /** The index row's `takes_effect`. */
  takesEffect?: string;
  /** Whether there is an unsaved draft, for a page that shows several documents. */
  onDirty?: (dirty: boolean) => void;
}

type Mode = "form" | "raw";

/** The last thing a save said. It outlives the editor's remount, which is why the outer part holds it. */
type Notice = { kind: "saved" } | { kind: "failed"; failure: WriteFailure };

/** Completes "Takes effect ___." The núcleo writes a phrase; the two tokens are tolerated beside it. */
export function takesEffectPhrase(raw: string): string {
  if (raw === "restart") return "when the daemon restarts";
  if (raw === "live") return "straight away";
  return raw;
}

function pretty(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

function parseJson(text: string): { ok: true; value: unknown } | { ok: false } {
  try {
    return { ok: true, value: JSON.parse(text) as unknown };
  } catch {
    return { ok: false };
  }
}

function fieldErrors(
  failure: WriteFailure | null,
  mode: Mode,
  schema: SchemaNode,
  value: unknown,
): Record<string, string> {
  if (failure?.kind !== "invalid" || failure.field === null || mode !== "form") return {};
  return hasField(schema, value, failure.field) ? { [failure.field]: failure.message } : {};
}

function failureWords(failure: WriteFailure): string {
  switch (failure.kind) {
    case "stale":
      return "Someone changed this while you were editing. This is what is stored now; your edit was not saved.";
    case "kill_switch":
      return "the emergency stop is engaged, so nothing writes, including this.";
    case "not_ours":
      return `the ${DAEMON_NAME} does not consider this app the author of this document.`;
    case "invalid":
    case "refused":
      return failure.message;
    case "unreachable":
      return `the ${DAEMON_NAME} is not responding: nothing was written`;
  }
}

/**
 * One settings document, drawn from the schema the núcleo served beside it.
 *
 * The outer part owns the query, the mutation, the notice and the mode. The editor below it is keyed on
 * the version it shows, so a save or a 409 remounts it with the server's body, while a 422 answers the
 * same version and leaves the draft where it is.
 *
 * `held` is what makes that safe to rely on. A refetch can bring a new version at any moment, and a key
 * that followed it would swap the editor out from under somebody typing. So a dirty editor keeps the
 * document its draft started from, and the newer version waits until the save that would have clobbered
 * it answers 409.
 */
export function SettingsForm({ scope, name, takesEffect, onDirty }: SettingsFormProps) {
  const doc = useSettingDoc(scope, name);
  const write = useWriteSetting(scope, name);
  const [held, setHeld] = useState<SettingDoc | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [mode, setMode] = useState<Mode>("form");

  const onDirtyChange = useCallback(
    (dirty: boolean, showing: SettingDoc) => {
      setHeld(dirty ? showing : null);
      onDirty?.(dirty);
    },
    [onDirty],
  );

  const fail = (error: unknown) => {
    const failure = classifyWriteFailure(error);
    if (failure.kind === "stale") setHeld(null);
    setNotice({ kind: "failed", failure });
  };

  const shown = held ?? doc.data;

  if (shown === undefined) {
    if (doc.isPending) return <Skeleton label={`Reading ${name}`} lines={3} />;
    if (doc.error instanceof ApiRefusal) {
      if (doc.error.status === 422) {
        return <ErrorNote>This document does not read: {doc.error.detail}</ErrorNote>;
      }
      return <RefusalNote refusal={doc.error} />;
    }
    return (
      <ErrorNote onRetry={() => void doc.refetch()}>
        the {DAEMON_NAME} is not responding: nothing is known about this document
      </ErrorNote>
    );
  }

  return (
    <Editor
      key={shown.version}
      doc={shown}
      mode={mode}
      onMode={setMode}
      notice={notice}
      takesEffect={takesEffect}
      saving={write.isPending}
      busy={write.isPending || doc.isFetching}
      onEdit={() => setNotice(null)}
      onDirtyChange={onDirtyChange}
      onSave={(body, version) => {
        setNotice(null);
        write.mutate(
          { body, version },
          {
            onSuccess: () => {
              setHeld(null);
              setNotice({ kind: "saved" });
            },
            onError: fail,
          },
        );
      }}
    />
  );
}

interface EditorProps {
  doc: SettingDoc;
  mode: Mode;
  onMode: (mode: Mode) => void;
  notice: Notice | null;
  takesEffect?: string;
  saving: boolean;
  busy: boolean;
  onEdit: () => void;
  onDirtyChange: (dirty: boolean, showing: SettingDoc) => void;
  onSave: (body: unknown, version: number) => void;
}

function Editor({
  doc,
  mode,
  onMode,
  notice,
  takesEffect,
  saving,
  busy,
  onEdit,
  onDirtyChange,
  onSave,
}: EditorProps) {
  const initial: unknown = doc.body ?? {};
  const [value, setValue] = useState<unknown>(initial);
  const [rawText, setRawText] = useState(() => pretty(initial));
  const [rawProblem, setRawProblem] = useState(false);

  const stored = JSON.stringify(initial);
  const parsedRaw = mode === "raw" ? parseJson(rawText) : null;
  const dirty =
    parsedRaw === null
      ? JSON.stringify(value) !== stored
      : !parsedRaw.ok || JSON.stringify(parsedRaw.value) !== stored;

  useEffect(() => {
    onDirtyChange(dirty, doc);
  }, [onDirtyChange, dirty, doc]);

  const failure = notice?.kind === "failed" ? notice.failure : null;
  const errors = fieldErrors(failure, mode, doc.schema, value);
  const onField = Object.keys(errors).length > 0;
  const phrase = takesEffect === undefined ? null : takesEffectPhrase(takesEffect);

  const save = () => {
    if (!dirty || busy) return;
    if (parsedRaw === null) {
      onSave(value, doc.version);
    } else if (parsedRaw.ok) {
      onSave(parsedRaw.value, doc.version);
    } else {
      setRawProblem(true);
    }
  };

  const toggleMode = () => {
    if (mode === "form") {
      setRawText(pretty(value));
      setRawProblem(false);
      onMode("raw");
    } else if (parsedRaw?.ok === true) {
      setValue(parsedRaw.value);
      onMode("form");
    } else {
      setRawProblem(true);
    }
  };

  const revert = () => {
    setValue(initial);
    setRawText(pretty(initial));
    setRawProblem(false);
    onEdit();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      save();
    }
  };

  return (
    <div className="flex flex-col gap-3" onKeyDown={onKeyDown}>
      {doc.schema.description !== undefined && <p className="sy-note">{doc.schema.description}</p>}
      {doc.version === 0 && <p className="sy-meta">Never written: these are the defaults.</p>}
      {phrase !== null && <p className="sy-meta">Takes effect {phrase}.</p>}

      {mode === "form" ? (
        <FieldNode
          node={doc.schema}
          root={doc.schema}
          value={value}
          onChange={(next) => {
            setValue(next ?? {});
            onEdit();
          }}
          path=""
          label="Settings"
          errors={errors}
          bare
        />
      ) : (
        <Field label="Raw JSON">
          <textarea
            aria-invalid={rawProblem ? true : undefined}
            /* Inline, because `.ui-field textarea` sets `font-family: inherit` unlayered and a utility cannot win. */
            style={{ fontFamily: "var(--font-mono)" }}
            spellCheck={false}
            rows={14}
            value={rawText}
            onChange={(event) => {
              setRawText(event.target.value);
              setRawProblem(false);
              onEdit();
            }}
          />
        </Field>
      )}
      {rawProblem && (
        <p className="sy-note" role="alert">
          Not valid JSON
        </p>
      )}

      <div className="sy-setting-controls">
        <Button disabled={!dirty || busy} onClick={save}>
          {saving ? "Saving…" : "Save"}
        </Button>
        {dirty && !saving && (
          <Button variant="ghost" onClick={revert}>
            Revert
          </Button>
        )}
        <Button variant="ghost" aria-pressed={mode === "raw"} onClick={toggleMode}>
          {mode === "raw" ? "Form" : "Raw JSON"}
        </Button>
        {dirty && !saving && <span className="sy-meta">unsaved changes</span>}
        <span className="sy-meta" role="status">
          {notice?.kind === "saved" && (phrase === null ? "saved" : `saved and takes effect ${phrase}`)}
        </span>
      </div>

      {failure !== null && !onField && <ErrorNote>{failureWords(failure)}</ErrorNote>}
    </div>
  );
}
