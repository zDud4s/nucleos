import { useEffect, useRef, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useCancelRun } from "../data/runs";
import { usePin } from "./pins";
import { useFleetUi } from "./ui-context";
import type { FleetNode as WireNode } from "./types";

/**
 * The footer's actions, from `capabilities` and nothing else.
 *
 * `nodrag nopan` so a press does not start a pan; the wrapper stops the click so pressing an
 * action does not also select the node (which would move the highlighted path).
 */
export function NodeActions({ node }: { node: WireNode }) {
  const ui = useFleetUi();
  const caps = node.capabilities;
  return (
    <span className="fleet-acts nodrag nopan" onClick={(event) => event.stopPropagation()}>
      {caps.full_transcript && (
        <button
          type="button"
          className="fleet-act"
          aria-label="Open the full transcript"
          title="Full transcript"
          onClick={() => ui.openPanel(node.id)}
        >
          ⤢
        </button>
      )}
      {caps.open_run && node.run_id !== null && (
        <Link
          className="fleet-act"
          to="/runs/$runId"
          params={{ runId: String(node.run_id) }}
          aria-label="Open the run"
          title="Open the run"
        >
          ↗
        </Link>
      )}
      {caps.pin && <PinButton node={node} />}
      {caps.stop && node.run_id !== null && <StopControl runId={node.run_id} label={node.label} />}
    </span>
  );
}

function PinButton({ node }: { node: WireNode }) {
  const pin = usePin();
  return (
    <>
      <button
        type="button"
        className="fleet-act"
        aria-label="Pin this tree"
        aria-pressed={node.pinned}
        title={node.pinned ? "Unpin this tree" : "Pin this tree"}
        disabled={pin.isPending}
        onClick={() => pin.mutate({ rootId: node.id, pinned: node.pinned })}
      >
        📌
      </button>
      {pin.isError && <span className="fleet-confirm-note">{node.pinned ? "could not unpin" : "could not pin"}</span>}
    </>
  );
}

/**
 * Stop with the confirmation drawn in the node: "Stop this run? Yes / No".
 *
 * The cancel is `useCancelRun`, the same one `RunDetail` uses, so the two cannot disagree about
 * what stopping means. (`RunDetail` confirms with the press-and-hold `ConfirmButton`; the spec,
 * section 7, asks for this inline "Yes / No" in the node, which is why the widget differs.) Focus moves to No when armed (the safe answer) and back to the stop button
 * when it is dismissed; Escape dismisses and does not reach the panel's Escape.
 */
function StopControl({ runId, label }: { runId: number; label: string }) {
  const cancel = useCancelRun();
  const [armed, setArmed] = useState(false);
  const stopButton = useRef<HTMLButtonElement>(null);
  const noButton = useRef<HTMLButtonElement>(null);
  const wasArmed = useRef(false);

  useEffect(() => {
    if (armed) noButton.current?.focus();
    else if (wasArmed.current) stopButton.current?.focus();
    wasArmed.current = armed;
  }, [armed]);

  if (!armed) {
    return (
      <>
        <button
          ref={stopButton}
          type="button"
          className="fleet-act fleet-act-stop"
          aria-label={`Stop ${label}`}
          title="Stop"
          disabled={cancel.isPending}
          onClick={() => {
            cancel.reset();
            setArmed(true);
          }}
        >
          ■
        </button>
        {cancel.isError && <span className="fleet-confirm-note">could not stop</span>}
      </>
    );
  }
  return (
    <span
      className="fleet-confirm"
      role="group"
      aria-label="Confirm stop"
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.stopPropagation();
        setArmed(false);
      }}
    >
      <span>Stop this run?</span>
      <button
        type="button"
        className="fleet-confirm-yes"
        onClick={() => {
          cancel.mutate(runId);
          setArmed(false);
        }}
      >
        Yes
      </button>
      <button ref={noButton} type="button" onClick={() => setArmed(false)}>
        No
      </button>
    </span>
  );
}
