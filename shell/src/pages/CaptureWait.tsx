import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useCaptureWait, useSetCaptureWait } from "../data/capture-wait";
import { ErrorNote, RefusalNote } from "../ui";

/**
 * How long a capture request waits for the owner, as one number at the top of this machine's
 * settings. It saves on blur and on Enter, and only when the typed value is a whole number that
 * differs from the stored one — an empty or half-typed box never reaches the daemon.
 */
export function CaptureWait() {
  const stored = useCaptureWait();
  const set = useSetCaptureWait();
  const [draft, setDraft] = useState<string | null>(null);

  const current = stored.data?.minutes;
  const shown = draft ?? (set.isPending ? String(set.variables) : current === undefined ? "" : String(current));
  const refusal = [set.error, stored.error].find(isApiRefusal);

  function save() {
    if (draft === null) return;
    const text = draft.trim();
    const minutes = Number(text);
    setDraft(null);
    if (text === "" || !Number.isInteger(minutes) || minutes === current) return;
    set.mutate(minutes);
  }

  return (
    <div className="sy-field">
      <label htmlFor="sy-capture-wait">Capture wait (minutes)</label>
      <input
        id="sy-capture-wait"
        className="sy-field-input"
        type="number"
        min={0}
        max={10080}
        value={shown}
        disabled={set.isPending}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={save}
        onKeyDown={(event) => {
          if (event.key === "Enter") save();
        }}
      />
      <p className="sy-field-hint">0 turns capture requests off; at most a week (10080).</p>
      {refusal ? (
        <RefusalNote refusal={refusal} />
      ) : set.error ? (
        <ErrorNote>the núcleo did not answer — the capture wait was not changed</ErrorNote>
      ) : null}
    </div>
  );
}
