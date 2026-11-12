import { useEffect, useRef, useState } from "react";
import {
  apiFetch,
  describeBlocking,
  isApiRefusal,
  isApiUnavailable,
  type UpdateReadiness,
} from "../data/client";
import { DAEMON_NAME } from "../copy";
import { Button, ConfirmButton, ErrorNote, Panel } from "../ui";

/** Where a restart is: nothing asked, waiting for running work, waiting for the new daemon, done. */
type Stage =
  | { kind: "idle" }
  | { kind: "waiting"; pid: number; seen: UpdateReadiness; ours: boolean }
  | { kind: "restarting"; pid: number; ours: boolean }
  | { kind: "back"; version: string }
  | { kind: "not-restarted" }
  | { kind: "no-return" };

const POLL = 2_000;
/** How long a stopped daemon may take to answer again before this gives up waiting. */
const RESTART_DEADLINE = 2 * 60_000;

/**
 * Restart the daemon without cutting running work.
 *
 * "Restart when idle" asks the daemon to wait for the runs that would be lost and says what it is
 * waiting on, read from `/system/update-readiness`. "Restart now" skips the wait and sits behind
 * the hold-to-confirm interlock. After the daemon goes down this waits for a daemon with another
 * process id to answer, and says so.
 */
export function RestartDaemon({
  pollMs = POLL,
  restartDeadlineMs = RESTART_DEADLINE,
}: {
  pollMs?: number;
  restartDeadlineMs?: number;
}) {
  const [stage, setStage] = useState<Stage>({ kind: "idle" });
  const [asking, setAsking] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [already, setAlready] = useState(false);
  const busy = useRef(false);

  // After a reload a drain may be under way already: show it rather than an idle panel. Its kind
  // (restart, update, shutdown) is unknown, so it is not presented as a restart.
  useEffect(() => {
    let live = true;
    apiFetch<UpdateReadiness>("/system/update-readiness")
      .then((seen) => {
        if (!live || !seen.draining) return;
        setStage((now) =>
          now.kind === "idle" ? { kind: "waiting", pid: seen.pid, seen, ours: false } : now,
        );
      })
      .catch(() => {
        // Nothing to follow.
      });
    return () => {
      live = false;
    };
  }, []);

  async function ask(force: boolean) {
    setFailure(null);
    setAlready(false);
    setAsking(true);
    try {
      const answer = await apiFetch<UpdateReadiness>(
        force ? "/system/restart?force=true" : "/system/restart",
        { method: "POST" },
      );
      setStage({ kind: "waiting", pid: answer.pid, seen: answer, ours: true });
    } catch (error) {
      if (isApiRefusal(error) && error.status === 409) {
        // A drain is already under way: follow it rather than start a second one. It may be an
        // update's or a plain shutdown's, so it is not called a restart.
        setAlready(true);
        try {
          const seen = await apiFetch<UpdateReadiness>("/system/update-readiness");
          setStage({ kind: "waiting", pid: seen.pid, seen, ours: false });
        } catch {
          // The sentence below is still true; there is just nothing to follow.
        }
      } else {
        setFailure(error);
      }
    } finally {
      setAsking(false);
    }
  }

  const pid = stage.kind === "waiting" || stage.kind === "restarting" ? stage.pid : null;
  const kind = stage.kind;
  useEffect(() => {
    if (pid === null) return;
    let live = true;
    const tick = async () => {
      if (busy.current) return;
      busy.current = true;
      try {
        const now = await apiFetch<UpdateReadiness>("/system/update-readiness");
        if (!live) return;
        if (kind === "waiting") {
          if (now.pid !== pid) setStage({ kind: "back", version: now.version });
          else if (!now.draining) setStage({ kind: "not-restarted" });
          else setStage((prev) => (prev.kind === "waiting" ? { ...prev, seen: now } : prev));
        } else if (now.pid !== pid) {
          setStage({ kind: "back", version: now.version });
        }
      } catch (error) {
        if (!live) return;
        // No answer: the daemon is down. From here the wait is for the new one to answer.
        if (isApiUnavailable(error) && kind === "waiting") {
          setStage((prev) =>
            prev.kind === "waiting" ? { kind: "restarting", pid, ours: prev.ours } : prev,
          );
        }
      } finally {
        busy.current = false;
      }
    };
    const timer = setInterval(() => void tick(), pollMs);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [pid, kind, pollMs]);

  // A drain that was an update's or a plain shutdown's never relaunches: stop waiting after a while.
  useEffect(() => {
    if (kind !== "restarting") return;
    const timer = setTimeout(() => setStage({ kind: "no-return" }), restartDeadlineMs);
    return () => clearTimeout(timer);
  }, [kind, restartDeadlineMs]);

  const waiting = stage.kind === "waiting";
  return (
    <Panel title="Restart the daemon">
      <p className="sy-note">
        Restarting waits for running work to finish, so no turn is cut. New work is refused while it
        waits.
      </p>
      <div className="sy-actions">
        <Button
          variant="ghost"
          disabled={asking || waiting || stage.kind === "restarting"}
          onClick={() => void ask(false)}
        >
          Restart when idle
        </Button>
        <ConfirmButton
          label="Restart now"
          confirmLabel="Really restart — running work is cut"
          sayAs="Restart the daemon now; the runs still working are cut and settled when it starts again"
          variant="ghost"
          disabled={asking || stage.kind === "restarting"}
          onConfirm={() => void ask(true)}
        />
      </div>
      {already && stage.kind === "idle" && (
        <p className="sy-note" role="status">
          A stop is already waiting for running work. Wait for it, or restart now.
        </p>
      )}
      {stage.kind === "waiting" && (
        <p className="sy-note" role="status">
          {stage.ours
            ? "Waiting for running work to finish."
            : "A stop is already waiting for running work to finish."}{" "}
          Waiting on: {describeBlocking(stage.seen.waiting_on)}.
        </p>
      )}
      {stage.kind === "restarting" && (
        <p className="sy-note" role="status">
          {stage.ours
            ? `The ${DAEMON_NAME} is restarting. Waiting for it to answer again.`
            : `The ${DAEMON_NAME} has stopped. Waiting for it to answer again.`}
        </p>
      )}
      {stage.kind === "no-return" && (
        <p className="sy-note" role="status">
          The {DAEMON_NAME} did not come back. It may have stopped without restarting; start it
          again with scripts/run-daemon.ps1, or open NucleOS again.
        </p>
      )}
      {stage.kind === "back" && (
        <p className="sy-note" role="status">
          The {DAEMON_NAME} is back, version {stage.version}.
        </p>
      )}
      {stage.kind === "not-restarted" && (
        <p className="sy-note" role="status">
          The {DAEMON_NAME} did not restart: running work did not finish in time, so it carries on.
        </p>
      )}
      {failure !== null && (
        <ErrorNote>
          the restart was not started:{" "}
          {isApiUnavailable(failure) ? `the ${DAEMON_NAME} is not responding` : String(failure)}
        </ErrorNote>
      )}
    </Panel>
  );
}
