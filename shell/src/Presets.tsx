import { useCallback, useEffect, useState } from "react";
import {
  createPreset, deletePreset, listPresets, runPreset, updatePreset,
  RUN_MODES, type Preset, type PresetInput,
} from "./api";
import { relativeTime } from "./derive";
import { Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

const BLANK: PresetInput = { name: "", prompt: "", project_id: null, cwd: null, mode: "real" };

/** The empty string a text input yields, stored the way the daemon reads "not set". */
function orNull(value: string): string | null {
  return value.trim() === "" ? null : value.trim();
}

interface PresetFormProps {
  initial: PresetInput;
  busy: boolean;
  submitLabel: string;
  onSubmit: (input: PresetInput) => void;
  onCancel?: () => void;
}

/**
 * The one form, used for both creating and editing.
 *
 * Keyed by the preset being edited at the call site, so switching which preset is open remounts it
 * with that preset's values — otherwise the fields would keep whatever the previous one had.
 */
function PresetForm({ initial, busy, submitLabel, onSubmit, onCancel }: PresetFormProps) {
  const [name, setName] = useState(initial.name);
  const [prompt, setPrompt] = useState(initial.prompt);
  const [projectId, setProjectId] = useState(initial.project_id ?? "");
  const [cwd, setCwd] = useState(initial.cwd ?? "");
  const [mode, setMode] = useState(initial.mode);

  const incomplete = name.trim() === "" || prompt.trim() === "";

  return (
    <form
      className="form-grid"
      onSubmit={(event) => {
        event.preventDefault();
        if (incomplete || busy) return;
        onSubmit({
          name: name.trim(),
          prompt: prompt.trim(),
          project_id: orNull(projectId),
          cwd: orNull(cwd),
          mode,
        });
      }}
    >
      <label>
        Name
        <input value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <label>
        Mode
        <select value={mode} onChange={(event) => setMode(event.target.value)}>
          {RUN_MODES.map((option) => <option key={option} value={option}>{option}</option>)}
        </select>
      </label>
      <label>
        Project
        <input
          value={projectId}
          placeholder="(none)"
          onChange={(event) => setProjectId(event.target.value)}
        />
      </label>
      <label>
        Working directory
        <input
          value={cwd}
          placeholder="(the project root)"
          onChange={(event) => setCwd(event.target.value)}
        />
      </label>
      <label className="wide">
        Prompt
        <textarea
          rows={4}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
        />
      </label>
      <div className="form-actions">
        <Button type="submit" variant="approve" disabled={incomplete || busy}>
          {busy ? "Saving…" : submitLabel}
        </Button>
        {onCancel !== undefined && (
          <Button onClick={onCancel} disabled={busy}>Cancel</Button>
        )}
      </div>
    </form>
  );
}

interface PresetsProps {
  token: string;
  /** Told after a preset starts a run, so the run list picks it up without waiting for a tick. */
  onRunStarted: (runId: number) => void;
}

/**
 * Saved run requests.
 *
 * A preset is nothing but a name attached to the exact body `/runs` takes, and running one goes
 * through the same front door a hand-written run does — same kill switch, same budget, same refusal
 * codes. So this panel deliberately does not explain what running one costs: whatever it costs is
 * already said next to the button that starts an ordinary run.
 */
function Presets({ token, onRunStarted }: PresetsProps) {
  const [presets, setPresets] = useState<Preset[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const next = await listPresets(token);
    setPresets(next);
    setLoading(false);
  }, [token]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 409 is the only refusal worth its own sentence: the name is already taken, and the fix is to
  // pick another rather than to try again.
  function explain(status: number): string {
    if (status === 409) return "A preset with that name already exists.";
    if (status === 400) return "The daemon rejected this — check the name and prompt.";
    return "Could not save this preset.";
  }

  async function save(input: PresetInput, id: number | null) {
    setBusy(true);
    setFailed(null);
    setNote(null);
    const result = id === null
      ? await createPreset(token, input)
      : await updatePreset(token, id, input);
    setBusy(false);
    if (!result.ok) {
      setFailed(explain(result.status));
      return;
    }
    setCreating(false);
    setEditing(null);
    await refresh();
  }

  async function remove(id: number) {
    setBusy(true);
    setFailed(null);
    const gone = await deletePreset(token, id);
    setBusy(false);
    if (!gone) {
      setFailed("Could not delete this preset.");
      return;
    }
    await refresh();
  }

  async function start(preset: Preset) {
    setBusy(true);
    setFailed(null);
    setNote(null);
    const result = await runPreset(token, preset.id);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 503
          ? "Refused: the kill switch is engaged."
          : result.status === 429
            ? "Refused: autopilot is paused by budget."
            : `Could not start "${preset.name}".`,
      );
      return;
    }
    setNote(`Run #${result.value} started from "${preset.name}".`);
    onRunStarted(result.value);
  }

  return (
    <Panel
      title="Presets"
      aside={presets === null ? undefined : `${presets.length} saved`}
    >
      <div className="form-actions">
        <Button
          onClick={() => { setCreating((open) => !open); setEditing(null); setFailed(null); }}
          disabled={busy}
        >
          {creating ? "Close" : "New preset"}
        </Button>
      </div>
      {creating && (
        <PresetForm
          initial={BLANK}
          busy={busy}
          submitLabel="Save preset"
          onSubmit={(input) => void save(input, null)}
          onCancel={() => setCreating(false)}
        />
      )}
      {note !== null && <p className="gate-note">{note}</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      {loading && presets === null && <p className="a-note">Loading…</p>}
      {!loading && presets === null && (
        <ErrorNote>Could not load presets from the daemon.</ErrorNote>
      )}
      {presets !== null && presets.length === 0 && !creating && (
        <Teach title="No presets yet.">
          A preset is a run request you have already written down — the prompt, the project, the
          mode. Save the ones you type more than once, then start them in a click.
        </Teach>
      )}
      {(presets ?? []).map((preset) => (
        <article className="feed-item" key={preset.id}>
          <div className="f-meta">
            <b>{preset.name}</b>
            <span className="p-mode">{preset.mode}</span>
            <span>{preset.project_id ?? "no project"}</span>
            <time dateTime={preset.updated_at} title={preset.updated_at}>
              {relativeTime(preset.updated_at)}
            </time>
          </div>
          <p className="f-body p-prompt">{preset.prompt}</p>
          {preset.cwd !== null && <p className="a-note">in {preset.cwd}</p>}
          <div className="a-actions">
            <Button size="sm" intent="go" disabled={busy} onClick={() => void start(preset)}>
              Run
            </Button>
            <Button
              size="sm"
              disabled={busy}
              onClick={() => {
                setEditing((open) => (open === preset.id ? null : preset.id));
                setCreating(false);
                setFailed(null);
              }}
            >
              {editing === preset.id ? "Close" : "Edit"}
            </Button>
            <ConfirmButton
              size="sm"
              variant="danger"
              confirmLabel="Confirm delete?"
              disabled={busy}
              onConfirm={() => void remove(preset.id)}
            >
              Delete
            </ConfirmButton>
          </div>
          {editing === preset.id && (
            // Keyed so opening a different preset remounts the form with that preset's values
            // rather than keeping the last one's.
            <PresetForm
              key={preset.id}
              initial={{
                name: preset.name,
                prompt: preset.prompt,
                project_id: preset.project_id,
                cwd: preset.cwd,
                mode: preset.mode,
              }}
              busy={busy}
              submitLabel="Save changes"
              onSubmit={(input) => void save(input, preset.id)}
              onCancel={() => setEditing(null)}
            />
          )}
        </article>
      ))}
    </Panel>
  );
}

export default Presets;
