import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { ingestChats, useDistillerSources, useSetDistillerSources } from "../data/knowledge";
import { useProjects } from "../data/system";
import { useCaptureConfig, useSetCaptureConfig, type CaptureConfigReading } from "../data/capture-config";
import { Button, ErrorNote, Field, RefusalNote, Select, Switch } from "../ui";
import { DAEMON_NAME } from "../copy";

/**
 * How capture questions are asked: the answer window in days and the hour of the daily digest, as
 * two numbers at the top of this machine's settings. Each saves on blur and on Enter, and only when
 * the typed value is a whole number that differs from the stored one — an empty or half-typed box
 * never reaches the daemon, and only the changed field is sent.
 */
export function CaptureWait() {
  const stored = useCaptureConfig();
  const set = useSetCaptureConfig();
  const refusal = [set.error, stored.error].find(isApiRefusal);

  return (
    <>
    <div className="sy-pref">
      <ConfigNumber
        field="answer_days"
        id="sy-capture-days"
        label="Answer window (days)"
        helper="0 turns capture questions off; at most 30."
        max={30}
        current={stored.data?.answer_days}
        set={set}
      />
      <ConfigNumber
        field="digest_hour"
        id="sy-capture-hour"
        label="Digest hour"
        helper="Local hour the daily digest is sent."
        max={23}
        current={stored.data?.digest_hour}
        set={set}
      />
      {refusal ? (
        <RefusalNote refusal={refusal} />
      ) : set.error ? (
        <ErrorNote>the {DAEMON_NAME} is not responding: the capture settings were not changed</ErrorNote>
      ) : null}
    </div>
    <DistillerSources />
    </>
  );
}

/**
 * Which sources the distiller reads (conversations and calendar, both on by default), and the
 * owner's one-time request to let it read the conversations from before it was switched on. Each
 * switch sends only the field that changed; the ingest button only marks chats, it asks no model.
 */
function DistillerSources() {
  const sources = useDistillerSources();
  const set = useSetDistillerSources();
  const projects = useProjects();
  const [project, setProject] = useState("");
  const [result, setResult] = useState<{ seeded: number; skipped: number } | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);

  // An absent answer reads as on, like the daemon: only a stored off turns a source off.
  const chatOn = sources.data?.chat !== false;
  const calendarOn = sources.data?.calendar !== false;
  const refusal = [set.error, sources.error].find(isApiRefusal);

  async function ingest() {
    setBusy(true);
    setFailed(false);
    try {
      setResult(await ingestChats(project === "" ? {} : { project }));
    } catch {
      setResult(null);
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="sy-pref">
      <Switch label="Conversations" checked={chatOn} disabled={set.isPending} onChange={(on) => set.mutate({ chat: on })} />
      <Switch label="Calendar" checked={calendarOn} disabled={set.isPending} onChange={(on) => set.mutate({ calendar: on })} />
      {refusal ? <RefusalNote refusal={refusal} /> : null}
      <Field
        label="Project to ingest"
        helper="Sends up to 40 exchanges per chat to the distiller model, a few chats a day."
      >
        <Select
          value={project}
          disabled={busy}
          onChange={(event) => setProject(event.target.value)}
        >
          <option value="">All projects</option>
          {(Array.isArray(projects.data) ? projects.data : []).map((p) => (
            <option key={p.project_id} value={p.project_id}>
              {p.project_id}
            </option>
          ))}
        </Select>
      </Field>
      <Button disabled={!chatOn || busy} onClick={() => void ingest()}>
        Ingest past conversations
      </Button>
      {!chatOn ? <p>Turn conversations on to ingest them</p> : null}
      {result ? (
        <p role="status">
          {result.seeded} conversations queued, {result.skipped} already ingested
        </p>
      ) : null}
      {failed ? <ErrorNote>the daemon is not responding: nothing was ingested</ErrorNote> : null}
    </div>
  );
}

function ConfigNumber({
  field,
  id,
  label,
  helper,
  max,
  current,
  set,
}: {
  field: keyof CaptureConfigReading;
  id: string;
  label: string;
  helper: string;
  max: number;
  current: number | undefined;
  set: ReturnType<typeof useSetCaptureConfig>;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const sent = set.isPending ? set.variables?.[field] : undefined;
  const shown = draft ?? (sent !== undefined ? String(sent) : current === undefined ? "" : String(current));

  function save() {
    if (draft === null) return;
    const text = draft.trim();
    const value = Number(text);
    setDraft(null);
    if (text === "" || !Number.isInteger(value) || value === current) return;
    set.mutate({ [field]: value });
  }

  return (
    <Field label={label} helper={helper}>
      <input
        id={id}
        className="sy-pref-input"
        type="number"
        min={0}
        max={max}
        value={shown}
        disabled={set.isPending}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={save}
        onKeyDown={(event) => {
          if (event.key === "Enter") save();
        }}
      />
    </Field>
  );
}
