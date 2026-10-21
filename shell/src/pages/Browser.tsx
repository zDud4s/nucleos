import { useEffect, useId, useRef, useState } from "react";
import { Globe } from "lucide-react";
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
  useOpenRealWindow,
  useOpenWindow,
  useOpenWithAgent,
  useTakeWheel,
  useRevokeSite,
  useReturnWheel,
  type BrowserSession,
  type Site,
  type SubsystemReadout,
  type Written,
} from "../data/browser";
import { useSeatNonce } from "../data/seat";
import { isWindows } from "../lib/platform";
import { useProjects } from "../data/system";
import { LiveView } from "./LiveView";
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
import { DAEMON_NAME } from "../copy";
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
  // On Windows the embedded browser replaces the sessions list and "Open real window".
  const windows = isWindows();
  const [chainDialogue, setChainDialogue] = useState<{ sessionId: number; chain: string[] } | null>(null);

  // The bar leads, as the archive's does: opening a window is what this tab is opened for, and an
  // empty list of sessions above it was the first thing anybody read. A pending "keep these?" is
  // the one thing that outranks the list, because the window it asks about has already closed.
  const body = (
    <>
      <OpenBar health={health} />

      {chainDialogue !== null && (
        <ChainDialogue
          sessionId={chainDialogue.sessionId}
          chain={chainDialogue.chain}
          onSettled={() => setChainDialogue(null)}
        />
      )}

      {!windows && (
        <LiveSessions
          view={sessions}
          onReturned={(sessionId, chain) => setChainDialogue({ sessionId, chain })}
        />
      )}

      <SiteGrants />
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

/** The daemon's own sentence, when it really sent one — `RunDetail.tsx`'s pattern. */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding — {what}</ErrorNote>;
}

/* --------------------------------------------------------- live sessions -- */

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
  const [watching, setWatching] = useState<number | null>(null);

  return (
    <Panel title="Live sessions" aside={rows.length > 0 ? <Count n={rows.length} /> : undefined}>
      {/* The empty line says where a session comes from, which is the next step; "nothing is open"
          alone read as a list that failed to load. */}
      {view.data !== undefined && rows.length === 0 && (
        <Quiet says="no windows are open — open one above, or an agent will when it needs a browser." />
      )}
      {rows.length > 0 && (
        <p className="browser-note">
          Oldest first. A session asking for the wheel is answered on <Link to="/waiting">Waiting</Link>.
        </p>
      )}
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
              onReturn={(to) =>
                returnWheel.mutate(to === undefined ? session.id : { sessionId: session.id, to }, {
                  onSuccess: (result) => onReturned(session.id, result.chain),
                })
              }
              returnPending={returnWheel.isPending}
              watching={watching === session.id}
              onWatch={() => setWatching(watching === session.id ? null : session.id)}
              onOpenView={() => setWatching(session.id)}
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
  watching,
  onWatch,
  onOpenView,
}: {
  session: BrowserSession;
  onClose: () => void;
  closePending: boolean;
  onReturn: (to?: "agent" | "close") => void;
  returnPending: boolean;
  watching: boolean;
  onWatch: () => void;
  onOpenView: () => void;
}) {
  const takeWheel = useTakeWheel();
  const openRealWindow = useOpenRealWindow();
  const nonce = useSeatNonce(session.id);
  const shellSeat = session.mode === "human" && session.seat === "shell";
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
        {session.mode === "human" && !shellSeat && (
          <ConfirmButton
            label="Give the wheel back"
            confirmLabel={
              // A visible window stays open: its wheel goes back to the agent, who drives on.
              session.visible
                ? "Hand it back to the agent in its window"
                : "Close the window and bring the chain back"
            }
            variant="approve"
            disabled={returnPending}
            onConfirm={() => onReturn()}
          />
        )}
        {shellSeat && (
          <>
            <Button variant="ghost" aria-pressed={watching} onClick={onWatch}>
              {watching ? "Stop driving" : "Drive"}
            </Button>
            {nonce === undefined && (
              // The shell lost its nonce (restart, reload, a Telegram approval): the core re-issues one.
              // Reversible, so a plain button: a hold is kept for what loses state (Give back, Close).
              <Button
                variant="approve"
                disabled={takeWheel.isPending}
                onClick={() => takeWheel.mutate(session.id, { onSuccess: () => onOpenView() })}
              >
                Drive here
              </Button>
            )}
            <ConfirmButton
              label="Give back to the agent"
              confirmLabel="Restore the fence and hand it back"
              variant="approve"
              disabled={returnPending}
              onConfirm={() => onReturn("agent")}
            />
            <ConfirmButton
              label="Close"
              confirmLabel="Close it and bring the chain back"
              variant="quiet"
              disabled={returnPending}
              onConfirm={() => onReturn("close")}
            />
            <Button variant="ghost" disabled={openRealWindow.isPending} onClick={() => openRealWindow.mutate(session.id)}>
              Open real window
            </Button>
          </>
        )}
        {(session.mode === "agent" || session.mode === "wheel-requested") && (
          <Button variant="ghost" aria-pressed={watching} onClick={onWatch}>
            {watching ? "Stop watching" : "Watch"}
          </Button>
        )}
        {session.mode === "agent" && session.shell_eligible && (
          <ConfirmButton
            label="Take the wheel"
            confirmLabel="Stop the agent and drive it here"
            variant="approve"
            disabled={takeWheel.isPending}
            onConfirm={() => takeWheel.mutate(session.id, { onSuccess: () => onOpenView() })}
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
      {takeWheel.isError && <MutationNote error={takeWheel.error} what="the wheel could not be taken" />}
      {openRealWindow.isError && <MutationNote error={openRealWindow.error} what="the real window could not be opened" />}
      {watching && <LiveView sessionId={session.id} nonce={shellSeat ? (nonce ?? null) : (nonce ?? undefined)} driven={shellSeat} />}
    </Row>
  );
}

/* ------------------------------------------------------- chain dialogue -- */

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
  const consequenceId = useId();
  // The question outranks the list and arrives when the window closes, so a keyboard or
  // screen-reader user who just held "Give the wheel back" is taken to it rather than left behind.
  const dialogueRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    dialogueRef.current?.focus();
  }, []);
  const origins = chain.length === 1 ? "origin" : "origins";
  const grantLabel = writable
    ? `Grant read on ${chain.length} ${origins}, and form submission on the site you ended on`
    : `Grant read on ${chain.length} ${origins}`;

  return (
    <div ref={dialogueRef} className="browser-chain-dialogue" tabIndex={-1} role="group" aria-label="Keep these?">
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
          aria-describedby={consequenceId}
          onChange={(event) => setWritable(event.target.checked)}
        />
        <span>Let agents submit forms here, as you</span>
      </label>
      {/* The consequence sits right under the box that causes it, and says what the answer covers:
          reading reaches every origin above, identity providers included. */}
      <p className="browser-note" id={consequenceId}>
        Agents will read all {chain.length} {origins} above, identity providers included.{" "}
        {writable
          ? "They may also submit forms on the site you ended on, never on the identity providers, without asking you again."
          : "They will not submit any form."}{" "}
        Every submission is listed under Site grants, by field names and never contents.
      </p>
      <div className="browser-actions">
        <ConfirmButton
          label="Keep them"
          confirmLabel={grantLabel}
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
    </div>
  );
}

/* -------------------------------------------------------------- open a window -- */

/**
 * The one door into this pillar that no agent asked for, as the tab's own bar — the archive leads
 * with one field and so does this, so the two tabs read as one page.
 *
 * Deliberately plain — a picker, a field and a button, no proposal and no confirmation — and the
 * plainness is the argument. Every ceremony elsewhere on this screen defends against an AGENT
 * having chosen a destination while carrying a stranger's words; here the person typed the
 * address, so there is nobody to approve. Asking them to approve their own request is the
 * ceremony that teaches people to click through the one that matters.
 *
 * What it is for: until it existed a profile could be repaired, never prepared — the only way to
 * log in was to wait for the agent to walk into the login first. What a session may GRANT is
 * unchanged; the window records where it went and the chain still answers on the way out.
 *
 * The sidecar's state lives here and not in a panel of its own at the bottom of the page: down or
 * not configured, the hint says so where it bites, with the last failure under it. The button stays
 * live. Disabled, it read as "an empty address is not allowed" — the one thing the field promises
 * is — and the daemon's own refusal, rendered below, says the rest if it is pressed anyway.
 *
 * Its own project picker rather than one lifted out of `SiteGrants`: the two answer different
 * questions, and choosing which project's grants to read should not move where a window opens.
 */
/**
 * What a window opens on when nobody named an address: the address is optional, because the usual
 * reason to open one is to go and log in somewhere, and typing it into this form first is a step
 * the window's own address bar already does. `about:blank` is safe to record in the chain the
 * window keeps: `origin_of` admits `https` origins only, so it can never be offered as a host.
 */
const BLANK_PAGE = "about:blank";

function OpenBar({ health }: { health: ReturnType<typeof useBrowserHealth> }) {
  const projects = useProjects();
  const [chosen, setChosen] = useState<string | undefined>(undefined);
  const [url, setUrl] = useState("");
  const open = useOpenWindow();
  const withAgent = useOpenWithAgent();
  const hintId = useId();
  const options = projects.data ?? [];
  const projectId = chosen ?? options[0]?.project_id;
  const subsystem = health.data?.subsystem ?? null;
  const unavailable = subsystem !== null && (subsystem.status === "down" || subsystem.status === "disabled");
  const ready = projectId !== undefined && !open.isPending && !withAgent.isPending;
  // The agent's browser opens `https` pages only, so there is no blank start with the agent.
  const readyWithAgent = ready && url.trim() !== "";

  if (projects.data !== undefined && options.length === 0) {
    return (
      <div className="browser-bar-block">
        <Quiet says="no project is registered yet — a window opens on a project's profile." />
        {unavailable && <SidecarTrouble health={health} />}
      </div>
    );
  }

  return (
    <div className="browser-bar-block">
      <form
        className="browser-bar"
        aria-label="Open a window"
        onSubmit={(event) => {
          event.preventDefault();
          if (projectId === undefined || !ready) return;
          open.mutate({ projectId, url: url.trim() === "" ? BLANK_PAGE : url.trim() });
        }}
      >
        {/* A visible label: the window opens on this project's profile, logins included, and the
            first project is selected until somebody chooses, so the choice has to be readable. */}
        <label className="browser-bar-project">
          <span>Project</span>
          <select value={projectId ?? ""} onChange={(event) => setChosen(event.target.value)}>
            {options.map((option) => (
              <option key={option.project_id} value={option.project_id}>
                {option.project_id}
              </option>
            ))}
          </select>
        </label>
        <label className="browser-bar-field">
          <Globe aria-hidden="true" size={16} strokeWidth={1.75} />
          <input
            type="text"
            aria-label="Address to open"
            aria-describedby={hintId}
            placeholder="Address to open"
            autoComplete="off"
            spellCheck={false}
            value={url}
            onChange={(event) => setUrl(event.target.value)}
          />
        </label>
        {/* A plain Button and not a ConfirmButton: this write is additive and reversible — the
            window closes, and it grants nothing on its own. */}
        <span className="browser-bar-buttons">
        <Button type="submit" variant="approve" disabled={!ready}>
          {open.isPending ? "Opening…" : "Open a window"}
        </Button>
        <Button
          type="button"
          variant="ghost"
          disabled={!readyWithAgent}
          onClick={() => {
            if (projectId === undefined || !readyWithAgent) return;
            withAgent.mutate({ projectId, url: url.trim() });
          }}
        >
          {withAgent.isPending ? "Opening…" : "Open with agent"}
        </Button>
        </span>
      </form>
      <p id={hintId} className="browser-bar-hint">
        {unavailable
          ? subsystem.status === "disabled"
            ? "The browser is not configured on this machine, so no window can open."
            : "The browser is not running, so no window can open until it restarts."
          : "Open a window: you drive, on the project's profile; the address is optional. Open with agent: the agent drives and you can take over; it needs an address."}
      </p>
      {unavailable && <SidecarTrouble health={health} />}

      {open.data !== undefined && (
        <p className="browser-outcome" role="status">
          opened session #{open.data.id} on the {open.data.profile_kind} profile {open.data.profile_id} —
          give it back below when you are done
        </p>
      )}
      {open.isError && <MutationNote error={open.error} what="the window could not be opened" />}
      {withAgent.data !== undefined && (
        <p className="browser-outcome" role="status">
          opened session #{withAgent.data.session.id} with the agent
          {withAgent.data.session.profile_kind === "ephemeral"
            ? " in a throwaway profile, with none of the project's logins — to use your login here, log in with Open a window and keep the site first"
            : ""}{" "}
          —{" "}
          <Link to="/chats/$chatId" params={{ chatId: withAgent.data.chat_id }}>
            open its conversation
          </Link>
        </p>
      )}
      {withAgent.isError && (
        <MutationNote error={withAgent.error} what="the window with the agent could not be opened" />
      )}
    </div>
  );
}

/**
 * Why the sidecar is down, in one line under the hint: the last failure in the daemon's own words,
 * how many times it has been restarted, and the door to the full record on System. It was a
 * disclosure of five mono facts, which put a status page's table on a page that only needs to know
 * whether a window can open. **One subsystem, `browser_sidecar`** — `health.rs` rejected folding
 * the Chromium download and page reachability into it, so this says nothing about either.
 */
function SidecarTrouble({ health }: { health: ReturnType<typeof useBrowserHealth> }) {
  const sidecar = health.data?.sidecar ?? null;
  const failure = sidecar?.last_failure ?? null;
  const restarts = sidecar?.restarts ?? 0;

  return (
    <p className="browser-bar-trouble">
      {failure !== null && (
        <>
          Last failure: <span className="browser-bar-failure">{failure}</span>
          {" · "}
        </>
      )}
      {restarts > 0 && (
        <>
          {restarts} restart{restarts === 1 ? "" : "s"}
          {" · "}
        </>
      )}
      <Link to="/system/$view" params={{ view: "health" }}>
        Sidecar record on System
      </Link>
    </p>
  );
}

/* ------------------------------------------------------------- site grants -- */

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

  // The picker sits in the panel's head, where the count was: it says whose grants these are, and
  // on its own line at 20rem it was the widest thing in a panel that was usually empty.
  const picker =
    options.length > 0 ? (
      <select
        className="browser-head-select"
        aria-label="Project whose grants are shown"
        value={projectId ?? ""}
        onChange={(event) => {
          setChosen(event.target.value);
          // Outcomes and errors belong to the project that produced them; left standing they read
          // as if the newly chosen project had just been revoked or forgotten.
          revoke.reset();
          readonly.reset();
          forget.reset();
        }}
      >
        {options.map((project) => (
          <option key={project.project_id} value={project.project_id}>
            {project.project_id}
          </option>
        ))}
      </select>
    ) : undefined;

  return (
    <Panel title="Site grants" aside={picker}>
      {/* Origins are shown exactly as recorded: a punycode host is never prettified back to the
          glyphs it encodes. */}
      <p className="browser-note">
        Sites this project&apos;s profile has logged into. Agents may read them; only the ones
        marked &ldquo;submits forms&rdquo; may send anything.
      </p>

      {projects.data !== undefined && options.length === 0 && <Quiet says="no project is registered yet." />}

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

          <WriteRecord projectId={projectId} anyWritable={rows.some((site) => site.writable)} />

          <div className="browser-forget">
            <ConfirmButton
              label="Forget this profile"
              confirmLabel={`Forget ${projectId}: every site, every login, every session`}
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
        {/* The drawn confirm labels stay short and the origin goes to `sayAs`: the button
            reserves the width of its confirm label, and with the origin in it "Stop
            submitting" was a 360px button that broke onto two lines at 800px. The row's head
            already names the site for the eye. */}
        <ConfirmButton
          label="Revoke"
          confirmLabel="Revoke this site"
          sayAs={`Revoke ${site.origin}`}
          variant="danger"
          disabled={pending}
          onConfirm={onRevoke}
        />
        {site.writable && (
          <ConfirmButton
            label="Stop submitting"
            confirmLabel="Stop form submits"
            sayAs={`Stop agents submitting forms on ${site.origin}`}
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
function WriteRecord({ projectId, anyWritable }: { projectId: string; anyWritable: boolean }) {
  const writes = useBrowserWrites(projectId);
  const rows = writes.data ?? [];
  // Nothing submitted is the usual state, and a heading between two rules to say so was the
  // heaviest thing in an empty panel. The record appears once there is something in it.
  // With a site that may submit and nothing in the record, "nothing yet" is information, and its
  // absence cannot be told from a record that did not load.
  if (rows.length === 0 && !writes.isError) {
    if (!anyWritable || writes.data === undefined) return null;
    return (
      <div className="browser-writes">
        <Quiet says="nothing has been submitted yet." />
      </div>
    );
  }

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
      <p className="browser-meta browser-write-meta">
        {wrote.field_count} field{wrote.field_count === 1 ? "" : "s"}
        {shown !== "" && <>: {shown}</>}
        {more > 0 && <> and {more} more</>}
      </p>
      {wrote.files.length > 0 && (
        // Its own line, and toned as a warning rather than as detail. "A comment was posted" and
        // "a document was posted" are not the same event, and a person scanning this list for
        // something they did not expect is looking for exactly this difference.
        <p className="browser-meta browser-write-meta browser-files">
          with {wrote.files.length === 1 ? "a file" : `${wrote.files.length} files`}:{" "}
          {wrote.files.join(", ")}
        </p>
      )}
      {wrote.verb !== "" && (
        <p className="browser-meta browser-write-meta">
          sent by a {wrote.verb}
          {wrote.element_ref !== "" && <> on {wrote.element_ref}</>}
        </p>
      )}
    </Row>
  );
}
