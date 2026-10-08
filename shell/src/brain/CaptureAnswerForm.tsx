import { useState } from "react";
import type { CaptureRequest } from "../data/captures";
import { useAnswerCapture, useDismissCapture } from "../data/captures";
import { Button, ConfirmButton } from "../ui";
import { DecisionRefusal } from "./knowledge/WaitingPanel";
import "./capture.css";

/** "1h 12m left", "5m left", "less than a minute left"; at or past the deadline, "expiring". */
export function timeLeft(secondsLeft: number): string {
  if (secondsLeft <= 0) return "expiring";
  const minutes = Math.floor(secondsLeft / 60);
  if (minutes < 1) return "less than a minute left";
  const hours = Math.floor(minutes / 60);
  return hours > 0 ? `${hours}h ${minutes % 60}m left` : `${minutes}m left`;
}

/**
 * The question and its two doors: an answer (which becomes a note) or a dismissal. Owns its own
 * mutations so it can stand in the waiting section and in the item panel alike.
 */
export function CaptureAnswerForm({ request }: { request: CaptureRequest }) {
  const [text, setText] = useState("");
  const answer = useAnswerCapture();
  const dismiss = useDismissCapture();
  const busy = answer.isPending || dismiss.isPending;
  const refusal = answer.error ?? dismiss.error;
  const label = `Answer for job #${request.job_id}`;

  return (
    <div className="capture-form">
      <p className="capture-prompt">{request.prompt_text}</p>
      <p className="capture-left">{timeLeft(request.seconds_left)}</p>
      <textarea
        aria-label={label}
        value={text}
        rows={3}
        onChange={(event) => setText(event.target.value)}
      />
      {refusal !== null && <DecisionRefusal error={refusal} />}
      <div className="capture-actions">
        <Button
          variant="approve"
          // Disabled once answered too: a second click would file a second note, the late-answer path.
          disabled={busy || answer.isSuccess || text.trim() === ""}
          onClick={() => answer.mutate({ jobId: request.job_id, text }, { onSuccess: () => setText("") })}
        >
          Answer
        </Button>
        <ConfirmButton
          label="Dismiss"
          confirmLabel="Dismiss for good"
          variant="quiet"
          onConfirm={() => dismiss.mutate({ jobId: request.job_id })}
          disabled={busy}
        />
      </div>
    </div>
  );
}
