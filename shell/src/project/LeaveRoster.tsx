import { useEffect, useId, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { RemoveProject } from "../pages/RemoveProject";
import { Button } from "../ui/Button";
import { Modal } from "../ui/Modal";

export interface LeaveRosterProps {
  projectId: string;
  /** The recorded root, or `null` for a project nobody ever pointed anywhere. */
  projectRoot: string | null;
}

/**
 * Taking this project off NucleOS's roster, from inside it.
 *
 * It was a `remove` on every row of the roster. The roster became a page of cards that are one link
 * each, and the argument `DeleteFolder` makes for itself applies here too: the way out of a project
 * belongs inside it, reached by somebody who has already opened the one they mean. The reversible
 * exit sits above the irreversible one, and is quieter than it: each row says what it does on the
 * leading side, and only the permanent one is outlined in danger.
 *
 * On success the project no longer has a page to stay on, so this goes to the roster and hands the
 * removal over on the history entry, where the roster says what left and offers the way back.
 */
export function LeaveRoster({ projectId, projectRoot }: LeaveRosterProps) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const navigate = useNavigate();

  /*
    Focus back on the toggle when the panel closes by `cancel` or Escape: the panel unmounts with
    focus inside it, and a keyboard user would otherwise land on the body at the foot of the page.
    `Button` takes no ref, so the wrapper holds one and finds the button in it. Never on first paint.
  */
  const trigger = useRef<HTMLDivElement>(null);
  const [closed, setClosed] = useState(false);
  useEffect(() => {
    if (!closed) return;
    trigger.current?.querySelector("button")?.focus();
    setClosed(false);
  }, [closed]);

  const close = () => {
    setOpen(false);
    setClosed(true);
  };

  return (
    <div className="flex flex-col gap-3">
      <div ref={trigger} className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <p className="text-sm text-text-muted">
          Takes this project off NucleOS. Its folder stays where it is.
        </p>
        <Button
          variant="ghost"
          aria-expanded={open}
          aria-controls={open ? panelId : undefined}
          onClick={() => (open ? close() : setOpen(true))}
        >
          {/* A disclosure head with a fixed name. It read `cancel` while open, which put two
              `cancel`s on screen — this one and the panel's own — for the same act. The panel's
              `remove` confirms what this opened, so the two agree rather than collide. */}
          remove from NucleOS…
        </Button>
      </div>
      {/* Over a dimmed page, like every confirmation that takes something away. */}
      <Modal
        open={open}
        onOpenChange={(next) => (next ? setOpen(true) : close())}
        title="Remove from NucleOS"
        size="md"
      >
        {open && (
          <RemoveProject
            id={panelId}
            projectId={projectId}
            projectRoot={projectRoot}
            onDone={(removed) => {
              void navigate({ to: "/projects", state: { leftTheRoster: removed } });
            }}
            onCancel={close}
          />
        )}
      </Modal>
    </div>
  );
}
