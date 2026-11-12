import { isApiRefusal } from "../data/client";
import { useRestartSidecar } from "../data/system";
import { DAEMON_NAME } from "../copy";
import { ConfirmButton, ErrorNote, Quiet, RefusalNote } from "../ui";

/**
 * Restart one sidecar: a confirm-armed button and the answer to it, in place. Shared by the
 * System health tiles and the channel cards, which each carried their own copy of this.
 */
export function RestartSidecar({ name }: { name: string }) {
  const restart = useRestartSidecar();
  return (
    <>
      <ConfirmButton
        label="Restart"
        /* No `subject`: the tile already names the sidecar, and the subject drawn into the
           armed label made the control wider than the tile it sits in. */
        confirmLabel="Start it again"
        variant="quiet"
        disabled={restart.isPending}
        onConfirm={() => {
          restart.mutate(name);
        }}
      />
      {restart.isSuccess && <Quiet compact announce says="asked: the supervisor is trying now" />}
      {restart.isError &&
        (isApiRefusal(restart.error) ? (
          <RefusalNote
            refusal={restart.error}
            sentences={{
              running: "it is running now, so there was nothing to start",
              not_supervised:
                "nothing is supervising it. The daemon starts a sidecar only when its pillar is switched on, and only at startup",
              kill_switch:
                "the emergency stop is engaged. Release it first; the supervisor keeps retrying on its own meanwhile",
            }}
          />
        ) : (
          <ErrorNote>the {DAEMON_NAME} is not responding, so nothing was asked of it</ErrorNote>
        ))}
    </>
  );
}
