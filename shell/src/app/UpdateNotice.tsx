import { useState } from "react";
import { applyUpdate, useUpdate, type UpdatePhase } from "../data/updater";
import { Button, ErrorNote } from "../ui";

const PHASE_TEXT: Record<UpdatePhase | "downloading", string> = {
  downloading: "downloading…",
  stopping: "stopping the núcleo…",
  installing: "installing…",
};

/**
 * An available update, offered in the sidebar footer. Renders nothing until the
 * release feed has offered one.
 */
export function UpdateNotice() {
  const update = useUpdate();
  const [phase, setPhase] = useState<UpdatePhase | "downloading" | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const offered = update.data;
  if (!offered) return null;

  const apply = () => {
    setFailure(null);
    setPhase("downloading");
    applyUpdate(offered, setPhase).catch((error: unknown) => {
      setFailure(error);
      setPhase(null);
    });
  };

  return (
    <div className="app-kill">
      <p>NucleOS {offered.version} is available</p>
      <p className="app-kill-unread">Running work is interrupted while it installs.</p>
      {phase === null ? null : <p role="status">{PHASE_TEXT[phase]}</p>}
      <Button onClick={apply} disabled={phase !== null}>
        Update
      </Button>
      {failure === null ? null : (
        <ErrorNote>
          the update could not be installed — {failure instanceof Error ? failure.message : String(failure)}
        </ErrorNote>
      )}
    </div>
  );
}
