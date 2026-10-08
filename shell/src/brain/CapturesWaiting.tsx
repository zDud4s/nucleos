import { useOpenCaptures } from "../data/captures";
import { Count, Panel } from "../ui";
import { CaptureAnswerForm } from "./CaptureAnswerForm";
import { formatItem } from "./item-ref";
import "./capture.css";

/**
 * "Asked of you": the open capture requests, soonest deadline first. One request, one form — no
 * batches and no groups. Draws nothing when nothing is open.
 */
export function CapturesWaiting({ onSelect }: { onSelect(item: string): void }) {
  const open = useOpenCaptures();
  const rows = [...(open.data ?? [])].sort((a, b) => a.deadline.localeCompare(b.deadline));
  if (rows.length === 0) return null;

  return (
    <Panel title="Asked of you" aside={<Count n={rows.length} />}>
      {rows.map((request) => (
        <div key={request.job_id} className="capture-item">
          <button
            type="button"
            className="capture-open"
            onClick={() => onSelect(formatItem({ kind: "capture", id: request.job_id }))}
          >
            Job #{request.job_id} · {request.project_id}
          </button>
          <CaptureAnswerForm request={request} />
        </div>
      ))}
    </Panel>
  );
}
