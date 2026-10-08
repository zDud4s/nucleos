import { useKnowledgeHistory } from "../../data/knowledge";
import { ErrorNote, Quiet, RelativeTime, Well } from "../../ui";
import "./knowledge.css";

export interface ChainProps {
  /** The knowledge row whose history is read. */
  id: number;
}

/**
 * What a row replaced, what replaced it, and every decision it has been through.
 *
 * A component of its own so the query lives and dies with the disclosure: an
 * `enabled: false` query on a closed row would still occupy a cache entry per
 * row, and the point of not fetching is that nothing is asked for.
 */
export function Chain({ id }: ChainProps) {
  const history = useKnowledgeHistory(id);

  if (history.isError) {
    return (
      <ErrorNote>
        the núcleo did not answer — this one&apos;s history is not known
      </ErrorNote>
    );
  }
  if (history.data === undefined)
    return <p className="learned-chain-loading">reading…</p>;

  const { events, replaced, replaced_by: replacedBy } = history.data;

  return (
    // A well and not a box: the chain is the same row seen further back in time,
    // and the rung below the row is what the system calls a recess cut into a
    // surface. `reads` because every line of it is somebody's writing, which the
    // well's default mono face would deny; `capped` because the decisions grow by
    // one each time a person answers something.
    <Well as="div" reads capped>
      {replacedBy !== null && (
        <p className="learned-chain-line">
          Replaced by <strong>{replacedBy.title}</strong>.
        </p>
      )}
      {replaced.length > 0 && (
        <>
          <p className="learned-chain-line">
            What it replaced, most recent first:
          </p>
          <ul className="learned-chain-list">
            {replaced.map((older) => (
              <li key={older.id}>
                <span className="learned-title">{older.title}</span>
                <span className="learned-body">{older.body}</span>
              </li>
            ))}
          </ul>
        </>
      )}
      {/* The one genuine absence on this page: a chain with no chain in it. One
          line, nothing to teach, and `Quiet`'s muted rung rather than the faint
          one — in an empty region the sentence is the content. */}
      {replaced.length === 0 && replacedBy === null && (
        <Quiet says="This one replaced nothing and nothing has replaced it." />
      )}
      {events.length > 0 && (
        <ul className="learned-events">
          {events.map((event) => (
            <li key={event.id}>
              <span className="learned-event-status">{event.to_status}</span>
              <span className="learned-event-note">{event.note ?? ""}</span>
              <RelativeTime at={event.at} />
            </li>
          ))}
        </ul>
      )}
    </Well>
  );
}
