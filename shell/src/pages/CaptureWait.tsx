import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useCaptureConfig, useSetCaptureConfig, type CaptureConfigReading } from "../data/capture-config";
import { ErrorNote, Field, RefusalNote } from "../ui";
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
