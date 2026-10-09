// §spec pilar-de-web
import { useEffect, useId, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { Search } from "lucide-react";
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
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
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
 * archive already has, and `SearchOutcome` reads it as that rather than as
 * an error.
 *
 * No address bar here (design §6.16): this tab is the archive, not a
 * browser — driving a real one is `pages/Browser.tsx`. The one field at the
 * top reads an address into the archive; it never navigates anywhere.
 */
export function Web({ embedded = false }: { embedded?: boolean } = {}) {
  const params = useParams({ strict: false }) as { pageId?: string };
  const rawId = params.pageId;
  const parsedId = rawId === undefined ? null : Number(rawId);
  const pageId = parsedId !== null && Number.isSafeInteger(parsedId) && parsedId > 0 ? parsedId : null;

  const [q, setQ] = useState<string | undefined>(undefined);
  const pages = useWebPages(q);
  const rows = pages.data;
  const loading = pages.data === undefined && !pages.isError;
  // Nothing read at all and nobody filtering: one teaching sentence across the page, rather than
  // an empty list beside an empty reader saying the same thing twice.
  const untouched = !loading && q === undefined && rows !== undefined && rows.length === 0 && pageId === null;

  const body = (
    <>
      <WebBar onFilter={setQ} />

      {untouched ? (
        <Teach title="Nothing has been read yet">
          <p>Paste an address above to read it now. Every page read stays here, indexed and searchable.</p>
        </Teach>
      ) : (
        <div className="web-layout">
          <ArchiveList rows={rows} loading={loading} q={q} selected={pageId} />

          <div className="web-detail">
            {pageId === null && (
              <Teach title="Nothing is open">
                <p>Pick a page from the archive to read it here.</p>
              </Teach>
            )}
            {pageId !== null && <ReaderView key={pageId} id={pageId} />}
          </div>
        </div>
      )}
    </>
  );

  if (embedded) return body;

  return (
    <>
      <PageHeader title="Web" headline={headline(rows)} />
      {body}
    </>
  );
}

/**
 * The archive tab's header, for `WebTabs`, which keeps the tab list outside both tabs.
 * It reads the whole archive, so the count no longer follows the filter box.
 */
export function WebHeader() {
  return <PageHeader title="Web" headline={headline(useWebPages(undefined).data)} />;
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

/* ---------------------------------------------------------------- the bar -- */

/**
 * An address, by its shape: a scheme, or one token with a dot in it and no spaces. Anything else
 * is words, and words filter the archive.
 */
export function looksLikeUrl(text: string): boolean {
  const t = text.trim();
  if (t === "" || /\s/.test(t)) return false;
  return /^https?:\/\//i.test(t) || /^[\w-]+(\.[\w-]+)+(:\d+)?(\/\S*)?$/.test(t);
}

function asUrl(text: string): string {
  const t = text.trim();
  return /^https?:\/\//i.test(t) ? t : `https://${t}`;
}

/** How long typing rests before the archive is asked again. */
const FILTER_REST_MS = 200;

/**
 * One field for the page's three ways in, which used to be three panels with a box and a button
 * each (HIG `searching.md`: "make your app's content searchable through a single location").
 *
 * The text decides what Enter does. An address is read; words filter the archive as they are
 * typed, and Enter takes the same words outward to the search provider — the archive answers
 * first, the web second, so nothing is fetched that is already here.
 */
function WebBar({ onFilter }: { onFilter: (q: string | undefined) => void }) {
  const read = useWebRead();
  const search = useWebSearch();
  const navigate = useNavigate();
  const [text, setText] = useState("");
  const trimmed = text.trim();
  const isUrl = looksLikeUrl(text);
  const hintId = useId();

  useEffect(() => {
    const next = isUrl || trimmed === "" ? undefined : trimmed;
    const timer = setTimeout(() => onFilter(next), FILTER_REST_MS);
    return () => clearTimeout(timer);
  }, [trimmed, isUrl, onFilter]);

  const busy = read.isPending || search.isPending;
  // A search answer belongs to the words that asked it; editing them retires it.
  const asked = search.variables?.query === trimmed;
  const searched = asked ? search.data : undefined;

  return (
    <div className="web-bar-block">
      <form
        className="web-bar"
        role="search"
        aria-label="Read or search"
        onSubmit={(event) => {
          event.preventDefault();
          if (trimmed === "" || busy) return;
          if (isUrl) {
            read.mutate(asUrl(trimmed), {
              onSuccess: (view) => {
                setText("");
                void navigate({ to: `/web/pages/${view.id}` });
              },
            });
          } else {
            search.mutate({ query: trimmed });
          }
        }}
      >
        <label className="web-bar-field">
          <Search aria-hidden="true" size={16} strokeWidth={1.75} />
          <input
            value={text}
            aria-label="Read an address or filter the archive"
            aria-describedby={hintId}
            placeholder="Paste an address to read, or type to filter the archive"
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => setText(event.target.value)}
          />
        </label>
        {isUrl || trimmed === "" ? (
          <Button type="submit" intent="go" disabled={trimmed === "" || busy}>
            {read.isPending ? "Reading…" : "Read"}
          </Button>
        ) : (
          <Button type="submit" disabled={busy}>
            {search.isPending ? "Searching…" : "Search the web"}
          </Button>
        )}
      </form>
      <p id={hintId} className="web-bar-hint">
        {isUrl || trimmed === ""
          ? "Pages from outside the allowlist are summarised by the local model before any agent reads them."
          : "Filtering the archive as you type. Press Enter to search the web as well."}
      </p>

      {read.data !== undefined && <ReadOutcome view={read.data} />}
      {read.isError && <ReadRefusal error={read.error} />}
      {searched !== undefined && <SearchOutcome query={trimmed} view={searched} />}
      {search.isError && asked && <SearchRefusal error={search.error} />}
    </div>
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
        {view.from_cache ? "Already in the archive" : "Read and stored"}
        {" — "}
        <Link to={`/web/pages/${view.id}`}>{view.title ?? view.final_url}</Link>
      </p>
      {view.final_url !== view.requested_url && (
        <p className="web-outcome-redirect">redirected to {view.final_url}</p>
      )}
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

function SearchOutcome({ query, view }: { query: string; view: SearchView }) {
  return (
    <Panel title={`Web search for “${query}”`}>
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
            the search, and announcing these two as its siblings would tell a
            screen reader the opposite of what the page means. */}
        {view.cached.length > 0 && (
          <Section label="already in the archive" level={3}>
            <Rows label="Already read">
              {view.cached.map((hit) => (
                <Row key={hit.id}>
                  <Link to={`/web/pages/${hit.id}`}>{hit.title ?? hit.final_url}</Link>
                  <span className="web-search-snippet">{hit.snippet}</span>
                </Row>
              ))}
            </Rows>
          </Section>
        )}

        {view.provider !== "unavailable" && (
          <Section label={`from ${view.provider}`} level={3}>
            {view.results.length === 0 ? (
              <Quiet says="nothing came back" />
            ) : (
              <Rows label="Search results">
                {view.results.map((result) => (
                  <Row key={result.url}>
                    <span className="web-search-title">{result.title}</span>
                    <span className="web-search-url">{result.url}</span>
                    <span className="web-search-snippet">{result.snippet}</span>
                  </Row>
                ))}
              </Rows>
            )}
          </Section>
        )}
      </div>
    </Panel>
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
  selected,
}: {
  rows: Hit[] | undefined;
  loading: boolean;
  q: string | undefined;
  selected: number | null;
}) {
  return (
    <Panel title="Archive">
      {loading && <p className="web-loading">reading the archive…</p>}
      {!loading && rows !== undefined && rows.length === 0 && (
        <Teach title={q === undefined ? "Nothing has been read yet" : "Nothing here matches"}>
          <p>
            {q === undefined
              ? "Paste an address above to read it now."
              : "Press Enter to search the web for it, or clear the field to see the whole archive."}
          </p>
        </Teach>
      )}
      {rows !== undefined && rows.length > 0 && (
        <Rows label="Archive">
          {rows.map((row) => (
            <ArchiveRow key={row.id} row={row} active={row.id === selected} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

/**
 * One archived page, as a `Row` of the archive's `Rows`: the column is scanned
 * down for the page to open, and the reader opens beside it.
 *
 * Two lines, not one four-column grid. The column is 22rem, and a grid that
 * gave the host a fixed `8rem` and the badge and the time their natural width
 * left the title nothing — it broke one letter to a line. The title now owns
 * its line with the time beside it, and the host sits under it.
 *
 * The trust badge appears only for a quarantined page. Raw is what almost
 * every row is, and a badge on every row says nothing; the exception is the
 * thing worth seeing while scanning. The reader still badges both.
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
    <Row current={active}>
      <Link className="web-row-link" to={`/web/pages/${row.id}`} aria-current={active ? "page" : undefined}>
        <span className="web-row-title">{row.title ?? row.final_url}</span>
        <RelativeTime at={row.fetched_at} />
        <span className="web-row-meta">
          <span className="web-row-host">{row.host}</span>
          {row.trust_at_fetch === "quarantined" && <StateBadge domain="web_trust" state={row.trust_at_fetch} />}
        </span>
      </Link>
      <p className="web-row-snippet">{row.snippet}</p>
    </Row>
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

/**
 * The page itself, under one line of where it came from.
 *
 * What a reader needs before the text is the source and whether it was trusted, so those lead,
 * in a sentence-sized line with the two badges. The record of how it was fetched — the address
 * as asked, the rule that decided the trust — stays in full and verbatim, but under a disclosure:
 * it is there to be checked, not read every time. A redirect is the exception and is shown open,
 * because an address that ended somewhere else is exactly what somebody must notice.
 */
function ReaderPanel({ page }: { page: Page }) {
  const redirected = page.final_url !== page.requested_url;

  return (
    <Panel title={page.title ?? page.final_url}>
      <div className="web-reader-meta">
        <span>{page.host}</span>
        {page.byline !== null && <span>{page.byline}</span>}
        <span>
          read <RelativeTime at={page.fetched_at} />
        </span>
        <StateBadge domain="web_trust" state={page.trust_at_fetch} />
        <StateBadge domain="web_extract" state={page.extract_status} />
      </div>

      {redirected && (
        <dl className="web-facts web-facts-open">
          <div className="web-fact">
            <dt>Requested</dt>
            {/* Verbatim, punycode and all — the same posture Waiting takes with an origin. */}
            <dd className="web-url">{page.requested_url}</dd>
          </div>
          <div className="web-fact">
            <dt>Ended at</dt>
            <dd className="web-url">{page.final_url}</dd>
          </div>
        </dl>
      )}

      {/* Plain text, never parsed apart and never `dangerouslySetInnerHTML` — a
          quarantined page's banner and summary are a stranger's prose, exactly as
          untrusted as the rest of it. The badge above already says which this is. */}
      <pre className="web-content">{page.content_md}</pre>

      <details className="web-record">
        <summary>How it was fetched</summary>
        <dl className="web-facts">
          {!redirected && (
            <div className="web-fact">
              <dt>Address</dt>
              <dd className="web-url">{page.final_url}</dd>
            </div>
          )}
          <div className="web-fact">
            <dt>Trust rule</dt>
            <dd>{page.trust_rule}</dd>
          </div>
          <div className="web-fact">
            <dt>Size</dt>
            <dd>{Math.max(1, Math.round(page.bytes / 1024))} KB</dd>
          </div>
        </dl>
      </details>
    </Panel>
  );
}
