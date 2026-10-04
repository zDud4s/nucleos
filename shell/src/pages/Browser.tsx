import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useBrowserHealth,
  useBrowserSessions,
  useBrowserSites,
  useCloseSession,
  useForgetProfile,
  useBrowserWrites,
  useKeepChain,
  useMakeReadonly,
  useOpenWindow,
  useRevokeSite,
  useReturnWheel,
  type BrowserSession,
  type Site,
  type SubsystemReadout,
  type Written,
} from "../data/browser";
import { useProjects } from "../data/system";
import {
  Badge,
  Button,
  ConfirmButton,
  Count,
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
} from "../ui";
import "./browser.css";

/**
 * Browser — the sessions in flight, the handover's own "keep these?"
 * question, the sites a project has logged into, and the one health
 * reading this pillar has.
 *
 * **Wheel decisions are not decided here.** Taking or refusing a requested
 * wheel is `POST /proposals/{id}/approve|reject`, and `Waiting.tsx` already
 * renders and decides them — this page shows that a session is asking, with
 * a link, and stops there, so the buttons appear exactly once in the app.
 *
 * **The chain dialogue is decided here, and nowhere else.** `POST
 * /browser/return` closes the session in the same call that produces the
 * candidate chain, so the session that question belongs to is already gone
 * from the next read of `GET /browser/sessions` by the time the question is
 * asked — the chain has to travel in the mutation's own response, held as
 * local state, rather than be read back from a list.
 *
 * **Browser health is one subsystem, `browser_sidecar`.** `health.rs` says
 * outright that collapsing it with the Chromium download and page
 * reachability was rejected; this page renders the one state the daemon
 * actually measures and says so, rather than inventing three.
 */
export function Browser({ embedded = false }: { embedded?: boolean } = {}) {
  const sessions = useBrowserSessions();
  const health = useBrowserHealth();
  const [chainDialogue, setChainDialogue] = useState<{ sessionId: number; chain: string[] } | null>(null);

  const body = (
    <>
      <LiveSessions
        view={sessions}
        onReturned={(sessionId, chain) => setChainDialogue({ sessionId, chain })}
      />

      {chainDialogue !== null && (
        <ChainDialogue
          sessionId={chainDialogue.sessionId}
          chain={chainDialogue.chain}
          onSettled={() => setChainDialogue(null)}
        />
      )}

      <OpenAWindow />

      <SiteGrants />

      <BrowserHealth health={health} />
    </>
  );

  if (embedded) return body;

  return (
    <>
      <SessionsHeader />
      {body}
    </>
  );
}

/**
 * The sessions tab's header (headline plus the sidecar's badge), for `WebTabs`, which keeps the
 * tab list outside both tabs. React Query dedups the queries it shares with the body.
 */
export function SessionsHeader() {
  const sessions = useBrowserSessions();
  const health = useBrowserHealth();

  return (
    <PageHeader
      title="Web"
      headline={headline(sessions.data, health.data?.subsystem ?? null)}
      actions={health.data?.subsystem == null ? undefined : <StateBadge domain="pillar" state={health.data.subsystem.status} />}
    />
  );
}

function headline(rows: BrowserSession[] | undefined, subsystem: SubsystemReadout | null): string | undefined {
  // The pillar being down outranks how many sessions are open: none of them can be doing
  // anything. The reason travels with it — it was in a panel at the bottom of the page.
  if (subsystem !== null && subsystem.status !== "ok" && subsystem.status !== "disabled") {
    const why = subsystem.reason === undefined ? "" : ` — ${subsystem.reason}`;
    return `the browser sidecar is ${subsystem.status}${why}`;
  }
  if (rows === undefined) return undefined;
  if (rows.length === 0) return "nothing is open right now";
  const asking = rows.filter((row) => row.mode === "wheel-requested").length;
  const noun = rows.length === 1 ? "session" : "sessions";
  return asking === 0 ? `${rows.length} ${noun} open` : `${rows.length} ${noun} open — ${asking} asking for the wheel`;
}

/**
 * A panel's own prose — in front of a list that has something in it, one click
 * behind the line when it has not.
 *
 * The sentences are the same either way and what changes is where a reader
 * meets them. Above a populated list the note is what somebody needs *before*
 * pressing a button: that a wheel request is answered on Waiting and not here.
 * Above an empty one it is a paragraph explaining rows that are not there.
 * Keeping it rather than cutting it is the point of the disclosure — "nothing
 * is open right now" on its own reads as a list that failed to load, and the
 * paragraph is what makes the emptiness a fact. `System.tsx` has the same
 * helper, for the same reason.
 *
 * Not every note on this page belongs behind one, and the two that do not are
 * both about placement rather than about prose. `SiteGrants` puts its project
 * picker between the note and the list, so folding the note into the empty line
 * would lift an answer above the control that changes it; `OpenAWindow`'s
 * absence is the project registry's rather than that panel's own, so the "why?"
 * would be answering a question nobody asked there.
 */
function PanelNote({ empty, says, children }: { empty: boolean; says: string; children: ReactNode }) {
  if (empty) return <Quiet says={says}>{children}</Quiet>;
  return <p className="browser-note">{children}</p>;
}

/** The daemon's own sentence, when it really sent one — `RunDetail.tsx`'s pattern. */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

/* --------------------------------------------------------- 1. live sessions -- */

const MODE_COPY: Record<BrowserSession["mode"], string> = {
  human: "you are driving",
  agent: "agent is driving",
  "wheel-requested": "asking for the wheel",
  "delivery-failed": "the window would not open",
};

function LiveSessions({
  view,
  onReturned,
}: {
  view: ReturnType<typeof useBrowserSessions>;
  onReturned: (sessionId: number, chain: string[]) => void;
}) {
  const closeSession = useCloseSession();
  const returnWheel = useReturnWheel();
  const rows = view.data ?? [];

  return (
    <Panel title="Live sessions" aside={<Count n={view.data?.length} />}>
      <PanelNote empty={view.data !== undefined && rows.length === 0} says="nothing is open right now.">
        Every open browsing session, whatever is driving it — oldest first. A session asking for the
        wheel is decided on <Link to="/waiting">Waiting</Link>; this page only shows that it is asking.
      </PanelNote>
      {view.isError && view.data === undefined && <MutationNote error={view.error} what="nothing is known about the open sessions" />}
      {view.data === undefined && !view.isError && <p className="browser-loading">reading the open sessions…</p>}
      {rows.length > 0 && (
        <Rows label="Live sessions">
          {rows.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              onClose={() => closeSession.mutate(session.id)}
              closePending={closeSession.isPending}
              onReturn={() =>
                returnWheel.mutate(session.id, {
                  onSuccess: (result) => onReturned(session.id, result.chain),
                })
              }
              returnPending={returnWheel.isPending}
            />
          ))}
        </Rows>
      )}
      {closeSession.isError && <MutationNote error={closeSession.error} what="that session could not be closed" />}
      {returnWheel.isError && <MutationNote error={returnWheel.error} what="the wheel could not be given back" />}
    </Panel>
  );
}

function SessionRow({
  session,
  onClose,
  closePending,
  onReturn,
  returnPending,
}: {
  session: BrowserSession;
  onClose: () => void;
  closePending: boolean;
  onReturn: () => void;
  returnPending: boolean;
}) {
  const redirected = session.final_url !== session.requested_url && session.final_url !== "";

  return (
    <Row className="browser-session">
      <div className="browser-card-head">
        <span className="browser-card-mode">{MODE_COPY[session.mode]}</span>
        <span className="browser-meta">{session.project_id ?? "no project"}</span>
        <span className="browser-meta">
          {session.profile_kind} {session.profile_id}
        </span>
        <RelativeTime at={session.opened_at} />
      </div>
      <dl className="browser-facts">
        <div className="browser-fact">
          <dt>asked for</dt>
          {/* Verbatim, punycode and all — an origin shown here is exactly the
              lookalike the wheel decision on Waiting exists to catch. */}
          <dd>{session.requested_url}</dd>
        </div>
        {redirected && (
          <div className="browser-fact">
            <dt>ended at</dt>
            <dd>{session.final_url}</dd>
          </div>
        )}
        <div className="browser-fact">
          <dt>rule</dt>
          <dd>{session.rule}</dd>
        </div>
      </dl>
      {session.refusal !== null && <p className="browser-refusal">{session.refusal}</p>}
      {session.mode === "wheel-requested" && (
        <p className="browser-waiting-link">
          waiting on a decision — <Link to="/waiting">answer it there</Link>
        </p>
      )}
      <div className="browser-actions">
        {session.mode === "human" && (
          <ConfirmButton
            label="Give the wheel back"
            confirmLabel="Close the window and bring the chain back"
            variant="approve"
            disabled={returnPending}
            onConfirm={onReturn}
          />
        )}
        {(session.mode === "agent" || session.mode === "delivery-failed") && (
          <ConfirmButton
            label="Close session"
            confirmLabel="Close it now"
            variant="quiet"
            disabled={closePending}
            onConfirm={onClose}
          />
        )}
      </div>
    </Row>
  );
}

/* ------------------------------------------------------- 2. chain dialogue -- */

/**
 * "Keep these?" — the only way a site grant is ever created.
 *
 * Fed entirely from {@link useReturnWheel}'s own response, held as local
 * state by the caller: the session this belongs to is already closed by the
 * time this renders, so there is no query to read it back from.
 */
function ChainDialogue({
  sessionId,
  chain,
  onSettled,
}: {
  sessionId: number;
  chain: string[];
  onSettled: () => void;
}) {
  const keepChain = useKeepChain();
  // Unticked, and it stays unticked until a person says otherwise. The permissive answer is the one
  // that has to be chosen; a box that arrived ticked would make writing something granted by not
  // reading the screen.
  const [writable, setWritable] = useState(false);

  return (
    <Panel title="Keep these?">
      <p className="browser-note">
        The window closed, and this is where it went. Nothing is granted yet — say yes to the whole
        set or no to all of it; there is no picking a few origins out of the chain.
      </p>
      <ol className="browser-chain" aria-label="Navigation chain">
        {chain.map((url, index) => (
          <li key={index} className="browser-url">
            {url}
          </li>
        ))}
      </ol>
      <label className="browser-check">
        <input
          type="checkbox"
          checked={writable}
          disabled={keepChain.isPending}
          onChange={(event) => setWritable(event.target.checked)}
        />
        <span>Let agents submit forms here, as you</span>
      </label>
      <p className="browser-note">
        The second half of the same question, and a narrower one. Keeping the chain lets an agent
        READ these sites; this lets it press Send on a form it can see — a reply, a ticket, a saved
        filter — on the site you just logged into, and never on the identity providers the login
        passed through. It works without asking you again, so what you get instead is a record: every
        submission is listed under Site grants, by the names of the fields and never their contents.
      </p>
      <div className="browser-actions">
        <ConfirmButton
          label="Keep them"
          confirmLabel="Grant these origins"
          variant="approve"
          disabled={keepChain.isPending}
          onConfirm={() => keepChain.mutate({ sessionId, keep: true, writable }, { onSuccess: onSettled })}
        />
        <ConfirmButton
          label="Keep none"
          confirmLabel="Discard the chain"
          variant="ghost"
          disabled={keepChain.isPending}
          onConfirm={() =>
            keepChain.mutate({ sessionId, keep: false, writable: false }, { onSuccess: onSettled })
          }
        />
      </div>
      {keepChain.isError && <MutationNote error={keepChain.error} what="that answer was not recorded" />}
    </Panel>
  );
}

/* -------------------------------------------------------------- 3. open a window -- */

/**
 * The one door into this pillar that no agent asked for.
 *
 * Deliberately plain — two fields and a button, no proposal and no confirmation — and the
 * plainness is the argument. Every ceremony elsewhere on this screen defends against an
 * AGENT having chosen a destination while carrying a stranger's words; here the person
 * typed the address, so there is nobody to approve. Asking them to approve their own
 * request is the ceremony that teaches people to click through the one that matters.
 *
 * What it is for: until it existed a profile could be repaired, never prepared — the only
 * way to log in was to wait for the agent to walk into the login first. What a session may
 * GRANT is unchanged; the window records where it went and the chain above still answers
 * on the way out.
 *
 * Its own project picker rather than one lifted out of `SiteGrants`: the two answer
 * different questions, and choosing which project's grants to read should not move where
 * a window opens.
 */
function OpenAWindow() {
  const projects = useProjects();
  const [chosen, setChosen] = useState<string | undefined>(undefined);
  const [url, setUrl] = useState("");
  const open = useOpenWindow();
  const options = projects.data ?? [];
  const projectId = chosen ?? options[0]?.project_id;
  const ready = projectId !== undefined && url.trim() !== "" && !open.isPending;

  return (
    <Panel title="Open a window yourself">
      <p className="browser-note">
        A real window on this project&apos;s profile, with no fence and nobody asking. Log in,
        look around, then give it back above — the hosts it went through are offered to keep
        on the way out, which is the only way the list below ever grows.
      </p>

      {projects.data !== undefined && options.length === 0 && <Quiet says="no project is registered yet." />}

      {options.length > 0 && (
        <form
          className="browser-open"
          onSubmit={(event) => {
            event.preventDefault();
            if (projectId === undefined || url.trim() === "" || open.isPending) return;
            open.mutate({ projectId, url: url.trim() });
          }}
        >
          <label className="browser-field">
            <span>Project</span>
            <select
              aria-label="Project for the new window"
              value={projectId ?? ""}
              onChange={(event) => setChosen(event.target.value)}
            >
              {options.map((option) => (
                <option key={option.project_id} value={option.project_id}>
                  {option.project_id}
                </option>
              ))}
            </select>
          </label>

          <label className="browser-field">
            <span>Address</span>
            <input
              type="text"
              aria-label="Address to open"
              placeholder="https://…"
              value={url}
              onChange={(event) => setUrl(event.target.value)}
            />
          </label>

          <div className="browser-actions">
            {/* A plain Button and not a ConfirmButton: this write is additive and
                reversible — the window closes, and it grants nothing on its own. */}
            <Button type="submit" variant="approve" disabled={!ready}>
              Open a window
            </Button>
          </div>
        </form>
      )}

      {open.data !== undefined && (
        <p className="browser-outcome" role="status">
          opened session #{open.data.id} on the {open.data.profile_kind} profile{" "}
          {open.data.profile_id} — give it back above when you are done
        </p>
      )}
      {open.isError && <MutationNote error={open.error} what="the window could not be opened" />}
    </Panel>
  );
}

/* ------------------------------------------------------------- 4. site grants -- */

function SiteGrants() {
  const projects = useProjects();
  const [chosen, setChosen] = useState<string | undefined>(undefined);
  const options = projects.data ?? [];
  const projectId = chosen ?? options[0]?.project_id;
  const sites = useBrowserSites(projectId);
  const revoke = useRevokeSite();
  const readonly = useMakeReadonly();
  const forget = useForgetProfile();
  const rows = sites.data ?? [];

  return (
    <Panel title="Site grants" aside={<Count n={projectId === undefined ? undefined : sites.data?.length} />}>
      <p className="browser-note">
        Where a project&apos;s profile has logged in — a destination it was let into, or an identity
        provider a login passed through on the way. Origins are shown exactly as recorded; a punycode
        host is never prettified back to the glyphs it encodes.
      </p>

      {projects.data !== undefined && options.length === 0 && <Quiet says="no project is registered yet." />}
      {options.length > 0 && (
        <label className="browser-field">
          <span>Project</span>
          <select aria-label="Project" value={projectId ?? ""} onChange={(event) => setChosen(event.target.value)}>
            {options.map((project) => (
              <option key={project.project_id} value={project.project_id}>
                {project.project_id}
              </option>
            ))}
          </select>
        </label>
      )}

      {projectId !== undefined && (
        <>
          {sites.isError && rows.length === 0 && <MutationNote error={sites.error} what="nothing is known about this project's sites" />}
          {sites.data === undefined && !sites.isError && <p className="browser-loading">reading the sites…</p>}
          {sites.data !== undefined && rows.length === 0 && (
            <Quiet says={`${projectId} has not logged into anything yet.`} />
          )}
          {rows.length > 0 && (
            <Rows label="Site grants">
              {rows.map((site) => (
                <SiteRow
                  key={site.origin}
                  site={site}
                  onRevoke={() => revoke.mutate({ projectId, origin: site.origin })}
                  onReadonly={() => readonly.mutate({ projectId, origin: site.origin })}
                  pending={revoke.isPending || readonly.isPending}
                />
              ))}
            </Rows>
          )}
          {revoke.isError && <MutationNote error={revoke.error} what="that site could not be revoked" />}
          {readonly.isError && (
            <MutationNote error={readonly.error} what="that grant could not be narrowed" />
          )}

          <WriteRecord projectId={projectId} />

          <div className="browser-forget">
            <ConfirmButton
              label="Forget this profile"
              confirmLabel="Forget everything — every site, every session"
              variant="danger"
              disabled={forget.isPending}
              onConfirm={() => forget.mutate(projectId)}
            />
            {forget.data !== undefined && (
              <p className="browser-outcome" role="status">
                stopped {forget.data.stopped} running session{forget.data.stopped === 1 ? "" : "s"} and cleared every site
              </p>
            )}
            {forget.isError && <MutationNote error={forget.error} what="the profile could not be forgotten" />}
          </div>
        </>
      )}
    </Panel>
  );
}

/**
 * One granted origin, with both ways of taking something back.
 *
 * Two buttons and not one, because there are two permissions and a person may
 * want to end only the larger. Revoking removes the site outright — the agent
 * cannot even load it. Making it read-only leaves the reading and ends the
 * submitting, which is the answer to "this has been useful and I would rather
 * it stopped pressing Send".
 */
function SiteRow({
  site,
  onRevoke,
  onReadonly,
  pending,
}: {
  site: Site;
  onRevoke: () => void;
  onReadonly: () => void;
  pending: boolean;
}) {
  return (
    <Row className="browser-row">
      <div className="browser-row-head">
        <span className="browser-url">{site.origin}</span>
        <Badge tone={site.kind === "destination" ? "info" : "shadow"}>{site.kind}</Badge>
        {site.writable && <Badge tone="danger">submits forms</Badge>}
        <RelativeTime at={site.granted_at} />
      </div>
      {site.granted_for !== null && <p className="browser-meta">brought in by {site.granted_for}</p>}
      <div className="browser-actions">
        <ConfirmButton label="Revoke" confirmLabel="Revoke this origin" variant="danger" disabled={pending} onConfirm={onRevoke} />
        {site.writable && (
          <ConfirmButton
            label="Read-only"
            confirmLabel="Stop agents submitting forms here"
            variant="ghost"
            disabled={pending}
            onConfirm={onReadonly}
          />
        )}
      </div>
    </Row>
  );
}

/**
 * What agents have actually submitted, under the grants above.
 *
 * It sits here rather than on a page of its own, and that placement is the
 * argument for the whole feature: a write grant works without asking anyone,
 * so the supervision it allows is necessarily afterwards — and supervision
 * that lives somewhere else is supervision nobody performs. On the screen
 * where the grant comes off, this is what it has been used for.
 *
 * Field names and never values. A form carries passwords, tokens and private
 * text, and the record deliberately cannot say what was typed — only that
 * something was.
 */
function WriteRecord({ projectId }: { projectId: string }) {
  const writes = useBrowserWrites(projectId);
  const rows = writes.data ?? [];

  return (
    // The rule above the heading is this page's; the heading and the rhythm
    // under it are `Section`'s. `level={3}` because the `Panel` around this has
    // already spent the `h2` on "Site grants", and announcing these as siblings
    // is the opposite of what the page means.
    <div className="browser-writes">
      <Section label="Submitted" level={3}>
        {writes.isError && rows.length === 0 && (
          <MutationNote error={writes.error} what="the record of submissions could not be read" />
        )}
        {writes.data !== undefined && rows.length === 0 && (
          <Quiet says="nothing has been submitted from this profile." />
        )}
        {rows.length > 0 && (
          <Rows label="Submitted forms">
            {rows.map((wrote) => (
              <WriteRow key={wrote.id} wrote={wrote} />
            ))}
          </Rows>
        )}
      </Section>
    </div>
  );
}

function WriteRow({ wrote }: { wrote: Written }) {
  // The names that were kept, and the count that is true. They disagree when a long form was
  // truncated, and saying so is better than a list that quietly became "the first few".
  const shown = wrote.fields.join(", ");
  const more = wrote.field_count - wrote.fields.length;

  return (
    <Row className="browser-row">
      <div className="browser-row-head">
        <span className="browser-url">{wrote.action}</span>
        <Badge tone="info">{wrote.method}</Badge>
        <RelativeTime at={wrote.written_at} />
      </div>
      <p className="browser-meta">
        {wrote.field_count} field{wrote.field_count === 1 ? "" : "s"}
        {shown !== "" && <>: {shown}</>}
        {more > 0 && <> and {more} more</>}
      </p>
      {wrote.files.length > 0 && (
        // Its own line, and toned as a warning rather than as detail. "A comment was posted" and
        // "a document was posted" are not the same event, and a person scanning this list for
        // something they did not expect is looking for exactly this difference.
        <p className="browser-meta browser-files">
          with {wrote.files.length === 1 ? "a file" : `${wrote.files.length} files`}:{" "}
          {wrote.files.join(", ")}
        </p>
      )}
      {wrote.verb !== "" && (
        <p className="browser-meta">
          sent by a {wrote.verb}
          {wrote.element_ref !== "" && <> on {wrote.element_ref}</>}
        </p>
      )}
    </Row>
  );
}

/* ------------------------------------------------------------- 5. health -- */

function BrowserHealth({ health }: { health: ReturnType<typeof useBrowserHealth> }) {
  const subsystem = health.data?.subsystem ?? null;
  const sidecar = health.data?.sidecar ?? null;

  return (
    <Panel title="Browser health" variant="dim">
      <p className="browser-note">
        The one state this pillar actually measures: whether the browser sidecar is up. Nothing here
        reports Chromium&apos;s download progress or whether a page can be reached — those are not
        separate readings the daemon takes.
      </p>
      {health.data === undefined && !health.isError && <p className="browser-loading">reading…</p>}
      {health.isError && health.data === undefined && <MutationNote error={health.error} what="nothing is known about the sidecar" />}
      {subsystem !== null && (
        <div className="browser-health-row">
          <StateBadge domain="pillar" state={subsystem.status} />
          {subsystem.reason !== undefined && <span className="browser-meta">reason: {subsystem.reason}</span>}
        </div>
      )}
      {sidecar !== null && (
        <dl className="browser-facts">
          <div className="browser-fact">
            <dt>sidecar state</dt>
            <dd>{sidecar.state}</dd>
          </div>
          {sidecar.started_at !== null && (
            <div className="browser-fact">
              <dt>started</dt>
              <dd>
                <RelativeTime at={sidecar.started_at} />
              </dd>
            </div>
          )}
          <div className="browser-fact">
            <dt>restarts</dt>
            <dd>{sidecar.restarts}</dd>
          </div>
          {sidecar.last_failure !== null && (
            <div className="browser-fact">
              <dt>last failure</dt>
              <dd>{sidecar.last_failure}</dd>
            </div>
          )}
          {sidecar.last_line !== null && (
            <div className="browser-fact">
              <dt>last line</dt>
              <dd>{sidecar.last_line}</dd>
            </div>
          )}
        </dl>
      )}
    </Panel>
  );
}
