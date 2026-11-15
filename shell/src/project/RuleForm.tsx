import type { FormEvent, ReactNode } from "react";
import { Button, Field, Select } from "../ui";
import { actionsFor, withEvent, type ActionKind, type Conclusion, type Draft, type Event } from "./automations-model";

const EVENT_LABEL: Record<Event, string> = {
  published_green: "The published branch is green",
  ci_finished: "A CI workflow finishes",
};

const ACTION_LABEL: Record<ActionKind, string> = {
  push: "Push",
  open_incident: "Open an incident",
};

const CONCLUSIONS: Conclusion[] = ["success", "failure", "cancelled"];

export interface RuleFormProps {
  draft: Draft;
  onChange: (draft: Draft) => void;
  onSave: () => void;
  onCancel: () => void;
  busy: boolean;
  /** The núcleo's sentence for a refused save, drawn next to the controls. */
  failure: ReactNode;
}

/**
 * One rule being written, controlled by a `Draft`.
 *
 * Controls that do not apply to the chosen event are not drawn at all (not disabled): a person
 * cannot build an invalid rule from here, and there is no JSON to edit. The rules the server adds
 * on top (a duplicate name, one push rule per branch) come back as a sentence in `failure`.
 */
export function RuleForm({ draft, onChange, onSave, onCancel, busy, failure }: RuleFormProps) {
  const ci = draft.event === "ci_finished";
  const set = (patch: Partial<Draft>) => onChange({ ...draft, ...patch });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (draft.name.trim() !== "" && !busy) onSave();
  };

  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <Field label="Name">
        <input
          type="text"
          className="ui-input"
          value={draft.name}
          onChange={(event) => set({ name: event.target.value })}
        />
      </Field>

      <Field label="When">
        <Select value={draft.event} onChange={(event) => onChange(withEvent(draft, event.target.value as Event))}>
          {(Object.keys(EVENT_LABEL) as Event[]).map((event) => (
            <option key={event} value={event}>
              {EVENT_LABEL[event]}
            </option>
          ))}
        </Select>
      </Field>

      {ci ? (
        <Field label="Workflow">
          <input
            type="text"
            className="ui-input"
            value={draft.workflow}
            onChange={(event) => set({ workflow: event.target.value })}
          />
        </Field>
      ) : null}

      <Field label="Branch" helper="Leave empty for any branch.">
        <input
          type="text"
          className="ui-input"
          value={draft.branch}
          onChange={(event) => set({ branch: event.target.value })}
        />
      </Field>

      {ci ? (
        <>
          <Field label="Conclusion">
            <Select
              value={draft.conclusion}
              onChange={(event) => set({ conclusion: event.target.value as "" | Conclusion })}
            >
              <option value="">Any</option>
              {CONCLUSIONS.map((conclusion) => (
                <option key={conclusion} value={conclusion}>
                  {conclusion}
                </option>
              ))}
            </Select>
          </Field>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={draft.byAutomation}
              onChange={(event) => set({ byAutomation: event.target.checked })}
            />
            Only runs of pushes made by an automation
          </label>
        </>
      ) : null}

      <Field label="Then">
        <Select value={draft.action} onChange={(event) => set({ action: event.target.value as ActionKind })}>
          {actionsFor(draft.event).map((action) => (
            <option key={action} value={action}>
              {ACTION_LABEL[action]}
            </option>
          ))}
        </Select>
      </Field>

      {draft.action === "push" ? (
        <Field label="Remote">
          <input
            type="text"
            className="ui-input"
            value={draft.remote}
            onChange={(event) => set({ remote: event.target.value })}
          />
        </Field>
      ) : (
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.autoFix}
            onChange={(event) => set({ autoFix: event.target.checked })}
          />
          Fix it automatically
        </label>
      )}

      {failure}

      <div className="flex items-center gap-2">
        <Button variant="primary" type="submit" disabled={draft.name.trim() === "" || busy}>
          Save rule
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
