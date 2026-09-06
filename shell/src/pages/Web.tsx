// §spec pilar-de-web
import { useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useWebPage,
  useWebPages,
  useWebRead,
  useWebSearch,
  type Hit,
  type Page,
  type ReadView,
  type SearchView,
} from "../data/web";
import {
  Button,
  ErrorNote,
  Inset,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Section,
  StateBadge,
  Teach,
} from "../ui";
import "./web.css";

/**
 * Web — the local archive, one component serving `/web` and
 * `/web/pages/$pageId` (the `Chats.tsx` master-detail idiom): a list that is
 * always on screen, and a reader that opens beside it rather than instead of
 * it.
 *
 * Two facts from `data/web.ts`'s header carry into every panel below.
 * **The quarantine is one string, not a structure** — `content_md` under
 * `trust_at_fetch: "quarantined"` is the local model's own prose, banner and
 * all, and this page renders it as plain text with a badge beside it rather
 * than parsing it apart. There is no `dangerouslySetInnerHTML` anywhere in
 * this file. **`provider: "unavailable"` is a state, not a failure** — a
 * search with no provider configured still answers 200 with whatever the
 * archive already has, and `WebSearchPanel` reads it as that rather than as
 * an error.
 *
 * No address bar here (design §6.16): this tab is the archive, not a
 * browser — driving a real one is `pages/Browser.tsx`.
 */
export function Web() {
  const params = useParams({ strict: false }) as { pageId?: string };
  const rawId = params.pageId;
  const parsedId = rawId === undefined ? null : Number(rawId);
  const pageId = parsedId !== null && Number.isSafeInteger(parsedId) && parsedId > 0 ? parsedId : null;

  const [q, setQ] = useState<string | undefined>(undefined);
  const pages = useWebPages(q);
  const rows = pages.data;

  return (
    <>
      <PageHeader title="Web" headline={headline(rows)} />

      <ReadForm />
      <WebSearchPanel />

      <div className="web-layout">
        <ArchiveList rows={rows} loading={pages.data === undefined && !pages.isError} q={q} onSearch={setQ} selected={pageId} />

        <div className="web-detail">
          {pageId === null && (
            <Teach title="Nothing is open">
              <p>Pick a page from the archive, or read one now above — every page read stays here, indexed and searchable.</p>
            </Teach>
          )}
          {pageId !== null && <ReaderView key={pageId} id={pageId} />}
        </div>
      </div>
    </>
  );
}

function headline(rows: Hit[] | undefined): string | undefined {
  if (rows === undefined) return undefined;
  if (rows.length === 0) return "nothing has been read yet";
  const noun = rows.length === 1 ? "page" : "pages";
  return `${rows.length} ${noun} in the archive`;
}

/** The daemon's own sentence, when it really sent one — `RunDetail.tsx`'s pattern. */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------- read a url -- */

function ReadForm() {
  const read = useWebRead();
  const [url, setUrl] = useState("");

  return (
    <Panel title="Read a page">
      <p className="web-note">
        Fetches the page now, quarantining it through the local model when its source is not on the
        allowlist. It is stored and indexed either way, even when the summary itself fails to arrive.
      </p>
      <form
        className="web-read-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (url.trim() === "" || read.isPending) return;
          read.mutate(url.trim(), { onSuccess: () => setUrl("") });
        }}
      >
        <label className="web-field">
          <span>URL</span>
          <input
            value={url}
            aria-label="URL to read"
            placeholder="https://…"
            onChange={(event) => setUrl(event.target.value)}
          />
        </label>
        <Button type="submit" intent="go" disabled={url.trim() === "" || read.isPending}>
          Read
        </Button>
      </form>
      {read.data !== undefined && <ReadOutcome view={read.data} />}
      {read.isError && <ReadRefusal error={read.error} />}
    </Panel>
  );
}

const READ_SENTENCES: Record<string, string> = {
  unprocessable: "nothing readable came back from that address",
};

function ReadOutcome({ view }: { view: ReadView }) {
  return (
    <div className="web-outcome" role="status">
      <p className="web-outcome-line">
        {/* The daemon sends no such prose — `from_cache` is the fact, and this is this
            shell's own sentence for it. */}
        {view.from_cache ? "already in the archive" : "read and stored"}
        {" — "}
        <Link to={`/web/pages/${view.id}`}>open it</Link>
      </p>
      {view.final_url !== view.requested_url && (
        <p className="web-outcome-redirect">redirected to {view.final_url}</p>
      )}
      <StateBadge domain="web_trust" state={view.trust} />
    </div>
  );
}

function ReadRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ ...READ_SENTENCES, ...daemonProse(error) }} />;
  }
  return <ErrorNote>the núcleo did not answer — nothing was read</ErrorNote>;
}

/* ------------------------------------------------------------------ search -- */

/**
 * Searching outward, from the page that is about the archive.
 *
 * **It stays `variant="dim"`, and it is not a `Well`.** `dim` means present but
 * not the thing you came for, which is exactly this: you are here for what has
 * already been read, and the provider is the way to find what has not. A `Well`
 * is the other thing entirely — a recess holding something the machine
 * produced, unbordered, mono, with no heading of its own. This panel has a
 * title, a form, a button and links in it, and none of that belongs in a hole
 * cut into a surface. The one thing it is not is `Projects`' `GatePanel`, which
 * came *out* of `dim` because it carried the page's most consequential
 * sentence; nothing here alerts anybody.
 */
function WebSearchPanel() {
  const search = useWebSearch();
  const [query, setQuery] = useState("");

  return (
    <Panel title="Search" variant="dim">
      <form
        className="web-search-form"
        role="search"
        aria-label="Search the web"
        onSubmit={(event) => {
          event.preventDefault();
          if (query.trim() === "" || search.isPending) return;
          search.mutate({ query: query.trim() });
        }}
      >
        <label className="web-field">
          <span>Search</span>
          <input
            value={query}
            aria-label="Search query"
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <Button type="submit" disabled={query.trim() === "" || search.isPending}>
          Search
        </Button>
      </form>
      {search.data !== undefined && <SearchOutcome view={search.data} />}
      {search.isError && <SearchRefusal error={search.error} />}
    </Panel>
  );
}

function SearchOutcome({ view }: { view: SearchView }) {
  return (
    <div className="web-search-results">
      {/* A 200 with an empty provider — not a failure, and never routed through
          RefusalNote or ErrorNote. It is an absence, so the sentence is `Quiet`;
          the live region stays this page's, because `Quiet` is a sentence and
          not an announcement, and this one arrives after a mutation lands. */}
      {view.provider === "unavailable" && (
        <div role="status">
          <Quiet says="no search provider is configured on this machine — showing only what is already in the archive" />
        </div>
      )}

      {/* `level={3}`: the `Panel` above has already spent this page's `h2` on
          "Search", and announcing these two as its siblings would tell a screen
          reader the opposite of what the page means. */}
      {view.cached.length > 0 && (
        <Section label="already in the archive" level={3}>
          <ul className="web-search-list" aria-label="Already read">
            {view.cached.map((hit) => (
              <Inset as="li" key={hit.id}>
                <Link to={`/web/pages/${hit.id}`}>{hit.title ?? hit.final_url}</Link>
                <span className="web-search-snippet">{hit.snippet}</span>
              </Inset>
            ))}
          </ul>
        </Section>
      )}

      {view.provider !== "unavailable" && (
        <Section label={`from ${view.provider}`} level={3}>
          {view.results.length === 0 ? (
            <Quiet says="nothing came back" />
          ) : (
            <ul className="web-search-list" aria-label="Search results">
              {view.results.map((result) => (
                <Inset as="li" key={result.url}>
                  <span className="web-search-title">{result.title}</span>
                  <span className="web-search-url">{result.url}</span>
                  <span className="web-search-snippet">{result.snippet}</span>
                </Inset>
              ))}
            </ul>
          )}
        </Section>
      )}
    </div>
  );
}

function SearchRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing was searched</ErrorNote>;
}

/* --------------------------------------------------------------- the list -- */

function ArchiveList({
  rows,
  loading,
  q,
  onSearch,
  selected,
}: {
  rows: Hit[] | undefined;
  loading: boolean;
  q: string | undefined;
  onSearch: (q: string | undefined) => void;
  selected: number | null;
}) {
  return (
    <Panel title="Archive">
      <form
        className="web-search-form"
        role="search"
        aria-label="Search the archive"
        onSubmit={(event) => {
          event.preventDefault();
          const typed = new FormData(event.currentTarget).get("q");
          const text = typeof typed === "string" ? typed.trim() : "";
          onSearch(text === "" ? undefined : text);
        }}
      >
        <label className="web-field">
          <span>Filter</span>
          <input name="q" defaultValue={q ?? ""} key={q ?? ""} aria-label="Filter the archive" />
        </label>
        <Button type="submit">Filter</Button>
        {q !== undefined && <Button onClick={() => onSearch(undefined)}>Clear</Button>}
      </form>

      {loading && <p className="web-loading">reading the archive…</p>}
      {!loading && rows !== undefined && rows.length === 0 && (
        <Teach title={q === undefined ? "Nothing has been read yet" : "Nothing matches that filter"}>
          <p>
            {q === undefined
              ? "Read a page above, or search outward — anything read lands here, indexed and searchable."
              : "Clear the filter to see the whole archive."}
          </p>
        </Teach>
      )}
      {rows !== undefined && rows.length > 0 && (
        <ul className="web-list" aria-label="Archive">
          {rows.map((row) => (
            <ArchiveRow key={row.id} row={row} active={row.id === selected} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/**
 * One archived page, as a box you reach into rather than a line you scan past
 * — which is the question that picks `Inset` over `Rows`: this column is a
 * master list, and the whole point of a row is that it opens the reader beside
 * it.
 *
 * `current` and not a border colour of its own. The row used to say "this one"
 * with `border-color: var(--accent)`, and the accent is the one colour in this
 * system that means nothing — the wordmark, links and the focus ring. The
 * neutral answer is `.ui-current`, a 2px rule on the leading edge, which the
 * owner settled on 2026-09-06 after seeing four candidates rendered in both
 * themes. `aria-current` stays on the link, where the destination is.
 */
function ArchiveRow({ row, active }: { row: Hit; active: boolean }) {
  return (
    <Inset as="li" current={active}>
      <Link className="web-row-link" to={`/web/pages/${row.id}`} aria-current={active ? "page" : undefined}>
        <span className="web-row-title">{row.title ?? row.final_url}</span>
        <StateBadge domain="web_trust" state={row.trust_at_fetch} />
        <span className="web-row-host">{row.host}</span>
        <RelativeTime at={row.fetched_at} />
      </Link>
      <p className="web-row-snippet">{row.snippet}</p>
    </Inset>
  );
}

/* ------------------------------------------------------------- the reader -- */

function ReaderView({ id }: { id: number }) {
  const page = useWebPage(id);
  const detail = page.data;

  if (detail === undefined) {
    return page.isError ? <ReaderError error={page.error} /> : <p className="web-loading">reading page {id}…</p>;
  }

  return <ReaderPanel page={detail} />;
}

function ReaderError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "there is no page with that number" }} />;
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this page</ErrorNote>;
}

function ReaderPanel({ page }: { page: Page }) {
  const redirected = page.final_url !== page.requested_url;

  return (
    <Panel title={page.title ?? page.final_url}>
      <dl className="web-facts">
        <div className="web-fact">
          <dt>requested</dt>
          {/* Verbatim, punycode and all — the same posture Waiting takes with an origin. */}
          <dd className="web-url">{page.requested_url}</dd>
        </div>
        {redirected && (
          <div className="web-fact">
            <dt>ended at</dt>
            <dd className="web-url">{page.final_url}</dd>
          </div>
        )}
        <div className="web-fact">
          <dt>host</dt>
          <dd>{page.host}</dd>
        </div>
        {page.byline !== null && (
          <div className="web-fact">
            <dt>byline</dt>
            <dd>{page.byline}</dd>
          </div>
        )}
        <div className="web-fact">
          <dt>read as</dt>
          <dd>
            <StateBadge domain="web_extract" state={page.extract_status} />
          </dd>
        </div>
        <div className="web-fact">
          <dt>trust</dt>
          <dd>
            <StateBadge domain="web_trust" state={page.trust_at_fetch} />
          </dd>
        </div>
        <div className="web-fact">
          <dt>rule</dt>
          <dd>{page.trust_rule}</dd>
        </div>
        <div className="web-fact">
          <dt>fetched</dt>
          <dd>
            <RelativeTime at={page.fetched_at} />
          </dd>
        </div>
      </dl>

      {/* Plain text, never parsed apart and never `dangerouslySetInnerHTML` — a
          quarantined page's banner and summary are a stranger's prose, exactly as
          untrusted as the rest of it. The badge above already says which this is. */}
      <pre className="web-content">{page.content_md}</pre>
    </Panel>
  );
}
