import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import { useRecentFeed, type FeedEntry } from "../data/feed";
import { ErrorNote, Panel, RefusalNote, RelativeTime } from "../ui";
import { KindBadge } from "../pages/Feed";
/*
  The rules this block is drawn with are `ap-` and live in `pages/autopilot.css`, where
  they were written for the cockpit. Imported here rather than copied, because a CSS
  import in this app is a global side effect and a second set of rules under a second
  name is how two surfaces that are meant to be one thing start to drift. The family name
  stays the cockpit's: it is the same list, embedded twice.
*/
import "../pages/autopilot.css";

/** How many lines the cockpit shows. Design §6.1: the last ten, and a way to the rest. */
export const EMBED_LINES = 10;

export interface FeedEmbedProps {
  /** How many lines to show. Ten in the cockpit; five on Home, where it is one block of five. */
  lines?: number;
}

/**
 * The last few lines, without freezing them.
 *
 * `useRecentFeed` rather than `useFeed({ limit: 10 })`, and the difference is
 * not cosmetic: `limit` is one of the daemon's *search* fields, so asking for it
 * flips `GET /feed` from a listing to a question about the past and the hook
 * turns its poll off. An embed that stopped refreshing the moment it was drawn
 * would be a cockpit showing this morning's news all afternoon.
 *
 * **In `app/` rather than in a page, because two pages want it.** It was inside
 * `pages/Autopilot.tsx`, which made "the last few lines" a thing you could only have by
 * importing a page from another page. It is not in `ui/` either: everything there is a
 * pure primitive that renders what it is handed, and this asks the daemon a question of
 * its own.
 */
export function FeedEmbed({ lines = EMBED_LINES }: FeedEmbedProps) {
  const feed = useRecentFeed();
  const rows = (feed.data ?? []).slice(0, lines);

  return (
    <Panel
      title="Lately"
      aside={
        <Link className="ap-link" to="/feed">
          The whole feed
        </Link>
      }
    >
      {feed.isError && feed.data === undefined && <FeedError error={feed.error} />}
      {feed.data !== undefined && rows.length === 0 && (
        <p className="ap-empty">the núcleo has not written a line yet.</p>
      )}
      {rows.length > 0 && (
        <ul className="ap-feed" aria-label="Recent feed lines">
          {rows.map((entry) => (
            <FeedLine key={entry.id} entry={entry} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/**
 * A daemon summary, written for a person. The núcleo's lines carry markdown backticks around
 * a literal (`completed`) and a hedged plural ("3 item(s)"); neither belongs on screen.
 */
function plainSummary(summary: string): string {
  return summary
    .replace(/`/g, "")
    .replace(/\b(\d+) item\(s\)/g, (_all, count: string) =>
      count === "1" ? "1 item" : `${count} items`,
    )
    .replace(/item\(s\)/g, "items");
}

function FeedLine({ entry }: { entry: FeedEntry }) {
  const summary = plainSummary(entry.summary);
  return (
    <li className="ap-feed-line">
      <KindBadge kind={entry.kind} />
      <span className="ap-feed-summary">
        {/* The same door the Feed page gives these rows: `/runs/{id}` exists. */}
        {entry.run_id === null ? (
          summary
        ) : (
          <Link
            to={`/runs/${String(entry.run_id)}`}
            className="ap-link"
            aria-label={`${summary} — open run ${String(entry.run_id)}`}
          >
            {summary}
          </Link>
        )}
      </span>
      <RelativeTime at={entry.created_at} />
    </li>
  );
}

function FeedError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the feed</ErrorNote>;
}
