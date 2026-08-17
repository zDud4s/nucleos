import { useCallback, useEffect, useState } from "react";
import {
  approveProposal, getProposals, keepBrowserChain, listBrowserSessions, listBrowserSites,
  closeBrowserSession, forgetBrowserProfile, rejectProposal, returnBrowserWheel, revokeBrowserSite,
  type BrowserSession, type BrowserSite, type ConnectionState, type Proposal,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/**
 * The browser pillar, seen from the outside (spec §10).
 *
 * Four things live here, and they are the four a person needs in order to be responsible for a
 * browser they are not looking at:
 *
 * - **What is open**, which url, in whose profile, and who is holding the wheel.
 * - **Who is asking for the wheel**, with the complete literal origin and how the agent got there.
 * - **Where each project has logged in**, and since when.
 * - **The way back**: revoke one host, or forget a whole profile.
 *
 * The last one is not an extra. The site list grows only when a person finishes a login, so it grows
 * for ever unless there is a way back, and what it grows by is a permanent right to load a host
 * inside a profile holding live session cookies. What is given has to be removable, in the same
 * place.
 */

/** Who is driving, in the four states of spec §4.4. */
function ModeBadge({ mode }: { mode: string }) {
  if (mode === "human") return <Badge tone="active">you are driving</Badge>;
  if (mode === "wheel-requested") return <Badge tone="pending">asking for the wheel</Badge>;
  if (mode === "delivery-failed") return <Badge tone="off">the window would not open</Badge>;
  return <Badge tone="shadow">the agent is driving</Badge>;
}

/**
 * Which profile a session ran in — and it is the security fact on this screen.
 *
 * A project profile carries the owner's logins. A throwaway carries nothing, is deleted with the
 * run, and is where anything not on the list goes (spec §5.3). "Throwaway" is the ordinary case and
 * is drawn as the quiet one, because the alarming state is a stranger's page in the profile that has
 * the cookies — which is what the fence exists to make impossible.
 */
function ProfileBadge({ kind, id }: { kind: string; id: string }) {
  return kind === "project" ? (
    <Badge tone="active">{id}&rsquo;s profile</Badge>
  ) : (
    <Badge tone="shadow">throwaway</Badge>
  );
}

/**
 * A wheel request, with the three measures of spec §5.2 against the confused deputy.
 *
 * The host in this dialogue was chosen by an AGENT, and the agent's context contains the words of
 * the page that sent it here. So: the origin is shown complete and literal, in the punycode the
 * daemon stored — `xn--exemp1o-…` is the information, and prettifying it is the attack. Beside it is
 * how the agent arrived, because a permission asked for from a page it followed a link to is not the
 * same request as one from an address a person typed.
 */
function WheelRequest({
  proposal, busy, onAccept, onRefuse,
}: {
  proposal: Proposal;
  busy: boolean;
  onAccept: () => void;
  onRefuse: () => void;
}) {
  const asked = parseAsk(proposal.tool_input);
  return (
    <li className="browser-ask">
      <span className="browser-ask__origin">{asked?.origin ?? "an unnamed host"}</span>
      <span className="browser-ask__why">{proposal.reasoning}</span>
      {asked !== null && (
        <span className="faint">
          The agent asked for <code>{asked.requested_url}</code>
          {asked.final_url !== asked.requested_url && (
            <> and landed on <code>{asked.final_url}</code></>
          )}
          . {proposal.run_id === null
            ? "Asked from a conversation, not a run."
            : `Asked by run ${proposal.run_id}.`}
        </span>
      )}
      <span className="faint">
        Accepting closes the agent&rsquo;s browser and opens a real window over{" "}
        {proposal.project_id ?? "this project"}&rsquo;s profile. Nothing is added to its list by
        accepting — that question comes when you hand the wheel back.
      </span>
      <span className="browser-ask__buttons">
        <Button variant="approve" disabled={busy} onClick={onAccept}>
          Take the wheel
        </Button>
        <Button variant="ghost" disabled={busy} onClick={onRefuse}>
          No
        </Button>
      </span>
    </li>
  );
}

interface WheelAsk {
  origin: string;
  requested_url: string;
  final_url: string;
}

/** The daemon's `tool_input`, or null when it is not the shape this screen knows. */
function parseAsk(raw: string | null): WheelAsk | null {
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<WheelAsk>;
    if (typeof parsed.origin !== "string") return null;
    return {
      origin: parsed.origin,
      requested_url: parsed.requested_url ?? parsed.origin,
      final_url: parsed.final_url ?? parsed.requested_url ?? parsed.origin,
    };
  } catch {
    return null;
  }
}

/**
 * The question spec §5.3a exists for, asked once, at the only moment it can be answered honestly.
 *
 * A login crosses hosts — the destination, an identity provider, and back — so the whole chain is
 * granted together or not at all. Granting them one at a time would mean a login abandoned halfway
 * had already produced a permanent permission.
 *
 * The list is what a browser recorded under the person's own hands. Nothing here was named by an
 * agent, which is why this dialogue is not reachable by the confused deputy above.
 */
function ChainDialogue({
  chain, project, busy, onAnswer,
}: {
  chain: string[];
  project: string;
  busy: boolean;
  onAnswer: (keep: boolean) => void;
}) {
  return (
    <Panel title="Keep these?" aside={project}>
      <p className="faint">
        These are the hosts your window went through. Keeping them lets the agent open them in{" "}
        {project}&rsquo;s profile — the one with your logins in it — until you take it back below.
        They are kept as a set, because a login is a set.
      </p>
      <ul className="browser-chain">
        {chain.map((step, index) => (
          <li key={`${step}-${index}`}>
            <code>{step}</code>
          </li>
        ))}
      </ul>
      <Button variant="approve" disabled={busy} onClick={() => onAnswer(true)}>
        Keep them
      </Button>
      <Button variant="ghost" disabled={busy} onClick={() => onAnswer(false)}>
        Keep none
      </Button>
    </Panel>
  );
}

interface BrowserProps {
  token: string | null;
  connection: ConnectionState;
}

export default function Browser({ token, connection }: BrowserProps) {
  const [sessions, setSessions] = useState<BrowserSession[] | null>(null);
  const [asks, setAsks] = useState<Proposal[]>([]);
  const [project, setProject] = useState("");
  const [sites, setSites] = useState<BrowserSite[] | null>(null);
  const [pending, setPending] = useState<{ session: number; project: string; chain: string[] } | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  const load = useCallback(async () => {
    if (token === null) return;
    const [open, proposals] = await Promise.all([
      listBrowserSessions(token),
      getProposals(token),
    ]);
    setFailed(open === null);
    setSessions(open ?? []);
    setAsks((proposals ?? []).filter((one) => one.kind === "browser-wheel"));
  }, [token]);

  useEffect(() => {
    if (connection !== "connected") return;
    void load();
  }, [connection, load]);

  const loadSites = useCallback(
    async (id: string) => {
      if (token === null || id.trim() === "") return;
      const found = await listBrowserSites(token, id.trim());
      setSites(found ?? []);
    },
    [token],
  );

  const accept = useCallback(
    async (proposal: Proposal) => {
      if (token === null) return;
      setBusy(true);
      setNote(null);
      const outcome = await approveProposal(token, proposal.id);
      setBusy(false);
      if (!outcome.ok) {
        // Spec §4.4a arrives here: the person accepted and the window would not open. The wheel does
        // NOT go back to the agent, so the sentence has to say what happened rather than "failed".
        setNote(outcome.reason);
      }
      await load();
    },
    [token, load],
  );

  const refuse = useCallback(
    async (proposal: Proposal) => {
      if (token === null) return;
      setBusy(true);
      await rejectProposal(token, proposal.id);
      setBusy(false);
      await load();
    },
    [token, load],
  );

  /**
   * Hands the wheel back, and opens the question about what the window went through.
   *
   * Two steps rather than one, because the person has to see the set before agreeing to it — and
   * because a chain that was never answered grants nothing, which is the safe direction.
   */
  const giveBack = useCallback(
    async (session: BrowserSession) => {
      if (token === null) return;
      setBusy(true);
      setNote(null);
      const returned = await returnBrowserWheel(token, session.id);
      setBusy(false);
      if (!returned.ok) {
        setNote("The window could not be closed. It may already be gone.");
        await load();
        return;
      }
      if (returned.value.chain.length > 0) {
        setPending({
          session: session.id,
          project: session.project_id ?? "this project",
          chain: returned.value.chain,
        });
      }
      await load();
    },
    [token, load],
  );

  const answerChain = useCallback(
    async (keep: boolean) => {
      if (token === null || pending === null) return;
      setBusy(true);
      const answered = await keepBrowserChain(token, pending.session, keep);
      setBusy(false);
      setPending(null);
      setNote(
        answered.ok && answered.value.granted.length > 0
          ? `Kept ${answered.value.granted.length} host${answered.value.granted.length === 1 ? "" : "s"}.`
          : keep
            ? "Nothing was kept — none of those hosts could be granted."
            : null,
      );
      if (project.trim() !== "") await loadSites(project);
    },
    [token, pending, project, loadSites],
  );

  if (connection !== "connected" || token === null) {
    return <ErrorNote>The daemon is not reachable, so there is nothing to show.</ErrorNote>;
  }

  if (pending !== null) {
    return (
      <ChainDialogue
        chain={pending.chain}
        project={pending.project}
        busy={busy}
        onAnswer={(keep) => void answerChain(keep)}
      />
    );
  }

  return (
    <>
      <Teach title="Browsing, and who is driving">
        The agent browses headless, behind a fence that lets it read and stops it acting on the world.
        When it meets a login it does not try — it asks, and you take the wheel in a real window. The
        list below is where each project has logged in; it grows only when you finish one of those
        logins, and it is taken back here.
      </Teach>

      {note !== null && <p className="gate-note">{note}</p>}
      {failed && <ErrorNote>Could not read what is open. The pillar may be off.</ErrorNote>}

      {asks.length > 0 && (
        <Panel title="Asking for the wheel" aside={`${asks.length} waiting`}>
          <ul className="browser-asks">
            {asks.map((ask) => (
              <WheelRequest
                key={ask.id}
                proposal={ask}
                busy={busy}
                onAccept={() => void accept(ask)}
                onRefuse={() => void refuse(ask)}
              />
            ))}
          </ul>
        </Panel>
      )}

      <Panel title="Open now" aside={sessions === null ? undefined : `${sessions.length}`}>
        {sessions !== null && sessions.length === 0 && (
          <p className="faint">
            Nothing is open. The pillar stays off until <code>enabled: true</code> in{" "}
            <code>.ai/browser.yaml</code>, and a browser only exists while a session does.
          </p>
        )}
        <ul className="browser-sessions">
          {(sessions ?? []).map((session) => (
            <li key={session.id}>
              <span className="browser-session__url">
                {session.final_url.length > 0 ? session.final_url : session.requested_url}
              </span>
              <span className="browser-session__meta">
                <ProfileBadge kind={session.profile_kind} id={session.profile_id} />
                <ModeBadge mode={session.mode} />
                <span className="faint">opened {relativeTime(session.opened_at)}</span>
              </span>
              {session.refusal !== null && (
                <span className="faint">
                  The fence stopped this page: <code>{session.refusal}</code>. The session exists and
                  is empty.
                </span>
              )}
              <span className="browser-session__buttons">
                {session.mode === "human" && (
                  <Button variant="approve" disabled={busy} onClick={() => void giveBack(session)}>
                    Give the wheel back
                  </Button>
                )}
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={async () => {
                    await closeBrowserSession(token, session.id);
                    await load();
                  }}
                >
                  Close
                </Button>
              </span>
            </li>
          ))}
        </ul>
      </Panel>

      <Panel title="Where a project has logged in" aside={sites === null ? undefined : `${sites.length}`}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void loadSites(project);
          }}
        >
          <input
            type="text"
            value={project}
            placeholder="Project id"
            aria-label="Project id"
            onChange={(event) => setProject(event.target.value)}
          />
          <Button type="submit" disabled={project.trim() === ""}>
            Show
          </Button>
        </form>

        {sites !== null && sites.length === 0 && (
          <p className="faint">
            Nothing. Until somebody logs in through this screen, every page that project visits opens
            in a throwaway profile that is deleted afterwards.
          </p>
        )}

        <ul className="browser-sites">
          {(sites ?? []).map((site) => (
            <li key={site.origin}>
              <code>{site.origin}</code>
              {site.kind === "idp" ? (
                <Badge tone="shadow">signed in through this</Badge>
              ) : (
                <Badge tone="active">logged in here</Badge>
              )}
              <span className="faint">
                since {relativeTime(site.granted_at)}
                {site.granted_for !== null && <> · came with {site.granted_for}</>}
              </span>
              <ConfirmButton
                confirmLabel="Take it back?"
                onConfirm={() => {
                  void (async () => {
                    await revokeBrowserSite(token, project.trim(), site.origin);
                    await loadSites(project);
                  })();
                }}
              >
                Take it back
              </ConfirmButton>
            </li>
          ))}
        </ul>

        {sites !== null && sites.length > 0 && (
          <p className="faint">
            Taking one back stops that host loading in this profile again. The cookies it already left
            stay on disk until the profile itself goes — which is what the button below does.
          </p>
        )}

        {sites !== null && project.trim() !== "" && (
          <ConfirmButton
            confirmLabel="Forget the profile and every login in it?"
            onConfirm={() => {
              void (async () => {
                await forgetBrowserProfile(token, project.trim());
                await loadSites(project);
                await load();
              })();
            }}
          >
            Forget this profile
          </ConfirmButton>
        )}
      </Panel>
    </>
  );
}
