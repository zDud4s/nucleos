import { useCallback, useEffect, useState } from "react";
import {
  getWebPage, listWebPages,
  type ConnectionState, type WebHit, type WebPage,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

/**
 * The archive of what NucleOS has read, and a search of it.
 *
 * Deliberately NOT a browser. There is no address bar and no way to fetch a page from this tab, and
 * that is the v1 scope rather than an omission: the browser belongs to the sidecar when it arrives
 * (spec §3.5), because the daemon runs without this window open and a browser that only exists
 * while the tray app is up is useless to the Autopilot.
 *
 * What this tab is for is the thing nothing else can show: WHICH pages an agent has read, and what
 * it actually saw when it read them.
 */

/**
 * The trust badge.
 *
 * `raw` means the page's own words went to the agent; `quarantined` means a local model read it and
 * the agent saw a summary. Showing this is the whole point of the tab — it is the only place a
 * person can see that a stranger's prose entered an agent's context, and in which form.
 *
 * The wording is deliberately about the past. `trust_at_fetch` records one read and grants nothing:
 * the daemon decides again every time, so a page marked `raw` here can still arrive quarantined to
 * a run tonight.
 */
function TrustBadge({ trust }: { trust: string }) {
  if (trust === "raw") {
    return <Badge tone="active">read as written</Badge>;
  }
  return <Badge tone="shadow">summarised locally</Badge>;
}

function Reader({ page, onClose }: { page: WebPage; onClose: () => void }) {
  return (
    <Panel
      title={page.title ?? page.host}
      aside={<TrustBadge trust={page.trust_at_fetch} />}
    >
      <p className="faint">
        <a href={page.final_url} target="_blank" rel="noreferrer noopener">
          {page.final_url}
        </a>
      </p>
      {page.requested_url !== page.final_url && (
        <p className="faint">
          Redirected here from <code>{page.requested_url}</code>. Trust was decided on both, and a
          redirect can only ever lose it.
        </p>
      )}
      <p className="faint">
        {page.extract_status === "fallback"
          ? "No article was found on this page, so what follows is its structure rather than its prose."
          : "Extracted article text."}
        {" "}
        {page.byline !== null && page.byline.length > 0 && <>By {page.byline}. </>}
        Read {relativeTime(page.fetched_at)}, {page.bytes.toLocaleString()} bytes of source.
      </p>
      <pre className="web-body">{page.content_md}</pre>
      <Button onClick={onClose}>Close</Button>
    </Panel>
  );
}

interface WebProps {
  token: string | null;
  connection: ConnectionState;
}

export default function Web({ token, connection }: WebProps) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<WebHit[] | null>(null);
  const [open, setOpen] = useState<WebPage | null>(null);
  const [failed, setFailed] = useState(false);

  const load = useCallback(
    async (q: string) => {
      if (token === null) return;
      const found = await listWebPages(token, q);
      setFailed(found === null);
      setHits(found ?? []);
    },
    [token],
  );

  useEffect(() => {
    if (connection !== "connected") return;
    void load("");
  }, [connection, load]);

  const openPage = useCallback(
    async (id: number) => {
      if (token === null) return;
      const page = await getWebPage(token, id);
      setFailed(page === null);
      if (page !== null) setOpen(page);
    },
    [token],
  );

  if (connection !== "connected" || token === null) {
    return <ErrorNote>The daemon is not reachable, so there is nothing to show.</ErrorNote>;
  }

  if (open !== null) {
    return <Reader page={open} onClose={() => setOpen(null)} />;
  }

  return (
    <>
      <Teach title="What has been read">
        Everything NucleOS has read from the web, searchable. This is an archive, not a browser:
        pages arrive here when you or an agent asks for one, and the badge says whether the agent
        saw the page itself or a summary a local model wrote from it.
      </Teach>

      <Panel title="Read" aside={hits === null ? undefined : `${hits.length} pages`}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void load(query);
          }}
        >
          <input
            type="search"
            value={query}
            placeholder="Search what has been read"
            aria-label="Search what has been read"
            onChange={(event) => setQuery(event.target.value)}
          />
          <Button type="submit">Search</Button>
          {query.length > 0 && (
            <Button
              variant="ghost"
              onClick={() => {
                setQuery("");
                void load("");
              }}
            >
              Clear
            </Button>
          )}
        </form>

        {failed && <ErrorNote>Could not read the archive.</ErrorNote>}

        {hits !== null && hits.length === 0 && !failed && (
          <p className="faint">
            {query.length > 0
              ? "Nothing read so far matches that."
              : "Nothing has been read yet. The pillar is off until enabled: true in .ai/web.yaml."}
          </p>
        )}

        <ul className="web-list">
          {(hits ?? []).map((hit) => (
            <li key={hit.id}>
              <button className="web-row" onClick={() => void openPage(hit.id)}>
                <span className="web-row__title">{hit.title ?? hit.final_url}</span>
                <span className="web-row__meta">
                  {hit.host} · {relativeTime(hit.fetched_at)}
                </span>
                <span className="web-row__snippet">{hit.snippet}</span>
              </button>
              <TrustBadge trust={hit.trust_at_fetch} />
            </li>
          ))}
        </ul>
      </Panel>
    </>
  );
}
