import { useCallback, useEffect, useState } from "react";
import {
  getWebPage, listWebPages, readWebPage, searchWeb,
  type ConnectionState, type WebHit, type WebPage, type WebSearchResult,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

/**
 * The archive of what NucleOS has read, a search of it, and the two doors into it.
 *
 * Still NOT a browser, and the distinction is narrower than it used to be, so it is worth stating
 * exactly. There is no rendering engine here, no navigation, no session and no cookies: a read
 * fetches one URL through the same sidecar an agent uses, extracts the article, files it, and shows
 * you the text. Following a link means asking for that link by name. The browsing sidecar (spec
 * §3.5) is still the thing that will browse, because the daemon runs without this window open.
 *
 * What changed is that `/web/search` and `/web/read` were reachable only by an agent, which made
 * this tab a mirror with no handle: you could see what a run had read and could not read the same
 * page yourself to check it. Both doors go through the daemon's own routes, so a page you fetch
 * here is trust-decided, quarantined and filed by exactly the rules a run's read obeys.
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

/**
 * What the provider offered, none of it fetched.
 *
 * Kept visually apart from the archive rows above it, because the difference is the whole point:
 * these are strangers' titles and snippets that no rule has judged, and reading one is an act with
 * consequences — it puts that page's prose through the trust decision and into the archive.
 */
function Destinations({
  results, provider, busy, onRead,
}: {
  results: WebSearchResult[];
  provider: string;
  busy: string | null;
  onRead: (url: string) => void;
}) {
  return (
    <Panel title="On the web" aside={provider}>
      {results.length === 0 ? (
        <p className="faint">
          The provider returned nothing. An installation with no API key still searches the archive
          above — that list is answered locally and does not need one.
        </p>
      ) : (
        <ul className="web-out">
          {results.map((result) => (
            <li key={result.url}>
              <span className="web-out__title">{result.title}</span>
              <span className="web-out__url">{result.url}</span>
              <span className="web-out__snippet">{result.snippet}</span>
              <Button
                size="sm"
                disabled={busy !== null}
                onClick={() => onRead(result.url)}
              >
                {busy === result.url ? "Reading…" : "Read it"}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

export default function Web({ token, connection }: WebProps) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<WebHit[] | null>(null);
  const [open, setOpen] = useState<WebPage | null>(null);
  const [failed, setFailed] = useState(false);
  const [url, setUrl] = useState("");
  /** The URL currently being fetched, so only its own button says "Reading…". */
  const [reading, setReading] = useState<string | null>(null);
  const [outward, setOutward] = useState<{ provider: string; results: WebSearchResult[] } | null>(null);
  const [note, setNote] = useState<string | null>(null);

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

  /**
   * Asks the provider as well as the archive.
   *
   * A separate action from the archive search rather than the same box doing both, because the two
   * cost different things: searching what has been read is a local query, and searching the web
   * spends an API call on a third party. A single box would spend it on every keystroke's worth of
   * curiosity about the archive.
   */
  const searchOut = useCallback(
    async (q: string) => {
      if (token === null || q.trim() === "") return;
      setNote(null);
      const result = await searchWeb(token, q.trim());
      if (!result.ok) {
        setNote(
          result.status === 503
            ? "The web pillar is off. It stays off until enabled: true in .ai/web.yaml."
            : "Could not search the web.",
        );
        setOutward(null);
        return;
      }
      // The cached half is the archive list this page already shows, so only the outward half is
      // new information. Showing both would put the same rows on screen twice under two headings.
      setOutward({ provider: result.value.provider, results: result.value.results });
      setHits(result.value.cached);
    },
    [token],
  );

  /**
   * Fetches one page and opens what was filed.
   *
   * Re-read through `getWebPage` rather than rendered from the read's own response: the two shapes
   * differ, and the archive row is the one that carries `bytes` and `byline` — so the reader shows
   * the page as the archive holds it, which is what a run would later see.
   */
  const read = useCallback(
    async (target: string) => {
      if (token === null || target.trim() === "") return;
      setReading(target);
      setNote(null);
      const result = await readWebPage(token, target.trim());
      setReading(null);
      if (!result.ok) {
        setNote(
          result.status === 503
            ? "The web pillar is off. It stays off until enabled: true in .ai/web.yaml."
            : result.status === 403
              ? "This token may search but not fetch. Reading a page needs the admin scope."
              : result.status === 400
                ? "The daemon would not fetch that address."
                : "Could not read that page.",
        );
        return;
      }
      setNote(
        result.value.from_cache
          ? "Already in the archive — this is the copy that was read before, not a fresh fetch."
          : null,
      );
      await load(query);
      await openPage(result.value.id);
    },
    [token, load, openPage, query],
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
        Everything NucleOS has read from the web, searchable, plus the two ways to add to it. The
        badge says whether the agent saw the page itself or a summary a local model wrote from it —
        and a page you fetch here goes through the same decision, so what you see is what a run
        would get.
      </Teach>

      <Panel title="Read a page" aside="through the same door an agent uses">
        <form
          className="web-fetch"
          onSubmit={(event) => {
            event.preventDefault();
            void read(url);
          }}
        >
          <input
            type="url"
            value={url}
            placeholder="https://…"
            aria-label="Address to read"
            onChange={(event) => setUrl(event.target.value)}
          />
          <Button type="submit" variant="approve" disabled={url.trim() === "" || reading !== null}>
            {reading === url.trim() ? "Reading…" : "Read"}
          </Button>
        </form>
        {note !== null && <p className="gate-note">{note}</p>}
      </Panel>

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
          {/* Second verb on the same words, and a separate button on purpose: this one spends an
              API call on a third party, and the one beside it does not. */}
          <Button
            variant="ghost"
            disabled={query.trim() === ""}
            onClick={() => void searchOut(query)}
          >
            Search the web too
          </Button>
          {query.length > 0 && (
            <Button
              variant="ghost"
              onClick={() => {
                setQuery("");
                setOutward(null);
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

      {outward !== null && (
        <Destinations
          results={outward.results}
          provider={outward.provider}
          busy={reading}
          onRead={(target) => void read(target)}
        />
      )}
    </>
  );
}
