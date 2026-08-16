import { useEffect, useState } from "react";
import { listIdeSessions, type IdeSession } from "../api";
import { projectName } from "./projectName";
import { Button, Teach } from "../ui";

interface IdeSessionsProps {
  token: string;
  /** Picks one up. The id is all that travels — the daemon looks up where it runs. */
  onContinue: (sessionId: string) => void;
  onClose: () => void;
}

/**
 * The conversations already had in the IDE, offered to be picked up here.
 *
 * Read when this opens rather than polled: the list is somebody else's store on disk, and nothing
 * about it changes because of anything this window does. Closing and opening it again is the
 * refresh, and that is the only moment a person is choosing from it.
 */
function IdeSessions({ token, onContinue, onClose }: IdeSessionsProps) {
  const [sessions, setSessions] = useState<IdeSession[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const found = await listIdeSessions(token);
      if (!cancelled) setSessions(found ?? []);
    })();
    return () => {
      cancelled = true;
    };
  }, [token]);

  return (
    <div className="ide-sessions">
      <div className="statusline">
        <span>continue one from the IDE</span>
        <Button size="sm" onClick={onClose}>
          Cancel
        </Button>
      </div>
      {sessions === null && <p className="a-note">Looking…</p>}
      {sessions !== null && sessions.length === 0 && (
        <Teach title="Nothing to pick up.">
          Conversations had in the editor show up here once they exist — and only while the folder
          they were had in is still on disk, because that is how they are found again. Ones already
          being continued are left out.
        </Teach>
      )}
      {sessions !== null && sessions.length > 0 && (
        <ul>
          {sessions.map((session) => (
            <li key={session.session_id} className="cl-row">
              <button
                type="button"
                className="cl-open"
                onClick={() => onContinue(session.session_id)}
              >
                <span className={session.title === null ? "cl-name a-note" : "cl-name"}>
                  {session.title ?? "Nothing said yet"}
                </span>
                <span className="cl-meta">
                  <span className="b-run">{projectName(session.cwd)}</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default IdeSessions;
