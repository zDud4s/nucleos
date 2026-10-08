import { useAllCaptures } from "../data/captures";
import { Button, Panel, Quiet } from "../ui";
import { CaptureAnswerForm } from "./CaptureAnswerForm";
import { formatItem } from "./item-ref";
import "./capture.css";

const CLOSED_TEXT = { answered: "answered", dismissed: "dismissed", expired: "expired" } as const;

/** The side panel of one capture request: the form while open, a read-only record once closed. */
export function CapturePanel({ id, onSelect }: { id: number; onSelect(item: string): void }) {
  const all = useAllCaptures();
  if (all.data === undefined) return <Quiet says="Loading…" />;
  const request = all.data.find((candidate) => candidate.job_id === id);
  if (request === undefined) return <Quiet says="That request is not known." />;

  return (
    <Panel title={`Capture request, job #${request.job_id}`}>
      {request.state === "open" ? (
        <CaptureAnswerForm request={request} />
      ) : (
        <div className="capture-form">
          <p className="capture-prompt">{request.prompt_text}</p>
          <p className="capture-left">{CLOSED_TEXT[request.state]}</p>
          {request.note_id !== null && (
            <div className="capture-actions">
              <Button onClick={() => onSelect(formatItem({ kind: "note", id: request.note_id as number }))}>
                Open note #{request.note_id}
              </Button>
            </div>
          )}
        </div>
      )}
    </Panel>
  );
}
