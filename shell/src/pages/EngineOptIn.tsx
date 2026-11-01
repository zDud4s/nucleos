import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useEngineOptIn, useSetEngineOptIn } from "../data/engines";
import { Button, ErrorNote, Modal, RefusalNote, Switch } from "../ui";
import { DAEMON_NAME } from "../copy";

/**
 * The opt-in for one CLI engine: a switch, and a dialog that shows the daemon's warning before
 * anything is turned on.
 *
 * The warning and `takes_effect` are drawn exactly as the daemon sent them; the shell holds no copy
 * of either. Turning off needs no dialog. Until the daemon has answered (or when it fails to), the
 * switch is not offered, so nothing can be enabled without the warning on screen.
 */
export function EngineOptIn({ engine, label }: { engine: string; label: string }) {
  const state = useEngineOptIn(engine);
  const set = useSetEngineOptIn(engine);
  const [asking, setAsking] = useState(false);

  const refusal = [set.error, state.error].find(isApiRefusal);
  const reading = state.data;

  const confirm = () => {
    setAsking(false);
    set.mutate({ enabled: true, acknowledged: true });
  };

  return (
    <div className="sy-pref">
      <strong>{label}</strong>
      {reading === undefined ? null : (
        <>
          <Switch
            label={`Use ${label}`}
            labelHidden
            checked={reading.enabled}
            disabled={set.isPending}
            onChange={(next) => {
              if (next) setAsking(true);
              else set.mutate({ enabled: false });
            }}
          />
          {reading.installed ? null : <span className="sy-meta">{label} was not found on this machine</span>}
          <Modal
            open={asking}
            onOpenChange={setAsking}
            title={`Enable ${label}?`}
            footer={
              <>
                <Button variant="ghost" onClick={() => setAsking(false)}>
                  Cancel
                </Button>
                <Button variant="approve" onClick={confirm}>
                  Enable
                </Button>
              </>
            }
          >
            <p>{reading.warning}</p>
          </Modal>
        </>
      )}
      {set.isSuccess && set.data.takes_effect !== "" ? <span className="sy-meta">{set.data.takes_effect}</span> : null}
      {refusal ? (
        <RefusalNote refusal={refusal} />
      ) : set.error ? (
        // A write that got no answer may still have landed, so it never claims "not changed".
        <ErrorNote>the {DAEMON_NAME} did not answer: whether {label} was changed is unknown</ErrorNote>
      ) : state.error ? (
        <ErrorNote>
          the {DAEMON_NAME} is not responding: could not read the setting for {label}
        </ErrorNote>
      ) : null}
    </div>
  );
}
