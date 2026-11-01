import { useEffect, useRef, useState } from "react";
import { DAEMON_NAME } from "../copy";
import { apiFetch, describeBlocking, type UpdateReadiness } from "../data/client";
import { applyUpdate, useInstallKind, useUpdate, type UpdatePhase } from "../data/updater";
import { Button, ErrorNote } from "../ui";

const PHASE_TEXT: Record<UpdatePhase | "downloading", string> = {
  downloading: "downloading…",
  waiting: "waiting for running work to finish…",
  stopping: `stopping the ${DAEMON_NAME}…`,
  installing: "installing…",
};

/**
 * An available update, offered in the sidebar footer. Renders nothing until the
 * release feed has offered one.
 *
 * The update waits for running work to settle and says what it waits on; "Update now" ends the
 * wait and cuts that work.
 */
export function UpdateNotice() {
  const update = useUpdate();
  const kind = useInstallKind();
  const [phase, setPhase] = useState<UpdatePhase | "downloading" | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [waitingOn, setWaitingOn] = useState<UpdateReadiness | null>(null);
  const forced = useRef(false);
  const running = useRef(false);
  const [joined, setJoined] = useState<UpdateReadiness | null>(null);

  // After a reload a drain may be under way already: show the wait instead of a plain "Update".
  useEffect(() => {
    let live = true;
    apiFetch<UpdateReadiness>("/system/update-readiness")
      .then((seen) => {
        if (!live || !seen.draining || running.current) return;
        setJoined(seen);
        setWaitingOn(seen);
        setPhase("waiting");
      })
      .catch(() => {
        // Nothing to follow.
      });
    return () => {
      live = false;
    };
  }, []);

  if (kind.data === "system-package") {
    return (
      <div className="app-kill">
        <p>
          NucleOS was installed from a .deb/.rpm package; update it with your package manager
        </p>
      </div>
    );
  }

  const offered = update.data;
  if (!offered) return null;

  const apply = (join?: UpdateReadiness) => {
    setFailure(null);
    setWaitingOn(null);
    running.current = true;
    forced.current = join !== undefined;
    setPhase("downloading");
    applyUpdate(offered, setPhase, {
      onWaiting: setWaitingOn,
      forced: () => forced.current,
      join,
    })
      .catch((error: unknown) => {
        setFailure(error);
        setPhase(null);
      })
      .finally(() => {
        running.current = false;
        setJoined(null);
      });
  };

  return (
    <div className="app-kill">
      <p>NucleOS {offered.version} is available</p>
      <p className="app-kill-unread">The update waits for running work to finish.</p>
      {phase === null ? null : <p role="status">{PHASE_TEXT[phase]}</p>}
      {phase === "waiting" && waitingOn !== null ? (
        <p className="app-kill-unread">Waiting on: {describeBlocking(waitingOn.waiting_on)}</p>
      ) : null}
      <Button onClick={() => apply()} disabled={phase !== null}>
        Update
      </Button>
      {phase === "waiting" ? (
        <Button
          variant="ghost"
          onClick={() => {
            // With no update running here (a drain seen after a reload) this one takes it over.
            if (joined !== null && !running.current) apply(joined);
            else forced.current = true;
          }}
        >
          Update now
        </Button>
      ) : null}
      {failure === null ? null : (
        <ErrorNote>
          the update could not be installed: {failure instanceof Error ? failure.message : String(failure)}
        </ErrorNote>
      )}
    </div>
  );
}
