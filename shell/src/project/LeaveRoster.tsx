import { useId } from "react";
import { useNavigate } from "@tanstack/react-router";

import { RemoveProject } from "../pages/RemoveProject";

export interface LeaveRosterProps {
  projectId: string;
  /** The recorded root, or `null` for a project nobody ever pointed anywhere. */
  projectRoot: string | null;
  /** Stands the panel down (cancel); the dialog around it owns the rest of the closing. */
  onClose: () => void;
}

/**
 * Taking this project off NucleOS's roster, from inside it.
 *
 * It was a `remove` on every row of the roster. The roster became a page of cards that are one link
 * each, and the argument `DeleteFolder` makes for itself applies here too: the way out of a project
 * belongs inside it, reached by somebody who has already opened the one they mean. It is reached
 * from the workspace header's `More actions` menu and drawn inside `LeavingDialog`'s modal, which
 * owns the open state and the focus; this is only the panel.
 *
 * On success the project no longer has a page to stay on, so this goes to the roster and hands the
 * removal over on the history entry, where the roster says what left and offers the way back.
 */
export function LeaveRoster({ projectId, projectRoot, onClose }: LeaveRosterProps) {
  const panelId = useId();
  const navigate = useNavigate();

  return (
    <RemoveProject
      id={panelId}
      projectId={projectId}
      projectRoot={projectRoot}
      onDone={(removed) => {
        void navigate({ to: "/projects", state: { leftTheRoster: removed } });
      }}
      onCancel={onClose}
    />
  );
}
