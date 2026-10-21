import { isApiRefusal } from "../data/client";
import {
  DISTILLER_MODELS,
  useDistillerModel,
  useSetDistillerModel,
  type DistillerModel as Choice,
} from "../data/distiller";
import { ErrorNote, Field, RefusalNote } from "../ui";
import { DAEMON_NAME } from "../copy";

const LABELS: Record<Choice, string> = {
  cloud: "Cloud: the agent CLI",
  local: "Local: this machine's model",
  openrouter: "OpenRouter: hosted",
};

/**
 * Which model the distiller asks, as one labelled choice at the top of this machine's settings.
 *
 * The select is always there and always readable: a daemon that has not answered yet shows `cloud`,
 * which is what the distiller uses until somebody chooses. While a save is in flight the new choice
 * is shown, so the control does not snap back under the owner's hand; the refetch that follows is
 * the daemon's own answer.
 */
export function DistillerModel() {
  const stored = useDistillerModel();
  const set = useSetDistillerModel();

  const shown: Choice = set.isPending ? set.variables : (stored.data ?? "cloud");
  const refusal = [set.error, stored.error].find(isApiRefusal);

  return (
    <div className="sy-pref">
      <Field
        label="Distiller model"
        helper="Read by the distiller on its next pass; a choice this machine cannot serve leaves the queue waiting rather than using the cloud."
      >
        <select
          id="sy-distiller-model"
          className="sy-pref-input"
          value={shown}
          disabled={set.isPending}
          onChange={(event) => set.mutate(event.target.value as Choice)}
        >
          {DISTILLER_MODELS.map((model) => (
            <option key={model} value={model}>
              {LABELS[model]}
            </option>
          ))}
        </select>
      </Field>
      {refusal ? (
        <RefusalNote refusal={refusal} />
      ) : set.error ? (
        <ErrorNote>the {DAEMON_NAME} is not responding: the distiller's model was not changed</ErrorNote>
      ) : null}
    </div>
  );
}
