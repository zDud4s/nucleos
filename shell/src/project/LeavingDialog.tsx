import { useRef, useState } from "react";

import { Modal } from "../ui/Modal";
import { DeleteFolder } from "./DeleteFolder";
import { LeaveRoster } from "./LeaveRoster";

/** Which way out of the project is open: off the roster (reversible) or off the disk (permanent). */
export type Leaving = "remove" | "delete";

export interface LeavingDialogProps {
  /** `null` when neither is open. */
  leaving: Leaving | null;
  projectId: string;
  projectRoot: string | null;
  onClose: () => void;
  /** Puts focus back on the header's `More actions` trigger, which a menu item leaves behind. */
  returnFocus: () => void;
}

/**
 * The two exits from a project, in the one shared modal.
 *
 * They are reached from the workspace header's `More actions` menu. A menu item is not a trigger
 * Radix can return focus to once the menu has closed, so the dialog hands focus back itself through
 * `returnFocus`. It also refuses to close over a request in flight: closing would hide the answer
 * to something already sent.
 */
export function LeavingDialog({
  leaving,
  projectId,
  projectRoot,
  onClose,
  returnFocus,
}: LeavingDialogProps) {
  const [busy, setBusy] = useState(false);
  const warning = useRef<HTMLParagraphElement>(null);
  // The last way out that was open, kept while the Modal animates closed so its title and body
  // do not switch to the other exit's mid-fade.
  const last = useRef<Leaving>("remove");
  if (leaving !== null) last.current = leaving;
  const shown = leaving ?? last.current;

  return (
    <Modal
      open={leaving !== null}
      onOpenChange={(next) => {
        if (!next && !busy) onClose();
      }}
      title={shown === "delete" ? "Delete this folder" : "Remove from NucleOS"}
      size="md"
      initialFocus={leaving === "delete" ? warning : undefined}
      onCloseAutoFocus={(event) => {
        event.preventDefault();
        returnFocus();
      }}
    >
      {shown === "remove" && (
        <LeaveRoster projectId={projectId} projectRoot={projectRoot} onClose={onClose} />
      )}
      {shown === "delete" && (
        <DeleteFolder
          projectId={projectId}
          onClose={onClose}
          warningRef={warning}
          onBusyChange={setBusy}
        />
      )}
    </Modal>
  );
}
