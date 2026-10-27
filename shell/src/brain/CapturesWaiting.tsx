import { useOpenCaptures, type CaptureRequest } from "../data/captures";
import { Count, Panel } from "../ui";
import { CaptureAnswerForm } from "./CaptureAnswerForm";
import { formatItem } from "./item-ref";
import "./capture.css";

/**
 * "Questions for you": the open capture requests, soonest deadline first. One request, one form; the
 * ones a daily digest asked sit together under it, older ones stand alone. Draws nothing when nothing is open.
 */
export function CapturesWaiting({ onSelect, selected }: { onSelect(item: string): void; selected?: string }) {
  const open = useOpenCaptures();
  const rows = [...(open.data ?? [])].sort((a, b) => a.deadline.localeCompare(b.deadline));
  if (rows.length === 0) return null;

  const digests = new Map<number, CaptureRequest[]>();
  const loose: CaptureRequest[] = [];
  for (const request of rows) {
    if (request.digest_id === null) {
      loose.push(request);
      continue;
    }
    digests.set(request.digest_id, [...(digests.get(request.digest_id) ?? []), request]);
  }

  const item = (request: CaptureRequest) => (
    <div key={request.job_id} className="capture-item">
      <button
        type="button"
        className="capture-open"
        onClick={() => onSelect(formatItem({ kind: "capture", id: request.job_id }))}
      >
        Job #{request.job_id} · {request.project_id}
      </button>
      {/* The open panel holds this request's form; two drafts of one answer would diverge. */}
      {selected === formatItem({ kind: "capture", id: request.job_id }) ? (
        <p className="capture-left">answering in the panel</p>
      ) : (
        <CaptureAnswerForm request={request} />
      )}
    </div>
  );

  return (
    <Panel title="Questions for you" aside={<Count n={rows.length} />}>
      {[...digests].map(([digestId, group]) => (
        <section key={digestId} className="capture-digest">
          <h3 className="capture-digest-head">Daily questions · {group.length} open</h3>
          {group.map(item)}
        </section>
      ))}
      {loose.map(item)}
    </Panel>
  );
}
